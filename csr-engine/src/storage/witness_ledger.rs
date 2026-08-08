//! Append-only witness ledger (v10 "dreaming" substrate).
//!
//! Schema lives in `storage::migrations` (table `witness_ledger`). Stores
//! `codewitness` stamps — content-hash claims anchored to a file/symbol/span
//! at a specific git commit (or the worktree) — so future audits can check
//! whether a claim still holds without re-deriving it from scratch or
//! trusting a wall-clock staleness heuristic. Populated today by
//! `import::backfill::backfill_stamp_spans` (`codegraph stamp-spans`); read
//! by future "dreaming" (evidence-grounded forgetting) passes.
//!
//! # Append-only invariant
//!
//! This module exposes INSERT and QUERY functions ONLY. There is
//! deliberately no UPDATE or DELETE for `witness_ledger`: a witness is
//! historical evidence — "at commit X, file/symbol Y had content Z" — and
//! rewriting or removing a row would destroy the audit trail the ledger
//! exists to provide. A witness that no longer holds is superseded by
//! inserting a NEW row (a fresh stamp at a later commit or a later
//! `try_audit` verdict), never by mutating the old one. If a future need for
//! correction arises, model it as a new `source_kind` layered ON TOP of the
//! ledger (e.g. a `'retraction'` row), never as a mutation of existing rows.
//!
//! Duplicate inserts (e.g. an idempotent backfill re-run) are a silent
//! no-op: [`insert_witness`] issues `INSERT OR IGNORE`, and the
//! `idx_witness_ledger_identity` UNIQUE expression index (see
//! `migrations::run`'s doc comment on this table) `COALESCE`-normalizes the
//! nullable key columns, so whole-file witnesses (`symbol`/`span_start`/
//! `span_end` all `NULL`) dedupe atomically at the DB level exactly like
//! symbol-level rows — no application-level pre-insert check is needed.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

/// Current extractor protocol version. Bump whenever re-derived anchor
/// identity or span semantics change. Persisted in `witness_generations` by
/// the publisher (`import::backfill`) and required by the binder
/// (`storage::chunk_binding`) when selecting the preferred generation — a
/// single definition so publisher and binder can never drift apart.
pub(crate) const WITNESS_EXTRACTOR_VERSION: &str = "codegraph-v3";

/// One re-derivation run's publication manifest. This bookkeeping table may
/// be appended independently of the evidence ledger: COMPLETE is inserted in
/// the same transaction as every ledger row; INCOMPLETE records a failed run
/// with no corresponding v2 evidence rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessGeneration {
    pub id: i64,
    pub generation_id: String,
    pub project: String,
    pub file: String,
    pub repo_root: Option<String>,
    pub head_oid: String,
    pub extractor_version: String,
    pub status: String,
}

/// One row of the witness ledger — a `codewitness` stamp anchored to a file
/// (optionally a symbol/span) at a specific tier/commit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WitnessLedgerRow {
    /// `witness_ledger.id` (the DB primary key). `0` on a not-yet-inserted
    /// row constructed for [`insert_witness`] — that call's `INSERT` never
    /// references this field, so it is safe to leave at its `Default`.
    /// Populated (non-zero) on every row read back via a query function in
    /// this module — `dream`'s successor join uses it as the foreign key it
    /// stamps into `witness_verdicts.witness_id`.
    pub id: i64,
    pub project: String,
    /// Absolute path, consistent with `code_edges.src_file` convention.
    pub file: String,
    /// `None` = whole-file witness.
    pub symbol: Option<String>,
    /// Line range when symbol-level; `None` for whole-file witnesses. Same
    /// 0-based convention as `code_nodes.span_start`/`span_end` (NOT the
    /// 1-based inclusive range `codewitness::Anchor` expects — callers
    /// convert at the `Anchor` construction boundary, see
    /// `import::backfill::backfill_stamp_spans`).
    pub span_start: Option<i64>,
    pub span_end: Option<i64>,
    /// codewitness stamp string, `"b3:<hex>"` or `"b3n:<hex>"`.
    pub stamp: String,
    /// 'worktree' | 'committed'.
    pub tier: String,
    /// Commit OID for `tier = 'committed'`; `None` for `'worktree'`.
    pub at_oid: Option<String>,
    /// 'backfill' | 'conversation' | 'commit'.
    pub source_kind: String,
    /// Conversation id / commit sha / `None`.
    pub source_id: Option<String>,
}

