//! The code graph's single writer: extraction and the project-wide resolve,
//! off the hook path.
//!
//! A PostToolUse hook writes no graph row. It snapshots the edit it just saw
//! (file content, conversation / session / transcript chunk, a capture-time
//! `seq`) into `code_graph_snapshots`, or, when the database will not take the
//! insert quickly, into the lock-free spool (`graph_spool`). The drain extracts
//! the snapshots in `seq` order, one transaction each, with the attribution the
//! hook captured, so the drain is the only writer of `code_nodes` / `code_edges`
//! / rank that hooks cause. A snapshot at or below the file's high-water mark
//! (`codegraph::consume_snapshot`) arrived late and is dropped.
//!
//! The drain then binds the `name:<symbol>` placeholder edges across the whole
//! project (which took 30 s inside the hook), wherever a long-lived csr-engine
//! process runs: the daemon's main loop, or the MCP server when there is no
//! daemon (`daemon::graph_drain_loop`).
//!
//! The pass is planned without the write lock and applied under it:
//!
//! 1. Extract the queued snapshots (spool first), then read the project's
//!    pending files and their generations.
//! 2. In a read transaction, [`plan_resolve`] the project: every write the
//!    resolve would make, none executed. This is nearly all of the time.
//! 3. In one short write transaction, re-read the project's counter
//!    (`codegraph::project_gen`, bumped by triggers on every table a resolve
//!    reads) and look for queued work: a snapshot or spool record of the
//!    project means an edit the plan knows nothing of, and resolving now could
//!    bind a caller to a definition that edit is about to retire. If either
//!    moved the plan is dropped, the new work extracted and the plan made again;
//!    otherwise [`apply_planned`] the list. After [`MAX_OPTIMISTIC_ATTEMPTS`]
//!    drops the project is drained in one write transaction that extracts what
//!    is queued first. Each replayed write is still guarded by the edge state it
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
//!
//! What this does not close: a snapshot that arrives after the apply commits and
//! retires a definition a caller was just bound to leaves that edge resolved to
//! a node that no longer exists. Retiring a node has never unbound the edges
//! pointing at it, and the resolver never revisits a resolved edge; an edit that
//! arrives after a settle does the same.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use rusqlite::{Connection, TransactionBehavior};

use super::graph_spool;
use super::resolver::{apply_planned, plan_resolve};
use crate::search::code_rank::{changed_rank_rows, rank_rows, write_rank_rows};
use crate::storage::{codegraph, Storage};

/// Seconds between pending checks. A check is one indexed query; the pass runs
/// only when a file is pending.
pub const DRAIN_INTERVAL: Duration = Duration::from_secs(15);

const LOCK_FILE: &str = "graph-drain.lock";

/// What a drainer's heartbeat says it can do. A binary that wrote this has the
/// generation drain (this module); an older daemon writes no heartbeat at all,
/// so "no heartbeat with the capability" means nobody is consuming generations.
pub const DRAIN_CAPABILITY: u32 = 1;

/// `meta` key of the drainer heartbeat: `<unix seconds> <capability> <owner>`.
pub const HEARTBEAT_KEY: &str = "graph_drain_heartbeat";

/// How old a heartbeat may be and still count as a live drainer.
pub const HEARTBEAT_FRESH_SECS: u64 = 60;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Record that `owner` is draining as of now. Best effort: a locked database
/// means the beat is skipped and the next tick writes it.
pub fn beat(storage: &Storage, owner: &str) {
    let value = format!("{} {} {}", unix_now(), DRAIN_CAPABILITY, owner);
    if let Err(e) = storage.set_meta(HEARTBEAT_KEY, &value) {
        tracing::debug!("graph drain heartbeat not written: {e}");
    }
}

/// Whether a drainer other than `own` beat recently with the drain capability.
/// `value` is the stored heartbeat, `now` unix seconds.
pub fn other_drainer_is_live(value: Option<&str>, own: &str, now: u64) -> bool {
    let Some(value) = value else { return false };
    let mut parts = value.splitn(3, ' ');
    let (Some(at), Some(capability), Some(owner)) = (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Ok(at), Ok(capability)) = (at.parse::<u64>(), capability.parse::<u32>()) else {
        return false;
    };
    owner != own && capability >= DRAIN_CAPABILITY && now.saturating_sub(at) < HEARTBEAT_FRESH_SECS
}

/// [`other_drainer_is_live`] against the stored heartbeat and the clock.
pub fn another_drainer_is_live(storage: &Storage, own: &str) -> bool {
    let stored = storage.get_meta(HEARTBEAT_KEY).ok().flatten();
    other_drainer_is_live(stored.as_deref(), own, unix_now())
}

