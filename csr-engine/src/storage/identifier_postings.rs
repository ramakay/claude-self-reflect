//! Identifier postings: an exact-match index of the names FTS5 shreds.
//!
//! One row per (term, chunk). Terms come from
//! [`crate::search::identifiers::extract_identifiers`]. Rows are written in the
//! same transaction as the chunk insert (see `queries::insert_chunk_with_source`),
//! so every import path gets them. [`backfill`] covers chunks that predate the
//! table; it is resumable from a watermark in `meta`. [`search_postings`] is the
//! read side and fails open: a database without the table answers with nothing.
//!
//! The table is additive. Nothing here touches the HNSW index files.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;

use crate::search::identifiers::{extract_identifiers, IdentKind, QueryIdent};

/// `meta` key: the highest chunk rowid the backfill has finished.
pub const WATERMARK_KEY: &str = "identifier_postings_watermark";
/// A term in more chunks than this is too common to rank by; it is skipped.
pub const MAX_TERM_DF: i64 = 20_000;
/// Chunks per backfill transaction.
pub const DEFAULT_BATCH: usize = 2000;
/// How long a backfill write waits behind the daemon.
pub const BACKFILL_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// Additive migration: table, chunk index, delete trigger. Safe to run twice.
pub fn migrate_identifier_postings(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS identifier_postings(
            term TEXT NOT NULL,
            kind INTEGER NOT NULL,
            chunk_rowid INTEGER NOT NULL,
            PRIMARY KEY(term, chunk_rowid)
         ) WITHOUT ROWID;
         CREATE INDEX IF NOT EXISTS identifier_postings_chunk
            ON identifier_postings(chunk_rowid);
         CREATE TRIGGER IF NOT EXISTS identifier_postings_ad AFTER DELETE ON chunks BEGIN
            DELETE FROM identifier_postings WHERE chunk_rowid = old.rowid;
         END;",
    )?;
    Ok(())
}

fn table_exists(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master
          WHERE type = 'table' AND name = 'identifier_postings')",
        [],
        |r| r.get(0),
    )?)
}

fn insert_terms(
    conn: &Connection,
    chunk_rowid: i64,
    terms: &[(String, IdentKind)],
) -> Result<usize> {
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO identifier_postings(term, kind, chunk_rowid) VALUES (?1, ?2, ?3)",
    )?;
    let mut written = 0;
    for (term, kind) in terms {
        written += stmt.execute(params![term, kind.as_u8(), chunk_rowid])?;
    }
    Ok(written)
}

/// Replace the postings of `chunk_rowid` with those of `content`. Returns the
/// rows written. O(content).
pub fn write_postings(conn: &Connection, chunk_rowid: i64, content: &str) -> Result<usize> {
    let terms = extract_identifiers(content);
    conn.prepare_cached("DELETE FROM identifier_postings WHERE chunk_rowid = ?1")?
        .execute([chunk_rowid])?;
    insert_terms(conn, chunk_rowid, &terms)
}

/// What one [`backfill`] run did.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct BackfillReport {
    pub scanned: u64,
    pub rows_written: u64,
    pub watermark: i64,
    pub wall_ms: u64,
}

fn read_watermark(conn: &Connection) -> Result<i64> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [WATERMARK_KEY],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0))
}