/// Insert one witness row. A duplicate (identical on every
/// `idx_witness_ledger_identity` key column, NULLs `COALESCE`-normalized) is
/// a silent no-op (`INSERT OR IGNORE` against that UNIQUE index) — never an
/// error, never an update. Whole-file (NULL-key) rows dedupe the same way;
/// see the module-level append-only invariant doc.
pub fn insert_witness(conn: &Connection, row: &WitnessLedgerRow) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO witness_ledger
            (project, file, symbol, span_start, span_end, stamp, tier, at_oid, source_kind, source_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            row.project,
            row.file,
            row.symbol,
            row.span_start,
            row.span_end,
            row.stamp,
            row.tier,
            row.at_oid,
            row.source_kind,
            row.source_id,
        ],
    )?;
    Ok(())
}

/// Record a generation manifest. Callers publish COMPLETE inside the same
/// transaction as its ledger rows; an INCOMPLETE manifest carries no rows.
pub fn insert_generation(conn: &Connection, generation: &WitnessGeneration) -> Result<()> {
    conn.execute(
        "INSERT INTO witness_generations
         (generation_id, project, file, repo_root, head_oid, extractor_version, status,
          completed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                 CASE WHEN ?7 = 'complete' THEN datetime('now') END)",
        params![
            generation.generation_id,
            generation.project,
            generation.file,
            generation.repo_root,
            generation.head_oid,
            generation.extractor_version,
            generation.status,
        ],
    )?;
    Ok(())
}

/// True when cadence can skip all extraction/stamping work for this exact
/// file, HEAD, and extractor protocol.
pub fn complete_generation_exists(
    conn: &Connection,
    project: &str,
    file: &str,
    head_oid: &str,
    extractor_version: &str,
) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM witness_generations
             WHERE project = ?1 AND file = ?2 AND head_oid = ?3
               AND extractor_version = ?4 AND status = 'complete'
         )",
        params![project, file, head_oid, extractor_version],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// Complete generations for a file, newest publication first. Binding still
