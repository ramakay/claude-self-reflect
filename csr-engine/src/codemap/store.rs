//! Every write the code map makes, and the invariants they keep.
//!
//! Invariants (each write transaction preserves them):
//!
//! 1. Every blob oid in `codemap_view_files` has a `codemap_blobs` row for the
//!    current extractor version once its view has been scanned.
//! 2. `codemap_fts` holds one row per (path, blob, def) for every (path, blob)
//!    pair at least one view currently holds, for the current extractor
//!    version, and nothing else. The FTS rowid is a hash of
//!    (extractor version, path, blob, kind, name, span), so adding is
//!    idempotent and a row can be removed without scanning the table.
//! 3. `codemap_blobs` / `codemap_defs` hold nothing no view references.
//!
//! Callers parse BEFORE they call in here; each function opens one short
//! `IMMEDIATE` transaction.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;

use super::extractor_version;

/// One code file a view holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRec {
    pub path: String,
    pub blob_oid: String,
    /// 0 = matches the index, 1 = modified in the worktree, 2 = untracked.
    pub dirty: i64,
    pub size: i64,
    pub mtime_ns: i64,
}

/// One definition found in a blob. Spans are the extractor's 0-based line
/// numbers, the same convention as `code_nodes.span_start/span_end`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefRow {
    pub name: String,
    pub fqname: Option<String>,
    pub kind: String,
    pub span_start: i64,
    pub span_end: i64,
    pub signature: String,
}

/// The result of parsing one blob.
#[derive(Debug, Clone)]
pub struct ParsedBlob {
    pub oid: String,
    pub lang: Option<String>,
    /// `ok`, `error` (not UTF-8), `minified` (a line over the threshold: by
    /// content, final), or one of the two that depend on the path the blob was
    /// first seen under and may be replaced by an `ok` parse from a path that
    /// can do better: `no_lang` (that path has no parser), `minified_name`
    /// (that path ends in `.min.js`).
    pub status: &'static str,
    pub defs: Vec<DefRow>,
}

/// A view row as stored.
#[derive(Debug, Clone, Serialize)]
pub struct StoredView {
    pub id: i64,
    pub common_dir: String,
    pub worktree_path: String,
    pub branch: Option<String>,
    pub head_oid: Option<String>,
    pub index_mtime_ns: Option<i64>,
    pub index_size: Option<i64>,
    pub scanned_at: Option<i64>,
    pub status: String,
    pub files: i64,
    pub truncated: bool,
    pub scan_ms: Option<i64>,
}

/// The mutable columns of a view row.
pub struct ViewWrite<'a> {
    pub common_dir: &'a str,
    pub worktree_path: &'a str,
    pub branch: Option<&'a str>,
    pub head_oid: Option<&'a str>,
    pub index_mtime_ns: Option<i64>,
    pub index_size: Option<i64>,
    pub scanned_at: i64,
    pub status: &'a str,
    pub truncated: bool,
    pub scan_ms: i64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WriteOutcome {
    pub view_id: i64,
    pub defs_written: usize,
    pub gc_blobs: usize,
}

/// A registered repo as stored.
#[derive(Debug, Clone, Serialize)]
pub struct StoredRepo {
    pub common_dir: String,
    pub main_path: Option<String>,
    pub source: String,
    pub enabled: bool,
    pub added_at: i64,
    pub last_poll_at: Option<i64>,
    pub status: String,
}

fn begin(conn: &Connection) -> Result<Transaction<'_>> {
    Ok(Transaction::new_unchecked(
        conn,
        TransactionBehavior::Immediate,
    )?)
}

/// True when the code map tables exist (a read-only database opened by an older
/// binary may not have them).
pub fn tables_ready(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name IN
            ('codemap_repos','codemap_views','codemap_view_files','codemap_blobs',
             'codemap_defs','codemap_queue','codemap_fts')",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n == 7)
    .unwrap_or(false)
}

// ---- FTS rows -------------------------------------------------------------