/// Write postings for every chunk past the watermark, `batch` chunks per
/// transaction, advancing the watermark in the same transaction as the rows.
/// Interrupt it anywhere and run it again: it resumes. Runs only
/// [`migrate_identifier_postings`], never the full migrations.
pub fn backfill(conn: &Connection, batch: usize) -> Result<BackfillReport> {
    let started = Instant::now();
    let batch = batch.max(1);
    conn.busy_timeout(BACKFILL_BUSY_TIMEOUT)?;
    migrate_identifier_postings(conn)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;

    let mut watermark = read_watermark(conn)?;
    let mut report = BackfillReport::default();
    loop {
        // Read and extract outside the write lock, so the daemon waits only for
        // the inserts.
        let rows: Vec<(i64, String)> = {
            let mut stmt = conn.prepare_cached(
                "SELECT rowid, content FROM chunks WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![watermark, batch as i64], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let Some(&(last, _)) = rows.last() else {
            break;
        };
        let extracted: Vec<(i64, Vec<(String, IdentKind)>)> = rows
            .iter()
            .map(|(rowid, content)| (*rowid, extract_identifiers(content)))
            .collect();

        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let mut written = 0usize;
        {
            // A chunk deleted since the read gets no rows (its trigger already ran).
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO identifier_postings(term, kind, chunk_rowid)
                 SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM chunks WHERE rowid = ?3)",
            )?;
            for (rowid, terms) in &extracted {
                for (term, kind) in terms {
                    written += stmt.execute(params![term, kind.as_u8(), rowid])?;
                }
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![WATERMARK_KEY, last.to_string()],
        )?;
        tx.commit()?;

        watermark = last;
        report.scanned += rows.len() as u64;
        report.rows_written += written as u64;
        if rows.len() < batch {
            break;
        }
    }
    report.watermark = watermark;
    report.wall_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// How much of the path-bearing corpus has postings.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Coverage {
    pub chunks_total: u64,
    pub chunks_touching_path: u64,
    pub covered: u64,
    pub coverage_pct: f64,
    pub rows: u64,
    pub watermark: i64,
    pub max_chunk_rowid: i64,
}