/// What one drain did.
#[derive(Debug, Default, Clone)]
pub struct GraphDrain {
    /// Projects resolved and ranked.
    pub projects: usize,
    /// Files the hook queued for extraction (out of write budget) that this
    /// pass extracted.
    pub extracted: usize,
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
    /// Projects whose pass failed, with the error. A failed project is left
    /// pending and rolled back; the others are drained regardless.
    pub errors: Vec<(String, String)>,
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
///
/// A lock that cannot be opened or taken for a reason other than another
/// process holding it is an error, not contention: it is returned, written to
/// `meta` (`graph_drain_error`, which `csr-engine status` shows) and so cannot
/// silently stop the drain.
pub fn drain_if_pending(storage: &Storage, db_path: &Path) -> Result<Option<GraphDrain>> {
    match drain_outcome(storage, db_path)? {
        Outcome::Drained(drain) => Ok(Some(drain)),
        Outcome::Idle | Outcome::Contended => Ok(None),
    }
}

/// Why a drain call did or did not run a pass.
#[derive(Debug)]
pub enum Outcome {
    /// Nothing was pending.
    Idle,
    /// Another process holds the drain lock and is draining.
    Contended,
    /// A pass ran. `GraphDrain::errors` lists projects that failed.
    Drained(GraphDrain),
}

/// [`drain_if_pending`] saying which of the three it was, and recording the
/// result in `meta`.
pub fn drain_outcome(storage: &Storage, db_path: &Path) -> Result<Outcome> {
    let result = drain_unrecorded(storage, db_path);
    record_error(storage, &result);
    result
}

fn drain_unrecorded(storage: &Storage, db_path: &Path) -> Result<Outcome> {
    // A queued snapshot counts although every file may read as settled: it has
    // not been extracted, so no generation covers it yet.
    // So does a project whose resolve inputs moved with no file pending: a repo
    // scan or a co-edit replay writes tables the resolve reads and queues no file.
    if storage.pending_resolve_files()?.is_empty()
        && !snapshots_waiting(storage, db_path)?
        && storage.dirty_graph_projects()?.is_empty()
    {
        return Ok(Outcome::Idle);
    }
    let data_dir = db_path.parent().unwrap_or(Path::new("."));
    let Some(_lock) = DrainLock::try_acquire(data_dir)? else {
        return Ok(Outcome::Contended);
    };
    let own = Storage::open(db_path)?;
    own.with_connection(|conn| drain(conn, &|file| exists(file)))
        .map(Outcome::Drained)
}

/// Whether a hook left an edit (queued, or spooled without the database) that
/// no pass has extracted.
pub fn snapshots_waiting(storage: &Storage, db_path: &Path) -> Result<bool> {
    if storage.has_queued_code_snapshots()? {
        return Ok(true);
    }
    Ok(graph_spool::dir_for_db(db_path).is_some_and(|d| graph_spool::any_pending(&d)))
}

/// Extract the waiting snapshots and nothing else: no resolve, no rank. Cheap
/// enough to run every second, which is how soon an edit's nodes and callers
/// show in `csr_code_graph` once the hook stops extracting them itself. Takes
/// the drain lock, so it never runs beside a full pass. `None` when nothing was
/// waiting or the lock was taken; a failure is recorded in `meta` but a clean
/// pass does not clear the full pass's error.
pub fn extract_waiting_snapshots(storage: &Storage, db_path: &Path) -> Result<Option<usize>> {
    if !snapshots_waiting(storage, db_path)? {
        return Ok(None);
    }
    let data_dir = db_path.parent().unwrap_or(Path::new("."));
    let Some(_lock) = DrainLock::try_acquire(data_dir)? else {
        return Ok(None);
    };
    let own = Storage::open(db_path)?;
    let spool = graph_spool::dir_for_db(db_path);
    let mut out = GraphDrain::default();
    own.with_connection(|conn| extract_snapshots(conn, spool.as_deref(), None, &mut out))?;
    if !out.errors.is_empty() {
        let message = out
            .errors
            .iter()
            .map(|(p, e)| format!("{p}: {e}"))
            .collect::<Vec<_>>()
            .join("; ");
        tracing::warn!("code graph snapshots: {message}");
        let _ = storage.set_meta(ERROR_KEY, &message);
    }
    Ok(Some(out.extracted))
}

/// `meta` key holding the last drain failure, empty when the last pass was clean.
pub const ERROR_KEY: &str = "graph_drain_error";

/// Keep `graph_drain_error` in step with what just happened: set on a failure,
/// cleared by a clean pass or an idle check, untouched by contention (that says
/// nothing about whether the lock works).
fn record_error(storage: &Storage, result: &Result<Outcome>) {
    let message = match result {
        Err(e) => format!("{e:#}"),
        Ok(Outcome::Drained(d)) if !d.errors.is_empty() => d
            .errors
            .iter()
            .map(|(p, e)| format!("{p}: {e}"))
            .collect::<Vec<_>>()
            .join("; "),
        Ok(Outcome::Contended) => return,
        Ok(_) => String::new(),
    };
    if storage
        .get_meta(ERROR_KEY)
        .ok()
        .flatten()
        .unwrap_or_default()
        == message
    {
        return;
    }
    if !message.is_empty() {
        tracing::warn!("code graph drain: {message}");
    }
    let _ = storage.set_meta(ERROR_KEY, &message);
}

fn exists(file: &str) -> bool {
    super::repo_path::canonical_repo_path(Path::new(file)).is_file()
}

/// Optimistic attempts before a project is drained under one write
/// transaction. A plan is discarded when a hook changed the project's graph
/// (or queued an edit) between planning and applying; a busy project could
/// discard forever.
pub const MAX_OPTIMISTIC_ATTEMPTS: usize = 3;

/// Points in a pass where a test can act as a concurrent hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The queued snapshots are extracted, the pending files not yet read.
    QueueExtracted,
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
    let spool = graph_spool::dir_for_conn(conn);
    // The edits hooks snapshotted are extracted first: that bumps each file's
    // `graph_gen`, so the resolve below covers them.
    extract_snapshots(conn, spool.as_deref(), None, &mut out)?;
    between(Phase::QueueExtracted);
    let pending = codegraph::pending_resolve_files(conn)?;
    let dirty = codegraph::dirty_projects(conn, codegraph::now_secs())?;
    let mut projects: Vec<&str> = pending
        .iter()
        .map(|(p, _, _)| p.as_str())
        .chain(dirty.iter().map(String::as_str))
        .collect();
    projects.sort_unstable();
    projects.dedup();

    for project in projects {
        // One project failing must not starve the ones after it: its
        // transaction rolled back, its files stay pending, the next pass tries
        // it again.
        match drain_project(
            conn,
            project,
            spool.as_deref(),
            file_exists,
            between,
            &mut out,
        ) {
            Ok(()) => out.projects += 1,
            Err(e) => out.errors.push((project.to_string(), format!("{e:#}"))),
        }
    }
    out.elapsed = started.elapsed();
    Ok(out)
}

/// Extract every queued snapshot (spool excluded) on `conn`, without resolving.
/// For tests and tools that want the graph an edit leads to.
pub fn extract_all_snapshots(conn: &Connection) -> Result<GraphDrain> {
    let mut out = GraphDrain::default();
    extract_snapshots(conn, None, None, &mut out)?;
    Ok(out)
}

/// [`extract_all_snapshots`] on a [`Storage`]'s own connection.
pub fn extract_all_snapshots_in(storage: &Storage) -> Result<GraphDrain> {
    storage.with_connection(extract_all_snapshots)
}

/// Ingest the spool into the snapshot queue and extract every queued snapshot
/// (of `only` project, or all), oldest capture first. Each snapshot is its own
/// transaction (inside the caller's, if it has one): consumed and stored
/// together or not at all. With `fail_fast` the first failure is returned (the
/// locked pass, whose transaction must roll back); otherwise it is reported with
/// the project's errors, the snapshot stays queued, and the rest go on.
fn extract_snapshots(
    conn: &Connection,
    spool: Option<&Path>,
    only: Option<&str>,
    out: &mut GraphDrain,
) -> Result<()> {
    extract_snapshots_with(conn, spool, only, out, false)
}

