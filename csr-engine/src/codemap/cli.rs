//! `csr-engine codemap ...`: add, remove, scan, drain, status, query, watch.
//!
//! The handlers print to stdout and return `Ok(())` unless the request itself is
//! wrong. `add`, `remove` and `scan` open the database read-write; `status` and
//! `query` open it read-only and say "code map not initialised" when the tables
//! are missing. `watch` runs the daemon's freshness loop in the foreground.

use std::path::Path;

use anyhow::{bail, Context, Result};

use super::discover::discover;
use super::identity::{self, home_dir};
use super::lock;
use super::scan::{rescan_paths, scan_repo, view_changed, RepoScanReport, ScanOpts};
use super::status::{repo_path, status_report, CodemapStatus};
use super::store;
use crate::storage::Storage;

const NOT_INITIALISED: &str = "code map not initialised";

fn disabled_notice() -> &'static str {
    "code map is disabled (CSR_NO_CODEMAP=1); nothing was done"
}

fn age(secs: i64) -> String {
    match secs {
        s if s < 90 => format!("{s}s"),
        s if s < 5400 => format!("{}m", s / 60),
        s if s < 172_800 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// `codemap add <path>`: register a repo and list its views.
pub fn add(db_path: &Path, path: &Path) -> Result<()> {
    let id = identity::identify(path)?;
    let main = main_path_of(&id.common_dir);
    let storage = Storage::open(db_path)?;
    storage.with_connection(|conn| {
        store::upsert_repo(
            conn,
            &id.common_dir.to_string_lossy(),
            main.as_deref().map(|m| m.to_string_lossy()).as_deref(),
            "cli",
            true,
        )
    })?;
    let listing = identity::list_views(&id.common_dir)?;
    println!("registered {}", id.common_dir.display());
    for v in &listing.views {
        let role = if v.is_main { "main" } else { "linked" };
        println!("  {role:6} {}", v.worktree_path.display());
    }
    if listing.stale > 0 {
        println!("  ({} stale worktree entries skipped)", listing.stale);
    }
    println!("run `csr-engine codemap scan --repo <path>` to map it");
    Ok(())
}

/// `codemap remove <path>`: tombstone a repo and drop its views.
pub fn remove(db_path: &Path, path: &Path) -> Result<()> {
    let storage = Storage::open(db_path)?;
    // A repo whose directory is gone is found by the path it was registered with.
    let wanted = identity::normalize_lexical(path)
        .to_string_lossy()
        .to_string();
    let (common, main) = match identity::identify(path) {
        Ok(id) => (
            id.common_dir.to_string_lossy().to_string(),
            id.worktree_root.to_string_lossy().to_string(),
        ),
        Err(identify_err) => {
            let found = storage.with_connection(|conn| {
                Ok(store::list_repos(conn)?.into_iter().find(|r| {
                    r.common_dir == wanted || r.main_path.as_deref() == Some(wanted.as_str())
                }))
            })?;
            match found {
                Some(r) => (r.common_dir, r.main_path.unwrap_or_default()),
                None => return Err(identify_err.context("not a known or readable repo")),
            }
        }
    };
    // Deleting views while a scan writes them would leave the scan's rows behind.
    let _lock = match lock::acquire(db_path, &common, lock::wait_from_env(), &|| false)? {
        lock::Acquired::Held(l) => l,
        _ => bail!("remove skipped: {common} locked by another scanner"),
    };
    let dropped = storage.with_connection(|conn| {
        store::disable_repo(conn, &common, (!main.is_empty()).then_some(main.as_str()))?;
        let mut views = 0usize;
        for v in store::stored_views(conn, Some(&common))? {
            store::delete_view(conn, v.id)?;
            views += 1;
        }
        store::gc(conn)?;
        Ok(views)
    })?;
    println!("removed {common}: {dropped} views dropped; discovery will not re-add it");
    Ok(())
}

fn print_report(r: &RepoScanReport) {
    println!("repo {}", r.common_dir);
    for v in &r.views {
        if v.status != "ok" {
            println!("  {}  {}", v.path, v.status);
            continue;
        }
        println!(
            "  {:24} {:12} files {:5} dirty {:3} untracked {:3}{} {:5}ms  {}",
            v.branch.as_deref().unwrap_or("(detached)"),
            v.head.as_deref().unwrap_or("-"),
            v.files,
            v.dirty,
            v.untracked,
            if v.truncated { " TRUNCATED" } else { "" },
            v.ms,
            v.path,
        );
    }
    let t = &r.totals;
    println!(
        "  totals: {} view-file pairs, {} unique blobs ({} parsed, {} reused), {} defs written, {} blobs gc'd, {} removed views, {} stale worktrees, {}ms",
        t.file_pairs, t.unique_blobs_seen, t.blobs_parsed, t.blobs_reused, t.defs_written,
        t.gc_blobs, r.removed_views, r.stale_worktrees, t.ms
    );
}

/// Run `f` while holding the repo's file lock (`lock.rs`), waiting at most
/// `CSR_CODEMAP_LOCK_WAIT_SECS`. `None` means another scanner held the repo for
/// the whole wait; the caller prints `lock::skipped_line` and exits 0.
fn scan_locked<T>(
    db_path: &Path,
    common_dir: &str,
    f: impl FnOnce() -> Result<T>,
) -> Result<Option<T>> {
    match lock::acquire(db_path, common_dir, lock::wait_from_env(), &|| false)? {
        lock::Acquired::Held(_lock) => f().map(Some),
        lock::Acquired::Busy | lock::Acquired::Cancelled => Ok(None),
    }
}

/// What a repo scan under the repo lock came to.
enum SessionScan {
    /// The repo carries a `codemap remove` tombstone: nothing was written.
    Removed,
    Done(RepoScanReport),
}

/// The main checkout of a repo whose common dir is `<main>/.git`.
fn main_path_of(common_dir: &Path) -> Option<std::path::PathBuf> {
    (common_dir.file_name().is_some_and(|n| n == ".git"))
        .then(|| common_dir.parent().map(Path::to_path_buf))
        .flatten()
}

/// Register a repo for a SESSION (`scan --session`, and a drain's first full pass):
/// source `session` for a protected folder, `signal` otherwise, never forcing a
/// tombstoned repo back on. For an ordinary repo it also resolves the session's
/// signal row: that row keeps the repo active in `status` (and in a daemon that
/// starts later), and only the daemon's poll would resolve it. Best effort.
/// Call it under the repo lock, after the `repo_removed` check.
fn register_session_repo(
    conn: &rusqlite::Connection,
    common_dir: &Path,
    cwd: &Path,
    protected: bool,
) -> Result<()> {
    let common = common_dir.to_string_lossy().to_string();
    let main = main_path_of(common_dir);
    let source = if protected { "session" } else { "signal" };
    store::upsert_repo(
        conn,
        &common,
        main.as_deref().map(|m| m.to_string_lossy()).as_deref(),
        source,
        false,
    )?;
    if !protected {
        let cwd = identity::normalize_lexical(cwd);
        let _ = store::signal_resolve(conn, &cwd.to_string_lossy(), Some(&common), "repo");
    }
    Ok(())
}

/// Has `codemap remove` tombstoned this repo (a row with `enabled = 0`)?
fn repo_removed(conn: &rusqlite::Connection, common: &str) -> Result<bool> {
    Ok(store::list_repos(conn)?
        .iter()
        .any(|r| r.common_dir == common && !r.enabled))
}

/// `codemap scan [--repo <path> [--session] | --all] [--json]`.
///
/// `session` is the opt-in the SessionStart hook's detached child passes for a
/// repo inside a macOS-protected folder (Desktop, Documents, Downloads, iCloud
/// Drive, a volume): the scan may enter THAT folder, because the user's own session
/// is already working there. The repo is registered with source `session` (or
/// `signal` outside a protected folder) and is not re-enabled if `codemap remove`
/// tombstoned it. Without `session` a protected path is refused, as ever.
pub fn scan(
    db_path: &Path,
    repo: Option<&Path>,
    all: bool,
    json: bool,
    session: bool,
) -> Result<()> {
    if super::disabled() {
        println!("{}", disabled_notice());
        return Ok(());
    }
    if repo.is_none() && !all {
        bail!("pass --repo <path> or --all");
    }
    if session && repo.is_none() {
        bail!("--session needs --repo <path>");
    }
    let storage = Storage::open(db_path)?;
    let opts = ScanOpts::default();
    if let Some(path) = repo {
        let home = home_dir();
        // Held until this function returns; only this thread, only this folder.
        let scope = session.then(|| identity::session_scope(path, home.as_deref()));
        let protected = scope.as_ref().is_some_and(|s| s.root.is_some());
        let id = identity::identify(path)?;
        let common = id.common_dir.to_string_lossy().to_string();
        let report = storage.with_connection(|conn| {
            scan_locked(db_path, &common, || {
                // Under the repo lock, which `codemap remove` holds while it
                // writes the tombstone: a check made before the lock could
                // pass, wait, and then register and scan a repo removed meanwhile.
                if session && repo_removed(conn, &common)? {
                    return Ok(SessionScan::Removed);
                }
                if session {
                    register_session_repo(conn, &id.common_dir, path, protected)?;
                } else {
                    let main = main_path_of(&id.common_dir);
                    store::upsert_repo(
                        conn,
                        &common,
                        main.as_deref().map(|m| m.to_string_lossy()).as_deref(),
                        "cli",
                        true,
                    )?;
                }
                let report = scan_repo(conn, &id.common_dir, &opts)?;
                Ok(SessionScan::Done(report))
            })
        })?;
        match report {
            Some(SessionScan::Removed) => {
                let line = format!("scan skipped: {common} was removed with `codemap remove`");
                if json {
                    println!("{}", serde_json::json!({ "skipped": line }));
                } else {
                    println!("{line}");
                }
            }
            Some(SessionScan::Done(report)) if json => {
                println!("{}", serde_json::to_string_pretty(&report)?)
            }
            Some(SessionScan::Done(report)) => print_report(&report),
            None if json => println!(
                "{}",
                serde_json::json!({ "skipped": lock::skipped_line(&common) })
            ),
            None => println!("{}", lock::skipped_line(&common)),
        }
        return Ok(());
    }

    let home = home_dir().context("HOME is not set")?;
    let reports = storage.with_connection(|conn| {
        let candidates = discover(conn, &home);
        let mut reports = Vec::new();
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
        for c in &candidates {
            let common = c.common_dir.to_string_lossy().to_string();
            match scan_locked(db_path, &common, || scan_repo(conn, &c.common_dir, &opts)) {
                Ok(Some(r)) => reports.push(r),
                Ok(None) => eprintln!("{}", lock::skipped_line(&common)),
                Err(e) => {
                    eprintln!("scan failed for {}: {e:#}", c.common_dir.display());
                }
            }
        }
        Ok(reports)
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        if reports.is_empty() {
            println!("no repos discovered");
        }
        for r in &reports {
            print_report(r);
        }
    }
    Ok(())
}

fn open_for_read(db_path: &Path) -> Result<Option<Storage>> {
    if !db_path.exists() {
        return Ok(None);
    }
    Ok(Some(Storage::open_read_only(db_path)?))
}

fn print_status(s: &CodemapStatus) {
    print!("{}", render_status(s));
}

/// The text of `codemap status`. A repo that is also on the skipped list carries
/// its reason on its own line, next to its views; skipped entries that map to no
/// repo row (a protected project nothing has mapped, a repo past the cap) follow.
pub(crate) fn render_status(s: &CodemapStatus) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if s.disabled {
        let _ = writeln!(out, "(CSR_NO_CODEMAP=1 is set; background mapping is off)");
    }
    if s.repos.is_empty() {
        let _ = writeln!(out, "no repos mapped yet");
    }
    let mut shown: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for r in &s.repos {
        let path = repo_path(&r.common_dir, r.main_path.as_deref());
        let reason = s
            .skipped
            .iter()
            .find(|k| k.path == path)
            .inspect(|k| {
                shown.insert(k.path.as_str());
            })
            .map(|k| format!("  [{}]", k.reason))
            .unwrap_or_default();
        let off = if r.enabled { "" } else { "  [removed]" };
        let _ = writeln!(out, "repo {}  ({}){off}{reason}", r.common_dir, r.source);
        for v in &r.views {
            let _ = writeln!(
                out,
                "  {:24} {:12} files {:5} dirty {:3} untracked {:3}{} scanned {} ago  {}",
                v.branch.as_deref().unwrap_or("(detached)"),
                v.head.as_deref().unwrap_or("-"),
                v.files,
                v.dirty,
                v.untracked,
                if v.truncated { " TRUNCATED" } else { "" },
                v.age_secs.map(age).unwrap_or_else(|| "never".into()),
                v.path,
            );
        }
    }
    let rest: Vec<_> = s
        .skipped
        .iter()
        .filter(|k| !shown.contains(k.path.as_str()))
        .collect();
    if !rest.is_empty() {
        let _ = writeln!(out, "skipped ({}):", rest.len());
        for k in rest {
            let _ = writeln!(out, "  {}  [{}]", k.path, k.reason);
        }
    }
    let t = &s.totals;
    let _ = writeln!(
        out,
        "totals: {} repos, {} views, {} view-file pairs, {} unique blobs, {} defs, {} fts rows, {} queued, extractor {}",
        t.repos, t.views, t.view_file_pairs, t.unique_blobs, t.defs, t.fts_rows, t.queue_depth,
        s.extractor_version
    );
    out
}

/// `codemap status [--json]`.
pub fn status(db_path: &Path, json: bool) -> Result<()> {
    let Some(storage) = open_for_read(db_path)? else {
        println!("{NOT_INITIALISED}");
        return Ok(());
    };
    let report = storage.with_connection(status_report)?;
    if !report.initialised {
        println!("{NOT_INITIALISED}");
    } else if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_status(&report);
    }
    Ok(())
}

/// `codemap query <text> [--json] [--limit N]`.
pub fn query(db_path: &Path, text: &str, json: bool, limit: usize) -> Result<()> {
    let Some(storage) = open_for_read(db_path)? else {
        println!("{NOT_INITIALISED}");
        return Ok(());
    };
    let hits = storage.with_connection(|conn| {
        if !store::tables_ready(conn) {
            return Ok(None);
        }
        super::query::query(conn, text, limit).map(Some)
    })?;
    let Some(hits) = hits else {
        println!("{NOT_INITIALISED}");
        return Ok(());
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&hits)?);
    } else if hits.is_empty() {
        println!("no matches");
    } else {
        for h in &hits {
            println!("{}", h.render());
        }
    }
    Ok(())
}

