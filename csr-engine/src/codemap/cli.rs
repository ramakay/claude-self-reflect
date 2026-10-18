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
    let main = (id.common_dir.file_name().is_some_and(|n| n == ".git"))
        .then(|| id.common_dir.parent().map(Path::to_path_buf))
        .flatten();
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
        let (source, force) = match (session, protected) {
            (false, _) => ("cli", true),
            (true, true) => ("session", false),
            (true, false) => ("signal", false),
        };
        if session {
            let removed = storage.with_connection(|conn| {
                Ok(store::list_repos(conn)?
                    .iter()
                    .any(|r| r.common_dir == common && !r.enabled))
            })?;
            if removed {
                let line = format!("scan skipped: {common} was removed with `codemap remove`");
                if json {
                    println!("{}", serde_json::json!({ "skipped": line }));
                } else {
                    println!("{line}");
                }
                return Ok(());
            }
        }
        let report = storage.with_connection(|conn| {
            let main = (id.common_dir.file_name().is_some_and(|n| n == ".git"))
                .then(|| id.common_dir.parent().map(Path::to_path_buf))
                .flatten();
            store::upsert_repo(
                conn,
                &common,
                main.as_deref().map(|m| m.to_string_lossy()).as_deref(),
                source,
                force,
            )?;
            scan_locked(db_path, &common, || scan_repo(conn, &id.common_dir, &opts))
        })?;
        match report {
            Some(report) if json => println!("{}", serde_json::to_string_pretty(&report)?),
            Some(report) => print_report(&report),
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
/// starts another. On exit it removes its session lock file.
pub fn drain(db_path: &Path, repo: &Path, debounce: std::time::Duration) -> Result<()> {
    if super::disabled() {
        println!("{}", disabled_notice());
        return Ok(());
    }
    // The lock file is removed on every way out of this function.
    let _hold = super::session::LockHold::new(super::session::lock_path(db_path, repo));
    let home = home_dir();
    let _scope = identity::session_scope(repo, home.as_deref());
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
        let did = storage
            .with_connection(|conn| drain_pass(conn, db_path, &id.common_dir, home.as_deref()))?;
        if !did {
            break;
        }
        passes += 1;
    }
    println!("drain: {passes} passes");
    Ok(())
}

/// One drain pass. True when it did work (or could not yet: another scanner holds
/// the repo), false when nothing was queued and no view moved.
fn drain_pass(
    conn: &rusqlite::Connection,
    db_path: &Path,
    common: &Path,
    home: Option<&Path>,
) -> Result<bool> {
    let common_str = common.to_string_lossy().to_string();
    let listing = identity::list_views_with(common, home)?;
    let live: std::collections::BTreeSet<String> = listing
        .views
        .iter()
        .map(|v| v.worktree_path.to_string_lossy().to_string())
        .collect();
    let stored: std::collections::BTreeSet<String> = store::stored_views(conn, Some(&common_str))?
        .into_iter()
        .map(|v| v.worktree_path)
        .collect();
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
        return Ok(false);
    }
    let held = lock::acquire(db_path, &common_str, lock::wait_from_env(), &|| false)?;
    let lock::Acquired::Held(_held) = held else {
        // Another scanner has the repo; the rows stay and the next pass retries.
        println!("{}", lock::skipped_line(&common_str));
        return Ok(true);
    };
    if changed {
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
    Ok(true)
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
