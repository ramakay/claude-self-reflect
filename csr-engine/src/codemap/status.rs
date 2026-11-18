//! Read-only report of what the code map holds. `csr-engine codemap status`
//! prints it; `csr-engine status` reuses `status_report`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

use super::discover::{active_hours_from_env, max_repos_from_env, select_repos};
use super::identity::{home_dir, is_tcc_path, normalize_lexical, tcc_root};
use super::store;

/// Why a repo under a macOS-protected folder is not polled or watched.
pub const REASON_PROTECTED: &str = "protected-folder: mapped by session only";
/// Why every repo is skipped while the kill switch is on.
pub const REASON_DISABLED: &str = "CSR_NO_CODEMAP";

#[derive(Debug, Clone, Serialize)]
pub struct ViewStatus {
    pub path: String,
    pub branch: Option<String>,
    /// First 12 hex chars of HEAD.
    pub head: Option<String>,
    pub files: i64,
    pub dirty: i64,
    pub untracked: i64,
    pub truncated: bool,
    pub scanned_at: Option<i64>,
    pub age_secs: Option<i64>,
    pub status: String,
    pub scan_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoStatus {
    pub common_dir: String,
    pub main_path: Option<String>,
    /// cli | env | signal | (legacy) code_nodes | history | unregistered
    pub source: String,
    pub enabled: bool,
    pub status: String,
    pub last_poll_at: Option<i64>,
    pub views: Vec<ViewStatus>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StatusTotals {
    pub repos: i64,
    pub views: i64,
    pub view_file_pairs: i64,
    pub unique_blobs: i64,
    pub defs: i64,
    pub fts_rows: i64,
    pub truncated_views: i64,
    /// Paths waiting in `codemap_queue` for a partial rescan.
    pub queue_depth: i64,
}

/// A repo (or protected project folder) the daemon's poller and watcher leave
/// alone, and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkippedRepo {
    pub path: String,
    /// `protected-folder: mapped by session only`, `idle > <N>h`, `cap <N>` or
    /// `CSR_NO_CODEMAP`.
    pub reason: String,
}

/// What the skipped list is computed against. Plain values so a test can pin
/// every one of them.
pub struct SkipContext<'a> {
    pub home: Option<&'a Path>,
    pub now: i64,
    pub active_hours: u64,
    pub max_repos: usize,
    pub disabled: bool,
}

/// The directory a stored repo is shown under: its main checkout, else the
/// parent of its `.git`.
pub fn repo_path(common_dir: &str, main_path: Option<&str>) -> String {
    if let Some(m) = main_path.filter(|m| !m.is_empty()) {
        return m.to_string();
    }
    let p = Path::new(common_dir);
    if p.file_name().is_some_and(|n| n == ".git") {
        if let Some(parent) = p.parent() {
            return parent.to_string_lossy().into_owned();
        }
    }
    common_dir.to_string()
}

#[derive(Debug, Clone, Serialize)]
pub struct CodemapStatus {
    /// False when the code map tables do not exist yet.
    pub initialised: bool,
    pub disabled: bool,
    pub extractor_version: String,
    pub repos: Vec<RepoStatus>,
    /// Repos the daemon does not poll or watch, with the reason. A protected
    /// repo a session mapped is here too, next to its views in `repos`.
    pub skipped: Vec<SkippedRepo>,
    pub totals: StatusTotals,
}

/// The first folder below a protected root (`~/Documents/proj` for
/// `~/Documents/proj/sub/dir`), lexically; the root itself when the path is the
/// root.
fn protected_project(cwd: &Path, home: &Path) -> Option<PathBuf> {
    let root = tcc_root(cwd, Some(home))?;
    let norm = normalize_lexical(cwd);
    match norm.strip_prefix(&root.shown) {
        Ok(rest) => match rest.components().next() {
            Some(first) => Some(root.shown.join(first)),
            None => Some(root.shown),
        },
        Err(_) => Some(norm),
    }
}