/// A chunk "touches a path" when [`extract_identifiers`] finds a `Path` in its
/// content; it is "covered" when it has at least one `kind = 1` row.
pub fn coverage(conn: &Connection) -> Result<Coverage> {
    conn.busy_timeout(BACKFILL_BUSY_TIMEOUT)?;
    let mut out = Coverage::default();
    if !table_exists(conn)? {
        out.max_chunk_rowid = max_chunk_rowid(conn)?;
        return Ok(out);
    }
    let path_kind = IdentKind::Path.as_u8();
    let mut after = 0i64;
    loop {
        let rows: Vec<(i64, String)> = {
            let mut stmt = conn.prepare_cached(
                "SELECT rowid, content FROM chunks WHERE rowid > ?1 ORDER BY rowid LIMIT 5000",
            )?;
            let rows = stmt
                .query_map([after], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let Some(&(last, _)) = rows.last() else {
            break;
        };
        let mut probe = conn.prepare_cached(
            "SELECT 1 FROM identifier_postings WHERE chunk_rowid = ?1 AND kind = ?2 LIMIT 1",
        )?;
        for (rowid, content) in &rows {
            out.chunks_total += 1;
            let touches = extract_identifiers(content)
                .iter()
                .any(|(_, kind)| *kind == IdentKind::Path);
            if !touches {
                continue;
            }
            out.chunks_touching_path += 1;
            if probe.exists(params![rowid, path_kind])? {
                out.covered += 1;
            }
        }
        after = last;
    }
    out.coverage_pct = if out.chunks_touching_path == 0 {
        100.0
    } else {
        out.covered as f64 * 100.0 / out.chunks_touching_path as f64
    };
    out.rows = conn.query_row("SELECT COUNT(*) FROM identifier_postings", [], |r| {
        r.get::<_, i64>(0)
    })? as u64;
    out.watermark = if conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        read_watermark(conn)?
    } else {
        0
    };
    out.max_chunk_rowid = max_chunk_rowid(conn)?;
    Ok(out)
}

fn max_chunk_rowid(conn: &Connection) -> Result<i64> {
    Ok(
        conn.query_row("SELECT COALESCE(MAX(rowid), 0) FROM chunks", [], |r| {
            r.get(0)
        })?,
    )
}

/// Chunks matching the identifier terms of a query, best first:
/// score = sum over matched terms of `weight * ln(N / df)`, with `N` the
/// highest chunk rowid and `df` the term's posting count. A term in more than
/// [`MAX_TERM_DF`] chunks is skipped. `family` limits results to those projects.
/// Returns `(chunk id, score)`. A database without the table answers with
/// nothing, and the call only reads, so it works on a `mode=ro` connection.
pub fn search_postings(
    conn: &Connection,
    idents: &[QueryIdent],
    fetch: usize,
    family: Option<&[&str]>,
) -> Result<Vec<(String, f64)>> {
    if idents.is_empty() || fetch == 0 || !table_exists(conn)? {
        return Ok(Vec::new());
    }
    let n = max_chunk_rowid(conn)?;
    if n <= 0 {
        return Ok(Vec::new());
    }
    // One entry per term, at its strongest weight, in first-seen order.
    let mut terms: Vec<(&str, f32)> = Vec::new();
    for ident in idents {
        match terms.iter_mut().find(|(t, _)| *t == ident.term) {
            Some(slot) => slot.1 = slot.1.max(ident.weight),
            None => terms.push((ident.term.as_str(), ident.weight)),
        }
    }
    let wanted = match family {
        Some(projects) => Some(serde_json::to_string(projects)?),
        None => None,
    };

    let mut scores: HashMap<i64, f64> = HashMap::new();
    for (term, weight) in terms {
        let df: i64 = conn.query_row(
            "SELECT COUNT(*) FROM
               (SELECT 1 FROM identifier_postings WHERE term = ?1 LIMIT ?2)",
            params![term, MAX_TERM_DF + 1],
            |r| r.get(0),
        )?;
        if df == 0 || df > MAX_TERM_DF {
            continue;
        }
        // Floored: a term in every chunk of a tiny corpus still counts a little.
        let idf = (n as f64 / df as f64).ln().max(0.01);
        let gain = f64::from(weight) * idf;
        let mut add = |rowid: i64| {
            *scores.entry(rowid).or_insert(0.0) += gain;
        };
        match &wanted {
            None => {
                let mut stmt = conn.prepare_cached(
                    "SELECT chunk_rowid FROM identifier_postings WHERE term = ?1",
                )?;
                for rowid in stmt.query_map([term], |r| r.get::<_, i64>(0))? {
                    add(rowid?);
                }
            }
            Some(projects) => {
                let mut stmt = conn.prepare_cached(
                    "SELECT p.chunk_rowid
                       FROM identifier_postings p
                       JOIN chunks c ON c.rowid = p.chunk_rowid
                      WHERE p.term = ?1
                        AND c.project_name IN (SELECT value FROM json_each(?2))",
                )?;
                for rowid in stmt.query_map(params![term, projects], |r| r.get::<_, i64>(0))? {
                    add(rowid?);
                }
            }
        }
    }

    let mut ranked: Vec<(i64, f64)> = scores.into_iter().collect();
    // Highest score first; the rowid breaks ties so the order is deterministic.
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(fetch);
    let mut id_stmt = conn.prepare_cached("SELECT id FROM chunks WHERE rowid = ?1")?;
    let mut out = Vec::with_capacity(ranked.len());
    for (rowid, score) in ranked {
        if let Some(id) = id_stmt
            .query_row([rowid], |r| r.get::<_, String>(0))
            .optional()?
        {
            out.push((id, score));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::ConversationChunk;
    use crate::provenance::Speaker;
    use crate::storage::{migrations, queries};

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA recursive_triggers=ON;")
            .unwrap();
        migrations::run(&conn).unwrap();
        conn
    }

    fn chunk(id: &str, project: &str, content: &str) -> ConversationChunk {
        ConversationChunk {
            id: id.into(),
            conversation_id: format!("conv-{id}"),
            project_name: project.into(),
            timestamp: "2026-10-25T00:00:00Z".into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: Speaker::User,
            seq: 0,
            is_sidechain: false,
        }
    }

    fn raw_insert(conn: &Connection, id: &str, project: &str, content: &str) -> i64 {
        conn.execute(
            "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, message_count)
             VALUES (?1, ?1, ?2, 'now', ?3, 1)",
            params![id, project, content],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn rows_for(conn: &Connection, rowid: i64) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM identifier_postings WHERE chunk_rowid = ?1",
            [rowid],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn identifier_migration_is_idempotent() {
        let conn = conn();
        migrate_identifier_postings(&conn).unwrap();
        migrate_identifier_postings(&conn).unwrap();
        migrations::run(&conn).unwrap();
        let objects: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN
                 ('identifier_postings','identifier_postings_chunk','identifier_postings_ad')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(objects, 3);
    }

    #[test]
    fn identifier_delete_trigger_removes_rows() {
        let conn = conn();
        let a = raw_insert(&conn, "a", "p", "see src/a.rs and foo_bar");
        let b = raw_insert(&conn, "b", "p", "see src/b.rs");
        write_postings(&conn, a, "see src/a.rs and foo_bar").unwrap();
        write_postings(&conn, b, "see src/b.rs").unwrap();
        assert!(rows_for(&conn, a) > 0);
        conn.execute("DELETE FROM chunks WHERE id = 'a'", [])
            .unwrap();
        assert_eq!(rows_for(&conn, a), 0);
        assert!(rows_for(&conn, b) > 0);
    }

    #[test]
    fn identifier_insert_chunk_writes_postings() {
        let conn = conn();
        queries::insert_chunk(
            &conn,
            &chunk(
                "c1",
                "p",
                "fixed in src/storage/queries.rs with --no-verify",
            ),
            &[0.0; 4],
        )
        .unwrap();
        let rowid: i64 = conn
            .query_row("SELECT rowid FROM chunks WHERE id = 'c1'", [], |r| r.get(0))
            .unwrap();
        let terms: Vec<String> = conn
            .prepare("SELECT term FROM identifier_postings WHERE chunk_rowid = ?1 ORDER BY term")
            .unwrap()
            .query_map([rowid], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(terms.contains(&"src/storage/queries.rs".to_string()));
        assert!(terms.contains(&"queries.rs".to_string()));
        assert!(terms.contains(&"--no-verify".to_string()));
    }

    #[test]
    fn identifier_reinsert_replaces_stale_postings() {
        let conn = conn();
        queries::insert_chunk(&conn, &chunk("c1", "p", "old src/old.rs"), &[0.0; 4]).unwrap();
        queries::insert_chunk(&conn, &chunk("c1", "p", "new src/new.rs"), &[0.0; 4]).unwrap();
        let old: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM identifier_postings WHERE term = 'src/old.rs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let new: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM identifier_postings WHERE term = 'src/new.rs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!((old, new), (0, 1));
    }

    #[test]
    fn identifier_backfill_resumes_from_watermark() {
        let conn = conn();
        for i in 0..7 {
            raw_insert(&conn, &format!("c{i}"), "p", &format!("edit src/f{i}.rs"));
        }
        let first = backfill(&conn, 3).unwrap();
        assert_eq!(first.scanned, 7);
        assert_eq!(first.watermark, 7);

        // New chunks past the watermark: what an interrupted run leaves behind.
        let n8 = raw_insert(&conn, "c7", "p", "edit src/f7.rs");
        let n9 = raw_insert(&conn, "c8", "p", "edit src/f8.rs");
        let second = backfill(&conn, 3).unwrap();
        assert_eq!(second.scanned, 2);
        assert_eq!(second.watermark, n9);
        assert!(rows_for(&conn, n8) > 0 && rows_for(&conn, n9) > 0);

        let third = backfill(&conn, 3).unwrap();
        assert_eq!(third.scanned, 0);
        assert_eq!(third.rows_written, 0);
        assert_eq!(third.watermark, n9);
    }

    #[test]
    fn identifier_backfill_midway_watermark_is_respected() {
        let conn = conn();
        for i in 0..6 {
            raw_insert(&conn, &format!("c{i}"), "p", &format!("edit src/g{i}.rs"));
        }
        migrate_identifier_postings(&conn).unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, '4')",
            [WATERMARK_KEY],
        )
        .unwrap();
        let report = backfill(&conn, 2).unwrap();
        assert_eq!(report.scanned, 2);
        assert_eq!(report.watermark, 6);
        // Chunks 1..=4 were "done" by an earlier run: untouched here.
        assert_eq!(rows_for(&conn, 1), 0);
        assert!(rows_for(&conn, 5) > 0);
    }

    #[test]
    fn identifier_coverage_on_fixture_is_full() {
        let conn = conn();
        raw_insert(&conn, "a", "p", "edit src/a.rs");
        raw_insert(&conn, "b", "p", "just prose here");
        raw_insert(&conn, "c", "p", "open /usr/local/bin and Cargo.toml");
        backfill(&conn, 2).unwrap();
        let cov = coverage(&conn).unwrap();
        assert_eq!(cov.chunks_total, 3);
        assert_eq!(cov.chunks_touching_path, 2);
        assert_eq!(cov.covered, 2);
        assert_eq!(cov.coverage_pct, 100.0);
        assert_eq!(cov.watermark, cov.max_chunk_rowid);
        assert!(cov.rows > 0);
    }

    #[test]
    fn identifier_coverage_reports_a_gap() {
        let conn = conn();
        raw_insert(&conn, "a", "p", "edit src/a.rs");
        raw_insert(&conn, "b", "p", "edit src/b.rs");
        let cov = coverage(&conn).unwrap();
        assert_eq!(cov.covered, 0);
        assert_eq!(cov.chunks_touching_path, 2);
        assert_eq!(cov.coverage_pct, 0.0);
    }

    #[test]
    fn identifier_search_ranks_rare_terms_and_filters_family() {
        let conn = conn();
        let a = raw_insert(
            &conn,
            "a",
            "alpha",
            "uses src/rare/needle.rs and foo_common",
        );
        let b = raw_insert(&conn, "b", "beta", "uses foo_common only");
        for i in 0..5 {
            raw_insert(&conn, &format!("pad{i}"), "alpha", "nothing to see");
        }
        write_postings(&conn, a, "uses src/rare/needle.rs and foo_common").unwrap();
        write_postings(&conn, b, "uses foo_common only").unwrap();

        let q = crate::search::identifiers::analyze_query("src/rare/needle.rs foo_common");
        let hits = search_postings(&conn, &q, 10, None).unwrap();
        assert_eq!(hits[0].0, "a");
        assert_eq!(hits.len(), 2);
        assert!(hits[0].1 > hits[1].1);

        let only_beta = search_postings(&conn, &q, 10, Some(&["beta"])).unwrap();
        assert_eq!(only_beta.len(), 1);
        assert_eq!(only_beta[0].0, "b");

        assert_eq!(search_postings(&conn, &q, 1, None).unwrap().len(), 1);
        assert!(search_postings(&conn, &[], 10, None).unwrap().is_empty());
    }

    #[test]
    fn identifier_search_fails_open_without_the_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE chunks(id TEXT PRIMARY KEY, project_name TEXT);")
            .unwrap();
        let q = crate::search::identifiers::analyze_query("src/a.rs");
        assert!(search_postings(&conn, &q, 10, None).unwrap().is_empty());
    }

    #[test]
    fn identifier_search_works_on_a_read_only_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ro.db");
        {
            let conn = Connection::open(&path).unwrap();
            migrations::run(&conn).unwrap();
            let id = raw_insert(&conn, "a", "p", "edit src/ro.rs");
            write_postings(&conn, id, "edit src/ro.rs").unwrap();
        }
        let ro =
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let q = crate::search::identifiers::analyze_query("src/ro.rs");
        let hits = search_postings(&ro, &q, 10, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "a");
    }

    #[test]
    fn identifier_common_terms_are_skipped() {
        let conn = conn();
        raw_insert(&conn, "x0", "p", "seed");
        // Fake a posting list longer than the cap with direct rows.
        conn.execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1)
             INSERT INTO identifier_postings(term, kind, chunk_rowid) SELECT 'ubiquitous_term', 2, i FROM n",
            [MAX_TERM_DF + 5],
        )
        .unwrap();
        let q = crate::search::identifiers::analyze_query("ubiquitous_term");
        assert!(search_postings(&conn, &q, 10, None).unwrap().is_empty());
    }
}
