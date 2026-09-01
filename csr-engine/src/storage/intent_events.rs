use std::path::PathBuf;

use anyhow::Result;
use rusqlite::{params, Connection};

use crate::transcript::intent_events::{IntentEvent, IntentEventKind};

pub const MIGRATION_ID: &str = "intent_events_v1";

fn parse_kind(value: &str) -> rusqlite::Result<IntentEventKind> {
    match value {
        "correction" => Ok(IntentEventKind::Correction),
        "redirect" => Ok(IntentEventKind::Redirect),
        "abandoned" => Ok(IntentEventKind::Abandoned),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("unknown intent event kind: {other}").into(),
        )),
    }
}

pub(crate) fn insert(conn: &Connection, events: &[IntentEvent]) -> Result<usize> {
    let transaction = conn.unchecked_transaction()?;
    let mut inserted = 0usize;
    {
        let mut statement = transaction.prepare_cached(
            "INSERT OR IGNORE INTO intent_events
             (session_id, project, turn, kind, quote, transcript_path,
              byte_start, byte_end, prior_claim, symbol, file, classifier_hash, ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        for event in events {
            inserted += statement.execute(params![
                event.session_id,
                event.project,
                i64::from(event.turn),
                event.kind.as_str(),
                event.quote,
                event.transcript_path.to_string_lossy(),
                i64::try_from(event.byte_start)?,
                i64::try_from(event.byte_end)?,
                event.prior_claim,
                event.symbol,
                event.file,
                event.classifier_hash,
                event.ts,
            ])?;
        }
    }
    transaction.commit()?;
    Ok(inserted)
}

pub(crate) fn list(
    conn: &Connection,
    project: Option<&str>,
    since: Option<&str>,
) -> Result<Vec<IntentEvent>> {
    let mut statement = conn.prepare(
        "SELECT session_id, project, turn, kind, quote, transcript_path,
                byte_start, byte_end, prior_claim, symbol, file, classifier_hash, ts
         FROM intent_events
         WHERE (?1 IS NULL OR project = ?1)
           AND (?2 IS NULL OR julianday(ts) >= julianday(?2))
         ORDER BY julianday(ts) ASC, id ASC",
    )?;
    let rows = statement.query_map(params![project, since], |row| {
        let turn: i64 = row.get(2)?;
        let byte_start: i64 = row.get(6)?;
        let byte_end: i64 = row.get(7)?;
        Ok(IntentEvent {
            session_id: row.get(0)?,
            project: row.get(1)?,
            turn: u32::try_from(turn).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })?,
            kind: parse_kind(&row.get::<_, String>(3)?)?,
            quote: row.get(4)?,
            transcript_path: PathBuf::from(row.get::<_, String>(5)?),
            byte_start: usize::try_from(byte_start).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    6,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })?,
            byte_end: usize::try_from(byte_end).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })?,
            prior_claim: row.get(8)?,
            symbol: row.get(9)?,
            file: row.get(10)?,
            classifier_hash: row.get(11)?,
            ts: row.get(12)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub(crate) fn count(
    conn: &Connection,
    project: Option<&str>,
    since: Option<&str>,
) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM intent_events
         WHERE (?1 IS NULL OR project = ?1)
           AND (?2 IS NULL OR julianday(ts) >= julianday(?2))",
        params![project, since],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(count)?)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::storage::Storage;
    use crate::transcript::intent_events::{IntentEvent, IntentEventKind};

    fn event() -> IntentEvent {
        IntentEvent {
            session_id: "session-1".into(),
            project: "project-a".into(),
            turn: 7,
            kind: IntentEventKind::Correction,
            quote: "No, use `new_parser`".into(),
            transcript_path: PathBuf::from("/tmp/session-1.jsonl"),
            byte_start: 100,
            byte_end: 120,
            prior_claim: "I used the old parser.".into(),
            symbol: Some("new_parser".into()),
            file: None,
            classifier_hash: "classifier-v1".into(),
            ts: "2026-09-01T10:00:00Z".into(),
        }
    }

    #[test]
    fn duplicate_identity_is_ignored_and_rows_roundtrip() {
        let storage = Storage::open_memory().unwrap();
        let event = event();

        assert_eq!(
            storage
                .insert_intent_events(std::slice::from_ref(&event))
                .unwrap(),
            1
        );
        assert_eq!(storage.insert_intent_events(&[event]).unwrap(), 0);

        let rows = storage.list_intent_events(Some("project-a"), None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].quote, "No, use `new_parser`");
        assert_eq!(rows[0].byte_start, 100);
        assert_eq!(rows[0].symbol.as_deref(), Some("new_parser"));
        assert_eq!(
            storage
                .count_intent_events(Some("project-a"), None)
                .unwrap(),
            1
        );
    }

    #[test]
    fn database_triggers_reject_update_and_delete() {
        let storage = Storage::open_memory().unwrap();
        storage.insert_intent_events(&[event()]).unwrap();

        let update = storage.with_connection(|conn| {
            conn.execute("UPDATE intent_events SET quote = 'rewritten'", [])?;
            Ok(())
        });
        let delete = storage.with_connection(|conn| {
            conn.execute("DELETE FROM intent_events", [])?;
            Ok(())
        });

        assert!(update.unwrap_err().to_string().contains("append-only"));
        assert!(delete.unwrap_err().to_string().contains("append-only"));
    }
}