fn extract_snapshots_with(
    conn: &Connection,
    spool: Option<&Path>,
    only: Option<&str>,
    out: &mut GraphDrain,
    fail_fast: bool,
) -> Result<()> {
    // Standalone only: the locked pass ingests inside its own transaction and
    // passes no spool here.
    if let Some(dir) = spool {
        if conn.is_autocommit() && graph_spool::any_pending(dir) {
            let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
            let paths = graph_spool::ingest(&tx, dir)?;
            tx.commit()?;
            graph_spool::remove(&paths);
        }
    }
    for s in codegraph::queued_snapshots(conn)? {
        if only.is_some_and(|p| p != s.project) {
            continue;
        }
        match extract_one(conn, &s) {
            Ok(true) => out.extracted += 1,
            Ok(false) => {}
            Err(e) if fail_fast => return Err(e.context(format!("extract {}", s.file))),
            Err(e) => out
                .errors
                .push((s.project.clone(), format!("extract {}: {e:#}", s.file))),
        }
    }
    Ok(())
}

/// Store one snapshot. `true` when it produced graph rows.
fn extract_one(conn: &Connection, s: &codegraph::Snapshot) -> Result<bool> {
    use super::file_extract::{extract_and_store, Extracted, Job};
    if s.kind == codegraph::SNAPSHOT_UNSUPPORTED {
        codegraph::store_unsupported(conn, s)?;
        return Ok(false);
    }
    let job = Job {
        project: &s.project,
        read_path: &s.read_path,
        stored_path: &s.file,
        conv_id: &s.conv_id,
        session_id: &s.session_id,
        source_chunk_id: s.source_chunk_id.as_deref(),
        content: s.content.as_deref(),
        claim: s.seq,
    };
    Ok(matches!(
        extract_and_store(conn, &job)?,
        Extracted::Stored { .. }
    ))
}

/// Whether the project's resolve inputs moved since a pass last covered them
/// (`codegraph::dirty_projects`).
fn project_dirty(conn: &Connection, project: &str) -> Result<bool> {
    Ok(codegraph::dirty_projects(conn, codegraph::now_secs())?
        .iter()
        .any(|p| p == project))
}

/// Whether an edit of `project` is waiting that a plan made now would not
/// contain: a queued snapshot, or a spool record not ingested yet.
fn edits_waiting(conn: &Connection, project: &str, spool: Option<&Path>) -> Result<bool> {
    Ok(codegraph::has_queued_snapshots_for(conn, project)?
        || spool.is_some_and(graph_spool::any_pending))
}

/// One project: resolve, then rank, then settle its pending files.
///
/// Plan and apply are linearized by the project's generation counter
/// (`codegraph::project_gen`): the plan is made in a read snapshot that also
/// reads the counter; under the write lock the counter is read again and the
/// plan is applied only if it has not moved and no edit of the project is
/// waiting. A hook or backfill that added a competing definition, retired the
/// planned destination or re-extracted a caller in between has bumped it, so the
/// plan is dropped and made again from the new state. After
/// [`MAX_OPTIMISTIC_ATTEMPTS`] drops in a row the project is drained inside one
/// write transaction, which no database writer can interleave with.
fn drain_project(
    conn: &Connection,
    project: &str,
    spool: Option<&Path>,
    file_exists: &dyn Fn(&str) -> bool,
    between: &dyn Fn(Phase),
    out: &mut GraphDrain,
) -> Result<()> {
    // 2/3. Resolve.
    let mut applied = None;
    for attempt in 0..MAX_OPTIMISTIC_ATTEMPTS {
        if attempt > 0 {
            // Whatever made the last plan stale may have queued edits.
            extract_snapshots(conn, spool, Some(project), out)?;
        }
        // Read after the extractions, so the generations settled below are the
        // ones this plan covers.
        let files = codegraph::pending_resolve_files_for(conn, project)?;
        if files.is_empty() && !project_dirty(conn, project)? {
            return Ok(());
        }
        let read = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Deferred)?;
        let seen = codegraph::project_gen(&read, project)?;
        let (_stats, writes) = plan_resolve(&read, project, file_exists)?;
        drop(read);
        between(Phase::ResolvePlanned { attempt });

        let lock_started = Instant::now();
        let write = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        if codegraph::project_gen(&write, project)? != seen
            || edits_waiting(&write, project, spool)?
        {
            drop(write);
            out.replans += 1;
            continue;
        }
        let skipped = apply_planned(&write, &writes)?;
        // The counters this pass covers: the plan's, plus the apply's own bumps.
        let covered = codegraph::project_gen_parts(&write, project)?;
        write.commit()?;
        out.longest_lock_hold = out.longest_lock_hold.max(lock_started.elapsed());
        out.edge_writes += writes.len();
        applied = Some((files, skipped, covered));
        break;
    }
    let Some((files, skipped, covered)) = applied else {
        return drain_project_locked(conn, project, spool, file_exists, out);
    };
    let files: Vec<&(String, String, i64)> = files.iter().collect();

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
        out.files += settle(&write, &files, skipped)?;
        if skipped == 0 {
            codegraph::mark_project_settled(&write, project, covered)?;
        }
        write.commit()?;
        out.longest_lock_hold = out.longest_lock_hold.max(lock_started.elapsed());
        out.rank_writes += changed.len();
        return Ok(());
    }
    // The graph kept moving under the rank plan; the resolve above stands (a
    // file whose edges moved is pending again), so redo the whole project under
    // the lock rather than leave the ranks behind.
    drain_project_locked(conn, project, spool, file_exists, out)
}