/// `codemap drain --repo <path> [--debounce SECS]`: the session's partial rescan
/// for a repo in a protected folder, started by the PostToolUse hook (detached).
///
/// Waits `debounce` so a burst of edits becomes one rescan, rescans the queued
/// paths that lie in the repo's views, or the whole repo when HEAD or the index
/// moved (or a worktree appeared or went), then repeats while new rows arrived, and
/// exits. It ends after `session::DRAIN_MAX` whatever is queued: the next hook
/// starts another.
///
/// On the way out it does not just remove its session lock file: it releases the
/// lock and then looks at the queue again (`session::release_and_recheck`), so an
/// edit queued while it was deciding to exit is never stranded.
pub fn drain(db_path: &Path, repo: &Path, debounce: std::time::Duration) -> Result<()> {
    if super::disabled() {
        println!("{}", disabled_notice());
        return Ok(());
    }
    // The lock file is removed on every way out of this function that does not
    // hand it over (see `release_and_recheck`).
    let mut hold = super::session::LockHold::new(super::session::lock_path(db_path, repo));
    let home = home_dir();
    let scope = identity::session_scope(repo, home.as_deref());
    let protected = scope.root.is_some();
    let id = identity::identify(repo)?;
    let storage = Storage::open(db_path)?;
    let started = std::time::Instant::now();
    let mut passes = 0usize;
    loop {
        std::thread::sleep(debounce);
        if started.elapsed() > super::session::DRAIN_MAX {
            println!("drain: time limit after {passes} passes");
            break;
        }
        match storage.with_connection(|conn| {
            drain_pass(
                conn,
                db_path,
                &id.common_dir,
                repo,
                protected,
                home.as_deref(),
            )
        })? {
            Pass::Worked => passes += 1,
            Pass::Removed => break,
            Pass::Idle => {
                #[cfg(test)]
                super::session::test_hooks::after_idle();
                let mut rows_left = || {
                    storage
                        .with_connection(|conn| {
                            let (_, live, stored) =
                                view_roots(conn, &id.common_dir, home.as_deref())?;
                            let roots: Vec<std::path::PathBuf> =
                                live.union(&stored).map(Into::into).collect();
                            Ok(!super::session::rows_under(conn, &roots)?.is_empty())
                        })
                        // Unknown counts as "rows left": better one more look.
                        .unwrap_or(true)
                };
                match super::session::release_and_recheck(
                    &mut hold,
                    super::now_secs(),
                    &super::session::pid_alive,
                    &mut rows_left,
                ) {
                    super::session::Handoff::Continue => {}
                    super::session::Handoff::Exit => break,
                }
            }
        }
    }
    println!("drain: {passes} passes");
    Ok(())
}

