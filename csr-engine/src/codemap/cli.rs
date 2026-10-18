//! `csr-engine codemap ...`: add, remove, scan, status, query.
//!
//! The handlers print to stdout and return `Ok(())` unless the request itself is
//! wrong. `add`, `remove` and `scan` open the database read-write; `status` and
//! `query` open it read-only and say "code map not initialised" when the tables
//! are missing.

use std::path::Path;

use anyhow::{bail, Context, Result};

use super::discover::discover;
use super::identity::{self, home_dir};
use super::scan::{scan_repo, RepoScanReport, ScanOpts};
use super::status::{status_report, CodemapStatus};
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

/// `codemap scan [--repo <path> | --all] [--json]`.
pub fn scan(db_path: &Path, repo: Option<&Path>, all: bool, json: bool) -> Result<()> {
    if super::disabled() {
        println!("{}", disabled_notice());
        return Ok(());
    }
    if repo.is_none() && !all {
        bail!("pass --repo <path> or --all");
    }
    let storage = Storage::open(db_path)?;
    let opts = ScanOpts::default();
    if let Some(path) = repo {
        let id = identity::identify(path)?;
        let report = storage.with_connection(|conn| {
            let main = (id.common_dir.file_name().is_some_and(|n| n == ".git"))
                .then(|| id.common_dir.parent().map(Path::to_path_buf))
                .flatten();
            store::upsert_repo(
                conn,
                &id.common_dir.to_string_lossy(),
                main.as_deref().map(|m| m.to_string_lossy()).as_deref(),
                "cli",
                true,
            )?;
            scan_repo(conn, &id.common_dir, &opts)
        })?;
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            print_report(&report);
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
            match scan_repo(conn, &c.common_dir, &opts) {
                Ok(r) => reports.push(r),
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
    if s.disabled {
        println!("(CSR_NO_CODEMAP=1 is set; background mapping is off)");
    }
    if s.repos.is_empty() {
        println!("no repos mapped yet");
    }
    for r in &s.repos {
        let off = if r.enabled { "" } else { "  [removed]" };
        println!("repo {}  ({}){off}", r.common_dir, r.source);
        for v in &r.views {
            println!(
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
    let t = &s.totals;
    println!(
        "totals: {} repos, {} views, {} view-file pairs, {} unique blobs, {} defs, {} fts rows, {} queued, extractor {}",
        t.repos, t.views, t.view_file_pairs, t.unique_blobs, t.defs, t.fts_rows, t.queue_depth,
        s.extractor_version
    );
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
