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
//! 3. In one short write transaction, re-read the project's counter
//!    (`codegraph::project_gen`, bumped by every write a resolve reads). If it
//!    moved since the plan's snapshot a hook changed the graph, the plan is
//!    dropped and made again; otherwise [`apply_planned`] the list. After
//!    [`MAX_OPTIMISTIC_ATTEMPTS`] drops the project is drained in one write
//!    transaction. Each replayed write is still guarded by the edge state it
//!    was planned from.
//! 4. Plan the rank from the resolved edges and, in one short write
//!    transaction under the same counter guard, write the rows that changed and
//!    settle the files.
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
    /// Plans dropped because a hook changed the project's graph meanwhile.
    pub replans: usize,
    /// Projects drained inside one write transaction after repeated drops.
    pub locked_passes: usize,
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

/// Optimistic attempts before a project is drained under one write
/// transaction. A plan is discarded when a hook changed the project's graph
/// between planning and applying; a busy project could discard forever.
pub const MAX_OPTIMISTIC_ATTEMPTS: usize = 3;

/// Points in a pass where a test can act as a concurrent hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The resolve is planned, nothing applied (`attempt` counts from 0).
    ResolvePlanned { attempt: usize },
    /// The rank is planned, nothing written.
    RankPlanned { attempt: usize },
}

/// The drain itself, on a connection the caller owns. Public for tests, which
/// pass an `exists` of their own.
pub fn drain(conn: &Connection, file_exists: &dyn Fn(&str) -> bool) -> Result<GraphDrain> {
    drain_with(conn, file_exists, &|_| {})
}

/// [`drain`] with `between` called at each [`Phase`], so a test can make the
/// change a hook would make at that exact point.
pub fn drain_with(
    conn: &Connection,
    file_exists: &dyn Fn(&str) -> bool,
    between: &dyn Fn(Phase),
) -> Result<GraphDrain> {
    let started = Instant::now();
    let mut out = GraphDrain::default();
    let pending = codegraph::pending_resolve_files(conn)?;
    let mut projects: Vec<&str> = pending.iter().map(|(p, _, _)| p.as_str()).collect();
    projects.dedup();

    for project in projects {
        let files: Vec<&(String, String, i64)> =
            pending.iter().filter(|(p, _, _)| p == project).collect();
        drain_project(conn, project, &files, file_exists, between, &mut out)?;
        out.projects += 1;
    }
    out.elapsed = started.elapsed();
    Ok(out)
}

/// One project: resolve, then rank, then settle `files`.
///
/// Plan and apply are linearized by the project's generation counter
/// (`codegraph::project_gen`): the plan is made in a read snapshot that also
/// reads the counter; under the write lock the counter is read again and the
/// plan is applied only if it has not moved. A hook that added a competing
/// definition, retired the planned destination or re-extracted a caller in
/// between has bumped it, so the plan is dropped and made again from the new
/// state. After [`MAX_OPTIMISTIC_ATTEMPTS`] drops in a row the project is
/// drained inside one write transaction, which no hook can interleave with.
fn drain_project(
    conn: &Connection,
    project: &str,
    files: &[&(String, String, i64)],
    file_exists: &dyn Fn(&str) -> bool,
    between: &dyn Fn(Phase),
    out: &mut GraphDrain,
) -> Result<()> {
    // 2/3. Resolve.
    let mut applied = None;
    for attempt in 0..MAX_OPTIMISTIC_ATTEMPTS {
        let read = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Deferred)?;
        let seen = codegraph::project_gen(&read, project)?;
        let (_stats, writes) = plan_resolve(&read, project, file_exists)?;
        drop(read);
        between(Phase::ResolvePlanned { attempt });

        let lock_started = Instant::now();
        let write = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        if codegraph::project_gen(&write, project)? != seen {
            drop(write);
            out.replans += 1;
            continue;
        }
        let skipped = apply_planned(&write, &writes)?;
        write.commit()?;
        out.longest_lock_hold = out.longest_lock_hold.max(lock_started.elapsed());
        out.edge_writes += writes.len();
        applied = Some(skipped);
        break;
    }
    let Some(skipped) = applied else {
        return drain_project_locked(conn, project, files, file_exists, out);
    };

    // 4. Rank from the resolved edges; write what changed and settle, under the
    // same counter guard.
    for attempt in 0..MAX_OPTIMISTIC_ATTEMPTS {
        let read = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Deferred)?;
        let seen = codegraph::project_gen(&read, project)?;
        let changed = changed_rank_rows(&read, rank_rows(&read, project)?)?;
        drop(read);
        between(Phase::RankPlanned { attempt });

        let lock_started = Instant::now();
        let write = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        if codegraph::project_gen(&write, project)? != seen {
            drop(write);
            out.replans += 1;
            continue;
        }
        write_rank_rows(&write, &changed)?;
        out.files += settle(&write, files, skipped)?;
        write.commit()?;
        out.longest_lock_hold = out.longest_lock_hold.max(lock_started.elapsed());
        out.rank_writes += changed.len();
        return Ok(());
    }
    // The graph kept moving under the rank plan; the resolve above stands (a
    // file whose edges moved is pending again), so redo the whole project under
    // the lock rather than leave the ranks behind.
    drain_project_locked(conn, project, files, file_exists, out)
}