/// Deterministic FTS rowid for one def of one (path, blob) pair.
pub(crate) fn fts_rowid(
    version: &str,
    path: &str,
    blob: &str,
    kind: &str,
    name: &str,
    span_start: i64,
    span_end: i64,
) -> i64 {
    let mut h = blake3::Hasher::new();
    for part in [version, path, blob, kind, name] {
        h.update(part.as_bytes());
        h.update(&[0]);
    }
    h.update(&span_start.to_le_bytes());
    h.update(&span_end.to_le_bytes());
    let bytes = h.finalize();
    let mut eight = [0u8; 8];
    eight.copy_from_slice(&bytes.as_bytes()[..8]);
    (u64::from_le_bytes(eight) & (i64::MAX as u64)) as i64
}

/// Add the FTS rows of a (path, blob) pair for the current extractor version.
/// Idempotent.
fn fts_add_pair(conn: &Connection, path: &str, blob: &str) -> Result<()> {
    let version = extractor_version();
    let mut sel = conn.prepare_cached(
        "SELECT name, COALESCE(signature, ''), kind, span_start, span_end
         FROM codemap_defs WHERE blob_oid = ?1 AND extractor_version = ?2",
    )?;
    let mut ins = conn.prepare_cached(
        "INSERT OR REPLACE INTO codemap_fts(rowid, name, signature, path, blob_oid)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let rows: Vec<(String, String, String, i64, i64)> = sel
        .query_map(params![blob, version], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    for (name, sig, kind, s, e) in rows {
        let rowid = fts_rowid(version, path, blob, &kind, &name, s, e);
        ins.execute(params![rowid, name, sig, path, blob])?;
    }
    Ok(())
}

/// Remove the FTS rows of a (path, blob) pair, whichever extractor version made
/// its defs. Must run while the blob's defs still exist.
fn fts_remove_pair(conn: &Connection, path: &str, blob: &str) -> Result<()> {
    let mut sel = conn.prepare_cached(
        "SELECT extractor_version, kind, name, span_start, span_end
         FROM codemap_defs WHERE blob_oid = ?1",
    )?;
    let mut del = conn.prepare_cached("DELETE FROM codemap_fts WHERE rowid = ?1")?;
    let rows: Vec<(String, String, String, i64, i64)> = sel
        .query_map(params![blob], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    for (ver, kind, name, s, e) in rows {
        del.execute(params![fts_rowid(&ver, path, blob, &kind, &name, s, e)])?;
    }
    Ok(())
}

fn pair_held(conn: &Connection, path: &str, blob: &str, except_view: Option<i64>) -> Result<bool> {
    let held: Option<i64> = conn
        .prepare_cached(
            "SELECT 1 FROM codemap_view_files
             WHERE blob_oid = ?1 AND path = ?2 AND view_id != ?3 LIMIT 1",
        )?
        .query_row(params![blob, path, except_view.unwrap_or(-1)], |r| r.get(0))
        .optional()?;
    Ok(held.is_some())
}

// ---- blobs ----------------------------------------------------------------

pub fn blob_known(conn: &Connection, oid: &str) -> Result<bool> {
    let found: Option<i64> = conn
        .prepare_cached(
            "SELECT 1 FROM codemap_blobs WHERE blob_oid = ?1 AND extractor_version = ?2",
        )?
        .query_row(params![oid, extractor_version()], |r| r.get(0))
        .optional()?;
    Ok(found.is_some())
}

/// The status stored for a blob at the current extractor version.
pub fn blob_status(conn: &Connection, oid: &str) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached(
            "SELECT status FROM codemap_blobs WHERE blob_oid = ?1 AND extractor_version = ?2",
        )?
        .query_row(params![oid, extractor_version()], |r| r.get(0))
        .optional()?)
}

/// Insert parsed blobs; returns (defs written, blobs actually written).
///
/// A blob already stored is left alone, with one exception: a row whose status
/// depends on the path it was first seen under (`no_lang`, `minified_name`) is
/// replaced by a parse that came out `ok`. A stored row is never downgraded. A
/// replaced row counts as written, so its defs get search rows for every
/// (path, blob) pair a view holds.
fn insert_parsed(conn: &Connection, parsed: &[ParsedBlob]) -> Result<(usize, HashSet<String>)> {
    let version = extractor_version();
    let now = super::now_secs();
    let mut fresh = HashSet::new();
    let mut defs_written = 0usize;
    let mut ins_blob = conn.prepare_cached(
        "INSERT INTO codemap_blobs
            (blob_oid, extractor_version, lang, status, defs, parsed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(blob_oid, extractor_version) DO UPDATE SET
            lang = excluded.lang, status = excluded.status,
            defs = excluded.defs, parsed_at = excluded.parsed_at
         WHERE codemap_blobs.status IN ('no_lang', 'minified_name')
           AND excluded.status = 'ok'",
    )?;
    let mut clear_defs = conn.prepare_cached(
        "DELETE FROM codemap_defs WHERE blob_oid = ?1 AND extractor_version = ?2",
    )?;
    let mut ins_def = conn.prepare_cached(
        "INSERT INTO codemap_defs
            (blob_oid, extractor_version, name, fqname, kind, span_start, span_end, signature)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for blob in parsed {
        let inserted = ins_blob.execute(params![
            blob.oid,
            version,
            blob.lang,
            blob.status,
            blob.defs.len() as i64,
            now
        ])?;
        if inserted == 0 {
            continue;
        }
        if blob.status == "ok" {
            // A replaced negative has no defs; clear any stray ones anyway.
            clear_defs.execute(params![blob.oid, version])?;
        }
        fresh.insert(blob.oid.clone());
        for d in &blob.defs {
            ins_def.execute(params![
                blob.oid,
                version,
                d.name,
                d.fqname,
                d.kind,
                d.span_start,
                d.span_end,
                d.signature
            ])?;
            defs_written += 1;
        }
    }
    Ok((defs_written, fresh))
}

/// Drop a blob's rows if no view references it. True when it was dropped.
fn gc_blob(conn: &Connection, blob: &str) -> Result<bool> {
    let referenced: Option<i64> = conn
        .prepare_cached("SELECT 1 FROM codemap_view_files WHERE blob_oid = ?1 LIMIT 1")?
        .query_row(params![blob], |r| r.get(0))
        .optional()?;
    if referenced.is_some() {
        return Ok(false);
    }
    conn.prepare_cached("DELETE FROM codemap_defs WHERE blob_oid = ?1")?
        .execute(params![blob])?;
    let n = conn
        .prepare_cached("DELETE FROM codemap_blobs WHERE blob_oid = ?1")?
        .execute(params![blob])?;
    Ok(n > 0)
}

fn gc_candidates(conn: &Connection, candidates: &HashSet<String>) -> Result<usize> {
    let mut n = 0;
    for blob in candidates {
        if gc_blob(conn, blob)? {
            n += 1;
        }
    }
    Ok(n)
}

/// Sweep blobs no view references (defence in depth after crashes; the
/// targeted candidate GC in each write covers the normal path).
fn gc_unreferenced(conn: &Connection) -> Result<usize> {
    let orphans: HashSet<String> = conn
        .prepare(
            "SELECT DISTINCT blob_oid FROM codemap_blobs
             WHERE blob_oid NOT IN (SELECT blob_oid FROM codemap_view_files)",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    gc_candidates(conn, &orphans)
}

/// Drop blobs, defs and FTS rows made by an older extractor version. Returns the
/// number of blob rows dropped.
fn gc_old_versions_tx(conn: &Connection) -> Result<usize> {
    let version = extractor_version();
    let old: Vec<String> = conn
        .prepare("SELECT DISTINCT blob_oid FROM codemap_blobs WHERE extractor_version != ?1")?
        .query_map(params![version], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    if old.is_empty() {
        return Ok(0);
    }
    let mut del_fts = conn.prepare_cached("DELETE FROM codemap_fts WHERE rowid = ?1")?;
    for blob in &old {
        let paths: Vec<String> = conn
            .prepare_cached("SELECT path FROM codemap_view_files WHERE blob_oid = ?1")?
            .query_map(params![blob], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        let defs: Vec<(String, String, String, i64, i64)> = conn
            .prepare_cached(
                "SELECT extractor_version, kind, name, span_start, span_end
                 FROM codemap_defs WHERE blob_oid = ?1 AND extractor_version != ?2",
            )?
            .query_map(params![blob, version], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        for path in &paths {
            for (ver, kind, name, s, e) in &defs {
                del_fts.execute(params![fts_rowid(ver, path, blob, kind, name, *s, *e)])?;
            }
        }
    }
    conn.execute(
        "DELETE FROM codemap_defs WHERE extractor_version != ?1",
        params![version],
    )?;
    let n = conn.execute(
        "DELETE FROM codemap_blobs WHERE extractor_version != ?1",
        params![version],
    )?;
    Ok(n)
}

/// Drop everything an older extractor version left behind, plus any blob no view
/// references. Returns the number of blob rows dropped.
pub fn gc(conn: &Connection) -> Result<usize> {
    let tx = begin(conn)?;
    let n = gc_old_versions_tx(&tx)? + gc_unreferenced(&tx)?;
    tx.commit()?;
    Ok(n)
}

// ---- view files -----------------------------------------------------------

/// Apply per-path changes to one view: `None` removes the path. Keeps the FTS
/// invariant for added and removed (path, blob) pairs and returns the old blobs
/// that may now be unreferenced.
fn apply_changes(
    conn: &Connection,
    view_id: i64,
    changes: &[(String, Option<FileRec>)],
    fresh: &HashSet<String>,
) -> Result<HashSet<String>> {
    let mut removed: Vec<(String, String)> = Vec::new();
    let mut added: Vec<(String, String)> = Vec::new();
    {
        let mut sel = conn.prepare_cached(
            "SELECT blob_oid FROM codemap_view_files WHERE view_id = ?1 AND path = ?2",
        )?;
        let mut del =
            conn.prepare_cached("DELETE FROM codemap_view_files WHERE view_id = ?1 AND path = ?2")?;
        let mut upsert = conn.prepare_cached(
            "INSERT INTO codemap_view_files (view_id, path, blob_oid, dirty, size, mtime_ns)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(view_id, path) DO UPDATE SET
                blob_oid = excluded.blob_oid, dirty = excluded.dirty,
                size = excluded.size, mtime_ns = excluded.mtime_ns",
        )?;
        for (path, new) in changes {
            let old: Option<String> = sel
                .query_row(params![view_id, path], |r| r.get(0))
                .optional()?;
            match (old, new) {
                (None, None) => {}
                (Some(old_blob), None) => {
                    del.execute(params![view_id, path])?;
                    removed.push((path.clone(), old_blob));
                }
                (None, Some(rec)) => {
                    upsert.execute(params![
                        view_id,
                        path,
                        rec.blob_oid,
                        rec.dirty,
                        rec.size,
                        rec.mtime_ns
                    ])?;
                    added.push((path.clone(), rec.blob_oid.clone()));
                }
                (Some(old_blob), Some(rec)) => {
                    upsert.execute(params![
                        view_id,
                        path,
                        rec.blob_oid,
                        rec.dirty,
                        rec.size,
                        rec.mtime_ns
                    ])?;
                    if old_blob != rec.blob_oid {
                        removed.push((path.clone(), old_blob));
                        added.push((path.clone(), rec.blob_oid.clone()));
                    }
                }
            }
        }
    }
    let mut candidates = HashSet::new();
    for (path, blob) in &removed {
        if !pair_held(conn, path, blob, None)? {
            fts_remove_pair(conn, path, blob)?;
        }
        candidates.insert(blob.clone());
    }
    for (path, blob) in &added {
        if fresh.contains(blob) {
            continue; // covered below, for every path that holds it
        }
        if !pair_held(conn, path, blob, Some(view_id))? {
            fts_add_pair(conn, path, blob)?;
        }
    }
    for blob in fresh {
        let paths: Vec<String> = conn
            .prepare_cached("SELECT DISTINCT path FROM codemap_view_files WHERE blob_oid = ?1")?
            .query_map(params![blob], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for path in paths {
            fts_add_pair(conn, &path, blob)?;
        }
    }
    Ok(candidates)
}

fn view_id_for(conn: &Connection, worktree_path: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM codemap_views WHERE worktree_path = ?1",
            params![worktree_path],
            |r| r.get(0),
        )
        .optional()?)
}

fn upsert_view(conn: &Connection, w: &ViewWrite<'_>, files: i64) -> Result<i64> {
    conn.execute(
        "INSERT INTO codemap_views
            (common_dir, worktree_path, branch, head_oid, index_mtime_ns, index_size,
             scanned_at, status, files, truncated, scan_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(worktree_path) DO UPDATE SET
            common_dir = excluded.common_dir, branch = excluded.branch,
            head_oid = excluded.head_oid, index_mtime_ns = excluded.index_mtime_ns,
            index_size = excluded.index_size, scanned_at = excluded.scanned_at,
            status = excluded.status, files = excluded.files,
            truncated = excluded.truncated, scan_ms = excluded.scan_ms",
        params![
            w.common_dir,
            w.worktree_path,
            w.branch,
            w.head_oid,
            w.index_mtime_ns,
            w.index_size,
            w.scanned_at,
            w.status,
            files,
            w.truncated as i64,
            w.scan_ms
        ],
    )?;
    view_id_for(conn, w.worktree_path)?
        .ok_or_else(|| anyhow::anyhow!("view row vanished after upsert"))
}

/// Write one view's full state in a single transaction: new blobs and defs, the
/// diff of its file rows, FTS maintenance, the view row, and GC of whatever the
/// diff left unreferenced.
pub fn write_view(
    conn: &Connection,
    w: &ViewWrite<'_>,
    files: &[FileRec],
    parsed: &[ParsedBlob],
) -> Result<WriteOutcome> {
    let tx = begin(conn)?;
    let (defs_written, fresh) = insert_parsed(&tx, parsed)?;
    let view_id = upsert_view(&tx, w, files.len() as i64)?;

    let mut old: HashMap<String, FileRec> = HashMap::new();
    {
        let mut stmt = tx.prepare(
            "SELECT path, blob_oid, dirty, COALESCE(size, 0), COALESCE(mtime_ns, 0)
             FROM codemap_view_files WHERE view_id = ?1",
        )?;
        for row in stmt.query_map(params![view_id], |r| {
            Ok(FileRec {
                path: r.get(0)?,
                blob_oid: r.get(1)?,
                dirty: r.get(2)?,
                size: r.get(3)?,
                mtime_ns: r.get(4)?,
            })
        })? {
            let rec = row?;
            old.insert(rec.path.clone(), rec);
        }
    }
    let mut changes: Vec<(String, Option<FileRec>)> = Vec::new();
    let mut keep: HashSet<&str> = HashSet::with_capacity(files.len());
    for rec in files {
        keep.insert(rec.path.as_str());
        if old.get(&rec.path) != Some(rec) {
            changes.push((rec.path.clone(), Some(rec.clone())));
        }
    }
    for path in old.keys() {
        if !keep.contains(path.as_str()) {
            changes.push((path.clone(), None));
        }
    }
    let candidates = apply_changes(&tx, view_id, &changes, &fresh)?;
    let gc_blobs = gc_candidates(&tx, &candidates)?;
    tx.commit()?;
    Ok(WriteOutcome {
        view_id,
        defs_written,
        gc_blobs,
    })
}

/// Record a failed scan of a view without touching its files.
pub fn mark_view_error(conn: &Connection, w: &ViewWrite<'_>) -> Result<()> {
    let tx = begin(conn)?;
    let existing: i64 = tx
        .query_row(
            "SELECT COALESCE((SELECT files FROM codemap_views WHERE worktree_path = ?1), 0)",
            params![w.worktree_path],
            |r| r.get(0),
        )
        .unwrap_or(0);
    upsert_view(&tx, w, existing)?;
    tx.commit()?;
    Ok(())
}

/// Delete a view and its files, then GC what that left unreferenced. Returns the
/// number of blobs dropped.
pub fn delete_view(conn: &Connection, view_id: i64) -> Result<usize> {
    let tx = begin(conn)?;
    let rows: Vec<(String, String)> = tx
        .prepare("SELECT path, blob_oid FROM codemap_view_files WHERE view_id = ?1")?
        .query_map(params![view_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    tx.execute(
        "DELETE FROM codemap_view_files WHERE view_id = ?1",
        params![view_id],
    )?;
    tx.execute("DELETE FROM codemap_views WHERE id = ?1", params![view_id])?;
    let mut candidates = HashSet::new();
    for (path, blob) in &rows {
        if !pair_held(&tx, path, blob, None)? {
            fts_remove_pair(&tx, path, blob)?;
        }
        candidates.insert(blob.clone());
    }
    let n = gc_candidates(&tx, &candidates)?;
    tx.commit()?;
    Ok(n)
}

/// Apply a partial rescan of one view: per-path changes plus the blobs parsed
/// for them. Updates the view's file count; leaves its HEAD, index stat and
/// scanned_at alone (those describe the last full scan).
pub fn apply_path_changes(
    conn: &Connection,
    view_id: i64,
    changes: &[(String, Option<FileRec>)],
    parsed: &[ParsedBlob],
    truncated: bool,
) -> Result<WriteOutcome> {
    let tx = begin(conn)?;
    let (defs_written, fresh) = insert_parsed(&tx, parsed)?;
    let candidates = apply_changes(&tx, view_id, changes, &fresh)?;
    let gc_blobs = gc_candidates(&tx, &candidates)?;
    tx.execute(
        "UPDATE codemap_views SET
            files = (SELECT COUNT(*) FROM codemap_view_files WHERE view_id = ?1),
            truncated = MAX(truncated, ?2)
         WHERE id = ?1",
        params![view_id, truncated as i64],
    )?;
    tx.commit()?;
    Ok(WriteOutcome {
        view_id,
        defs_written,
        gc_blobs,
    })
}

// ---- reads ----------------------------------------------------------------

const VIEW_COLUMNS: &str = "id, common_dir, worktree_path, branch, head_oid, index_mtime_ns,
    index_size, scanned_at, status, files, truncated, scan_ms";

fn view_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<StoredView> {
    Ok(StoredView {
        id: r.get(0)?,
        common_dir: r.get(1)?,
        worktree_path: r.get(2)?,
        branch: r.get(3)?,
        head_oid: r.get(4)?,
        index_mtime_ns: r.get(5)?,
        index_size: r.get(6)?,
        scanned_at: r.get(7)?,
        status: r.get(8)?,
        files: r.get(9)?,
        truncated: r.get::<_, i64>(10)? != 0,
        scan_ms: r.get(11)?,
    })
}

/// Stored views, optionally only those of one repo, ordered by path.
pub fn stored_views(conn: &Connection, common_dir: Option<&str>) -> Result<Vec<StoredView>> {
    let sql = format!(
        "SELECT {VIEW_COLUMNS} FROM codemap_views
         WHERE (?1 IS NULL OR common_dir = ?1) ORDER BY worktree_path"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![common_dir], view_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn stored_view_by_path(conn: &Connection, worktree_path: &str) -> Result<Option<StoredView>> {
    let sql = format!("SELECT {VIEW_COLUMNS} FROM codemap_views WHERE worktree_path = ?1");
    Ok(conn
        .query_row(&sql, params![worktree_path], view_from_row)
        .optional()?)
}

/// Stored file rows of one view.
pub fn stored_files(conn: &Connection, view_id: i64) -> Result<Vec<FileRec>> {
    let mut stmt = conn.prepare(
        "SELECT path, blob_oid, dirty, COALESCE(size, 0), COALESCE(mtime_ns, 0)
         FROM codemap_view_files WHERE view_id = ?1 ORDER BY path",
    )?;
    let rows = stmt
        .query_map(params![view_id], |r| {
            Ok(FileRec {
                path: r.get(0)?,
                blob_oid: r.get(1)?,
                dirty: r.get(2)?,
                size: r.get(3)?,
                mtime_ns: r.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Paths a view holds below the directory `dir_rel` (repo-relative, no trailing
/// slash). One range scan of the primary key: `'0'` is `'/'` plus one, so
/// `path >= 'dir/' AND path < 'dir0'` is exactly the paths that start with
/// `dir/`, and `dir.py`, `dir-x/a` and `dir2/a` fall outside it.
pub fn stored_paths_under(conn: &Connection, view_id: i64, dir_rel: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT path FROM codemap_view_files
         WHERE view_id = ?1 AND path >= ?2 AND path < ?3 ORDER BY path",
    )?;
    let rows = stmt
        .query_map(
            params![view_id, format!("{dir_rel}/"), format!("{dir_rel}0")],
            |r| r.get(0),
        )?
        .collect::<std::result::Result<Vec<String>, _>>()?;
    Ok(rows)
}

/// One stored file row, if the view holds the path.
pub fn stored_file(conn: &Connection, view_id: i64, path: &str) -> Result<Option<FileRec>> {
    Ok(conn
        .query_row(
            "SELECT path, blob_oid, dirty, COALESCE(size, 0), COALESCE(mtime_ns, 0)
             FROM codemap_view_files WHERE view_id = ?1 AND path = ?2",
            params![view_id, path],
            |r| {
                Ok(FileRec {
                    path: r.get(0)?,
                    blob_oid: r.get(1)?,
                    dirty: r.get(2)?,
                    size: r.get(3)?,
                    mtime_ns: r.get(4)?,
                })
            },
        )
        .optional()?)
}

/// True when some file row of the view points at a blob with no current-version
/// row (an extractor bump, or a scan that died halfway).
pub fn view_has_unparsed_blobs(conn: &Connection, view_id: i64) -> Result<bool> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM codemap_view_files f
             WHERE f.view_id = ?1 AND NOT EXISTS (
                SELECT 1 FROM codemap_blobs b
                WHERE b.blob_oid = f.blob_oid AND b.extractor_version = ?2)
             LIMIT 1",
            params![view_id, extractor_version()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

// ---- repos ----------------------------------------------------------------

/// Insert a repo or refresh it. `source = "cli"` upgrades an existing row's
/// source; `force_enable` re-enables a tombstoned repo (an explicit `add`).
pub fn upsert_repo(
    conn: &Connection,
    common_dir: &str,
    main_path: Option<&str>,
    source: &str,
    force_enable: bool,
) -> Result<()> {
    conn.execute(
        "INSERT INTO codemap_repos (common_dir, main_path, source, enabled, added_at)
         VALUES (?1, ?2, ?3, 1, ?4)
         ON CONFLICT(common_dir) DO UPDATE SET
            main_path = COALESCE(excluded.main_path, codemap_repos.main_path),
            source = CASE WHEN excluded.source = 'cli' THEN 'cli' ELSE codemap_repos.source END,
            enabled = CASE WHEN ?5 = 1 THEN 1 ELSE codemap_repos.enabled END",
        params![
            common_dir,
            main_path,
            source,
            super::now_secs(),
            force_enable as i64
        ],
    )?;
    Ok(())
}

/// Tombstone a repo (`enabled = 0`) so discovery never re-adds it, creating the
/// row when the repo was never registered.
pub fn disable_repo(conn: &Connection, common_dir: &str, main_path: Option<&str>) -> Result<()> {
    conn.execute(
        "INSERT INTO codemap_repos (common_dir, main_path, source, enabled, added_at)
         VALUES (?1, ?2, 'cli', 0, ?3)
         ON CONFLICT(common_dir) DO UPDATE SET enabled = 0",
        params![common_dir, main_path, super::now_secs()],
    )?;
    Ok(())
}

pub fn list_repos(conn: &Connection) -> Result<Vec<StoredRepo>> {
    let mut stmt = conn.prepare(
        "SELECT common_dir, main_path, source, enabled, added_at, last_poll_at, status
         FROM codemap_repos ORDER BY common_dir",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(StoredRepo {
                common_dir: r.get(0)?,
                main_path: r.get(1)?,
                source: r.get(2)?,
                enabled: r.get::<_, i64>(3)? != 0,
                added_at: r.get(4)?,
                last_poll_at: r.get(5)?,
                status: r.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Stamp a repo's last poll, if it is registered.
pub fn record_poll(conn: &Connection, common_dir: &str, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE codemap_repos SET last_poll_at = ?2, status = ?3 WHERE common_dir = ?1",
        params![common_dir, super::now_secs(), status],
    )?;
    Ok(())
}

// ---- session signals --------------------------------------------------------

/// A session cwd as stored.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StoredSignal {
    pub cwd: String,
    /// Seconds since the epoch of the latest hook fire for this cwd.
    pub last_at: i64,
    /// `session_start` or `post_tool_use`.
    pub source: String,
    /// The repo the daemon resolved the cwd to (set by `signal_resolve`).
    pub common_dir: Option<String>,
    /// What resolving it came to: `repo`, `protected`, `not-a-repo`, `temp`,
    /// `home`, `removed` (set by `signal_resolve`).
    pub status: Option<String>,
}

/// True when the signals table exists (a database from an older binary may not
/// have it; every reader then sees no signals).
pub fn signals_ready(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'codemap_signals'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n == 1)
    .unwrap_or(false)
}

/// Record that a session is working in `cwd` at `at` (seconds). ONE statement:
/// the hooks call this, so it does no read, no git and no filesystem call. The
/// daemon-filled columns survive the update.
pub fn signal_upsert(conn: &Connection, cwd: &str, source: &str, at: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO codemap_signals (cwd, last_at, source) VALUES (?1, ?2, ?3)
         ON CONFLICT(cwd) DO UPDATE SET
            last_at = MAX(excluded.last_at, codemap_signals.last_at),
            source = excluded.source",
        params![cwd, at, source],
    )?;
    Ok(())
}

/// Signals at or after `since` (seconds), newest first.
pub fn list_signals(conn: &Connection, since: i64) -> Result<Vec<StoredSignal>> {
    if !signals_ready(conn) {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT cwd, last_at, source, common_dir, status FROM codemap_signals
         WHERE last_at >= ?1 ORDER BY last_at DESC, cwd",
    )?;
    let rows = stmt
        .query_map(params![since], |r| {
            Ok(StoredSignal {
                cwd: r.get(0)?,
                last_at: r.get(1)?,
                source: r.get(2)?,
                common_dir: r.get(3)?,
                status: r.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The latest signal time (seconds) per repo, for signals at or after `since`
/// whose cwd the daemon resolved to a repo. Keyed by common dir.
pub fn signal_activity(
    conn: &Connection,
    since: i64,
) -> Result<std::collections::HashMap<String, i64>> {
    if !signals_ready(conn) {
        return Ok(Default::default());
    }
    let mut stmt = conn.prepare(
        "SELECT common_dir, MAX(last_at) FROM codemap_signals
         WHERE status = 'repo' AND common_dir IS NOT NULL AND last_at >= ?1
         GROUP BY common_dir",
    )?;
    let rows = stmt
        .query_map(params![since], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    Ok(rows)
}

/// Signals at or after `since` the daemon has not resolved yet (a cwd seen for the
/// first time). Discovery runs ahead of its schedule when there are any.
pub fn signals_unresolved(conn: &Connection, since: i64) -> Result<i64> {
    if !signals_ready(conn) {
        return Ok(0);
    }
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM codemap_signals WHERE status IS NULL AND last_at >= ?1",
        params![since],
        |r| r.get(0),
    )?)
}

/// Record what the daemon made of a signal's cwd. Writes only when the stored
/// answer differs, so a steady state costs no write.
pub fn signal_resolve(
    conn: &Connection,
    cwd: &str,
    common_dir: Option<&str>,
    status: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE codemap_signals SET common_dir = ?2, status = ?3
         WHERE cwd = ?1 AND (common_dir IS NOT ?2 OR status IS NOT ?3)",
        params![cwd, common_dir, status],
    )?;
    Ok(())
}

/// Drop signals last seen before `cutoff` (seconds): the table must not grow
/// with every directory a session ever started in.
pub fn signal_gc(conn: &Connection, cutoff: i64) -> Result<usize> {
    if !signals_ready(conn) {
        return Ok(0);
    }
    Ok(conn.execute(
        "DELETE FROM codemap_signals WHERE last_at < ?1",
        params![cutoff],
    )?)
}

// ---- queue ----------------------------------------------------------------

/// Queue one absolute path for a partial rescan. `at` is nanoseconds since the
/// epoch. Enqueueing a path that is already queued strictly raises its
/// `enqueued_at`, even on an equal clock reading, so a drain that deletes "rows
/// at or below the value I read" can never delete a later re-enqueue.
pub fn queue_push(conn: &Connection, path: &str, at: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO codemap_queue (path, enqueued_at) VALUES (?1, ?2)
         ON CONFLICT(path) DO UPDATE SET
            enqueued_at = MAX(excluded.enqueued_at, codemap_queue.enqueued_at + 1)",
        params![path, at],
    )?;
    Ok(())
}

/// Every queued (path, enqueued_at), oldest first.
pub fn queue_read(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt =
        conn.prepare("SELECT path, enqueued_at FROM codemap_queue ORDER BY enqueued_at")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Delete a queued path only if it has not been re-enqueued since `at` was read.
pub fn queue_delete_upto(conn: &Connection, path: &str, at: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM codemap_queue WHERE path = ?1 AND enqueued_at <= ?2",
        params![path, at],
    )?)
}

/// Drop rows enqueued before `cutoff` (nanoseconds): a path whose rescan keeps
/// failing must not stay queued for ever.
pub fn queue_gc(conn: &Connection, cutoff: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM codemap_queue WHERE enqueued_at < ?1",
        params![cutoff],
    )?)
}