/// What one drain pass came to.
#[derive(Debug, PartialEq, Eq)]
enum Pass {
    /// Nothing was queued and no view moved.
    Idle,
    /// Work was done (or could not be yet: another scanner holds the repo).
    Worked,
    /// The repo carries a `codemap remove` tombstone: nothing was written, and
    /// the drain ends.
    Removed,
}

type ViewRoots = (
    identity::ViewListing,
    std::collections::BTreeSet<String>,
    std::collections::BTreeSet<String>,
);

/// The repo's live views and its stored views, as worktree path strings.
fn view_roots(
    conn: &rusqlite::Connection,
    common: &Path,
    home: Option<&Path>,
) -> Result<ViewRoots> {
    let common_str = common.to_string_lossy().to_string();
    let listing = identity::list_views_with(common, home)?;
    let live = listing
        .views
        .iter()
        .map(|v| v.worktree_path.to_string_lossy().to_string())
        .collect();
    let stored = store::stored_views(conn, Some(&common_str))?
        .into_iter()
        .map(|v| v.worktree_path)
        .collect();
    Ok((listing, live, stored))
}

/// One drain pass. `cwd` is the path the drain was started for and `protected`
/// says whether it lies in a macOS-protected folder: a repo the map does not
/// know yet is registered by the first full pass, as `scan --session` would.
fn drain_pass(
    conn: &rusqlite::Connection,
    db_path: &Path,
    common: &Path,
    cwd: &Path,
    protected: bool,
    home: Option<&Path>,
) -> Result<Pass> {
    let common_str = common.to_string_lossy().to_string();
    let (listing, live, stored) = view_roots(conn, common, home)?;
    let mut changed = live != stored;
    if !changed {
        for v in &listing.views {
            if view_changed(conn, v).unwrap_or(true) {
                changed = true;
                break;
            }
        }
    }
    let roots: Vec<std::path::PathBuf> = live.union(&stored).map(Into::into).collect();
    let rows = super::session::rows_under(conn, &roots)?;
    if rows.is_empty() && !changed {
        return Ok(Pass::Idle);
    }
    let held = lock::acquire(db_path, &common_str, lock::wait_from_env(), &|| false)?;
    let lock::Acquired::Held(_held) = held else {
        // Another scanner has the repo; the rows stay and the next pass retries.
        println!("{}", lock::skipped_line(&common_str));
        return Ok(Pass::Worked);
    };
    // Under the repo lock, which `codemap remove` holds while it writes the
    // tombstone and drops the views: a removed repo is not scanned, not
    // rescanned, and its views are not recreated by the "views differ" full scan.
    if repo_removed(conn, &common_str)? {
        println!("drain skipped: {common_str} was removed with `codemap remove`");
        return Ok(Pass::Removed);
    }
    if changed {
        // A repo the map has no row for (the first edit of a session with no
        // daemon) is registered here, under the lock and after the tombstone check.
        if !store::list_repos(conn)?
            .iter()
            .any(|r| r.common_dir == common_str)
        {
            register_session_repo(conn, common, cwd, protected)?;
        }
        let report = scan_repo(conn, common, &ScanOpts::default())?;
        println!(
            "drain: full scan {} views, {} defs written",
            report.views.len(),
            report.totals.defs_written
        );
    } else {
        let paths: Vec<std::path::PathBuf> = rows
            .iter()
            .map(|(p, _)| std::path::PathBuf::from(p))
            .collect();
        let report = rescan_paths(conn, &paths)?;
        println!(
            "drain: {} paths, {} updated, {} removed, {} unchanged",
            report.paths, report.updated, report.removed, report.unchanged
        );
    }
    // Only the rows this pass saw: one re-enqueued meanwhile keeps its newer stamp.
    for (p, at) in &rows {
        store::queue_delete_upto(conn, p, *at)?;
    }
    Ok(Pass::Worked)
}