/// The project drained inside one write transaction: plan, apply, rank, settle.
/// Holds the write lock for the whole pass (seconds on a large project), which
/// is why it is the last resort and not the path.
fn drain_project_locked(
    conn: &Connection,
    project: &str,
    files: &[&(String, String, i64)],
    file_exists: &dyn Fn(&str) -> bool,
    out: &mut GraphDrain,
) -> Result<()> {
    let lock_started = Instant::now();
    let write = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let (_stats, writes) = plan_resolve(&write, project, file_exists)?;
    let skipped = apply_planned(&write, &writes)?;
    let changed = changed_rank_rows(&write, rank_rows(&write, project)?)?;
    write_rank_rows(&write, &changed)?;
    out.files += settle(&write, files, skipped)?;
    write.commit()?;
    out.longest_lock_hold = out.longest_lock_hold.max(lock_started.elapsed());
    out.edge_writes += writes.len();
    out.rank_writes += changed.len();
    out.locked_passes += 1;
    Ok(())
}

/// Settle `files` at the generation the drain read, unless a write found its
/// edge changed (then the plan was made from state that has moved on).
fn settle(conn: &Connection, files: &[&(String, String, i64)], skipped: usize) -> Result<usize> {
    if skipped > 0 {
        return Ok(0);
    }
    let mut settled = 0;
    for (p, file, seen) in files {
        if codegraph::settle_resolved_gen(conn, p, file, *seen)? {
            settled += 1;
        }
    }
    Ok(settled)
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

    /// `caller` (c.rs) calls `foo`, defined only in a.rs; c.rs is pending.
    fn only_one_foo() -> (tempfile::TempDir, std::path::PathBuf, Storage) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("csr-engine.db");
        let storage = Storage::open(&db).unwrap();
        storage
            .with_connection(|c| {
                upsert_node(c, &def("foo_a", "a.rs", "foo"))?;
                upsert_node(c, &def("caller", "c.rs", "caller"))?;
                codegraph::replace_file_edges(c, "proj", "c.rs", &[call("caller", "foo", "c.rs")])?;
                Ok(())
            })
            .unwrap();
        storage
            .upsert_code_file_state_pending("proj", "c.rs", "hash-1")
            .unwrap();
        (dir, db, storage)
    }

    /// What a hook does: extract b.rs, which defines a second `foo`.
    fn add_competing_foo(db: &std::path::Path) {
        let hook = Storage::open(db).unwrap();
        let nodes = [def("foo_b", "b.rs", "foo")];
        hook.store_code_extraction(&codegraph::Extraction {
            project: "proj",
            file: "b.rs",
            content_hash: "hash-b",
            nodes: &nodes,
            source_chunk_id: None,
            edges: Some(&[]),
        })
        .unwrap();
    }

    #[test]
    fn a_plan_made_before_a_competing_definition_landed_is_not_replayed() {
        let (_dir, db, storage) = only_one_foo();
        let report = storage
            .with_connection(|c| {
                drain_with(c, &|_: &str| true, &|phase| {
                    if phase == (Phase::ResolvePlanned { attempt: 0 }) {
                        add_competing_foo(&db);
                    }
                })
            })
            .unwrap();
        assert_eq!(report.replans, 1, "the first plan was dropped");
        assert_eq!(report.locked_passes, 0);
        // Planned against both definitions, the call is ambiguous: it must not
        // be bound to a.rs for good.
        assert_eq!(edge_state(&storage), ("name:foo".into(), 0));
    }

    #[test]
    fn a_project_that_keeps_changing_is_drained_under_one_write_transaction() {
        let (_dir, db, storage) = only_one_foo();
        let n = std::cell::Cell::new(0);
        let report = storage
            .with_connection(|c| {
                drain_with(c, &|_: &str| true, &|phase| {
                    if matches!(phase, Phase::ResolvePlanned { .. }) {
                        // A different file each time, so every plan is stale.
                        n.set(n.get() + 1);
                        let hook = Storage::open(&db).unwrap();
                        hook.upsert_code_node(&def(
                            &format!("noise_{}", n.get()),
                            "n.rs",
                            &format!("noise{}", n.get()),
                        ))
                        .unwrap();
                    }
                })
            })
            .unwrap();
        assert_eq!(report.replans, MAX_OPTIMISTIC_ATTEMPTS);
        assert_eq!(report.locked_passes, 1);
        assert_eq!(edge_state(&storage), ("foo_a".into(), 1));
        assert!(pending(&storage).is_empty(), "settled by the locked pass");
    }

    #[test]
    fn a_node_retired_after_the_rank_was_planned_does_not_abort_the_drain() {
        let (_dir, db, storage) = extracted();
        let report = storage
            .with_connection(|c| {
                drain_with(c, &|_: &str| true, &|phase| {
                    if phase == (Phase::RankPlanned { attempt: 0 }) {
                        // The hook re-extracts a.rs without `callee`: the node
                        // the rank plan holds a row for is gone.
                        let hook = Storage::open(&db).unwrap();
                        let seen = ["other".to_string()];
                        hook.retire_missing_code_nodes("proj", "a.rs", &seen)
                            .unwrap();
                    }
                })
            })
            .unwrap();
        assert_eq!(report.replans, 1, "the rank plan was dropped and redone");
        let violations: i64 = storage
            .with_connection(|c| {
                Ok(
                    c.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(violations, 0);
    }
}
