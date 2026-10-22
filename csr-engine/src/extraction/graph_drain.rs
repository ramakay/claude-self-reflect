//! The project-wide edge resolve, off the hook path.
//!
//! A PostToolUse hook re-extracts the edited file and stops: its edges land as
//! `name:<symbol>` placeholders, which the code-graph queries already read
//! ("callers surface even before resolution", `storage::codegraph::
//! query_callers`), and its `graph_gen` goes up by one. Binding those names
//! across the whole project took 30 s inside the hook.
//!
//! The drain does it later, wherever a long-lived csr-engine process runs: the
//! daemon's main loop, or the MCP server when there is no daemon
//! (`daemon::graph_drain_loop`). For each project with a pending file it runs
//! the ordinary project-wide resolve and rank, and then settles those files.
//!
//! The pass is planned without the write lock and applied under it:
//!
//! 1. Read the pending files and their generations.
//! 2. In a read transaction, [`plan_resolve`] the project: every write the
//!    resolve would make, none executed. This is nearly all of the time.
//! 3. In one short write transaction, [`apply_planned`] the list. Each write is
//!    guarded by the state it was planned from, so an edge an edit replaced in
//!    the meantime is left alone and its file stays pending.
//! 4. Plan the rank from the resolved edges and, in one short write
//!    transaction, write the rows that changed and settle the files.
//!
//! Settling is a compare-and-set on the generation read in step 1: a file
//! extracted again since keeps the newer generation and stays pending. Nothing
//! is settled before the rank is written, and a process killed anywhere leaves
//! the files pending, so the next pass runs the whole thing again. The apply
//! transaction is atomic, so a kill never leaves half of a resolve.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use rusqlite::{Connection, TransactionBehavior};

use super::resolver::{apply_planned, plan_resolve};
use crate::search::code_rank::{changed_rank_rows, rank_rows, write_rank_rows};
use crate::storage::{codegraph, Storage};

/// Seconds between pending checks. A check is one indexed query; the pass runs
/// only when a file is pending.
pub const DRAIN_INTERVAL: Duration = Duration::from_secs(15);

const LOCK_FILE: &str = "graph-drain.lock";

/// What one drain did.
#[derive(Debug, Default, Clone, Copy)]
pub struct GraphDrain {
    /// Projects resolved and ranked.
    pub projects: usize,
    /// Files settled (not extracted again during the pass).
    pub files: usize,
    /// Edge writes the pass made.
    pub edge_writes: usize,
    /// Rank rows written.
    pub rank_writes: usize,
    /// Wall time of the whole drain.
    pub elapsed: Duration,
    /// Longest the database write lock was held by one transaction.
    pub longest_lock_hold: Duration,
}

/// Drain once, unless nothing is pending or another process is draining.
/// The lock is per data directory, so the daemon and any number of MCP servers
/// never run two passes at once. `storage` answers the cheap pending check; the
/// pass uses a connection of its own, so it does not hold up what else the
/// calling process does with `storage`.
pub fn drain_if_pending(storage: &Storage, db_path: &Path) -> Result<Option<GraphDrain>> {
    if storage.pending_resolve_files()?.is_empty() {
        return Ok(None);
    }
    let data_dir = db_path.parent().unwrap_or(Path::new("."));
    let Some(_lock) = DrainLock::try_acquire(data_dir) else {
        return Ok(None);
    };
    let own = Storage::open(db_path)?;
    own.with_connection(|conn| drain(conn, &|file| exists(file)))
        .map(Some)
}

fn exists(file: &str) -> bool {
    super::repo_path::canonical_repo_path(Path::new(file)).is_file()
}