fn stamp() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", d.as_secs(), d.subsec_millis())
}

/// `codemap watch [--for SECS]`: the daemon's poll + watcher + queue-drain loop
/// in the foreground against `--db-path`, one timestamped line (epoch seconds)
/// per action. Runs until Ctrl-C, or for SECS seconds.
pub fn watch(db_path: &Path, for_secs: Option<u64>) -> Result<()> {
    if super::disabled() {
        println!("{}", disabled_notice());
        return Ok(());
    }
    let home = home_dir().context("HOME is not set")?;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Some(secs) = for_secs {
        let flag = shutdown.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(secs));
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
    }
    let opts = super::fresh::LoopOpts::from_env();
    println!(
        "{} watch start db={} poll={}s debounce={}s",
        stamp(),
        db_path.display(),
        opts.poll.as_secs(),
        opts.debounce.as_secs()
    );
    super::fresh::run_loop(db_path, &home, shutdown, opts, None, |msg| {
        println!("{} {msg}", stamp());
    });
    println!("{} watch stop", stamp());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codemap::session::{self, test_hooks};
    use crate::codemap::testutil::{count, py, write, Fx};

    /// A repo with one tracked file, a db path next to it, and the repo path.
    fn repo_with_db(fx: &Fx) -> (std::path::PathBuf, std::path::PathBuf) {
        let repo = fx.init_repo("r");
        write(&repo, "a.py", &py(&["alpha"]));
        fx.commit_all(&repo, "init");
        let data = fx.home.join("data");
        std::fs::create_dir_all(&data).unwrap();
        (repo, data.join("csr-engine.db"))
    }

    fn defs_named(db_path: &Path, name: &str) -> i64 {
        let conn = rusqlite::Connection::open(db_path).unwrap();
        count(
            &conn,
            &format!("SELECT COUNT(*) FROM codemap_defs WHERE name = '{name}'"),
        )
    }

    #[test]
    fn a_drain_picks_up_an_edit_queued_while_it_was_deciding_to_exit() {
        let fx = Fx::new();
        let (repo, db_path) = repo_with_db(&fx);
        let lock = session::lock_path(&db_path, &repo);
        // The child as the hook leaves it: a lock file naming this process.
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        let held = session::try_take(&lock, super::super::now_secs(), &session::pid_alive)
            .unwrap()
            .expect("lock is free");
        held.set_pid(std::process::id(), super::super::now_secs());

        let (r, d, l) = (repo.clone(), db_path.clone(), lock.clone());
        test_hooks::set_after_idle(move || {
            // What PostToolUse does, in its order: the file changes, the row is
            // queued, THEN the lock is tried. The drain still holds it, so no
            // second drain is spawned and the row has no other owner.
            write(&r, "a.py", &py(&["alpha", "gamma_late"]));
            let conn = rusqlite::Connection::open(&d).unwrap();
            super::super::enqueue(&conn, &r.join("a.py")).unwrap();
            let taken =
                session::try_take(&l, super::super::now_secs(), &session::pid_alive).unwrap();
            assert!(taken.is_none(), "the live drain's lock turns the hook away");
        });

        drain(&db_path, &repo, std::time::Duration::ZERO).unwrap();

        assert_eq!(
            defs_named(&db_path, "gamma_late"),
            1,
            "the late edit was rescanned by the same drain"
        );
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        assert!(
            store::queue_read(&conn).unwrap().is_empty(),
            "no row is left stranded"
        );
        assert!(!lock.exists(), "the drain's lock goes on the way out");
    }

    #[test]
    fn a_session_scan_of_an_ordinary_repo_registers_it_and_resolves_its_signal() {
        let fx = Fx::new();
        let (repo, db_path) = repo_with_db(&fx);
        let storage = Storage::open(&db_path).unwrap();
        storage
            .with_connection(|conn| super::super::record_signal(conn, &repo, "session_start"))
            .unwrap();

        scan(&db_path, Some(&repo), false, true, true).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let repos = store::list_repos(&conn).unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].source, "signal");
        let signals = store::list_signals(&conn, 0).unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(
            signals[0].common_dir.as_deref(),
            Some(repos[0].common_dir.as_str())
        );
        assert_eq!(signals[0].status.as_deref(), Some("repo"));
        assert_eq!(defs_named(&db_path, "alpha"), 1);
    }

    #[test]
    fn a_drain_of_an_unregistered_ordinary_repo_registers_maps_and_resolves_it() {
        let fx = Fx::new();
        let (repo, db_path) = repo_with_db(&fx);
        let storage = Storage::open(&db_path).unwrap();
        storage
            .with_connection(|conn| {
                super::super::record_signal(conn, &repo, "post_tool_use")?;
                // the edit the hook queued
                super::super::enqueue(conn, &repo.join("a.py"))
            })
            .unwrap();
        drop(storage);

        drain(&db_path, &repo, std::time::Duration::ZERO).unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let repos = store::list_repos(&conn).unwrap();
        assert_eq!(repos.len(), 1, "the drain registered the repo");
        assert_eq!(repos[0].source, "signal");
        assert!(repos[0].enabled);
        assert_eq!(defs_named(&db_path, "alpha"), 1, "and mapped its defs");
        let signals = store::list_signals(&conn, 0).unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].status.as_deref(), Some("repo"));
        assert_eq!(
            signals[0].common_dir.as_deref(),
            Some(repos[0].common_dir.as_str())
        );
        assert!(store::queue_read(&conn).unwrap().is_empty());
    }

    #[test]
    fn a_drain_pass_on_a_removed_repo_writes_nothing() {
        let fx = Fx::new();
        let (repo, db_path) = repo_with_db(&fx);
        let common = repo.join(".git");
        let storage = Storage::open(&db_path).unwrap();
        storage
            .with_connection(|conn| {
                scan_repo(
                    conn,
                    &common,
                    &ScanOpts {
                        home: Some(fx.home.clone()),
                        ..Default::default()
                    },
                )?;
                store::upsert_repo(
                    conn,
                    &common.to_string_lossy(),
                    Some(&repo.to_string_lossy()),
                    "cli",
                    true,
                )?;
                // `codemap remove`: tombstone, then drop the views.
                store::disable_repo(conn, &common.to_string_lossy(), None)?;
                for v in store::stored_views(conn, Some(&common.to_string_lossy()))? {
                    store::delete_view(conn, v.id)?;
                }
                store::gc(conn)?;
                // An edit that was queued before the removal, and a changed file.
                write(&repo, "a.py", &py(&["alpha", "gamma_removed"]));
                super::super::enqueue(conn, &repo.join("a.py"))?;
                Ok(())
            })
            .unwrap();

        let pass = storage
            .with_connection(|conn| {
                drain_pass(conn, &db_path, &common, &repo, false, Some(&fx.home))
            })
            .unwrap();

        assert_eq!(pass, Pass::Removed);
        assert_eq!(defs_named(&db_path, "gamma_removed"), 0);
        assert_eq!(defs_named(&db_path, "alpha"), 0, "no view was recreated");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        assert!(store::stored_views(&conn, None).unwrap().is_empty());
        assert_eq!(
            store::queue_read(&conn).unwrap().len(),
            1,
            "the drain writes nothing, not even the queue"
        );
        assert!(!repo_removed(&conn, "no/such/repo").unwrap());
        assert!(repo_removed(&conn, &common.to_string_lossy()).unwrap());
    }
}
