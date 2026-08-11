//! Durable usage reservations — spend accounting that cannot fail open.
//!
//! `narrative_usage` is written *after* a model call returns. Anything that
//! happens between "we decided to invoke" and "we wrote the row" — a crash, a
//! discarded insert error, a killed daemon — spends real tokens that no row
//! records. The per-dream spend figure then reads LOW, or reads "unmeasured"
//! when the honest answer is "spent, amount unknown".
//!
//! A reservation closes that window by making the window itself a durable
//! row: [`reserve`] before the invocation, [`finalise`] after it, [`abandon`]
//! when the call provably never happened.
//!
//! | state | meaning |
//! |---|---|
//! | `reserved` | the invocation started and its outcome is unknown. **Evidence of an unaccounted call**, never evidence of zero spend. |
//! | `finalised` | `usage_id` points at the `narrative_usage` row that measured it. |
//! | `abandoned` | the call provably did not happen (gate refused, budget exhausted before invoking). |
//!
//! Defined for Journal v4 Wave 3, which owns the producers that will call it.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

/// A reservation as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    pub id: i64,
    pub attempt_key: String,
    pub ref_id: Option<String>,
    pub call_site: String,
    pub model: Option<String>,
    pub state: String,
    pub usage_id: Option<i64>,
    pub reserved_at: String,
    pub settled_at: Option<String>,
    pub note: Option<String>,
}

/// Claim `attempt_key` before invoking. Returns the reservation row id.
///
/// Idempotent on `attempt_key`: a retried reservation reuses its row rather
/// than double-counting the same intended call. Errors propagate — a
/// reservation that could not be written must stop the invocation, because
/// spending without a reservation is exactly the fail-open this exists to
/// prevent.
pub fn reserve(
    conn: &Connection,
    attempt_key: &str,
    call_site: &str,
    ref_id: Option<&str>,
    model: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO narrative_reservations
            (attempt_key, ref_id, call_site, model)
         VALUES (?1, ?2, ?3, ?4)",
        params![attempt_key, ref_id, call_site, model],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM narrative_reservations WHERE attempt_key = ?1",
        params![attempt_key],
        |row| row.get(0),
    )?;
    Ok(id)
}

/// Settle a reservation against the `narrative_usage` row that measured it.
///
/// Only a `reserved` row may be finalised: a settled reservation is never
/// rewritten, so a late duplicate cannot overwrite what was already measured.
/// Returns `false` when nothing was in `reserved` state.
pub fn finalise(conn: &Connection, attempt_key: &str, usage_id: i64) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE narrative_reservations
            SET state = 'finalised', usage_id = ?2, settled_at = datetime('now')
          WHERE attempt_key = ?1 AND state = 'reserved'",
        params![attempt_key, usage_id],
    )?;
    Ok(updated > 0)
}

/// Settle a reservation whose call provably never happened. `note` records
/// *why* — it is the only thing distinguishing "we chose not to spend" from
/// "we do not know what happened", and the latter must stay `reserved`.
pub fn abandon(conn: &Connection, attempt_key: &str, note: &str) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE narrative_reservations
            SET state = 'abandoned', settled_at = datetime('now'), note = ?2
          WHERE attempt_key = ?1 AND state = 'reserved'",
        params![attempt_key, note],
    )?;
    Ok(updated > 0)
}

/// Load one reservation by key.
pub fn load(conn: &Connection, attempt_key: &str) -> Result<Option<Reservation>> {
    let mut stmt = conn.prepare(
        "SELECT id, attempt_key, ref_id, call_site, model, state, usage_id,
                reserved_at, settled_at, note
         FROM narrative_reservations WHERE attempt_key = ?1",
    )?;
    let row = stmt
        .query_row(params![attempt_key], |row| {
            Ok(Reservation {
                id: row.get(0)?,
                attempt_key: row.get(1)?,
                ref_id: row.get(2)?,
                call_site: row.get(3)?,
                model: row.get(4)?,
                state: row.get(5)?,
                usage_id: row.get(6)?,
                reserved_at: row.get(7)?,
                settled_at: row.get(8)?,
                note: row.get(9)?,
            })
        })
        .optional()?;
    Ok(row)
}

/// How many reservations are still open — i.e. how many invocations started
/// without their spend ever being measured. A non-zero count is a *known
/// unknown* and must be surfaced as such, never rounded to zero.
pub fn unaccounted_count(conn: &Connection) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM narrative_reservations WHERE state = 'reserved'",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().expect("memory db");
        crate::storage::migrations::run(&conn).expect("migrations");
        conn
    }

    #[test]
    fn a_reservation_is_durable_before_the_call_and_settled_after() {
        let conn = conn();
        let id = reserve(&conn, "k1", "dream_plan", Some("hash-a"), Some("sonnet-5")).expect("res");
        assert!(id > 0);
        assert_eq!(unaccounted_count(&conn).expect("count"), 1);

        assert!(finalise(&conn, "k1", 42).expect("finalise"));
        let row = load(&conn, "k1").expect("load").expect("row");
        assert_eq!(row.state, "finalised");
        assert_eq!(row.usage_id, Some(42));
        assert!(row.settled_at.is_some());
        assert_eq!(unaccounted_count(&conn).expect("count"), 0);
    }

    #[test]
    fn an_unsettled_reservation_is_counted_as_unaccounted_not_as_zero_spend() {
        let conn = conn();
        reserve(&conn, "k1", "dream_plan", None, None).expect("res");
        assert_eq!(
            unaccounted_count(&conn).expect("count"),
            1,
            "a call that started and never reported is a known unknown"
        );
        let row = load(&conn, "k1").expect("load").expect("row");
        assert_eq!(row.state, "reserved");
        assert_eq!(row.usage_id, None);
    }

    #[test]
    fn reserving_twice_under_one_key_claims_one_row() {
        let conn = conn();
        let first = reserve(&conn, "k1", "dream_plan", None, None).expect("first");
        let second = reserve(&conn, "k1", "dream_plan", None, None).expect("second");
        assert_eq!(first, second);
        assert_eq!(unaccounted_count(&conn).expect("count"), 1);
    }

    #[test]
    fn a_settled_reservation_is_never_rewritten() {
        let conn = conn();
        reserve(&conn, "k1", "dream_plan", None, None).expect("res");
        assert!(finalise(&conn, "k1", 7).expect("finalise"));
        assert!(
            !finalise(&conn, "k1", 9).expect("second finalise"),
            "a late duplicate must not overwrite a measured row"
        );
        assert!(
            !abandon(&conn, "k1", "budget").expect("abandon"),
            "a finalised call cannot be retroactively declared not to have happened"
        );
        assert_eq!(
            load(&conn, "k1").expect("load").expect("row").usage_id,
            Some(7)
        );
    }

    #[test]
    fn abandoning_records_why_the_call_never_happened() {
        let conn = conn();
        reserve(&conn, "k1", "dream_plan", None, None).expect("res");
        assert!(abandon(&conn, "k1", "budget exhausted before invoking").expect("abandon"));
        let row = load(&conn, "k1").expect("load").expect("row");
        assert_eq!(row.state, "abandoned");
        assert_eq!(
            row.note.as_deref(),
            Some("budget exhausted before invoking")
        );
        assert_eq!(unaccounted_count(&conn).expect("count"), 0);
    }
}