/// The drain itself, on a connection the caller owns. Public for tests, which
/// pass an `exists` of their own.
pub fn drain(conn: &Connection, file_exists: &dyn Fn(&str) -> bool) -> Result<GraphDrain> {
    let started = Instant::now();
    let mut out = GraphDrain::default();
    let pending = codegraph::pending_resolve_files(conn)?;
    let mut projects: Vec<&str> = pending.iter().map(|(p, _, _)| p.as_str()).collect();
    projects.dedup();

    for project in projects {
        // 2. Plan under one read snapshot.
        let read = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Deferred)?;
        let (_stats, writes) = plan_resolve(&read, project, file_exists)?;
        drop(read);

        // 3. Apply under the write lock.
        let lock_started = Instant::now();
        let write = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let skipped = apply_planned(&write, &writes)?;
        write.commit()?;
        out.longest_lock_hold = out.longest_lock_hold.max(lock_started.elapsed());
        out.edge_writes += writes.len();

        // 4. Rank from the resolved edges, then write what changed and settle.
        let read = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Deferred)?;
        let changed = changed_rank_rows(&read, rank_rows(&read, project)?)?;
        drop(read);
        let lock_started = Instant::now();
        let write = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        write_rank_rows(&write, &changed)?;
        // A write that found its edge changed means the plan was made from
        // state that has moved on: leave the project pending for the next pass.
        if skipped == 0 {
            for (p, file, seen) in pending.iter().filter(|(p, _, _)| p == project) {
                if codegraph::settle_resolved_gen(&write, p, file, *seen)? {
                    out.files += 1;
                }
            }
        }
        write.commit()?;
        out.longest_lock_hold = out.longest_lock_hold.max(lock_started.elapsed());
        out.rank_writes += changed.len();
        out.projects += 1;
    }
    out.elapsed = started.elapsed();
    Ok(out)
}

struct DrainLock {
    _file: std::fs::File,
}

impl DrainLock {
    fn try_acquire(data_dir: &Path) -> Option<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(data_dir.join(LOCK_FILE))
            .ok()?;
        fs2::FileExt::try_lock_exclusive(&file).ok()?;
        Some(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::codegraph::{upsert_node, EdgeRow, NodeRow};

    fn def(id: &str, file: &str, name: &str) -> NodeRow {
        NodeRow {
            id: id.into(),
            repo: "r".into(),
            project: "proj".into(),
            file: file.into(),
            lang: "rust".into(),
            kind: "function".into(),
            name: name.into(),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        }
    }

    fn call(src: &str, name: &str, file: &str) -> EdgeRow {
        EdgeRow {
            src_id: src.into(),
            dst_id: format!("name:{name}"),
            kind: "calls".into(),
            src_file: file.into(),
            resolved: 0,
            weight: 1.0,
            ..EdgeRow::default()
        }
    }

    /// A database file with `caller` (b.rs) calling `callee` (a.rs) and the
    /// edge extracted but not resolved: what the hook leaves behind.
    fn extracted() -> (tempfile::TempDir, std::path::PathBuf, Storage) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("csr-engine.db");
        let storage = Storage::open(&db).unwrap();
        storage
            .with_connection(|c| {
                upsert_node(c, &def("callee", "a.rs", "callee"))?;
                upsert_node(c, &def("caller", "b.rs", "caller"))?;
                codegraph::replace_file_edges(
                    c,
                    "proj",
                    "b.rs",
                    &[call("caller", "callee", "b.rs")],
                )?;
                Ok(())
            })
            .unwrap();
        storage
            .upsert_code_file_state_pending("proj", "b.rs", "hash-1")
            .unwrap();
        (dir, db, storage)
    }

