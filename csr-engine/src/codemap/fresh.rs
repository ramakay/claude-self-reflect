//! Keeps the code map fresh. One loop, two callers: the daemon (inside
//! `spawn_blocking`) and `csr-engine codemap watch` (foreground).
//!
//! Three signals feed it:
//!
//! 1. **Poll** (~60 s). For every enabled repo (capped at
//!    `CSR_CODEMAP_MAX_REPOS`), compare the live worktree listing with the stored
//!    views and ask `view_changed` per view. A changed, added or removed view
//!    runs `scan_repo` (which re-parses only new blobs). Discovery (which reads
//!    `history.jsonl`) runs on its own 10-minute cadence, first at start-up.
//! 2. **Watcher.** One `notify` watcher over the roots of the stored views. A
//!    cheap lexical filter runs in the callback; survivors are debounced (5 s of
//!    quiet, never held longer than 30 s) and handed to `rescan_paths`, which
//!    applies `.gitignore` and routes each path to its longest-prefix view.
//! 3. **Queue.** PostToolUse hooks only insert into `codemap_queue`; this loop
//!    drains it on a 5 s tick.
//!
//! Two threads, one connection each, neither on the daemon's shared storage
//! mutex: the *poller* (discovery, poll, full scans, which wait for the daemon's
//! heavy-work permit) and the *pump* (watcher batches and the queue, which never
//! wait for it unless a batch is larger than `SMALL_BATCH`). A path edit
//! therefore lands in seconds even while a dream holds the permit. The two
//! threads do share one scan lock (`Ctx::scan_lock`): a path batch waits for a
//! full scan in progress, so a scan that read an older blob can never commit it
//! over a newer partial rescan.
//!
//! Nothing here busy-loops: both threads wake once a second to compare clocks,
//! and an idle wake does no I/O except the pump's 5 s queue probe (one SELECT on
//! a table that is empty at rest).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, TryLockError};
use std::time::{Duration, Instant};

use anyhow::Result;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

use super::discover::{discover, max_repos_from_env, tier};
use super::identity::{self, is_tcc_path, ViewId};
use super::scan::{
    rel_passes_caps, rescan_paths, scan_repo, view_changed, PathScanReport, ScanOpts,
};
use super::store::{self, StoredRepo};
use crate::extraction::repo_scan::has_code_extension;
use crate::storage::Storage;

/// Path batches up to this size skip the heavy-work permit.
pub const SMALL_BATCH: usize = 64;

/// A queued row older than this is dropped when its rescan keeps failing.
const QUEUE_STALE_NANOS: i64 = 3_600 * 1_000_000_000;

#[derive(Debug, Clone)]
pub struct LoopOpts {
    /// How often every enabled repo is polled (`CSR_CODEMAP_POLL_SECS`).
    pub poll: Duration,
    /// Quiet time before a watcher batch is flushed.
    pub debounce: Duration,
    /// A batch is flushed after this long even under constant churn.
    pub max_wait: Duration,
    /// How often discovery re-reads `history.jsonl` and friends.
    pub discovery: Duration,
    /// How often the hook queue is drained.
    pub queue_tick: Duration,
    /// Wake granularity (shutdown latency, debounce resolution).
    pub tick: Duration,
    /// A repo whose scan failed is not retried for this long.
    pub error_backoff: Duration,
    /// Start the notify watcher (tests that only need the poller turn it off).
    pub watch: bool,
    /// Scan options; `home` is overwritten with the loop's home.
    pub scan: ScanOpts,
}

impl Default for LoopOpts {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(60),
            debounce: Duration::from_secs(5),
            max_wait: Duration::from_secs(30),
            discovery: Duration::from_secs(600),
            queue_tick: Duration::from_secs(5),
            tick: Duration::from_secs(1),
            error_backoff: Duration::from_secs(600),
            watch: true,
            scan: ScanOpts::default(),
        }
    }
}

impl LoopOpts {
    /// Defaults, with `CSR_CODEMAP_POLL_SECS` applied and `CSR_CODEMAP_NO_WATCH=1`
    /// (poll and hook queue only, no file watcher: for a host where `notify`
    /// misbehaves, and for measuring the poll path alone) honoured.
    pub fn from_env() -> Self {
        let mut o = Self {
            watch: !std::env::var("CSR_CODEMAP_NO_WATCH")
                .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            ..Self::default()
        };
        if let Some(n) = std::env::var("CSR_CODEMAP_POLL_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
        {
            o.poll = Duration::from_secs(n);
        }
        o
    }
}

/// Everything both threads share.
pub(crate) struct Ctx<'a> {
    db_path: &'a Path,
    home: &'a Path,
    shutdown: Arc<AtomicBool>,
    opts: LoopOpts,
    heavy: Option<Arc<Semaphore>>,
    log: &'a (dyn Fn(&str) + Sync),
    /// Bumped by the poller whenever the set of stored views may have changed;
    /// the pump re-syncs its watched roots when it sees a new value.
    roots_version: AtomicU64,
    /// Serialises the poller's full `scan_repo` and the pump's `rescan_paths`.
    /// Without it a full scan can read a file's blob A, a dirty edit then be
    /// committed as blob B by the pump, and the scan commit A over it: a dirty
    /// edit moves neither HEAD nor the index, so `view_changed` never notices
    /// and A stays until the next edit. Lock order is heavy permit first, then
    /// this lock; the heavy permit is never taken while this lock is held.
    ///
    /// Covers the two threads of this loop only. A separate `csr-engine codemap
    /// scan` process (or a second daemon) has its own lock and is not covered.
    scan_lock: Mutex<()>,
}

impl<'a> Ctx<'a> {
    fn new(
        db_path: &'a Path,
        home: &'a Path,
        shutdown: Arc<AtomicBool>,
        mut opts: LoopOpts,
        heavy: Option<Arc<Semaphore>>,
        log: &'a (dyn Fn(&str) + Sync),
    ) -> Self {
        opts.scan.home = Some(home.to_path_buf());
        Self {
            db_path,
            home,
            shutdown,
            opts,
            heavy,
            log,
            roots_version: AtomicU64::new(0),
            scan_lock: Mutex::new(()),
        }
    }

