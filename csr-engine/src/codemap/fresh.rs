//! Keeps the code map fresh. One loop, two callers: the daemon (inside
//! `spawn_blocking`) and `csr-engine codemap watch` (foreground).
//!
//! Three signals feed it:
//!
//! 1. **Poll** (~60 s). For every ACTIVE repo (a session signalled from it inside
//!    `CSR_CODEMAP_ACTIVE_HOURS`, or `codemap add` / `CSR_CODEMAP_REPOS` names it;
//!    capped at `CSR_CODEMAP_MAX_REPOS`), compare the live worktree listing with
//!    the stored views and ask `view_changed` per view. A changed, added or removed
//!    view runs `scan_repo` (which re-parses only new blobs). A repo with no recent
//!    signal is IDLE: not polled, not watched, its views, blobs and defs kept, so
//!    the next signal rescans it warm. Discovery (which resolves the signals) runs
//!    on its own 10-minute cadence, first at start-up, and early when a cwd is seen
//!    for the first time.
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
//! therefore lands in seconds even while a dream holds the permit. Scans of one
//! repo are serialised by a per-repo FILE lock (`lock.rs`), shared with every other
//! process that scans (`codemap scan`, the session children): a path batch waits
//! for a full scan in progress, so a scan that read an older blob can never commit
//! it over a newer partial rescan. Lock order: heavy permit, then repo lock.
//!
//! Nothing here busy-loops: both threads wake once a second to compare clocks,
//! and an idle wake does no I/O except the pump's 5 s queue probe (one SELECT on
//! a table that is empty at rest).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

use super::discover::{
    active_hours_from_env, discover_with, max_repos_from_env, select_repos, Selection,
    DEFAULT_ACTIVE_HOURS, DEFAULT_MAX_REPOS,
};
use super::identity::{self, is_tcc_path, ViewId};
use super::scan::{
    dir_event_rel_ok, expand_dir_events, rel_passes_caps, rescan_paths_with, scan_repo,
    view_changed, PathScanReport, ScanOpts,
};
use super::store;
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
    /// How often discovery re-resolves the session signals and registers repos.
    pub discovery: Duration,
    /// How often the hook queue is drained.
    pub queue_tick: Duration,
    /// Wake granularity (shutdown latency, debounce resolution).
    pub tick: Duration,
    /// A repo whose scan failed is not retried for this long.
    pub error_backoff: Duration,
    /// How long a scan waits for another scanner of the same repo
    /// (`CSR_CODEMAP_LOCK_WAIT_SECS`) before it skips the repo.
    pub lock_wait: Duration,
    /// A repo is polled and watched while its newest session signal is younger
    /// than this many hours, or while `cli`/`env` names it
    /// (`CSR_CODEMAP_ACTIVE_HOURS`).
    pub active_hours: u64,
    /// At most this many repos are polled and watched (`CSR_CODEMAP_MAX_REPOS`).
    pub max_repos: usize,
    /// `CSR_CODEMAP_REPOS`: `:`-separated paths mapped whatever the signals say.
    pub env_repos: Option<String>,
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
            lock_wait: super::lock::DEFAULT_WAIT,
            active_hours: DEFAULT_ACTIVE_HOURS,
            max_repos: DEFAULT_MAX_REPOS,
            env_repos: None,
            watch: true,
            scan: ScanOpts::default(),
        }
    }
}

impl LoopOpts {
    /// Defaults, with `CSR_CODEMAP_POLL_SECS`, `CSR_CODEMAP_ACTIVE_HOURS`,
    /// `CSR_CODEMAP_MAX_REPOS` and `CSR_CODEMAP_REPOS` applied and `CSR_CODEMAP_NO_WATCH=1`
    /// (poll and hook queue only, no file watcher: for a host where `notify`
    /// misbehaves, and for measuring the poll path alone) honoured.
    pub fn from_env() -> Self {
        let mut o = Self {
            watch: !std::env::var("CSR_CODEMAP_NO_WATCH")
                .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            lock_wait: super::lock::wait_from_env(),
            active_hours: active_hours_from_env(),
            max_repos: max_repos_from_env(),
            env_repos: std::env::var("CSR_CODEMAP_REPOS").ok(),
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
        }
    }