    fn edge_state(storage: &Storage) -> (String, i64) {
        storage
            .with_connection(|c| {
                Ok(c.query_row(
                    "SELECT dst_id, resolved FROM code_edges WHERE src_id = 'caller'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap()
    }

    fn pending(storage: &Storage) -> Vec<(String, String, i64)> {
        storage.pending_resolve_files().unwrap()
    }

    #[test]
    fn the_drain_resolves_the_edge_and_settles_the_file() {
        let (_dir, db, storage) = extracted();
        assert_eq!(edge_state(&storage), ("name:callee".into(), 0));
        assert_eq!(pending(&storage).len(), 1);

        let report = drain_if_pending(&storage, &db).unwrap().unwrap();
        assert_eq!((report.projects, report.files), (1, 1));
        assert_eq!(edge_state(&storage), ("callee".into(), 1));
        assert!(pending(&storage).is_empty());
        // Nothing pending: no pass at all.
        assert!(drain_if_pending(&storage, &db).unwrap().is_none());
    }

    #[test]
    fn a_file_extracted_again_during_the_pass_stays_pending() {
        let (_dir, db, storage) = extracted();
        // `mystery` reaches the stale tier, which asks whether the source file
        // exists: the one point in a plan where a test can act mid-pass. Another
        // connection extracts b.rs again, as a hook would.
        storage
            .with_connection(|c| {
                codegraph::replace_file_edges(
                    c,
                    "proj",
                    "b.rs",
                    &[
                        call("caller", "callee", "b.rs"),
                        call("caller", "mystery", "b.rs"),
                    ],
                )?;
                Ok(())
            })
            .unwrap();
        let hook = Storage::open(&db).unwrap();
        let bumped = std::cell::Cell::new(false);
        let report = storage
            .with_connection(|c| {
                drain(c, &|_: &str| {
                    if !bumped.replace(true) {
                        hook.upsert_code_file_state_pending("proj", "b.rs", "hash-2")
                            .unwrap();
                    }
                    true
                })
            })
            .unwrap();
        assert!(bumped.get(), "the pass reached the stale tier");
        assert_eq!(report.files, 0, "the newer generation was not settled");
        assert_eq!(pending(&storage).len(), 1);
        assert_eq!(pending(&storage)[0].2, 2);

        // The next pass settles it.
        let report = drain_if_pending(&storage, &db).unwrap().unwrap();
        assert_eq!(report.files, 1);
        assert!(pending(&storage).is_empty());
    }

    #[test]
    fn a_pass_killed_while_planning_changes_nothing_and_the_rerun_matches() {
        let (_dir, db, storage) = extracted();
        let (_twin_dir, twin_db, twin) = extracted();

        // A process dies at its first write: the plan is made, the apply
        // transaction is open and never commits.
        storage
            .with_connection(|c| {
                let read = rusqlite::Transaction::new_unchecked(c, TransactionBehavior::Deferred)?;
                let (_, writes) = super::super::resolver::plan_resolve(&read, "proj", &|_| true)?;
                drop(read);
                assert!(!writes.is_empty());
                // Applied inside a transaction that is dropped, not committed.
                let write =
                    rusqlite::Transaction::new_unchecked(c, TransactionBehavior::Immediate)?;
                apply_planned(&write, &writes)?;
                drop(write);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            edge_state(&storage),
            ("name:callee".into(), 0),
            "rolled back"
        );
        assert_eq!(pending(&storage).len(), 1);

        drain_if_pending(&storage, &db).unwrap().unwrap();
        drain_if_pending(&twin, &twin_db).unwrap().unwrap();
        assert_eq!(edge_state(&storage), edge_state(&twin));
        assert_eq!(edge_state(&storage), ("callee".into(), 1));
    }

    #[test]
    fn a_second_drain_over_a_settled_graph_changes_nothing() {
        let (_dir, db, storage) = extracted();
        drain_if_pending(&storage, &db).unwrap().unwrap();
        let once = edge_state(&storage);
        // The file is extracted again unchanged: pending, already resolved.
        storage
            .upsert_code_file_state_pending("proj", "b.rs", "hash-1")
            .unwrap();
        let report = drain_if_pending(&storage, &db).unwrap().unwrap();
        assert_eq!(report.files, 1);
        assert_eq!(edge_state(&storage), once);
    }

    #[test]
    fn only_one_drain_runs_at_a_time() {
        let (dir, db, storage) = extracted();
        let held = DrainLock::try_acquire(dir.path()).unwrap();
        assert!(drain_if_pending(&storage, &db).unwrap().is_none());
        assert_eq!(pending(&storage).len(), 1, "left for the lock holder");
        drop(held);
        assert!(drain_if_pending(&storage, &db).unwrap().is_some());
    }

    #[test]
    fn the_drain_runs_with_the_code_map_switched_off() {
        // The drain is the daemon's own loop, not a tick of the code map pump,
        // so `CSR_NO_CODEMAP=1` does not reach it.
        let _off = crate::codemap::test_switch::force(true);
        assert!(crate::codemap::disabled());
        let (_dir, db, storage) = extracted();
        assert!(drain_if_pending(&storage, &db).unwrap().is_some());
        assert_eq!(edge_state(&storage), ("callee".into(), 1));
    }
}