/// Everything the daemon leaves alone, from stored rows and signals only: no
/// filesystem call, so reading it under a protected folder never prompts.
pub fn skipped_repos(conn: &Connection, ctx: &SkipContext<'_>) -> Result<Vec<SkippedRepo>> {
    if ctx.disabled {
        return Ok(store::list_repos(conn)?
            .into_iter()
            .filter(|r| r.enabled)
            .map(|r| SkippedRepo {
                path: repo_path(&r.common_dir, r.main_path.as_deref()),
                reason: REASON_DISABLED.to_string(),
            })
            .collect());
    }
    let Some(home) = ctx.home else {
        return Ok(Vec::new());
    };
    let sel = select_repos(conn, home, ctx.now, ctx.active_hours, ctx.max_repos)?;
    let mut out = Vec::new();

    // Protected: the repos a session mapped, then the project folders sessions
    // were seen in that nothing has mapped (yet).
    let mut protected: BTreeSet<String> = BTreeSet::new();
    let mut roots: Vec<PathBuf> = Vec::new();
    for r in &sel.protected {
        let path = repo_path(&r.common_dir, r.main_path.as_deref());
        roots.push(normalize_lexical(Path::new(&path)));
        protected.insert(path);
    }
    let since = ctx
        .now
        .saturating_sub((ctx.active_hours as i64).saturating_mul(3600));
    for s in store::list_signals(conn, since)? {
        let cwd = Path::new(&s.cwd);
        if !is_tcc_path(cwd, Some(home)) {
            continue;
        }
        let norm = normalize_lexical(cwd);
        if roots.iter().any(|root| norm.starts_with(root)) {
            continue;
        }
        if let Some(project) = protected_project(cwd, home) {
            protected.insert(project.to_string_lossy().into_owned());
        }
    }
    out.extend(protected.into_iter().map(|path| SkippedRepo {
        path,
        reason: REASON_PROTECTED.to_string(),
    }));

    let idle = format!("idle > {}h", ctx.active_hours);
    out.extend(sel.idle.iter().map(|i| SkippedRepo {
        path: repo_path(&i.repo.common_dir, i.repo.main_path.as_deref()),
        reason: idle.clone(),
    }));
    let cap = format!("cap {}", ctx.max_repos);
    out.extend(sel.over_cap.iter().map(|r| SkippedRepo {
        path: repo_path(&r.common_dir, r.main_path.as_deref()),
        reason: cap.clone(),
    }));
    out.extend(sel.unregistered.iter().map(|(common_dir, _)| SkippedRepo {
        path: repo_path(common_dir, None),
        reason: cap.clone(),
    }));
    Ok(out)
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0)
}

/// Everything the code map holds, per repo and view, plus totals.
pub fn status_report(conn: &Connection) -> Result<CodemapStatus> {
    let home = home_dir();
    status_report_with(
        conn,
        &SkipContext {
            home: home.as_deref(),
            now: super::now_secs(),
            active_hours: active_hours_from_env(),
            max_repos: max_repos_from_env(),
            disabled: super::disabled(),
        },
    )
}