    fn stop(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// Take `scan_lock`, polling so a shutdown is noticed while a long full scan
    /// holds it. `None` means shutdown was requested first. A poisoned lock (a
    /// scan thread panicked) is recovered, never a panic: it guards no data.
    fn lock_scan(&self) -> Option<MutexGuard<'_, ()>> {
        loop {
            match self.scan_lock.try_lock() {
                Ok(guard) => return Some(guard),
                Err(TryLockError::Poisoned(p)) => return Some(p.into_inner()),
                Err(TryLockError::WouldBlock) => {
                    if self.stop() {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }

    fn log(&self, msg: &str) {
        (self.log)(msg)
    }
}

enum Gate {
    Free,
    Held(#[allow(dead_code)] OwnedSemaphorePermit),
    Shutdown,
}

/// Take the daemon's heavy-work permit from a blocking thread. Polls rather than
/// parking so a shutdown is noticed while a dream holds the permit.
fn acquire_heavy(sem: Option<&Arc<Semaphore>>, shutdown: &AtomicBool) -> Gate {
    let Some(sem) = sem else {
        return Gate::Free;
    };
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return Gate::Shutdown;
        }
        match sem.clone().try_acquire_owned() {
            Ok(p) => return Gate::Held(p),
            Err(TryAcquireError::Closed) => return Gate::Shutdown,
            Err(TryAcquireError::NoPermits) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// The repos the loop works on: enabled rows, cli first, then env, code_nodes
/// and history (newest registration first), at most `CSR_CODEMAP_MAX_REPOS`.
fn active_repos(conn: &Connection) -> Result<Vec<StoredRepo>> {
    let mut rows: Vec<StoredRepo> = store::list_repos(conn)?
        .into_iter()
        .filter(|r| r.enabled)
        .collect();
    rows.sort_by(|a, b| {
        tier(&a.source)
            .cmp(&tier(&b.source))
            .then(b.added_at.cmp(&a.added_at))
            .then(a.common_dir.cmp(&b.common_dir))
    });
    rows.truncate(max_repos_from_env());
    Ok(rows)
}

fn short_err(e: &anyhow::Error) -> String {
    format!("error: {e:#}").chars().take(160).collect()
}

// ---- poller -------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub(crate) struct PollOutcome {
    pub repos: usize,
    /// Common dirs `scan_repo` ran for.
    pub scanned: Vec<String>,
    pub views_added: Vec<String>,
    pub views_removed: Vec<String>,
    pub backed_off: usize,
    pub errors: usize,
}

pub(crate) struct Poller<'a> {
    storage: Storage,
    ctx: &'a Ctx<'a>,
    retry_after: HashMap<String, Instant>,
    /// Repos this process has already checked once. The first poll of each runs
    /// a full scan whatever the probe says: the probe watches HEAD and the
    /// index, so a worktree edit made while nothing was watching (daemon down,
    /// view not yet watched) is invisible to it.
    checked: HashSet<String>,
    last_discovery: Option<Instant>,
    last_poll: Option<Instant>,
    pub discoveries: usize,
    pub polls: usize,
}

impl<'a> Poller<'a> {
    pub(crate) fn new(ctx: &'a Ctx<'a>) -> Result<Self> {
        Ok(Self {
            storage: Storage::open(ctx.db_path)?,
            ctx,
            retry_after: HashMap::new(),
            checked: HashSet::new(),
            last_discovery: None,
            last_poll: None,
            discoveries: 0,
            polls: 0,
        })
    }

    /// One scheduler step at time `now`: discovery if its interval has passed,
    /// then a poll if its interval has passed. Does nothing otherwise.
    pub(crate) fn step(&mut self, now: Instant) -> Option<PollOutcome> {
        if self.ctx.stop() {
            return None;
        }
        if self
            .last_discovery
            .is_none_or(|t| now.duration_since(t) >= self.ctx.opts.discovery)
        {
            self.last_discovery = Some(now);
            self.discover_now();
        }
        if self
            .last_poll
            .is_none_or(|t| now.duration_since(t) >= self.ctx.opts.poll)
        {
            self.last_poll = Some(now);
            let outcome = self.poll_once(now);
            self.ctx.log(&format!(
                "poll repos={} scanned={} views_added={} views_removed={} backed_off={} errors={}",
                outcome.repos,
                outcome.scanned.len(),
                outcome.views_added.len(),
                outcome.views_removed.len(),
                outcome.backed_off,
                outcome.errors
            ));
            return Some(outcome);
        }
        None
    }

    /// Re-run discovery and register what it finds.
    pub(crate) fn discover_now(&mut self) {
        self.discoveries += 1;
        let home = self.ctx.home;
        let res = self.storage.with_connection(|conn| {
            if !store::tables_ready(conn) {
                return Ok((0, 0));
            }
            let before = store::list_repos(conn)?.len();
            let candidates = discover(conn, home);
            for c in &candidates {
                store::upsert_repo(
                    conn,
                    &c.common_dir.to_string_lossy(),
                    c.main_path
                        .as_deref()
                        .map(|m| m.to_string_lossy())
                        .as_deref(),
                    c.source,
                    false,
                )?;
            }
            let after = store::list_repos(conn)?.len();
            Ok((candidates.len(), after.saturating_sub(before)))
        });
        match res {
            Ok((candidates, new)) => {
                self.ctx
                    .log(&format!("discovery candidates={candidates} new={new}"));
                if new > 0 {
                    self.ctx.roots_version.fetch_add(1, Ordering::SeqCst);
                }
            }
            Err(e) => self
                .ctx
                .log(&format!("discovery failed: {}", short_err(&e))),
        }
    }

    /// Poll every active repo once.
    pub(crate) fn poll_once(&mut self, now: Instant) -> PollOutcome {
        self.polls += 1;
        let mut out = PollOutcome::default();
        let repos = match self.storage.with_connection(|conn| {
            if !store::tables_ready(conn) {
                return Ok(Vec::new());
            }
            active_repos(conn)
        }) {
            Ok(r) => r,
            Err(e) => {
                self.ctx.log(&format!("poll failed: {}", short_err(&e)));
                out.errors += 1;
                return out;
            }
        };
        out.repos = repos.len();
        for repo in repos {
            if self.ctx.stop() {
                break;
            }
            let common = PathBuf::from(&repo.common_dir);
            if is_tcc_path(&common, Some(self.ctx.home)) {
                continue;
            }
            if self
                .retry_after
                .get(&repo.common_dir)
                .is_some_and(|t| *t > now)
            {
                out.backed_off += 1;
                continue;
            }
            if let Err(e) = self.poll_repo(&repo.common_dir, &common, now, &mut out) {
                out.errors += 1;
                let msg = short_err(&e);
                self.ctx
                    .log(&format!("repo {} failed: {msg}", repo.common_dir));
                let _ = self
                    .storage
                    .with_connection(|c| store::record_poll(c, &repo.common_dir, &msg));
                self.retry_after
                    .insert(repo.common_dir.clone(), now + self.ctx.opts.error_backoff);
            }
        }
        out
    }

    fn poll_repo(
        &mut self,
        common_str: &str,
        common: &Path,
        now: Instant,
        out: &mut PollOutcome,
    ) -> Result<()> {
        let common_ok = identity::is_dir_safe(common, Some(self.ctx.home));
        let listing = if common_ok {
            identity::list_views_with(common, Some(self.ctx.home))?
        } else {
            identity::ViewListing::default()
        };
        let live: BTreeMap<String, &ViewId> = listing
            .views
            .iter()
            .map(|v| (v.worktree_path.to_string_lossy().to_string(), v))
            .collect();
        let first_pass = !self.checked.contains(common_str);
        let (added, removed, changed) = self.storage.with_connection(|conn| {
            let stored: BTreeSet<String> = store::stored_views(conn, Some(common_str))?
                .into_iter()
                .map(|v| v.worktree_path)
                .collect();
            let added: Vec<String> = live
                .keys()
                .filter(|p| !stored.contains(*p))
                .cloned()
                .collect();
            let removed: Vec<String> = stored
                .iter()
                .filter(|p| !live.contains_key(*p))
                .cloned()
                .collect();
            let mut changed = first_pass || !added.is_empty() || !removed.is_empty();
            if !changed {
                for v in live.values() {
                    if view_changed(conn, v).unwrap_or(true) {
                        changed = true;
                        break;
                    }
                }
            }
            Ok((added, removed, changed))
        })?;
        if !changed {
            let status = if common_ok { "ok" } else { "missing" };
            self.storage
                .with_connection(|c| store::record_poll(c, common_str, status))?;
            return Ok(());
        }

        let gate = acquire_heavy(self.ctx.heavy.as_ref(), &self.ctx.shutdown);
        if matches!(gate, Gate::Shutdown) {
            return Ok(());
        }
        // Heavy permit first (taken above), then the scan lock.
        let Some(scan_guard) = self.ctx.lock_scan() else {
            return Ok(());
        };
        let opts = self.ctx.opts.scan.clone();
        let report = self
            .storage
            .with_connection(|conn| scan_repo(conn, common, &opts))?;
        drop(scan_guard);
        drop(gate);

        self.checked.insert(common_str.to_string());
        out.scanned.push(common_str.to_string());
        for p in &added {
            self.ctx.log(&format!("view added {p}"));
        }
        for p in &removed {
            self.ctx.log(&format!("view removed {p}"));
        }
        out.views_added.extend(added);
        out.views_removed.extend(removed);
        let t = &report.totals;
        self.ctx.log(&format!(
            "scan {common_str} views={} parsed={} reused={} defs={} removed_views={} {}ms",
            report.views.len(),
            t.blobs_parsed,
            t.blobs_reused,
            t.defs_written,
            report.removed_views,
            t.ms
        ));
        if let Some(bad) = report.views.iter().find(|v| v.status != "ok") {
            let msg = format!("{}: {}", bad.path, bad.status);
            let msg: String = msg.chars().take(160).collect();
            self.storage
                .with_connection(|c| store::record_poll(c, common_str, &msg))?;
            self.retry_after
                .insert(common_str.to_string(), now + self.ctx.opts.error_backoff);
        }
        self.ctx.roots_version.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// ---- watcher filter ---------------------------------------------------------------

/// The cheap lexical filter the watcher callback applies before anything is
/// queued: a code extension, a stored view root above the path, no hard-skip
/// directory (`target/`, `node_modules/`, `.git/` ...) below that root, and not
/// in a protected folder. No filesystem call.
pub(crate) fn event_path_relevant(path: &Path, roots: &[PathBuf], home: Option<&Path>) -> bool {
    if !has_code_extension(path) {
        return false;
    }
    let Some(root) = roots
        .iter()
        .filter(|r| path.starts_with(r))
        .max_by_key(|r| r.components().count())
    else {
        return false;
    };
    let Some(rel) = path.strip_prefix(root).ok().and_then(|r| r.to_str()) else {
        return false;
    };
    rel_passes_caps(rel) && !is_tcc_path(path, home)
}

/// The roots that cover all of `roots`: those not inside another root.
fn cover(roots: &[PathBuf]) -> BTreeSet<PathBuf> {
    roots
        .iter()
        .filter(|r| !roots.iter().any(|o| o != *r && r.starts_with(o)))
        .cloned()
        .collect()
}

// ---- pump -------------------------------------------------------------------------

/// Drain `codemap_queue`: read the rows, call `rescan` with their paths, then
/// delete only rows not re-enqueued meanwhile. A failed rescan keeps the rows
/// (and drops the ones older than an hour). Returns (rows read, report).
pub(crate) fn drain_queue<F>(
    conn: &Connection,
    rescan: F,
) -> Result<Option<(usize, PathScanReport)>>
where
    F: FnOnce(&[PathBuf]) -> Result<PathScanReport>,
{
    let rows = store::queue_read(conn)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let paths: Vec<PathBuf> = rows.iter().map(|(p, _)| PathBuf::from(p)).collect();
    match rescan(&paths) {
        Ok(report) => {
            for (p, at) in &rows {
                store::queue_delete_upto(conn, p, *at)?;
            }
            Ok(Some((rows.len(), report)))
        }
        Err(e) => {
            let _ = store::queue_gc(conn, super::now_nanos() - QUEUE_STALE_NANOS);
            Err(e)
        }
    }
}

/// `rescan_paths`, taking the heavy-work permit only for a large batch.
fn rescan_gated(ctx: &Ctx<'_>, conn: &Connection, paths: &[PathBuf]) -> Result<PathScanReport> {
    let gate = if paths.len() > SMALL_BATCH {
        acquire_heavy(ctx.heavy.as_ref(), &ctx.shutdown)
    } else {
        Gate::Free
    };
    if matches!(gate, Gate::Shutdown) {
        return Ok(PathScanReport {
            paths: paths.len(),
            ..Default::default()
        });
    }
    // After the heavy permit, never before it. An Err keeps the queue rows.
    let Some(_scan) = ctx.lock_scan() else {
        anyhow::bail!("code map loop is shutting down");
    };
    rescan_paths(conn, paths)
}

fn log_paths(ctx: &Ctx<'_>, via: &str, n: usize, r: &PathScanReport) {
    ctx.log(&format!(
        "paths via={via} n={n} updated={} removed={} unchanged={} ignored={} outside={} views={} {}ms",
        r.updated,
        r.removed,
        r.unchanged,
        r.skipped_ignored,
        r.skipped_outside_views,
        r.views_touched.len(),
        r.ms
    ));
}

pub(crate) struct Pump<'a> {
    storage: Storage,
    ctx: &'a Ctx<'a>,
    watcher: Option<RecommendedWatcher>,
    rx: Receiver<PathBuf>,
    /// Every stored view root of the active repos (the callback reads this).
    view_roots: Arc<RwLock<Vec<PathBuf>>>,
    /// Roots handed to the watcher.
    pub(crate) watched: BTreeSet<PathBuf>,
    pub(crate) pending: BTreeSet<PathBuf>,
    first_at: Option<Instant>,
    last_at: Option<Instant>,
    last_queue: Option<Instant>,
    last_sync: Option<Instant>,
    seen_version: Option<u64>,
    /// Watcher batches handed to `rescan_paths`, and the paths in them.
    pub(crate) flushes: usize,
    pub(crate) flushed_paths: usize,
}

impl<'a> Pump<'a> {
    pub(crate) fn new(ctx: &'a Ctx<'a>) -> Result<Self> {
        let storage = Storage::open(ctx.db_path)?;
        let (tx, rx): (Sender<PathBuf>, Receiver<PathBuf>) = mpsc::channel();
        let view_roots: Arc<RwLock<Vec<PathBuf>>> = Arc::new(RwLock::new(Vec::new()));
        let watcher = if ctx.opts.watch {
            let roots = view_roots.clone();
            let home = ctx.home.to_path_buf();
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let Ok(event) = res else { return };
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
                }
                let Ok(roots) = roots.read() else { return };
                for p in event.paths {
                    if event_path_relevant(&p, &roots, Some(&home)) {
                        let _ = tx.send(p);
                    }
                }
            }) {
                Ok(w) => Some(w),
                Err(e) => {
                    ctx.log(&format!("watcher unavailable: {e}"));
                    None
                }
            }
        } else {
            None
        };
        Ok(Self {
            storage,
            ctx,
            watcher,
            rx,
            view_roots,
            watched: BTreeSet::new(),
            pending: BTreeSet::new(),
            first_at: None,
            last_at: None,
            last_queue: None,
            last_sync: None,
            seen_version: None,
            flushes: 0,
            flushed_paths: 0,
        })
    }

    /// Re-read the stored views of the active repos and make the watcher watch
    /// exactly their (covering) roots. Protected folders are dropped lexically
    /// before any filesystem call.
    pub(crate) fn sync_roots(&mut self, now: Instant) {
        self.last_sync = Some(now);
        let home = self.ctx.home;
        let roots: Vec<PathBuf> = self
            .storage
            .with_connection(|conn| {
                if !store::tables_ready(conn) {
                    return Ok(Vec::new());
                }
                let mut roots = Vec::new();
                for r in active_repos(conn)? {
                    for v in store::stored_views(conn, Some(&r.common_dir))? {
                        roots.push(PathBuf::from(v.worktree_path));
                    }
                }
                Ok(roots)
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|p| !is_tcc_path(p, Some(home)))
            // Resolved without stat'ing a protected folder: a stored root that
            // is now a symlink into one is dropped, not followed.
            .filter_map(|p| {
                identity::resolve_safe(&p, Some(home))
                    .ok()
                    .filter(|real| real.is_dir())
            })
            .collect();
        if let Ok(mut shared) = self.view_roots.write() {
            if *shared != roots {
                *shared = roots.clone();
            }
        }
        let want = cover(&roots);
        let Some(watcher) = self.watcher.as_mut() else {
            return;
        };
        for p in want.difference(&self.watched.clone()) {
            match watcher.watch(p, RecursiveMode::Recursive) {
                Ok(()) => {
                    self.watched.insert(p.clone());
                    self.ctx.log(&format!("watch +{}", p.display()));
                }
                Err(e) => self
                    .ctx
                    .log(&format!("watch failed for {}: {e}", p.display())),
            }
        }
        for p in self.watched.clone().difference(&want) {
            let _ = watcher.unwatch(p);
            self.watched.remove(p);
            self.ctx.log(&format!("watch -{}", p.display()));
        }
    }

    pub(crate) fn add_pending(&mut self, path: PathBuf, now: Instant) {
        self.pending.insert(path);
        self.first_at.get_or_insert(now);
        self.last_at = Some(now);
    }

    fn take_events(&mut self, now: Instant) {
        while let Ok(p) = self.rx.try_recv() {
            self.add_pending(p, now);
        }
    }

    fn flush_due(&self, now: Instant) -> bool {
        match (self.first_at, self.last_at) {
            (Some(first), Some(last)) if !self.pending.is_empty() => {
                now.duration_since(last) >= self.ctx.opts.debounce
                    || now.duration_since(first) >= self.ctx.opts.max_wait
            }
            _ => false,
        }
    }

    /// Hand the pending batch to `rescan_paths`.
    pub(crate) fn flush(&mut self) {
        let paths: Vec<PathBuf> = std::mem::take(&mut self.pending).into_iter().collect();
        self.first_at = None;
        self.last_at = None;
        if paths.is_empty() {
            return;
        }
        self.flushes += 1;
        self.flushed_paths += paths.len();
        let ctx = self.ctx;
        match self
            .storage
            .with_connection(|conn| rescan_gated(ctx, conn, &paths))
        {
            Ok(r) => log_paths(ctx, "watch", paths.len(), &r),
            Err(e) => ctx.log(&format!("paths via=watch failed: {}", short_err(&e))),
        }
    }

    pub(crate) fn drain_queue_now(&mut self) {
        let ctx = self.ctx;
        match self
            .storage
            .with_connection(|conn| drain_queue(conn, |paths| rescan_gated(ctx, conn, paths)))
        {
            Ok(Some((n, r))) => log_paths(ctx, "queue", n, &r),
            Ok(None) => {}
            Err(e) => ctx.log(&format!("paths via=queue failed: {}", short_err(&e))),
        }
    }

    /// One scheduler step at time `now`.
    pub(crate) fn tick(&mut self, now: Instant) {
        self.take_events(now);
        let version = self.ctx.roots_version.load(Ordering::SeqCst);
        let resync_due = self
            .last_sync
            .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(30));
        if self.seen_version != Some(version) || resync_due {
            self.seen_version = Some(version);
            self.sync_roots(now);
        }
        if self.flush_due(now) {
            self.flush();
        }
        if self
            .last_queue
            .is_none_or(|t| now.duration_since(t) >= self.ctx.opts.queue_tick)
        {
            self.last_queue = Some(now);
            self.drain_queue_now();
        }
    }
}

// ---- the loop ---------------------------------------------------------------------

fn poller_main(ctx: &Ctx<'_>) {
    let mut poller = match Poller::new(ctx) {
        Ok(p) => p,
        Err(e) => {
            ctx.log(&format!(
                "poller cannot open the database: {}",
                short_err(&e)
            ));
            return;
        }
    };
    while !ctx.stop() {
        poller.step(Instant::now());
        std::thread::sleep(ctx.opts.tick);
    }
}

fn pump_main(ctx: &Ctx<'_>) {
    let mut pump = match Pump::new(ctx) {
        Ok(p) => p,
        Err(e) => {
            ctx.log(&format!("pump cannot open the database: {}", short_err(&e)));
            return;
        }
    };
    ctx.log(&format!(
        "watcher {}",
        if pump.watcher.is_some() {
            "started"
        } else {
            "off"
        }
    ));
    while !ctx.stop() {
        match pump.rx.recv_timeout(ctx.opts.tick) {
            Ok(p) => pump.add_pending(p, Instant::now()),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
        }
        pump.tick(Instant::now());
    }
}

/// Run the freshness loop until `shutdown` is set. Blocking; the daemon calls it
/// from `spawn_blocking` and `csr-engine codemap watch` from its main thread.
/// Returns at once when the code map is disabled (`CSR_NO_CODEMAP=1`).
///
/// `heavy` is the daemon's heavy-work semaphore: full scans wait for it, path
/// batches of up to `SMALL_BATCH` do not. `log` receives one line per action
/// (no timestamp; the caller adds its own).
pub fn run_loop(
    db_path: &Path,
    home: &Path,
    shutdown: Arc<AtomicBool>,
    opts: LoopOpts,
    heavy: Option<Arc<Semaphore>>,
    log: impl Fn(&str) + Sync,
) {
    run_loop_inner(
        db_path,
        home,
        shutdown,
        opts,
        heavy,
        &log,
        super::disabled(),
    )
}

fn run_loop_inner(
    db_path: &Path,
    home: &Path,
    shutdown: Arc<AtomicBool>,
    opts: LoopOpts,
    heavy: Option<Arc<Semaphore>>,
    log: &(dyn Fn(&str) + Sync),
    disabled: bool,
) {
    if disabled {
        log("disabled via CSR_NO_CODEMAP");
        return;
    }
    let ctx = Ctx::new(db_path, home, shutdown, opts, heavy, log);
    std::thread::scope(|s| {
        s.spawn(|| poller_main(&ctx));
        pump_main(&ctx);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codemap::query;
    use crate::codemap::testutil::{py, write, Fx};
    use std::sync::Mutex;

    struct Rig {
        fx: Fx,
        _db_dir: tempfile::TempDir,
        db: PathBuf,
        shutdown: Arc<AtomicBool>,
    }

    impl Rig {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            Self {
                fx: Fx::new(),
                db: dir.path().join("csr-engine.db"),
                _db_dir: dir,
                shutdown: Arc::new(AtomicBool::new(false)),
            }
        }
        fn db_path(&self) -> PathBuf {
            self.db.clone()
        }
        fn ctx(&self, opts: LoopOpts) -> Ctx<'_> {
            Ctx::new(
                &self.db,
                &self.fx.home,
                self.shutdown.clone(),
                opts,
                None,
                &quiet,
            )
        }
        fn register(&self, repo: &Path) {
            let storage = Storage::open(&self.db_path()).unwrap();
            let common = repo.join(".git");
            storage
                .with_connection(|c| {
                    store::upsert_repo(
                        c,
                        &common.to_string_lossy(),
                        Some(&repo.to_string_lossy()),
                        "cli",
                        true,
                    )
                })
                .unwrap();
        }
        fn conn(&self) -> Storage {
            Storage::open(&self.db_path()).unwrap()
        }
        fn hits(&self, name: &str) -> usize {
            self.conn()
                .with_connection(|c| query::query(c, name, 20))
                .unwrap()
                .len()
        }
        fn opts(&self) -> LoopOpts {
            let mut o = LoopOpts::default();
            o.scan.home = Some(self.fx.home.clone());
            o.debounce = Duration::from_millis(150);
            o.max_wait = Duration::from_secs(2);
            o.queue_tick = Duration::from_millis(200);
            o.tick = Duration::from_millis(100);
            o
        }
    }

    fn quiet(_: &str) {}

    fn wait_until(secs: u64, mut f: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        f()
    }

    #[test]
    fn poll_scans_a_repo_after_a_commit_and_skips_an_unchanged_one() {
        let rig = Rig::new();
        let repo = rig.fx.init_repo("r");
        write(&repo, "a.py", &py(&["alpha"]));
        rig.fx.commit_all(&repo, "one");
        rig.register(&repo);

        let ctx = rig.ctx(rig.opts());
        let mut p = Poller::new(&ctx).unwrap();
        let t0 = Instant::now();

        // First sight of a registered repo: its cold scan.
        let o = p.step(t0).expect("first step polls");
        assert_eq!(o.repos, 1);
        assert_eq!(o.scanned.len(), 1, "cold scan on first pass");
        assert_eq!(rig.hits("alpha"), 1);

        // Nothing changed: the poll runs, scan_repo does not.
        let o = p.step(t0 + Duration::from_secs(61)).expect("polls again");
        assert_eq!(o.repos, 1);
        assert!(o.scanned.is_empty(), "unchanged repo is not rescanned");
        let polled = rig.conn().with_connection(store::list_repos).unwrap()[0].last_poll_at;
        assert!(polled.is_some(), "last_poll_at is recorded on every poll");

        // Not due yet: nothing runs.
        assert!(p.step(t0 + Duration::from_secs(70)).is_none());

        // A commit moves HEAD and the index: the next poll rescans.
        write(&repo, "b.py", &py(&["beta_new"]));
        rig.fx.commit_all(&repo, "two");
        let o = p.step(t0 + Duration::from_secs(125)).unwrap();
        assert_eq!(o.scanned.len(), 1);
        assert_eq!(rig.hits("beta_new"), 1);
    }

    #[test]
    fn a_restarted_poller_rescans_each_repo_once_to_catch_edits_made_while_nothing_watched() {
        let rig = Rig::new();
        let repo = rig.fx.init_repo("r");
        write(&repo, "a.py", &py(&["alpha"]));
        rig.fx.commit_all(&repo, "one");
        rig.register(&repo);
        let t0 = Instant::now();
        {
            let ctx = rig.ctx(rig.opts());
            let mut p = Poller::new(&ctx).unwrap();
            assert_eq!(p.step(t0).unwrap().scanned.len(), 1);
        }
        // A dirty edit with the daemon down: HEAD and the index do not move.
        write(&repo, "a.py", &py(&["alpha", "while_down"]));
        let ctx = rig.ctx(rig.opts());
        let mut p = Poller::new(&ctx).unwrap();
        assert_eq!(
            p.step(t0).unwrap().scanned.len(),
            1,
            "first poll after start"
        );
        assert_eq!(rig.hits("while_down"), 1);
        assert!(
            p.step(t0 + Duration::from_secs(61))
                .unwrap()
                .scanned
                .is_empty(),
            "later polls trust the probe"
        );
    }

    #[test]
    fn poll_sees_a_worktree_added_and_removed() {
        let rig = Rig::new();
        let repo = rig.fx.init_repo("r");
        write(&repo, "a.py", &py(&["alpha"]));
        rig.fx.commit_all(&repo, "one");
        rig.register(&repo);
        let ctx = rig.ctx(rig.opts());
        let mut p = Poller::new(&ctx).unwrap();
        let t0 = Instant::now();
        p.step(t0).unwrap();
        let views = |rig: &Rig| {
            rig.conn()
                .with_connection(|c| store::stored_views(c, None))
                .unwrap()
                .len()
        };
        assert_eq!(views(&rig), 1);

        let wt = rig.fx.add_worktree(&repo, &rig.fx.root.join("wt1"), "feat");
        let o = p.step(t0 + Duration::from_secs(61)).unwrap();
        assert_eq!(o.views_added, vec![wt.to_string_lossy().to_string()]);
        assert_eq!(views(&rig), 2);

        rig.fx.git(
            &repo,
            &["worktree", "remove", "--force", &wt.to_string_lossy()],
        );
        let o = p.step(t0 + Duration::from_secs(122)).unwrap();
        assert_eq!(o.views_removed, vec![wt.to_string_lossy().to_string()]);
        assert_eq!(views(&rig), 1);
    }

    #[test]
    fn discovery_runs_on_its_cadence_not_every_poll() {
        let rig = Rig::new();
        let mut opts = rig.opts();
        opts.poll = Duration::from_secs(60);
        opts.discovery = Duration::from_secs(600);
        let ctx = rig.ctx(opts);
        let mut p = Poller::new(&ctx).unwrap();
        let t0 = Instant::now();
        for i in 0..10u64 {
            p.step(t0 + Duration::from_secs(i * 60));
        }
        assert_eq!(p.polls, 10, "ten polls in ten minutes");
        assert_eq!(p.discoveries, 1, "discovery is not run on every poll");
        p.step(t0 + Duration::from_secs(10 * 60 + 1));
        assert_eq!(p.discoveries, 2, "and runs again after its interval");
    }

    #[test]
    fn repos_beyond_the_cap_are_not_polled() {
        // active_repos truncates to CSR_CODEMAP_MAX_REPOS (default 16).
        let conn = crate::codemap::testutil::db();
        for i in 0..20 {
            store::upsert_repo(&conn, &format!("/x/r{i}/.git"), None, "cli", true).unwrap();
        }
        assert_eq!(active_repos(&conn).unwrap().len(), 16);
    }

    #[test]
    fn lexical_filter_drops_build_output_and_git_and_protected_paths() {
        let roots = vec![
            PathBuf::from("/w/repo"),
            PathBuf::from("/w/repo/.claude/worktrees/x"),
        ];
        let ok = |p: &str| event_path_relevant(Path::new(p), &roots, None);
        assert!(ok("/w/repo/src/a.rs"));
        assert!(ok("/w/repo/.claude/worktrees/x/src/b.py"));
        assert!(!ok("/w/repo/target/debug/build/x/out.rs"));
        assert!(!ok("/w/repo/web/node_modules/pkg/index.js"));
        assert!(!ok("/w/repo/.git/hooks/pre-commit.py"));
        assert!(!ok("/w/repo/.claude/worktrees/x/target/gen.rs"));
        assert!(!ok("/w/repo/README.md"));
        assert!(!ok("/elsewhere/a.rs"));
        let tcc = vec![PathBuf::from("/Users/nobody/Desktop/p")];
        assert!(!event_path_relevant(
            Path::new("/Users/nobody/Desktop/p/a.rs"),
            &tcc,
            None
        ));
    }

    #[test]
    fn cover_keeps_only_outermost_roots() {
        let roots = vec![
            PathBuf::from("/w/repo"),
            PathBuf::from("/w/repo/.claude/worktrees/x"),
            PathBuf::from("/w/other"),
        ];
        let c = cover(&roots);
        assert_eq!(c.len(), 2);
        assert!(c.contains(Path::new("/w/repo")) && c.contains(Path::new("/w/other")));
    }

    #[test]
    fn watcher_batch_rescans_an_edited_file_and_ignores_build_output() {
        let rig = Rig::new();
        let repo = rig.fx.init_repo("r");
        write(&repo, "a.py", &py(&["alpha"]));
        rig.fx.commit_all(&repo, "one");
        rig.register(&repo);
        let ctx = rig.ctx(rig.opts());
        let mut p = Poller::new(&ctx).unwrap();
        p.step(Instant::now()).unwrap();
        let mut pump = Pump::new(&ctx).unwrap();
        pump.tick(Instant::now());
        assert_eq!(pump.watched.len(), 1, "the one view root is watched");

        // Build output and dependency trees: never reach a batch.
        for i in 0..50 {
            write(&repo, &format!("target/debug/gen{i}.rs"), "fn gen() {}\n");
            write(
                &repo,
                &format!("node_modules/p/f{i}.js"),
                "function g(){}\n",
            );
        }
        // A real edit.
        write(&repo, "a.py", &py(&["alpha", "gamma_dirty"]));

        let ok = wait_until(15, || {
            pump.tick(Instant::now());
            rig.hits("gamma_dirty") == 1
        });
        assert!(ok, "the edited file is rescanned by the watcher batch");
        assert!(pump.flushes >= 1);
        assert_eq!(
            pump.flushed_paths, 1,
            "only a.py reached rescan_paths; target/ and node_modules/ did not"
        );
    }

    #[test]
    fn target_churn_alone_never_flushes() {
        let rig = Rig::new();
        let repo = rig.fx.init_repo("r");
        write(&repo, "a.py", &py(&["alpha"]));
        rig.fx.commit_all(&repo, "one");
        rig.register(&repo);
        let ctx = rig.ctx(rig.opts());
        Poller::new(&ctx).unwrap().step(Instant::now()).unwrap();
        let mut pump = Pump::new(&ctx).unwrap();
        pump.tick(Instant::now());
        for i in 0..300 {
            write(
                &repo,
                &format!("target/release/deps/x{i}.rs"),
                "fn x() {}\n",
            );
        }
        let end = Instant::now() + Duration::from_secs(3);
        while Instant::now() < end {
            pump.tick(Instant::now());
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(pump.flushes, 0);
        assert!(pump.pending.is_empty());
    }

    #[test]
    fn queue_drain_keeps_a_row_enqueued_during_the_drain() {
        let conn = crate::codemap::testutil::db();
        let a = Path::new("/w/r/a.py");
        let b = Path::new("/w/r/b.py");
        crate::codemap::enqueue(&conn, a).unwrap();
        crate::codemap::enqueue(&conn, b).unwrap();
        let (n, _) = drain_queue(&conn, |paths| {
            assert_eq!(paths.len(), 2);
            // The hook fires again for a.py while the rescan runs.
            crate::codemap::enqueue(&conn, a).unwrap();
            Ok(PathScanReport::default())
        })
        .unwrap()
        .unwrap();
        assert_eq!(n, 2);
        let left = store::queue_read(&conn).unwrap();
        assert_eq!(left.len(), 1, "only the re-enqueued row survives");
        assert_eq!(left[0].0, "/w/r/a.py");
        // The next drain takes it.
        drain_queue(&conn, |_| Ok(PathScanReport::default()))
            .unwrap()
            .unwrap();
        assert!(store::queue_read(&conn).unwrap().is_empty());
    }

    #[test]
    fn a_failed_rescan_keeps_the_queue_rows() {
        let conn = crate::codemap::testutil::db();
        crate::codemap::enqueue(&conn, Path::new("/w/r/a.py")).unwrap();
        let r = drain_queue(&conn, |_| Err(anyhow::anyhow!("boom")));
        assert!(r.is_err());
        assert_eq!(store::queue_read(&conn).unwrap().len(), 1);
    }

    #[test]
    fn enqueue_twice_strictly_raises_the_stamp() {
        let conn = crate::codemap::testutil::db();
        let p = Path::new("/w/r/a.py");
        store::queue_push(&conn, &p.to_string_lossy(), 100).unwrap();
        store::queue_push(&conn, &p.to_string_lossy(), 100).unwrap();
        let rows = store::queue_read(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].1 > 100,
            "an equal clock reading still raises the stamp"
        );
    }

    #[test]
    fn queue_rows_are_rescanned_by_the_pump() {
        let rig = Rig::new();
        let repo = rig.fx.init_repo("r");
        write(&repo, "a.py", &py(&["alpha"]));
        rig.fx.commit_all(&repo, "one");
        rig.register(&repo);
        let mut opts = rig.opts();
        opts.watch = false;
        let ctx = rig.ctx(opts);
        Poller::new(&ctx).unwrap().step(Instant::now()).unwrap();
        let mut pump = Pump::new(&ctx).unwrap();
        write(&repo, "a.py", &py(&["alpha", "queued_fn"]));
        rig.conn()
            .with_connection(|c| crate::codemap::enqueue(c, &repo.join("a.py")))
            .unwrap();
        pump.tick(Instant::now());
        assert_eq!(rig.hits("queued_fn"), 1);
        assert!(rig
            .conn()
            .with_connection(store::queue_read)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn protected_roots_are_neither_watched_nor_polled() {
        let rig = Rig::new();
        let real = rig.fx.init_repo("r");
        write(&real, "a.py", &py(&["alpha"]));
        rig.fx.commit_all(&real, "one");
        rig.register(&real);
        // A repo and a view under ~/Desktop, planted straight into the tables.
        let tcc_root = "/Users/nobody-here/Desktop/proj";
        rig.conn()
            .with_connection(|c| {
                store::upsert_repo(c, &format!("{tcc_root}/.git"), Some(tcc_root), "cli", true)?;
                c.execute(
                    "INSERT INTO codemap_views (common_dir, worktree_path, status, files, truncated)
                     VALUES (?1, ?2, 'ok', 0, 0)",
                    rusqlite::params![format!("{tcc_root}/.git"), tcc_root],
                )?;
                Ok(())
            })
            .unwrap();
        let ctx = rig.ctx(rig.opts());
        let mut poller = Poller::new(&ctx).unwrap();
        let o = poller.step(Instant::now()).unwrap();
        assert_eq!(o.repos, 2);
        assert_eq!(o.scanned.len(), 1, "the protected repo is not scanned");
        let mut pump = Pump::new(&ctx).unwrap();
        pump.tick(Instant::now());
        assert_eq!(pump.watched.len(), 1);
        assert!(pump.watched.iter().all(|p| !p.starts_with(tcc_root)));
        assert!(pump
            .view_roots
            .read()
            .unwrap()
            .iter()
            .all(|p| !p.starts_with(tcc_root)));
    }

    fn repo_with_alpha(rig: &Rig) -> PathBuf {
        let repo = rig.fx.init_repo("r");
        write(&repo, "a.py", &py(&["alpha"]));
        rig.fx.commit_all(&repo, "one");
        rig.register(&repo);
        repo
    }

    #[test]
    fn a_pump_flush_waits_for_the_scan_lock_and_then_writes() {
        let rig = Rig::new();
        let repo = repo_with_alpha(&rig);
        let mut opts = rig.opts();
        opts.watch = false;
        let ctx = rig.ctx(opts);
        Poller::new(&ctx).unwrap().step(Instant::now()).unwrap();
        write(&repo, "a.py", &py(&["alpha", "newest_fn"]));

        let guard = ctx.lock_scan().expect("lock is free");
        std::thread::scope(|s| {
            let flusher = s.spawn(|| {
                let mut pump = Pump::new(&ctx).unwrap();
                pump.add_pending(repo.join("a.py"), Instant::now());
                pump.flush();
            });
            std::thread::sleep(Duration::from_millis(600));
            assert!(!flusher.is_finished(), "the flush waits for the lock");
            assert_eq!(rig.hits("newest_fn"), 0, "and writes nothing meanwhile");
            drop(guard);
            flusher.join().unwrap();
        });
        assert_eq!(rig.hits("newest_fn"), 1, "it writes once the lock is free");
    }

    #[test]
    fn a_full_scan_cannot_overwrite_a_newer_partial_rescan() {
        let rig = Rig::new();
        let repo = repo_with_alpha(&rig);
        let mut opts = rig.opts();
        opts.watch = false;
        let ctx = rig.ctx(opts);
        let t0 = Instant::now();
        Poller::new(&ctx).unwrap().step(t0).unwrap();
        // Blob A: a dirty edit that neither HEAD nor the index will ever show.
        write(&repo, "a.py", &py(&["alpha", "zeta_older"]));

        let (collected_tx, collected_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        std::thread::scope(|s| {
            // A fresh poller runs a full scan. The hook parks it after it has
            // read blob A and before it writes.
            let scanner = s.spawn(|| {
                let mut p = Poller::new(&ctx).unwrap();
                crate::codemap::scan::test_hooks::set_after_collect(move || {
                    collected_tx.send(()).unwrap();
                    // Bounded: a failing run must end, not hang the scope.
                    let _ = go_rx.recv_timeout(Duration::from_secs(20));
                });
                let o = p.step(t0 + Duration::from_secs(61)).unwrap();
                assert_eq!(o.scanned.len(), 1, "the full scan ran");
            });
            collected_rx
                .recv_timeout(Duration::from_secs(20))
                .expect("the scan reached the hook");
            // Blob B lands, and the pump is asked to rescan the path.
            write(&repo, "a.py", &py(&["alpha", "omega_newer"]));
            let flusher = s.spawn(|| {
                let mut pump = Pump::new(&ctx).unwrap();
                pump.add_pending(repo.join("a.py"), Instant::now());
                pump.flush();
            });
            std::thread::sleep(Duration::from_millis(500));
            let waited = !flusher.is_finished();
            go_tx.send(()).unwrap();
            scanner.join().unwrap();
            flusher.join().unwrap();
            assert!(
                waited,
                "the pump rescan waits while the scan holds the lock"
            );
        });
        assert_eq!(
            rig.hits("omega_newer"),
            1,
            "the newest blob is what the map holds"
        );
        assert_eq!(
            rig.hits("zeta_older"),
            0,
            "the older scan did not overwrite it"
        );
    }

    #[test]
    fn a_poisoned_scan_lock_is_recovered_and_a_shutdown_ends_the_wait() {
        let rig = Rig::new();
        let ctx = rig.ctx(rig.opts());
        let joined = std::thread::scope(|s| {
            s.spawn(|| {
                let _held = ctx.scan_lock.lock().unwrap();
                panic!("a scan thread died holding the lock");
            })
            .join()
        });
        assert!(joined.is_err());
        assert!(ctx.scan_lock.is_poisoned());
        assert!(ctx.lock_scan().is_some(), "recovered with into_inner");

        let held = ctx.lock_scan().unwrap();
        rig.shutdown.store(true, Ordering::SeqCst);
        let t0 = Instant::now();
        assert!(ctx.lock_scan().is_none(), "shutdown ends the wait");
        assert!(t0.elapsed() < Duration::from_secs(2));
        drop(held);
    }

    #[test]
    fn the_kill_switch_ends_the_loop_at_once() {
        let rig = Rig::new();
        let lines: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let t0 = Instant::now();
        run_loop_inner(
            &rig.db_path(),
            &rig.fx.home,
            rig.shutdown.clone(), // never set: only the switch can end this
            rig.opts(),
            None,
            &|m| lines.lock().unwrap().push(m.to_string()),
            true,
        );
        assert!(t0.elapsed() < Duration::from_secs(1));
        assert_eq!(lines.lock().unwrap().len(), 1);
        assert!(lines.lock().unwrap()[0].contains("CSR_NO_CODEMAP"));
        assert!(!rig.db_path().exists(), "disabled loop opens no database");
    }

    #[test]
    fn the_loop_follows_a_dirty_edit_and_a_commit_and_stops_on_shutdown() {
        let rig = Rig::new();
        let repo = rig.fx.init_repo("r");
        write(&repo, "a.py", &py(&["alpha"]));
        rig.fx.commit_all(&repo, "one");
        rig.register(&repo);
        let mut opts = rig.opts();
        opts.poll = Duration::from_secs(1);
        let db = rig.db_path();
        let home = rig.fx.home.clone();
        let shutdown = rig.shutdown.clone();
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = lines.clone();
        let handle = std::thread::spawn(move || {
            run_loop(&db, &home, shutdown, opts, None, move |m| {
                sink.lock().unwrap().push(m.to_string())
            });
        });
        assert!(
            wait_until(20, || rig.hits("alpha") == 1),
            "cold scan on the first pass"
        );
        // The watcher needs a tick to watch a view the cold scan just stored.
        assert!(
            wait_until(20, || lines
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("watch +"))),
            "the new view's root is watched"
        );
        std::thread::sleep(Duration::from_millis(500));
        // Dirty edit, no commit: the watcher.
        write(&repo, "a.py", &py(&["alpha", "dirty_fn"]));
        assert!(wait_until(20, || rig.hits("dirty_fn") == 1), "watcher");
        // Commit a new file: the poll (and the watcher).
        write(&repo, "c.py", &py(&["committed_fn"]));
        rig.fx.commit_all(&repo, "two");
        assert!(wait_until(20, || rig.hits("committed_fn") == 1), "poll");
        let t0 = Instant::now();
        rig.shutdown.store(true, Ordering::SeqCst);
        handle.join().unwrap();
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "stops within a tick or two"
        );
    }
}