/// applies git ancestry before choosing one; insertion order only breaks ties
/// between distinct successful runs at the same causal HEAD.
pub fn complete_generations_for_file(
    conn: &Connection,
    project: &str,
    file: &str,
) -> Result<Vec<WitnessGeneration>> {
    let mut stmt = conn.prepare(
        "SELECT id, generation_id, project, file, repo_root, head_oid,
                extractor_version, status
         FROM witness_generations
         WHERE project = ?1 AND file = ?2 AND status = 'complete'
         ORDER BY id DESC",
    )?;
    let rows = stmt.query_map(params![project, file], |row| {
        Ok(WitnessGeneration {
            id: row.get(0)?,
            generation_id: row.get(1)?,
            project: row.get(2)?,
            file: row.get(3)?,
            repo_root: row.get(4)?,
            head_oid: row.get(5)?,
            extractor_version: row.get(6)?,
            status: row.get(7)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn row_from_sql(row: &rusqlite::Row) -> rusqlite::Result<WitnessLedgerRow> {
    Ok(WitnessLedgerRow {
        id: row.get(0)?,
        project: row.get(1)?,
        file: row.get(2)?,
        symbol: row.get(3)?,
        span_start: row.get(4)?,
        span_end: row.get(5)?,
        stamp: row.get(6)?,
        tier: row.get(7)?,
        at_oid: row.get(8)?,
        source_kind: row.get(9)?,
        source_id: row.get(10)?,
    })
}

const SELECT_COLUMNS: &str =
    "id, project, file, symbol, span_start, span_end, stamp, tier, at_oid, source_kind, source_id";

/// All witness rows recorded for `(project, file)`, oldest-first (`id ASC`)
/// — the full append-only history for that anchor, not just the latest claim.
pub fn witnesses_for_file(
    conn: &Connection,
    project: &str,
    file: &str,
) -> Result<Vec<WitnessLedgerRow>> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM witness_ledger
         WHERE project = ?1 AND file = ?2
         ORDER BY id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![project, file], row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The most recently inserted witness for a specific `(project, file,
/// symbol)` — `symbol = None` selects the whole-file witness. `None` if no
/// witness has ever been recorded for that anchor.
pub fn latest_witness_for_symbol(
    conn: &Connection,
    project: &str,
    file: &str,
    symbol: Option<&str>,
) -> Result<Option<WitnessLedgerRow>> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM witness_ledger
         WHERE project = ?1 AND file = ?2 AND symbol IS ?3
         ORDER BY id DESC LIMIT 1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let row = stmt
        .query_row(params![project, file, symbol], row_from_sql)
        .optional()?;
    Ok(row)
}

/// Count of ledger rows for `(project, file)` — used to check idempotency
/// (a backfill re-run must not grow this).
pub fn count_witnesses_for_file(conn: &Connection, project: &str, file: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM witness_ledger WHERE project = ?1 AND file = ?2",
        params![project, file],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

/// Total ledger rows across every project/file/tier — a cheap
/// "is there anything here at all" check the daemon's dream-cadence
/// catch-up decision uses (`daemon::dream_cadence::should_catch_up`): a
/// fresh install with an empty ledger has nothing to catch up on, so its
/// first cycle follows the normal cadence interval instead of firing
/// shortly after startup.
pub fn count_all(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM witness_ledger", [], |r| r.get(0))
        .map_err(Into::into)
}

/// Every `tier = 'committed'` witness row, ordered so that all rows sharing
/// `(project, file, symbol)` are contiguous (`ORDER BY project, file,
/// COALESCE(symbol,''), id`) — the grouping order `dream`'s successor join
/// needs to walk each anchor's history without a second sort pass.
/// `tier = 'worktree'` rows are excluded: only committed history can support
/// a supersession claim (mirrors `codewitness::Auditor::audit_against_successor`'s
/// own tier gate).
pub fn all_committed_witnesses(conn: &Connection) -> Result<Vec<WitnessLedgerRow>> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM witness_ledger
         WHERE tier = 'committed'
         ORDER BY project, file, COALESCE(symbol,''), id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], row_from_sql)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// A single witness row by its primary key — `dream`'s successor join uses
/// this to re-fetch a candidate successor's full row (stamp, at_oid) when
/// only the `witness_id` is at hand. `None` if the id doesn't exist (should
/// not happen for a real foreign reference, but never assumed).
pub fn witness_by_id(conn: &Connection, id: i64) -> Result<Option<WitnessLedgerRow>> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM witness_ledger WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let row = stmt.query_row(params![id], row_from_sql).optional()?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        conn
    }

    fn row(symbol: Option<&str>, stamp: &str) -> WitnessLedgerRow {
        WitnessLedgerRow {
            id: 0,
            project: "proj".into(),
            file: "/repo/src/lib.rs".into(),
            symbol: symbol.map(|s| s.to_string()),
            span_start: symbol.map(|_| 1),
            span_end: symbol.map(|_| 3),
            stamp: stamp.into(),
            tier: "committed".into(),
            at_oid: Some("deadbeef".into()),
            source_kind: "backfill".into(),
            source_id: Some("deadbeef".into()),
        }
    }

    #[test]
    fn insert_and_query_round_trip() {
        let conn = open();
        insert_witness(&conn, &row(Some("foo"), "b3:aaa")).unwrap();
        let rows = witnesses_for_file(&conn, "proj", "/repo/src/lib.rs").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].symbol.as_deref(), Some("foo"));
        assert_eq!(rows[0].stamp, "b3:aaa");
        assert_eq!(rows[0].tier, "committed");
    }

    #[test]
    fn duplicate_symbol_insert_is_a_no_op() {
        let conn = open();
        let r = row(Some("foo"), "b3:aaa");
        insert_witness(&conn, &r).unwrap();
        insert_witness(&conn, &r).unwrap();
        assert_eq!(
            count_witnesses_for_file(&conn, "proj", "/repo/src/lib.rs").unwrap(),
            1,
            "identical symbol-level insert must be ignored, not duplicated"
        );
    }

    #[test]
    fn append_only_new_stamp_adds_a_row_never_overwrites() {
        // A changed stamp for the same anchor must APPEND a new row, not
        // replace the old one — that is the entire point of an append-only
        // ledger: the history of claims survives.
        let conn = open();
        insert_witness(&conn, &row(Some("foo"), "b3:aaa")).unwrap();
        insert_witness(&conn, &row(Some("foo"), "b3:bbb")).unwrap();
        let rows = witnesses_for_file(&conn, "proj", "/repo/src/lib.rs").unwrap();
        assert_eq!(rows.len(), 2, "both the old and new stamp must survive");
        let latest = latest_witness_for_symbol(&conn, "proj", "/repo/src/lib.rs", Some("foo"))
            .unwrap()
            .unwrap();
        assert_eq!(
            latest.stamp, "b3:bbb",
            "latest lookup must return the newest row"
        );
    }

    #[test]
    fn duplicate_whole_file_insert_is_a_no_op() {
        // NULL-key (whole-file) rows dedupe at the DB level via the
        // COALESCE-based `idx_witness_ledger_identity` UNIQUE index — no
        // application-level pre-insert guard involved.
        let conn = open();
        let r = row(None, "b3:ccc");
        insert_witness(&conn, &r).unwrap();
        insert_witness(&conn, &r).unwrap();
        assert_eq!(
            count_witnesses_for_file(&conn, "proj", "/repo/src/lib.rs").unwrap(),
            1,
            "identical whole-file insert must be ignored, not duplicated"
        );
    }

    #[test]
    fn whole_file_witness_symbol_is_none() {
        let conn = open();
        insert_witness(&conn, &row(None, "b3:ccc")).unwrap();
        let latest = latest_witness_for_symbol(&conn, "proj", "/repo/src/lib.rs", None)
            .unwrap()
            .unwrap();
        assert_eq!(latest.symbol, None);
        assert_eq!(latest.span_start, None);
        assert_eq!(latest.stamp, "b3:ccc");
    }

    #[test]
    fn no_witness_recorded_returns_none() {
        let conn = open();
        assert!(
            latest_witness_for_symbol(&conn, "proj", "/nope.rs", Some("foo"))
                .unwrap()
                .is_none()
        );
        assert!(witnesses_for_file(&conn, "proj", "/nope.rs")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn all_committed_witnesses_excludes_worktree_tier() {
        let conn = open();
        insert_witness(&conn, &row(Some("foo"), "b3:aaa")).unwrap();
        let mut worktree_row = row(Some("bar"), "b3:bbb");
        worktree_row.tier = "worktree".into();
        worktree_row.at_oid = None;
        insert_witness(&conn, &worktree_row).unwrap();

        let committed = all_committed_witnesses(&conn).unwrap();
        assert_eq!(committed.len(), 1, "worktree-tier row must be excluded");
        assert_eq!(committed[0].symbol.as_deref(), Some("foo"));
        assert_ne!(committed[0].id, 0, "a row read back must carry its real id");
    }

    #[test]
    fn all_committed_witnesses_groups_same_symbol_contiguously() {
        let conn = open();
        insert_witness(&conn, &row(Some("foo"), "b3:aaa")).unwrap();
        insert_witness(&conn, &row(Some("foo"), "b3:bbb")).unwrap();
        insert_witness(&conn, &row(Some("zzz"), "b3:ccc")).unwrap();
        let rows = all_committed_witnesses(&conn).unwrap();
        assert_eq!(rows.len(), 3);
        // Both "foo" rows must be adjacent (grouping order), regardless of
        // where "zzz" sorts.
        let foo_positions: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.symbol.as_deref() == Some("foo"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            foo_positions,
            vec![0, 1],
            "foo rows must be contiguous and stamp-ordered"
        );
    }

    #[test]
    fn witness_by_id_round_trips() {
        let conn = open();
        insert_witness(&conn, &row(Some("foo"), "b3:aaa")).unwrap();
        let id = all_committed_witnesses(&conn).unwrap()[0].id;
        let fetched = witness_by_id(&conn, id).unwrap().unwrap();
        assert_eq!(fetched.stamp, "b3:aaa");
        assert_eq!(fetched.id, id);
    }

    #[test]
    fn witness_by_id_missing_returns_none() {
        let conn = open();
        assert!(witness_by_id(&conn, 99999).unwrap().is_none());
    }
}