/// [`status_report`] with the clock, home and limits supplied.
pub fn status_report_with(conn: &Connection, ctx: &SkipContext<'_>) -> Result<CodemapStatus> {
    let mut out = CodemapStatus {
        initialised: store::tables_ready(conn),
        disabled: ctx.disabled,
        extractor_version: super::extractor_version().to_string(),
        repos: Vec::new(),
        skipped: Vec::new(),
        totals: StatusTotals::default(),
    };
    if !out.initialised {
        return Ok(out);
    }
    let now = ctx.now;
    out.skipped = skipped_repos(conn, ctx)?;
    let mut dirty_counts: HashMap<i64, (i64, i64)> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT view_id, SUM(dirty = 1), SUM(dirty = 2) FROM codemap_view_files GROUP BY view_id",
        )?;
        for row in stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })? {
            let (id, d, u) = row?;
            dirty_counts.insert(id, (d, u));
        }
    }
    let mut repos: BTreeMap<String, RepoStatus> = BTreeMap::new();
    for r in store::list_repos(conn)? {
        repos.insert(
            r.common_dir.clone(),
            RepoStatus {
                common_dir: r.common_dir,
                main_path: r.main_path,
                source: r.source,
                enabled: r.enabled,
                status: r.status,
                last_poll_at: r.last_poll_at,
                views: Vec::new(),
            },
        );
    }
    for v in store::stored_views(conn, None)? {
        let (dirty, untracked) = dirty_counts.get(&v.id).copied().unwrap_or((0, 0));
        let repo = repos
            .entry(v.common_dir.clone())
            .or_insert_with(|| RepoStatus {
                common_dir: v.common_dir.clone(),
                main_path: None,
                source: "unregistered".into(),
                enabled: true,
                status: "ok".into(),
                last_poll_at: None,
                views: Vec::new(),
            });
        if v.truncated {
            out.totals.truncated_views += 1;
        }
        repo.views.push(ViewStatus {
            path: v.worktree_path,
            branch: v.branch,
            head: v.head_oid.map(|h| h.chars().take(12).collect()),
            files: v.files,
            dirty,
            untracked,
            truncated: v.truncated,
            scanned_at: v.scanned_at,
            age_secs: v.scanned_at.map(|t| (now - t).max(0)),
            status: v.status,
            scan_ms: v.scan_ms,
        });
    }
    out.repos = repos.into_values().collect();
    out.totals.repos = out.repos.len() as i64;
    out.totals.views = out.repos.iter().map(|r| r.views.len() as i64).sum();
    out.totals.view_file_pairs = count(conn, "SELECT COUNT(*) FROM codemap_view_files");
    out.totals.unique_blobs = count(conn, "SELECT COUNT(DISTINCT blob_oid) FROM codemap_blobs");
    out.totals.defs = count(conn, "SELECT COUNT(*) FROM codemap_defs");
    out.totals.fts_rows = count(conn, "SELECT COUNT(*) FROM codemap_fts");
    out.totals.queue_depth = count(conn, "SELECT COUNT(*) FROM codemap_queue");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codemap::testutil::{db, map_fixture, scan_repo_into};

    const NOW: i64 = 1_800_000_000;
    // A home that does not exist anywhere: every path below is invented, so a
    // stat of any of them would fail rather than return what the test expects.
    const HOME: &str = "/nonexistent-home/u";

    fn ctx(home: Option<&Path>, max_repos: usize, disabled: bool) -> SkipContext<'_> {
        SkipContext {
            home,
            now: NOW,
            active_hours: 24,
            max_repos,
            disabled,
        }
    }

    fn repo(conn: &Connection, dir: &str, source: &str) {
        store::upsert_repo(conn, &format!("{dir}/.git"), Some(dir), source, false).unwrap();
    }

    fn signal(conn: &Connection, cwd: &str, common_dir: Option<&str>, ago: i64) {
        store::signal_upsert(conn, cwd, "session_start", NOW - ago).unwrap();
        if let Some(c) = common_dir {
            store::signal_resolve(conn, cwd, Some(c), "repo").unwrap();
        }
    }

    fn pairs(v: &[SkippedRepo]) -> Vec<(&str, &str)> {
        v.iter()
            .map(|k| (k.path.as_str(), k.reason.as_str()))
            .collect()
    }

    #[test]
    fn skipped_names_protected_idle_cap_and_unregistered_repos_with_their_reasons() {
        let conn = db();
        let home = Path::new(HOME);
        repo(&conn, "/nonexistent-home/u/work/a", "history");
        signal(
            &conn,
            "/nonexistent-home/u/work/a",
            Some("/nonexistent-home/u/work/a/.git"),
            100,
        );
        repo(&conn, "/nonexistent-home/u/work/b", "history");
        signal(
            &conn,
            "/nonexistent-home/u/work/b",
            Some("/nonexistent-home/u/work/b/.git"),
            200,
        );
        // c: stored, no signal at all.
        repo(&conn, "/nonexistent-home/u/work/c", "code_nodes");
        // d: signalled but never registered (discovery's cap kept it out).
        signal(
            &conn,
            "/nonexistent-home/u/work/d/src",
            Some("/nonexistent-home/u/work/d/.git"),
            300,
        );
        // A repo a session mapped in Documents, and two more signals in protected folders.
        repo(&conn, "/nonexistent-home/u/Documents/probe", "session");
        signal(&conn, "/nonexistent-home/u/Documents/probe/src", None, 50);
        signal(&conn, "/nonexistent-home/u/Desktop/other/deep/er", None, 60);
        // A removed repo is not "skipped": it is gone.
        repo(&conn, "/nonexistent-home/u/work/gone", "history");
        store::disable_repo(&conn, "/nonexistent-home/u/work/gone/.git", None).unwrap();

        let skipped = skipped_repos(&conn, &ctx(Some(home), 1, false)).unwrap();
        assert_eq!(
            pairs(&skipped),
            vec![
                ("/nonexistent-home/u/Desktop/other", REASON_PROTECTED),
                ("/nonexistent-home/u/Documents/probe", REASON_PROTECTED),
                ("/nonexistent-home/u/work/c", "idle > 24h"),
                ("/nonexistent-home/u/work/b", "cap 1"),
                ("/nonexistent-home/u/work/d", "cap 1"),
            ],
            "a is the one active repo; the Documents signal sits under the stored probe repo"
        );
        // The reasons are the documented strings.
        assert_eq!(REASON_PROTECTED, "protected-folder: mapped by session only");
    }

    #[test]
    fn a_signal_outside_the_window_names_no_protected_project() {
        let conn = db();
        let home = Path::new(HOME);
        signal(&conn, "/nonexistent-home/u/Downloads/old", None, 25 * 3600);
        signal(&conn, "/nonexistent-home/u/Downloads/new", None, 3600);
        let skipped = skipped_repos(&conn, &ctx(Some(home), 16, false)).unwrap();
        assert_eq!(
            pairs(&skipped),
            vec![("/nonexistent-home/u/Downloads/new", REASON_PROTECTED)]
        );
    }

    #[test]
    fn the_kill_switch_skips_every_enabled_repo_and_the_report_says_so() {
        let conn = db();
        let home = Path::new(HOME);
        repo(&conn, "/nonexistent-home/u/work/a", "cli");
        repo(&conn, "/nonexistent-home/u/Documents/probe", "session");
        repo(&conn, "/nonexistent-home/u/work/gone", "history");
        store::disable_repo(&conn, "/nonexistent-home/u/work/gone/.git", None).unwrap();
        let report = status_report_with(&conn, &ctx(Some(home), 16, true)).unwrap();
        assert!(report.disabled);
        let mut got = pairs(&report.skipped);
        got.sort_unstable();
        assert_eq!(
            got,
            vec![
                ("/nonexistent-home/u/Documents/probe", "CSR_NO_CODEMAP"),
                ("/nonexistent-home/u/work/a", "CSR_NO_CODEMAP"),
            ]
        );
    }

    #[test]
    fn without_a_home_nothing_is_guessed() {
        let conn = db();
        repo(&conn, "/nonexistent-home/u/work/a", "history");
        assert!(skipped_repos(&conn, &ctx(None, 16, false))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn pinned_repos_and_signalled_repos_inside_the_cap_are_not_skipped() {
        let conn = db();
        let home = Path::new(HOME);
        repo(&conn, "/nonexistent-home/u/work/pinned", "cli");
        repo(&conn, "/nonexistent-home/u/work/env", "env");
        repo(&conn, "/nonexistent-home/u/work/live", "signal");
        signal(
            &conn,
            "/nonexistent-home/u/work/live",
            Some("/nonexistent-home/u/work/live/.git"),
            10,
        );
        assert!(skipped_repos(&conn, &ctx(Some(home), 16, false))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_protected_repo_a_session_mapped_shows_its_views_and_the_protected_reason_together() {
        // The repo is scanned where it really is (a temp dir, not protected), then
        // reported with a home that makes `<root>/Documents/probe` a protected
        // folder, so no real Documents folder is involved anywhere.
        let m = map_fixture();
        let conn = db();
        scan_repo_into(&conn, &m.fx, &m.main);
        let common = m.main.join(".git").to_string_lossy().into_owned();
        let main = m.main.to_string_lossy().into_owned();
        store::upsert_repo(&conn, &common, Some(&main), "session", false).unwrap();
        // Make the fixture repo look like it lives in <home>/Documents.
        let home = m.main.parent().unwrap().to_path_buf();
        let docs_home = home.join("fakehome");
        let fake_main = docs_home.join("Documents/mapfx");
        let fake_common = fake_main.join(".git");
        conn.execute(
            "UPDATE codemap_repos SET common_dir = ?1, main_path = ?2",
            rusqlite::params![fake_common.to_string_lossy(), fake_main.to_string_lossy()],
        )
        .unwrap();
        conn.execute(
            "UPDATE codemap_views SET common_dir = ?1",
            rusqlite::params![fake_common.to_string_lossy()],
        )
        .unwrap();

        let report = status_report_with(&conn, &ctx(Some(&docs_home), 16, false)).unwrap();
        assert_eq!(report.repos.len(), 1);
        assert_eq!(report.repos[0].views.len(), 3, "the views are still listed");
        assert_eq!(
            report.skipped,
            vec![SkippedRepo {
                path: fake_main.to_string_lossy().into_owned(),
                reason: REASON_PROTECTED.to_string(),
            }]
        );
        let text = crate::codemap::cli::render_status(&report);
        let repo_line = text.lines().find(|l| l.starts_with("repo ")).unwrap();
        assert!(
            repo_line.ends_with("[protected-folder: mapped by session only]"),
            "{text}"
        );
        assert!(text.contains("files"), "views are printed under it: {text}");
        assert!(
            !text.contains("skipped ("),
            "a repo shown above is not listed twice: {text}"
        );
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["skipped"][0]["reason"], REASON_PROTECTED);
        assert_eq!(
            json["skipped"][0]["path"],
            fake_main.to_string_lossy().as_ref()
        );
    }

    #[test]
    fn the_text_lists_skipped_entries_that_have_no_repo_row() {
        let report = CodemapStatus {
            initialised: true,
            disabled: false,
            extractor_version: "x".into(),
            repos: vec![],
            skipped: vec![
                SkippedRepo {
                    path: "/h/Desktop/other".into(),
                    reason: REASON_PROTECTED.into(),
                },
                SkippedRepo {
                    path: "/w/d".into(),
                    reason: "cap 16".into(),
                },
            ],
            totals: StatusTotals::default(),
        };
        let text = crate::codemap::cli::render_status(&report);
        assert!(text.contains("no repos mapped yet"), "{text}");
        assert!(text.contains("skipped (2):"), "{text}");
        assert!(
            text.contains("  /h/Desktop/other  [protected-folder: mapped by session only]"),
            "{text}"
        );
        assert!(text.contains("  /w/d  [cap 16]"), "{text}");
    }

    #[test]
    fn repo_path_prefers_the_main_checkout_then_the_parent_of_dot_git() {
        assert_eq!(repo_path("/w/a/.git", Some("/w/main")), "/w/main");
        assert_eq!(repo_path("/w/a/.git", None), "/w/a");
        assert_eq!(repo_path("/w/bare.git", None), "/w/bare.git");
        assert_eq!(repo_path("/w/a/.git", Some("")), "/w/a");
    }
}