/// The project drained inside one write transaction: ingest the spool, extract
/// what is queued, then plan, apply, rank, settle. Holds the write lock for the
/// whole pass (seconds on a large project), which is why it is the last resort
/// and not the path. Hooks that cannot get the lock meanwhile spool, so the lock
/// never stalls them; records that arrive after the in-transaction ingest are
/// the next pass's.
fn drain_project_locked(
    conn: &Connection,
    project: &str,
    spool: Option<&Path>,
    file_exists: &dyn Fn(&str) -> bool,
    out: &mut GraphDrain,
) -> Result<()> {
    let lock_started = Instant::now();
    let write = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let spooled = match spool {
        Some(dir) => graph_spool::ingest(&write, dir)?,
        None => Vec::new(),
    };
    let mut extraction = GraphDrain::default();
    extract_snapshots_with(&write, None, Some(project), &mut extraction, true)?;
    let files = codegraph::pending_resolve_files_for(&write, project)?;
    let files: Vec<&(String, String, i64)> = files.iter().collect();
    let (_stats, writes) = plan_resolve(&write, project, file_exists)?;
    let skipped = apply_planned(&write, &writes)?;
    let changed = changed_rank_rows(&write, rank_rows(&write, project)?)?;
    write_rank_rows(&write, &changed)?;
    let settled = settle(&write, &files, skipped)?;
    if skipped == 0 {
        let covered = codegraph::project_gen_parts(&write, project)?;
        codegraph::mark_project_settled(&write, project, covered)?;
    }
    write.commit()?;
    graph_spool::remove(&spooled);
    out.extracted += extraction.extracted;
    out.files += settled;
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
    /// `Ok(None)` when another process holds the lock. Anything else that goes
    /// wrong (the file cannot be created or opened, the filesystem has no
    /// locks) is an `Err`: it will not clear by itself.
    fn try_acquire(data_dir: &Path) -> Result<Option<Self>> {
        let path = data_dir.join(LOCK_FILE);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| {
                anyhow::anyhow!("graph drain lock {} cannot be opened: {e}", path.display())
            })?;
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => Ok(None),
            Err(e) => Err(anyhow::anyhow!(
                "graph drain lock {} cannot be taken: {e}",
                path.display()
            )),
        }
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
        let held = DrainLock::try_acquire(dir.path()).unwrap().unwrap();
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
            claim: 1,
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

    #[test]
    fn a_project_that_fails_does_not_stop_the_projects_after_it() {
        let (_dir, db, storage) = extracted();
        // An alphabetically earlier project whose rank write fails.
        storage
            .with_connection(|c| {
                let other = |id: &str, file: &str, name: &str| NodeRow {
                    project: "aaa".into(),
                    ..def(id, file, name)
                };
                upsert_node(c, &other("bad_node", "x.rs", "bad"))?;
                codegraph::replace_file_edges(c, "aaa", "x.rs", &[])?;
                c.execute_batch(
                    "CREATE TRIGGER rank_fails BEFORE INSERT ON code_node_rank
                     WHEN NEW.node_id = 'bad_node'
                     BEGIN SELECT RAISE(ABORT, 'rank write failed'); END;",
                )?;
                Ok(())
            })
            .unwrap();
        storage
            .upsert_code_file_state_pending("aaa", "x.rs", "hash-x")
            .unwrap();

        let report = drain_if_pending(&storage, &db).unwrap().unwrap();
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].0, "aaa");
        assert!(report.errors[0].1.contains("rank write failed"));
        // The later project was drained and settled.
        assert_eq!(report.projects, 1);
        assert_eq!(edge_state(&storage), ("callee".into(), 1));
        let left = pending(&storage);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].0, "aaa", "the failed project stays pending");
    }

    #[test]
    fn a_heartbeat_counts_only_when_fresh_foreign_and_capable() {
        let now = 1_000_000;
        let live = |v: &str, own: &str| other_drainer_is_live(Some(v), own, now);
        assert!(live("999990 1 daemon-7", "mcp-3"));
        assert!(!live("999990 1 mcp-3", "mcp-3"), "own beat");
        assert!(!live("999900 1 daemon-7", "mcp-3"), "stale");
        assert!(!live("999990 0 daemon-7", "mcp-3"), "no capability");
        assert!(!live("garbage", "mcp-3"));
        assert!(!other_drainer_is_live(None, "mcp-3", now));
    }

    #[test]
    fn a_lock_that_cannot_be_opened_is_an_error_not_contention() {
        let (dir, db, storage) = extracted();
        // The lock path is a directory: opening it for write fails for good.
        std::fs::create_dir(dir.path().join(LOCK_FILE)).unwrap();
        let err = drain_if_pending(&storage, &db).unwrap_err();
        assert!(format!("{err:#}").contains("cannot be opened"), "{err:#}");
        // Recorded for `csr-engine status`.
        let stored = storage.get_meta(ERROR_KEY).unwrap().unwrap();
        assert!(stored.contains("cannot be opened"), "{stored}");
        assert_eq!(pending(&storage).len(), 1, "nothing drained");

        // Once the cause is gone the next pass drains and clears the error.
        std::fs::remove_dir(dir.path().join(LOCK_FILE)).unwrap();
        assert!(drain_if_pending(&storage, &db).unwrap().is_some());
        assert_eq!(storage.get_meta(ERROR_KEY).unwrap().unwrap(), "");
    }

    #[test]
    fn contention_is_not_an_error_and_leaves_the_recorded_error_alone() {
        let (dir, db, storage) = extracted();
        storage.set_meta(ERROR_KEY, "an earlier failure").unwrap();
        let held = DrainLock::try_acquire(dir.path()).unwrap().unwrap();
        assert!(matches!(
            drain_outcome(&storage, &db).unwrap(),
            Outcome::Contended
        ));
        assert_eq!(
            storage.get_meta(ERROR_KEY).unwrap().unwrap(),
            "an earlier failure"
        );
        drop(held);
    }

    #[test]
    fn a_failed_project_is_recorded_for_status() {
        let (_dir, db, storage) = extracted();
        storage
            .with_connection(|c| {
                c.execute_batch(
                    "CREATE TRIGGER rank_fails BEFORE INSERT ON code_node_rank
                     BEGIN SELECT RAISE(ABORT, 'rank write failed'); END;",
                )?;
                Ok(())
            })
            .unwrap();
        let report = drain_if_pending(&storage, &db).unwrap().unwrap();
        assert_eq!(report.errors.len(), 1);
        let stored = storage.get_meta(ERROR_KEY).unwrap().unwrap();
        assert!(
            stored.starts_with("proj: ") && stored.contains("rank write failed"),
            "{stored}"
        );
    }

    /// What a hook leaves for the drain when an edit lands: the file is
    /// rewritten and snapshotted. Nothing is extracted.
    fn edit_and_snapshot(conn: &Connection, path: &str, src: &str) -> i64 {
        std::fs::write(path, src).unwrap();
        codegraph::insert_snapshot(
            conn,
            &codegraph::Snapshot {
                seq: codegraph::now_ns(),
                project: "proj".into(),
                file: path.into(),
                read_path: path.into(),
                kind: codegraph::SNAPSHOT_EXTRACT.into(),
                content: Some(src.into()),
                conv_id: "c".into(),
                session_id: "s".into(),
                source_chunk_id: None,
                origin: None,
            },
        )
        .unwrap()
    }

    /// Snapshot `name` under `dir` and extract it, as a drain pass does.
    fn extract_file(conn: &Connection, dir: &Path, name: &str, src: &str) -> String {
        let path = dir.join(name).to_string_lossy().into_owned();
        edit_and_snapshot(conn, &path, src);
        extract_all_snapshots(conn).unwrap();
        path
    }

    fn dangling_resolved_edges(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT e.dst_id FROM code_edges e
                 WHERE e.resolved = 1 AND e.dst_id NOT LIKE 'name:%'
                   AND NOT EXISTS (SELECT 1 FROM code_nodes n WHERE n.id = e.dst_id)",
            )
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.collect::<std::result::Result<_, _>>().unwrap()
    }

    fn drain_until_idle(conn: &Connection) {
        for _ in 0..6 {
            let d = drain(conn, &|f: &str| Path::new(f).exists()).unwrap();
            assert!(d.errors.is_empty(), "{:?}", d.errors);
            if conn
                .query_row(
                    "SELECT NOT EXISTS(SELECT 1 FROM code_graph_file_state WHERE graph_gen > resolved_gen)",
                    [],
                    |r| r.get::<_, bool>(0),
                )
                .unwrap()
                && !codegraph::has_queued_snapshots(conn).unwrap()
            {
                return;
            }
        }
        panic!("the drain did not settle in six passes");
    }

    fn function_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM code_nodes WHERE kind = 'function' ORDER BY name")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get(0)).unwrap();
        rows.collect::<std::result::Result<_, _>>().unwrap()
    }

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, Storage) {
        let dir = tempfile::TempDir::new().unwrap();
        let dir_path = dir.path().canonicalize().unwrap();
        let storage = Storage::open(&dir_path.join("csr-engine.db")).unwrap();
        (dir, dir_path, storage)
    }

    /// P1 (re-review 2): a hook snapshots a callee edit after the pass read the
    /// queue and before it read the pending files. The resolve must not bind
    /// the caller to the callee version the queued edit is about to retire:
    /// resolved edges are never revisited, so a bind to a retired node is
    /// permanent.
    #[test]
    fn a_callee_edit_queued_after_the_queue_read_is_not_bound_to_the_old_callee() {
        let (_keep, dir, storage) = fixture();
        storage
            .with_connection(|conn| {
                let callee = extract_file(conn, &dir, "a.rs", "fn callee() {}\n");
                extract_file(conn, &dir, "b.rs", "fn caller() { callee(); }\n");
                let fired = std::cell::Cell::new(false);
                let d = drain_with(conn, &|f: &str| Path::new(f).exists(), &|phase| {
                    if phase == Phase::QueueExtracted && !fired.replace(true) {
                        edit_and_snapshot(conn, &callee, "fn callee_renamed() {}\n");
                    }
                })?;
                assert!(fired.get());
                assert!(d.errors.is_empty(), "{:?}", d.errors);
                assert_eq!(d.replans, 1, "the resolve noticed the waiting edit");
                drain_until_idle(conn);
                assert_eq!(
                    dangling_resolved_edges(conn),
                    Vec::<String>::new(),
                    "a caller is still bound to a callee the edit retired"
                );
                assert_eq!(function_names(conn), ["callee_renamed", "caller"]);
                Ok(())
            })
            .unwrap();
    }

    /// The same edit arriving while the plan is made (not before it).
    #[test]
    fn a_callee_edit_queued_during_the_plan_is_not_bound_to_the_old_callee() {
        let (_keep, dir, storage) = fixture();
        storage
            .with_connection(|conn| {
                let callee = extract_file(conn, &dir, "a.rs", "fn callee() {}\n");
                extract_file(conn, &dir, "b.rs", "fn caller() { callee(); }\n");
                let fired = std::cell::Cell::new(false);
                drain_with(conn, &|f: &str| Path::new(f).exists(), &|phase| {
                    if matches!(phase, Phase::ResolvePlanned { attempt: 0 }) && !fired.replace(true)
                    {
                        edit_and_snapshot(conn, &callee, "fn callee_renamed() {}\n");
                    }
                })?;
                assert!(fired.get());
                drain_until_idle(conn);
                assert_eq!(dangling_resolved_edges(conn), Vec::<String>::new());
                Ok(())
            })
            .unwrap();
    }

    /// P1 (re-review 2): hook A read the old source, edit B snapshotted a newer
    /// version, and A's snapshot reaches the queue afterwards. The drain must not
    /// leave the old version as the last word.
    #[test]
    fn an_older_snapshot_that_arrives_late_never_replaces_a_newer_one() {
        let (_keep, dir, storage) = fixture();
        storage
            .with_connection(|conn| {
                let path = dir.join("a.rs").to_string_lossy().into_owned();
                let older = codegraph::now_ns();
                // B captures and is drained first...
                extract_file(conn, &dir, "a.rs", "fn new_name() {}\n");
                // ...then A's older capture is published.
                codegraph::insert_snapshot(
                    conn,
                    &codegraph::Snapshot {
                        seq: older,
                        project: "proj".into(),
                        file: path.clone(),
                        read_path: path,
                        kind: codegraph::SNAPSHOT_EXTRACT.into(),
                        content: Some("fn old_name() {}\n".into()),
                        conv_id: "c-old".into(),
                        session_id: "s".into(),
                        source_chunk_id: None,
                        origin: None,
                    },
                )?;
                drain_until_idle(conn);
                assert_eq!(function_names(conn), ["new_name"]);
                Ok(())
            })
            .unwrap();
    }

    /// Snapshots of one file are extracted in capture order, each with its own
    /// attribution, whatever order they were inserted in.
    #[test]
    fn snapshots_extract_in_capture_order_with_their_own_attribution() {
        let (_keep, dir, storage) = fixture();
        storage
            .with_connection(|conn| {
                let path = dir.join("a.rs").to_string_lossy().into_owned();
                let snap = |seq: i64, src: &str, conv: &str| codegraph::Snapshot {
                    seq,
                    project: "proj".into(),
                    file: path.clone(),
                    read_path: path.clone(),
                    kind: codegraph::SNAPSHOT_EXTRACT.into(),
                    content: Some(src.into()),
                    conv_id: conv.into(),
                    session_id: "s".into(),
                    source_chunk_id: None,
                    origin: None,
                };
                let t = codegraph::now_ns();
                // Inserted newest first.
                codegraph::insert_snapshot(conn, &snap(t + 2, "fn one() {}\nfn two() {}\n", "c2"))?;
                codegraph::insert_snapshot(conn, &snap(t + 1, "fn one() {}\n", "c1"))?;
                let out = extract_all_snapshots(conn)?;
                assert_eq!(out.extracted, 2);
                let credit = |name: &str| -> (String, String) {
                    conn.query_row(
                        "SELECT first_conv_id, last_conv_id FROM code_nodes WHERE name = ?1",
                        [name],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap()
                };
                assert_eq!(credit("one"), ("c1".to_string(), "c2".to_string()));
                assert_eq!(credit("two"), ("c2".to_string(), "c2".to_string()));
                Ok(())
            })
            .unwrap();
    }

    fn snapshot_of(path: &str, seq: i64, src: &str, origin: Option<&str>) -> codegraph::Snapshot {
        codegraph::Snapshot {
            seq,
            project: "proj".into(),
            file: path.into(),
            read_path: path.into(),
            kind: codegraph::SNAPSHOT_EXTRACT.into(),
            content: Some(src.into()),
            conv_id: "c".into(),
            session_id: "s".into(),
            source_chunk_id: None,
            origin: origin.map(str::to_string),
        }
    }

    /// A spool record ingested and extracted, then replayed because the drain
    /// died before deleting the file, changes nothing.
    #[test]
    fn a_spool_record_replayed_after_a_crash_is_a_no_op() {
        let (_keep, dir, storage) = fixture();
        let spool = dir.join("keep/graph-spool");
        let path = dir.join("a.rs").to_string_lossy().into_owned();
        let t = codegraph::now_ns();
        graph_spool::write(
            &spool,
            &snapshot_of(&path, t, "fn first_version() {}\n", None),
        )
        .unwrap();
        storage
            .with_connection(|conn| {
                let out = drain(conn, &|f: &str| Path::new(f).exists())?;
                assert_eq!(out.extracted, 1);
                assert_eq!(function_names(conn), ["first_version"]);
                assert!(
                    !graph_spool::any_pending(&spool),
                    "deleted after the commit"
                );
                // The next edit lands, then the old record reappears (the
                // unlink never happened).
                extract_file(conn, &dir, "a.rs", "fn second_version() {}\n");
                graph_spool::write(
                    &spool,
                    &snapshot_of(&path, t, "fn first_version() {}\n", None),
                )
                .unwrap();
                drain_until_idle(conn);
                assert_eq!(function_names(conn), ["second_version"]);
                Ok(())
            })
            .unwrap();
    }

    /// Spooled and queued edits of one file are extracted on one clock, whichever
    /// path carried them.
    #[test]
    fn spooled_and_queued_snapshots_extract_in_capture_order() {
        let (_keep, dir, storage) = fixture();
        let spool = dir.join("keep/graph-spool");
        let path = dir.join("a.rs").to_string_lossy().into_owned();
        let t = codegraph::now_ns();
        // The older edit went to the spool (the lock was held), the newer to the
        // table; the table row is in the database before the drain ingests.
        graph_spool::write(&spool, &snapshot_of(&path, t, "fn from_spool() {}\n", None)).unwrap();
        storage
            .with_connection(|conn| {
                codegraph::insert_snapshot(
                    conn,
                    &snapshot_of(&path, t + 10, "fn from_table() {}\n", None),
                )?;
                drain_until_idle(conn);
                assert_eq!(function_names(conn), ["from_table"]);
                Ok(())
            })
            .unwrap();
    }

    /// An edit of the project waiting in the spool keeps a resolve from running,
    /// and the locked pass takes it in the same transaction.
    #[test]
    fn a_hammered_project_falls_back_to_the_locked_pass_and_settles() {
        let (_keep, dir, storage) = fixture();
        let spool = dir.join("keep/graph-spool");
        storage
            .with_connection(|conn| {
                let callee = extract_file(conn, &dir, "a.rs", "fn callee() {}\n");
                extract_file(conn, &dir, "b.rs", "fn caller() { callee(); }\n");
                // An edit arrives at every plan, so no optimistic attempt applies.
                let n = std::cell::Cell::new(0u64);
                let d = drain_with(conn, &|f: &str| Path::new(f).exists(), &|phase| {
                    if matches!(phase, Phase::ResolvePlanned { .. }) {
                        n.set(n.get() + 1);
                        let src = format!("fn callee_v{}() {{}}\n", n.get());
                        std::fs::write(&callee, &src).unwrap();
                        graph_spool::write(
                            &spool,
                            &snapshot_of(&callee, codegraph::now_ns(), &src, None),
                        )
                        .unwrap();
                    }
                })?;
                assert!(d.errors.is_empty(), "{:?}", d.errors);
                assert_eq!(d.locked_passes, 1);
                assert_eq!(d.replans, MAX_OPTIMISTIC_ATTEMPTS);
                assert_eq!(dangling_resolved_edges(conn), Vec::<String>::new());
                // The locked pass took the records that were there when it ran.
                assert_eq!(function_names(conn), ["callee_v3", "caller"]);
                Ok(())
            })
            .unwrap();
    }

    /// A file the extractor cannot parse leaves a file-level row, written by the
    /// drain; the hook wrote nothing.
    #[test]
    fn an_unsupported_snapshot_records_the_file_level_row() {
        let (_keep, dir, storage) = fixture();
        storage
            .with_connection(|conn| {
                let path = dir.join("notes.md").to_string_lossy().into_owned();
                let mut s = snapshot_of(&path, codegraph::now_ns(), "", None);
                s.kind = codegraph::SNAPSHOT_UNSUPPORTED.into();
                s.content = None;
                codegraph::insert_snapshot(conn, &s)?;
                assert_eq!(
                    graph_rows_total(conn, "code_graph_file_state"),
                    0,
                    "nothing before the drain"
                );
                extract_all_snapshots(conn)?;
                let status: String = conn.query_row(
                    "SELECT ast_status FROM code_graph_file_state WHERE file = ?1",
                    [&path],
                    |r| r.get(0),
                )?;
                assert_eq!(status, "unsupported");
                Ok(())
            })
            .unwrap();
    }

    fn graph_rows_total(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    /// A file with 64 snapshots waiting keeps at most 64: the newest is replaced.
    #[test]
    fn a_file_cannot_pile_up_snapshots_without_bound() {
        let (_keep, dir, storage) = fixture();
        storage
            .with_connection(|conn| {
                let path = dir.join("a.rs").to_string_lossy().into_owned();
                let t = codegraph::now_ns();
                for i in 0..(codegraph::MAX_SNAPSHOTS_PER_FILE + 10) {
                    codegraph::insert_snapshot(
                        conn,
                        &snapshot_of(&path, t + i, &format!("fn v{i}() {{}}\n"), None),
                    )?;
                }
                assert_eq!(
                    graph_rows_total(conn, "code_graph_snapshots"),
                    codegraph::MAX_SNAPSHOTS_PER_FILE
                );
                extract_all_snapshots(conn)?;
                // The last write wins: the final version is the one stored.
                assert_eq!(
                    function_names(conn),
                    [format!("v{}", codegraph::MAX_SNAPSHOTS_PER_FILE + 9)]
                );
                Ok(())
            })
            .unwrap();
    }

    /// A late arrival (a delayed hook, a spooled record) must not evict the
    /// newest version of a file: with versions 2..=65 waiting, version 1 is
    /// dropped and the drain still ends on version 65.
    #[test]
    fn a_late_snapshot_never_evicts_the_newest_version() {
        let (_keep, dir, storage) = fixture();
        storage
            .with_connection(|conn| {
                let path = dir.join("a.rs").to_string_lossy().into_owned();
                let t = codegraph::now_ns();
                let last = codegraph::MAX_SNAPSHOTS_PER_FILE + 1;
                for i in 2..=last {
                    codegraph::insert_snapshot(
                        conn,
                        &snapshot_of(&path, t + i, &format!("fn v{i}() {{}}\n"), None),
                    )?;
                }
                codegraph::insert_snapshot(conn, &snapshot_of(&path, t + 1, "fn v1() {}\n", None))?;
                assert_eq!(
                    graph_rows_total(conn, "code_graph_snapshots"),
                    codegraph::MAX_SNAPSHOTS_PER_FILE
                );
                extract_all_snapshots(conn)?;
                assert_eq!(function_names(conn), [format!("v{last}")]);
                Ok(())
            })
            .unwrap();
    }

    /// An arrival at or below the file's high-water mark was already superseded:
    /// it is dropped on arrival, not queued, and evicts nothing.
    #[test]
    fn a_snapshot_below_the_high_water_mark_is_not_queued() {
        let (_keep, dir, storage) = fixture();
        storage
            .with_connection(|conn| {
                let path = dir.join("a.rs").to_string_lossy().into_owned();
                let t = codegraph::now_ns();
                codegraph::insert_snapshot(
                    conn,
                    &snapshot_of(&path, t + 5, "fn new() {}\n", None),
                )?;
                extract_all_snapshots(conn)?;
                codegraph::insert_snapshot(
                    conn,
                    &snapshot_of(&path, t + 2, "fn old() {}\n", None),
                )?;
                assert_eq!(graph_rows_total(conn, "code_graph_snapshots"), 0);
                extract_all_snapshots(conn)?;
                assert_eq!(function_names(conn), ["new"]);
                Ok(())
            })
            .unwrap();
    }

    fn mystery_edge(storage: &Storage) -> (String, String, String, i64) {
        storage
            .with_connection(|c| {
                Ok(c.query_row(
                    "SELECT dst_id, COALESCE(callee_kind,''), COALESCE(evidence,''), resolved
                     FROM code_edges WHERE src_id = 'caller' AND dst_id LIKE '%mystery'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?)
            })
            .unwrap()
    }

    /// A write the resolve reads but that queues no file (a repo scan, a co-edit
    /// replay) is work: one drain tick after everything has settled re-resolves
    /// the project, and then the project is idle again.
    #[test]
    fn a_repo_scan_after_everything_settled_gets_one_resolve_pass() {
        let (_dir, db, storage) = extracted();
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
        storage
            .upsert_code_file_state_pending("proj", "b.rs", "hash-2")
            .unwrap();
        assert!(drain_if_pending(&storage, &db).unwrap().is_some());
        assert!(pending(&storage).is_empty());
        // Settled: nothing to do.
        assert!(drain_if_pending(&storage, &db).unwrap().is_none());
        let before = mystery_edge(&storage);

        // The repo scan finds `mystery` defined in a file the graph has no nodes for.
        storage
            .upsert_repo_defs(
                "proj",
                "other.rs",
                &[("mystery".into(), "function".into(), "rust".into())],
            )
            .unwrap();
        assert!(pending(&storage).is_empty(), "no file is queued by a scan");

        let report = drain_if_pending(&storage, &db).unwrap();
        assert!(report.is_some(), "the scan left the drain idle");
        assert_ne!(mystery_edge(&storage), before, "the resolve did not rerun");
        // One pass settles it.
        assert!(drain_if_pending(&storage, &db).unwrap().is_none());
    }

    /// A co-edit row for a file of the project asks for a pass only once the
    /// project has been settled for `STAR_ONLY_COOLDOWN_SECS`: a row per edit
    /// must not re-resolve the project per edit.
    #[test]
    fn an_evolution_row_asks_for_a_pass_only_after_the_cooldown() {
        let (_dir, db, storage) = extracted();
        assert!(drain_if_pending(&storage, &db).unwrap().is_some());
        assert!(drain_if_pending(&storage, &db).unwrap().is_none());

        storage
            .with_connection(|c| {
                c.execute(
                    "INSERT INTO code_evolution (id, session_id, project_name, file_path)
                     VALUES ('e1', 's', 'elsewhere', 'b.rs')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(
            drain_if_pending(&storage, &db).unwrap().is_none(),
            "inside the cooldown"
        );

        storage
            .with_connection(|c| {
                c.execute(
                    "UPDATE code_graph_project_settled SET settled_at = settled_at - ?1",
                    [codegraph::STAR_ONLY_COOLDOWN_SECS + 1],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(drain_if_pending(&storage, &db).unwrap().is_some());
        assert!(drain_if_pending(&storage, &db).unwrap().is_none());
    }

    fn every_project_counter(storage: &Storage) -> i64 {
        storage
            .with_connection(|c| {
                Ok(c.query_row(
                    "SELECT COALESCE(SUM(gen), 0) FROM code_graph_project_gen WHERE project = '*'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap()
    }

    fn age_settled(storage: &Storage) {
        storage
            .with_connection(|c| {
                c.execute(
                    "UPDATE code_graph_project_settled SET settled_at = settled_at - ?1",
                    [codegraph::STAR_ONLY_COOLDOWN_SECS + 1],
                )?;
                Ok(())
            })
            .unwrap();
    }

    /// Co-edit rows reach only the projects that own the row's file, however old
    /// the settled mark is: a row for a file nobody has nodes in, however many,
    /// never asks for a pass; a row for another project's file asks for that
    /// project's pass and no one else's.
    #[test]
    fn an_evolution_row_dirties_only_the_project_that_owns_its_file() {
        let (_dir, db, storage) = extracted();
        storage
            .with_connection(|c| {
                let mut other = def("other-fn", "z.rs", "other_fn");
                other.project = "other".into();
                upsert_node(c, &other)?;
                Ok(())
            })
            .unwrap();
        while drain_if_pending(&storage, &db).unwrap().is_some() {}
        for (i, file) in ["nowhere.rs", "also-nowhere.rs"].iter().enumerate() {
            storage
                .with_connection(|c| {
                    c.execute(
                        "INSERT INTO code_evolution (id, session_id, project_name, file_path)
                         VALUES (?1, 's', 'proj', ?2)",
                        rusqlite::params![format!("e{i}"), file],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        age_settled(&storage);
        assert!(storage.dirty_graph_projects().unwrap().is_empty());
        assert!(drain_if_pending(&storage, &db).unwrap().is_none());

        storage
            .with_connection(|c| {
                c.execute(
                    "UPDATE code_graph_project_settled
                     SET settled_at = CAST(strftime('%s', 'now') AS INTEGER)",
                    [],
                )?;
                c.execute(
                    "INSERT INTO code_evolution (id, session_id, project_name, file_path)
                     VALUES ('e9', 's', 'proj', 'z.rs')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert!(
            storage.dirty_graph_projects().unwrap().is_empty(),
            "inside the cooldown"
        );
        age_settled(&storage);
        assert_eq!(
            storage.dirty_graph_projects().unwrap(),
            ["other".to_string()]
        );
        let report = drain_if_pending(&storage, &db).unwrap().unwrap();
        assert_eq!(report.projects, 1);
        assert!(drain_if_pending(&storage, &db).unwrap().is_none());
    }

    /// The drain's own work never makes a project it settled dirty again, and
    /// dangling edges (which nothing resolves) neither cost a pass nor move the
    /// every-project counter.
    #[test]
    fn a_drain_over_dangling_edges_settles_and_leaves_the_every_project_counter() {
        let (_dir, db, storage) = extracted();
        storage
            .with_connection(|c| {
                // Dangling: unresolved names, as a retired node leaves them.
                for (i, name) in ["a", "b", "c"].iter().enumerate() {
                    c.execute(
                        "INSERT INTO code_edges (src_id, dst_id, kind, src_file)
                         VALUES (?1, ?2, 'calls', 'ghost.rs')",
                        rusqlite::params![format!("ghost{i}"), format!("name:{name}")],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let star = every_project_counter(&storage);
        let report = drain_if_pending(&storage, &db).unwrap().unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(every_project_counter(&storage), star, "the pass bumped it");
        assert!(storage.dirty_graph_projects().unwrap().is_empty());
        assert!(drain_if_pending(&storage, &db).unwrap().is_none());

        // More dangling edges, written and swept by a re-extraction: still idle.
        storage
            .with_connection(|c| {
                c.execute(
                    "INSERT INTO code_edges (src_id, dst_id, kind, src_file)
                     VALUES ('ghost9', 'name:z', 'calls', 'ghost.rs')",
                    [],
                )?;
                codegraph::replace_file_edges(c, "proj", "ghost.rs", &[])?;
                Ok(())
            })
            .unwrap();
        assert_eq!(every_project_counter(&storage), star);
        assert!(drain_if_pending(&storage, &db).unwrap().is_none());
        age_settled(&storage);
        assert!(drain_if_pending(&storage, &db).unwrap().is_none());
    }

    /// Upgrading from a build with no settled table: the old drain settled every
    /// file, and a repo scan bumped the generation after that without queueing a
    /// file. Nothing may be declared covered: each existing project gets one
    /// resolve pass, after which it is idle.
    #[test]
    fn an_upgrade_gives_every_existing_project_exactly_one_pass() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("csr-engine.db");
        {
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
                    // The old drain resolved and settled it ...
                    c.execute("UPDATE code_edges SET dst_id = 'callee', resolved = 1", [])?;
                    // ... and a repo scan afterwards queued no file.
                    codegraph::upsert_repo_defs(
                        c,
                        "proj",
                        "other.rs",
                        &[("later".into(), "function".into(), "rust".into())],
                    )?;
                    // An older build: no settled table, and no row for a project
                    // that only has repo_defs.
                    c.execute_batch(
                        "DROP TABLE code_graph_project_settled;
                         INSERT INTO repo_defs (project, file, name, kind)
                             VALUES ('scan-only', 'x.rs', 'f', 'function');
                         DELETE FROM code_graph_project_gen WHERE project = 'scan-only';",
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        let storage = Storage::open(&db).unwrap();
        assert_eq!(
            storage.dirty_graph_projects().unwrap(),
            ["proj".to_string(), "scan-only".to_string()]
        );
        let report = drain_if_pending(&storage, &db).unwrap().unwrap();
        assert_eq!(report.projects, 2);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(storage.dirty_graph_projects().unwrap().is_empty());
        assert!(drain_if_pending(&storage, &db).unwrap().is_none());
        // Opening it again does not hand out a second pass.
        drop(storage);
        let storage = Storage::open(&db).unwrap();
        assert!(storage.dirty_graph_projects().unwrap().is_empty());
    }

    fn chunk_of(conn: &Connection, name: &str) -> Option<String> {
        conn.query_row(
            "SELECT last_chunk_id FROM code_nodes WHERE name = ?1",
            [name],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// An edit whose chunk is unknown (spooled without a database) that changes
    /// a node's body clears the credit an earlier edit left; one that leaves the
    /// body alone keeps it.
    #[test]
    fn a_chunkless_edit_that_changes_the_body_clears_the_stale_credit() {
        let (_keep, dir, storage) = fixture();
        storage
            .with_connection(|conn| {
                let path = dir.join("a.rs").to_string_lossy().into_owned();
                let t = codegraph::now_ns();
                let mut a = snapshot_of(&path, t + 1, "fn target() { one(); }\n", None);
                a.source_chunk_id = Some("chunk-A".into());
                codegraph::insert_snapshot(conn, &a)?;
                extract_all_snapshots(conn)?;
                assert_eq!(chunk_of(conn, "target").as_deref(), Some("chunk-A"));

                // Same body, no chunk: the credit stands.
                codegraph::insert_snapshot(
                    conn,
                    &snapshot_of(&path, t + 2, "fn target() { one(); }\n", None),
                )?;
                extract_all_snapshots(conn)?;
                assert_eq!(chunk_of(conn, "target").as_deref(), Some("chunk-A"));

                // Changed body, no chunk: A no longer describes it.
                codegraph::insert_snapshot(
                    conn,
                    &snapshot_of(&path, t + 3, "fn target() { two(); }\n", None),
                )?;
                extract_all_snapshots(conn)?;
                assert_eq!(chunk_of(conn, "target"), None);

                // Changed again with a chunk: it is credited.
                let mut c = snapshot_of(&path, t + 4, "fn target() { three(); }\n", None);
                c.source_chunk_id = Some("chunk-C".into());
                codegraph::insert_snapshot(conn, &c)?;
                extract_all_snapshots(conn)?;
                assert_eq!(chunk_of(conn, "target").as_deref(), Some("chunk-C"));
                Ok(())
            })
            .unwrap();
    }
}