    fn stop(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// Take the repo's file lock (see `lock.rs`), polling so a shutdown is
    /// noticed while another scanner holds it. Always called AFTER the heavy
    /// permit, never before it.
    fn lock_repo(&self, common_dir: &str) -> Result<super::lock::Acquired> {
        super::lock::acquire(self.db_path, common_dir, self.opts.lock_wait, &|| {
            self.stop()
        })
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

/// The repos the loop works on, and the ones it leaves alone: see
/// `discover::select_repos`. Stored rows and signals only; no filesystem call.
fn selection(conn: &Connection, ctx: &Ctx<'_>) -> Result<Selection> {
    select_repos(
        conn,
        ctx.home,
        super::now_secs(),
        ctx.opts.active_hours,
        ctx.opts.max_repos,
    )
}

fn short_err(e: &anyhow::Error) -> String {
    format!("error: {e:#}").chars().take(160).collect()
}

// ---- poller -------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub(crate) struct PollOutcome {
    pub repos: usize,
    /// Enabled repos left alone: no session signal inside the active window and
    /// not named by `cli`/`env`.
    pub idle: usize,
    /// Active repos past `CSR_CODEMAP_MAX_REPOS`.
    pub over_cap: usize,
    /// Common dirs `scan_repo` ran for.
    pub scanned: Vec<String>,
    pub views_added: Vec<String>,
    pub views_removed: Vec<String>,
    pub backed_off: usize,
    pub errors: usize,
    /// Repos whose scan was skipped because another scanner held the repo lock.
    pub skipped_locked: usize,
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
    /// The common dirs active at the last poll. A change re-syncs the watcher.
    active: Option<BTreeSet<String>>,
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
            active: None,
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
                "poll repos={} idle={} over_cap={} scanned={} views_added={} views_removed={} backed_off={} errors={}",
                outcome.repos,
                outcome.idle,
                outcome.over_cap,
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
        let opts = &self.ctx.opts;
        let res = self.storage.with_connection(|conn| {
            if !store::tables_ready(conn) {
                return Ok((0, 0));
            }
            let before = store::list_repos(conn)?.len();
            let candidates = discover_with(
                conn,
                home,
                opts.env_repos.as_deref(),
                opts.max_repos,
                opts.active_hours,
                super::now_secs(),
            );
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
        // A cwd seen for the first time cannot name a repo until discovery has
        // resolved it: run discovery now instead of up to its interval later.
        let since = super::now_secs()
            .saturating_sub((self.ctx.opts.active_hours as i64).saturating_mul(3600));
        let unresolved = self
            .storage
            .with_connection(|conn| store::signals_unresolved(conn, since))
            .unwrap_or(0);
        if unresolved > 0 && self.last_discovery != Some(now) {
            self.last_discovery = Some(now);
            self.discover_now();
        }
        let ctx = self.ctx;
        let picked = match self.storage.with_connection(|conn| {
            if !store::tables_ready(conn) {
                return Ok(Selection::default());
            }
            selection(conn, ctx)
        }) {
            Ok(r) => r,
            Err(e) => {
                self.ctx.log(&format!("poll failed: {}", short_err(&e)));
                out.errors += 1;
                return out;
            }
        };
        out.idle = picked.idle.len();
        out.over_cap = picked.over_cap.len();
        // A repo that went idle is checked afresh when a signal brings it back
        // (one warm full scan), and the watcher follows the active set.
        let now_active: BTreeSet<String> =
            picked.active.iter().map(|r| r.common_dir.clone()).collect();
        self.checked.retain(|c| now_active.contains(c));
        if self.active.as_ref() != Some(&now_active) {
            if self.active.is_some() {
                self.ctx.roots_version.fetch_add(1, Ordering::SeqCst);
            }
            self.active = Some(now_active);
        }
        let repos = picked.active;
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
        // Heavy permit first (taken above), then the repo's file lock.
        let repo_lock = match self.ctx.lock_repo(common_str)? {
            super::lock::Acquired::Held(l) => l,
            super::lock::Acquired::Cancelled => return Ok(()),
            super::lock::Acquired::Busy => {
                // Not an error and no backoff: the next poll tries again.
                self.ctx.log(&super::lock::skipped_line(common_str));
                out.skipped_locked += 1;
                return Ok(());
            }
        };
        let opts = self.ctx.opts.scan.clone();
        let report = self
            .storage
            .with_connection(|conn| scan_repo(conn, common, &opts))?;
        drop(repo_lock);
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
/// queued: a stored view root above the path, no hard-skip directory
/// (`target/`, `node_modules/`, `.git/` ...) below that root, not in a protected
/// folder, and either
///
/// - a code extension (a file edit), or
/// - no extension at all, with no hard-skip or tool-cache segment even in the
///   last position: a directory that may have been renamed, created or removed
///   (`mv src lib` makes events for `src` and `lib`, neither with an extension).
///   The pump decides what such a path is; the callback only forwards it.
///
/// No filesystem call.
pub(crate) fn event_path_relevant(path: &Path, roots: &[PathBuf], home: Option<&Path>) -> bool {
    let code = has_code_extension(path);
    if !code && path.extension().is_some() {
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
    let shape_ok = if code {
        rel_passes_caps(rel)
    } else {
        dir_event_rel_ok(rel)
    };
    shape_ok && !is_tcc_path(path, home)
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
///
/// Rows inside a protected folder (lexical check against `home`) are LEFT IN PLACE:
/// the session's own `codemap drain` takes them. The daemon never rescans them,
/// but it does drop the ones older than an hour, so a protected repo nobody edits
/// any more does not keep rows forever.
pub(crate) fn drain_queue<F>(
    conn: &Connection,
    home: Option<&Path>,
    rescan: F,
) -> Result<Option<(usize, PathScanReport)>>
where
    F: FnOnce(&[PathBuf]) -> Result<PathScanReport>,
{
    let mut rows = store::queue_read(conn)?;
    let all = rows.len();
    rows.retain(|(p, _)| !is_tcc_path(Path::new(p), home));
    if rows.len() < all {
        store::queue_gc(conn, super::now_nanos() - QUEUE_STALE_NANOS)?;
    }
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
    // Each repo's file lock is taken after the heavy permit, never before it, and
    // only for that repo's views. An Err keeps the queue rows.
    let report = rescan_paths_with(conn, paths, &|common| match ctx.lock_repo(common) {
        Ok(super::lock::Acquired::Held(l)) => Some(Box::new(l)),
        Ok(super::lock::Acquired::Busy) => {
            ctx.log(&super::lock::skipped_line(common));
            None
        }
        Ok(super::lock::Acquired::Cancelled) => None,
        Err(e) => {
            ctx.log(&format!("lock failed for {common}: {}", short_err(&e)));
            None
        }
    })?;
    if let Some(repo) = report.locked_repos.first() {
        return Err(RepoLocked(repo.clone()).into());
    }
    Ok(report)
}

/// A path batch could not take a repo's lock: the paths must be tried again, not
/// dropped.
#[derive(Debug)]
pub(crate) struct RepoLocked(pub String);

impl std::fmt::Display for RepoLocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", super::lock::skipped_line(&self.0))
    }
}

impl std::error::Error for RepoLocked {}

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
    /// Scheduler steps taken (`tick` calls).
    pub(crate) ticks: usize,
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
            ticks: 0,
        })
    }

    /// Re-read the stored views of the active repos and make the watcher watch
    /// exactly their (covering) roots. Protected folders are dropped lexically
    /// before any filesystem call.
    pub(crate) fn sync_roots(&mut self, now: Instant) {
        self.last_sync = Some(now);
        let home = self.ctx.home;
        let ctx = self.ctx;
        let roots: Vec<PathBuf> = self
            .storage
            .with_connection(|conn| {
                if !store::tables_ready(conn) {
                    return Ok(Vec::new());
                }
                let mut roots = Vec::new();
                for r in selection(conn, ctx)?.active {
                    for v in store::stored_views(conn, Some(&r.common_dir))? {
                        roots.push(PathBuf::from(v.worktree_path));
                    }
                }
                Ok(roots)
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|p| !is_tcc_path(p, Some(home)))
            // A stored view that contains the home folder or a protected folder
            // (written before `list_views_with` dropped them) is not watched; the
            // next full scan deletes it.
            .filter(|p| !identity::contains_tcc_root(p, Some(home)))
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
        match self.storage.with_connection(|conn| {
            // Directory events (renames) become the code files below them.
            let expanded = expand_dir_events(conn, &paths)?;
            if expanded.dir_events > 0 {
                ctx.log(&format!(
                    "dirs n={} -> {} paths",
                    expanded.dir_events,
                    expanded.paths.len()
                ));
            }
            rescan_gated(ctx, conn, &expanded.paths)
        }) {
            Ok(r) => log_paths(ctx, "watch", paths.len(), &r),
            Err(e) if e.downcast_ref::<RepoLocked>().is_some() => {
                // Another scanner held the repo for the whole wait: keep the
                // paths for the next flush instead of losing the edit.
                ctx.log(&format!("paths via=watch deferred: {e}"));
                if !ctx.stop() {
                    let now = Instant::now();
                    for p in paths {
                        self.add_pending(p, now);
                    }
                }
            }
            Err(e) => ctx.log(&format!("paths via=watch failed: {}", short_err(&e))),
        }
    }

    pub(crate) fn drain_queue_now(&mut self) {
        let ctx = self.ctx;
        match self.storage.with_connection(|conn| {
            drain_queue(conn, Some(ctx.home), |paths| rescan_gated(ctx, conn, paths))
        }) {
            Ok(Some((n, r))) => log_paths(ctx, "queue", n, &r),
            Ok(None) => {}
            Err(e) => ctx.log(&format!("paths via=queue failed: {}", short_err(&e))),
        }
    }

    /// One scheduler step at time `now`.
    pub(crate) fn tick(&mut self, now: Instant) {
        self.ticks += 1;
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
    pump_loop(ctx, &mut pump);
}

/// The pump's wake loop: once per `tick`, or at once when the watcher sends a
/// path. With the watcher off (`CSR_CODEMAP_NO_WATCH=1`, or no backend) the only
/// `Sender` is already dropped, so `recv_timeout` reports `Disconnected` without
/// waiting; the loop must sleep the tick itself or it spins a core.
fn pump_loop(ctx: &Ctx<'_>, pump: &mut Pump<'_>) {
    while !ctx.stop() {
        match pump.rx.recv_timeout(ctx.opts.tick) {
            Ok(p) => pump.add_pending(p, Instant::now()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(ctx.opts.tick),
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
#[path = "fresh_tests.rs"]
mod tests;
