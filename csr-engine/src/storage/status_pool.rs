//! The status-intent pool read: the newest report chunks of given sessions.
//!
//! A status question ("current state of X, what is left") asks for the newest
//! report on X. The first stage ranks by relevance, so the newest report of a
//! matched session can sit outside the candidate window. This reads it straight
//! from the table: one indexed query, only for status questions.

use anyhow::Result;
use rusqlite::Connection;

/// At most `per_session` newest chunks of each session in `conversation_ids`
/// whose stored author is the user or the assistant (tool output never is a
/// report) and which carry at least `min_hits` of the lower-cased `stems`.
/// The subject filter is in the query because the newest chunks of a long
/// session are usually about something else. Rows come back grouped by
/// session, newest first: `(chunk_id, conversation_id, timestamp)`. Empty
/// input, or an empty stem list, reads nothing.
pub fn newest_report_chunks(
    conn: &Connection,
    conversation_ids: &[String],
    per_session: usize,
    stems: &[String],
    min_hits: usize,
) -> Result<Vec<(String, String, String)>> {
    if conversation_ids.is_empty() || per_session == 0 || stems.is_empty() {
        return Ok(Vec::new());
    }
    let sessions = vec!["?"; conversation_ids.len()].join(",");
    let hits = stems
        .iter()
        .map(|_| "(instr(lower(c.content), ?) > 0)")
        .collect::<Vec<_>>()
        .join(" + ");
    let sql = format!(
        "SELECT id, conversation_id, timestamp FROM (
             SELECT c.id AS id, c.conversation_id AS conversation_id,
                    c.timestamp AS timestamp,
                    ROW_NUMBER() OVER (
                        PARTITION BY c.conversation_id
                        ORDER BY c.timestamp DESC, c.seq DESC, c.id DESC
                    ) AS rank
               FROM chunks c
               JOIN chunk_provenance p ON p.chunk_id = c.id
              WHERE c.conversation_id IN ({sessions})
                AND p.author IN ('user', 'assistant')
                AND ({hits}) >= ?
         )
         WHERE rank <= ?
         ORDER BY conversation_id, timestamp DESC, id DESC"
    );
    let mut params: Vec<rusqlite::types::Value> = conversation_ids
        .iter()
        .map(|id| rusqlite::types::Value::Text(id.clone()))
        .collect();
    params.extend(
        stems
            .iter()
            .map(|stem| rusqlite::types::Value::Text(stem.to_lowercase())),
    );
    params.push(rusqlite::types::Value::Integer(
        i64::try_from(min_hits).unwrap_or(i64::MAX),
    ));
    params.push(rusqlite::types::Value::Integer(
        i64::try_from(per_session).unwrap_or(i64::MAX),
    ));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::Speaker;
    use crate::storage::Storage;

    fn put(storage: &Storage, id: &str, session: &str, when: &str, author: Speaker, text: &str) {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: session.into(),
            project_name: "p".into(),
            timestamp: when.into(),
            content: text.into(),
            message_count: 1,
            summary: None,
            author,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[0.0; 4]).unwrap();
        storage
            .insert_chunk_provenance(
                id,
                &crate::provenance::ChunkProvenance {
                    author,
                    source_conv_id: String::new(),
                    supersedes: None,
                },
            )
            .unwrap();
    }

    fn stems() -> Vec<String> {
        ["vidhi", "androi", "maestr", "harden", "workfl"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn f3b_status_pool_reads_newest_user_assistant_chunks_with_the_subject() {
        let storage = Storage::open_memory().unwrap();
        let report = "Vidhi Android maestro status";
        for (index, minute) in ["01", "02", "03", "04"].iter().enumerate() {
            put(
                &storage,
                &format!("a{index}"),
                "s1",
                &format!("2026-09-15T04:{minute}:00Z"),
                Speaker::Assistant,
                report,
            );
        }
        // Newest of all, but tool output and off topic: never admitted.
        put(
            &storage,
            "tool",
            "s1",
            "2026-09-15T09:00:00Z",
            Speaker::ToolResult,
            report,
        );
        put(
            &storage,
            "off",
            "s1",
            "2026-09-15T09:30:00Z",
            Speaker::Assistant,
            "radio",
        );
        put(
            &storage,
            "other",
            "s2",
            "2026-09-15T05:00:00Z",
            Speaker::User,
            report,
        );
        put(
            &storage,
            "unasked",
            "s3",
            "2026-09-15T05:00:00Z",
            Speaker::User,
            report,
        );

        let rows = storage
            .with_connection(|conn| {
                newest_report_chunks(conn, &["s1".into(), "s2".into()], 3, &stems(), 3)
            })
            .unwrap();
        let ids: Vec<&str> = rows.iter().map(|row| row.0.as_str()).collect();
        assert_eq!(ids, ["a3", "a2", "a1", "other"]);
        assert_eq!(rows[0].1, "s1");
        assert_eq!(rows[0].2, "2026-09-15T04:04:00Z");
        // No sessions, no stems: nothing read.
        let none = storage
            .with_connection(|conn| newest_report_chunks(conn, &[], 3, &stems(), 3))
            .unwrap();
        assert!(none.is_empty());
    }
}
