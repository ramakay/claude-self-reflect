//! The hook's way to hand an edit to the graph drain without the write lock.
//!
//! A PostToolUse hook tries one INSERT into `code_graph_snapshots` under a short
//! lock wait. If the database will not take it (the daemon or a drain holds the
//! write lock, the table is missing, the disk is full), the same record goes to
//! `<data dir>/keep/graph-spool/<seq>-<pid>.json` instead: a file the hook
//! creates and renames into place, which needs no database at all. The drain
//! ingests the directory before it extracts anything and deletes a file only
//! after the transaction that ingested it has committed.
//!
//! One record per file, published by rename, is a deliberate departure from one
//! append-only `<pid>.jsonl`: ingesting and then deleting a shared append file
//! loses a line a hook appends between the drain's read and its unlink, while a
//! record published by rename is either whole and visible or not there.
//!
//! Crash points: a hook that dies before the rename leaves a `.tmp` that nothing
//! reads (swept after an hour); a drain that dies after the ingest commit and
//! before the unlink re-reads the file next pass, where `origin` (the file name)
//! and the per-file high-water mark (`codegraph::consume_snapshot`) make it a
//! no-op.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::storage::codegraph::{self, Snapshot};

/// Directory under the data dir the spool lives in.
const SPOOL_SUBDIR: &str = "keep/graph-spool";

/// A `.tmp` older than this is a hook that died mid-write.
const STALE_TMP_SECS: u64 = 3600;

/// The spool directory for the database file at `db_path`.
pub fn dir_for_db(db_path: &Path) -> Option<PathBuf> {
    if db_path.as_os_str().is_empty() {
        return None;
    }
    Some(db_path.parent()?.join(SPOOL_SUBDIR))
}

/// The spool directory for the database `conn` has open (`None` in memory).
pub fn dir_for_conn(conn: &Connection) -> Option<PathBuf> {
    dir_for_db(Path::new(conn.path().filter(|p| !p.is_empty())?))
}

#[derive(Serialize, Deserialize)]
struct Record {
    v: u32,
    seq: i64,
    project: String,
    file: String,
    read_path: String,
    kind: String,
    /// base64 of the file's text; absent for an `unsupported` snapshot.
    content_b64: Option<String>,
    conv_id: String,
    session_id: String,
    source_chunk_id: Option<String>,
}

/// Write `s` as one spool record. Never opens the database.
pub fn write(dir: &Path, s: &Snapshot) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let record = Record {
        v: 1,
        seq: s.seq,
        project: s.project.clone(),
        file: s.file.clone(),
        read_path: s.read_path.clone(),
        kind: s.kind.clone(),
        content_b64: s.content.as_ref().map(|c| STANDARD.encode(c)),
        conv_id: s.conv_id.clone(),
        session_id: s.session_id.clone(),
        source_chunk_id: s.source_chunk_id.clone(),
    };
    let name = format!("{}-{}.json", s.seq, std::process::id());
    let tmp = dir.join(format!("{name}.tmp"));
    let done = dir.join(&name);
    std::fs::write(&tmp, serde_json::to_vec(&record)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &done).with_context(|| format!("publish {}", done.display()))?;
    Ok(done)
}

fn records(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files
}

/// Whether the spool holds a record the drain has not ingested.
pub fn any_pending(dir: &Path) -> bool {
    !records(dir).is_empty()
}

/// Ingest every record into `code_graph_snapshots` on `conn` (inside the
/// caller's transaction if it has one) and return the files to delete once that
/// transaction has committed. A record that cannot be parsed is renamed
/// `.bad` and left for a human.
pub fn ingest(conn: &Connection, dir: &Path) -> Result<Vec<PathBuf>> {
    sweep_stale_tmp(dir);
    let mut ingested = Vec::new();
    for path in records(dir) {
        let parsed = std::fs::read(&path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<Record>(&raw).ok())
            .and_then(|r| {
                let content = match &r.content_b64 {
                    Some(b) => Some(String::from_utf8(STANDARD.decode(b).ok()?).ok()?),
                    None => None,
                };
                Some(Snapshot {
                    seq: r.seq,
                    project: r.project,
                    file: r.file,
                    read_path: r.read_path,
                    kind: r.kind,
                    content,
                    conv_id: r.conv_id,
                    session_id: r.session_id,
                    source_chunk_id: r.source_chunk_id,
                    origin: path.file_name().map(|n| n.to_string_lossy().into_owned()),
                })
            });
        match parsed {
            Some(snapshot) => {
                codegraph::insert_snapshot(conn, &snapshot)?;
                ingested.push(path);
            }
            None => {
                let _ = std::fs::rename(&path, path.with_extension("bad"));
            }
        }
    }
    Ok(ingested)
}

/// Delete ingested records. Call after the ingesting transaction committed.
pub fn remove(paths: &[PathBuf]) {
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
}

fn sweep_stale_tmp(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_some_and(|x| x == "tmp")
            && e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_secs() > STALE_TMP_SECS)
        {
            let _ = std::fs::remove_file(path);
        }
    }
}
