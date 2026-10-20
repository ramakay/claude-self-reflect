//! Which repos to map: the ones a session is working in.
//!
//! Sources, in priority order:
//!
//! 1. enabled `codemap_repos` rows registered by `codemap add` (`cli`);
//! 2. `CSR_CODEMAP_REPOS` (`:`-separated paths, `env`), and enabled rows an earlier
//!    run registered from it;
//! 3. **session signals** (`codemap_signals`): the cwd every SessionStart and
//!    PostToolUse hook fire records. A signal newer than `CSR_CODEMAP_ACTIVE_HOURS`
//!    (default 24) makes its repo a candidate (`signal`).
//!
//! The old triggers are gone: nothing reads `history.jsonl` (30 days of every
//! directory the owner ever opened) or `code_nodes` any more, and a repo row an
//! older binary registered from them (`history`, `code_nodes`) names no repo by
//! itself: it is mapped again only while a signal or a `cli`/`env` source names it.
//!
//! Every path is mapped to its git common dir and deduplicated, so a signal from
//! any worktree, or any subdirectory, of a repo maps the whole repo (all its
//! worktrees). Dropped: paths that are missing or not in git, `$HOME` itself,
//! protected folders (the daemon never stats them; a session maps those, see
//! `session.rs`), temp roots (unless registered by `cli` or `env`), and any repo
//! with an `enabled = 0` row (`codemap remove` leaves that tombstone so discovery
//! never re-adds it). What became of each signal's cwd is written back to its row
//! (`status`, `common_dir`), which `select_repos` and `codemap status` read. The
//! result is capped at `CSR_CODEMAP_MAX_REPOS` (default 16): `cli` first, then
//! `env`, then the most recent signal.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::identity::normalize_lexical;
use super::identity::{
    contains_tcc_root, identify_with, is_dir_safe, is_protected_error, is_tcc_path,
    only_excluded_views, temp_excluded,
};
use super::store::{self, StoredRepo};

pub const DEFAULT_MAX_REPOS: usize = 16;
pub const DEFAULT_ACTIVE_HOURS: u64 = 24;

/// Signal rows are kept at least this long after their last fire (and never less
/// than twice the active window), so the table does not grow with every directory a
/// session ever started in.
const SIGNAL_KEEP_SECS: i64 = 7 * 24 * 3600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub common_dir: PathBuf,
    /// The main worktree, when the common dir is a normal `<main>/.git`.
    pub main_path: Option<PathBuf>,
    /// cli | env | signal
    pub source: &'static str,
    /// Latest session signal (seconds since epoch) seen for the repo, 0 if none.
    pub recent_ts: i64,
}

/// Cap order: `cli`, `env`, `signal`; anything else (a legacy `history` or
/// `code_nodes` row) last.
pub(super) fn tier(source: &str) -> u8 {
    match source {
        "cli" => 0,
        "env" => 1,
        "signal" => 2,
        _ => 3,
    }
}

/// Discover the repos to map, reading `CSR_CODEMAP_REPOS`, `CSR_CODEMAP_MAX_REPOS`
/// and `CSR_CODEMAP_ACTIVE_HOURS` from the environment.
pub fn discover(conn: &Connection, home: &Path) -> Vec<Candidate> {
    let env_repos = std::env::var("CSR_CODEMAP_REPOS").ok();
    discover_with(
        conn,
        home,
        env_repos.as_deref(),
        max_repos_from_env(),
        active_hours_from_env(),
        super::now_secs(),
    )
}

/// `CSR_CODEMAP_MAX_REPOS`, default 16. The freshness loop caps what it polls
/// with the same number.
pub fn max_repos_from_env() -> usize {
    std::env::var("CSR_CODEMAP_MAX_REPOS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_REPOS)
}

/// `CSR_CODEMAP_ACTIVE_HOURS`, default 24: how long after its last session signal
/// a repo stays mapped.
pub fn active_hours_from_env() -> u64 {
    active_hours_from_value(std::env::var("CSR_CODEMAP_ACTIVE_HOURS").ok().as_deref())
}

pub fn active_hours_from_value(value: Option<&str>) -> u64 {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_ACTIVE_HOURS)
}

/// A repo found by one input, before deduplication.
struct Found {
    common: PathBuf,
    source: &'static str,
    /// Signal time (seconds), 0 for an input that is not a signal.
    ts: i64,
}

/// `discover` with its environment inputs passed in (testable). `now_secs` is
/// seconds since the epoch, the unit signals are stored in.
pub fn discover_with(
    conn: &Connection,
    home: &Path,
    env_repos: Option<&str>,
    max_repos: usize,
    active_hours: u64,
    now_secs: i64,
) -> Vec<Candidate> {
    discover_impl(
        conn,
        home,
        env_repos,
        max_repos,
        active_hours,
        now_secs,
        &temp_excluded,
    )
}

/// `discover_with`, with the temp-root rule injected: tests build their repos
/// under the system temp dir, which the real rule refuses.
pub(super) fn discover_impl(
    conn: &Connection,
    home: &Path,
    env_repos: Option<&str>,
    max_repos: usize,
    active_hours: u64,
    now_secs: i64,
    is_temp: &dyn Fn(&Path) -> bool,
) -> Vec<Candidate> {
    let home_opt = Some(home);
    let mut found: Vec<Found> = Vec::new();
    let mut tombstones: Vec<PathBuf> = Vec::new();

    // 1. Rows the user (or CSR_CODEMAP_REPOS on an earlier run) registered. A row
    //    an older binary registered from history or the code graph is not a source.
    if store::tables_ready(conn) {
        if let Ok(rows) = store::list_repos(conn) {
            for r in rows {
                let common = normalize_lexical(Path::new(&r.common_dir));
                if !r.enabled {
                    tombstones.push(common);
                    continue;
                }
                let source = match r.source.as_str() {
                    "cli" => "cli",
                    "env" => "env",
                    _ => continue,
                };
                // Resolved without stat'ing a protected folder: a registered common
                // dir that is now a symlink into one is dropped.
                if is_tcc_path(&common, home_opt) || !is_dir_safe(&common, home_opt) {
                    continue;
                }
                found.push(Found {
                    common,
                    source,
                    ts: 0,
                });
            }
        }
    }

    // 2. CSR_CODEMAP_REPOS.
    for p in env_repos.unwrap_or("").split(':').filter(|p| !p.is_empty()) {
        let path = normalize_lexical(Path::new(p));
        if is_tcc_path(&path, home_opt) {
            continue;
        }
        let Ok(id) = identify_with(&path, home_opt) else {
            continue;
        };
        if is_tcc_path(&id.common_dir, home_opt) {
            continue;
        }
        found.push(Found {
            common: id.common_dir,
            source: "env",
            ts: 0,
        });
    }

    // 3. Session signals inside the window. What each cwd came to is written back.
    let since = now_secs.saturating_sub((active_hours as i64).saturating_mul(3600));
    let signals = store::list_signals(conn, since).unwrap_or_default();
    for s in &signals {
        let resolved = resolve_signal(Path::new(&s.cwd), home, is_temp, &tombstones);
        let (common, status) = match &resolved {
            Resolved::Repo(common) => (Some(common.to_string_lossy().to_string()), "repo"),
            Resolved::Dropped(why) => (None, *why),
        };
        let _ = store::signal_resolve(conn, &s.cwd, common.as_deref(), status);
        if let Resolved::Repo(common) = resolved {
            found.push(Found {
                common,
                source: "signal",
                ts: s.last_at,
            });
        }
    }
    let keep = SIGNAL_KEEP_SECS.max((active_hours as i64).saturating_mul(2 * 3600));
    let _ = store::signal_gc(conn, now_secs.saturating_sub(keep));

    // Merge: one candidate per common dir, the best source, the newest signal.
    let mut by_common: HashMap<PathBuf, Candidate> = HashMap::new();
    for f in found {
        if tombstones.contains(&f.common) {
            continue;
        }
        let main_path = (f.common.file_name().is_some_and(|n| n == ".git"))
            .then(|| f.common.parent().map(Path::to_path_buf))
            .flatten();
        by_common
            .entry(f.common.clone())
            .and_modify(|c| {
                if tier(f.source) < tier(c.source) {
                    c.source = f.source;
                }
                c.recent_ts = c.recent_ts.max(f.ts);
            })
            .or_insert(Candidate {
                common_dir: f.common,
                main_path,
                source: f.source,
                recent_ts: f.ts,
            });
    }
    let mut out: Vec<Candidate> = by_common.into_values().collect();
    out.sort_by(|a, b| {
        tier(a.source)
            .cmp(&tier(b.source))
            .then(b.recent_ts.cmp(&a.recent_ts))
            .then(a.common_dir.cmp(&b.common_dir))
    });
    out.truncate(max_repos);
    out
}

/// Sources that keep a repo active whatever its last signal: the user (or their
/// environment) named it.
pub(super) fn is_pinned(source: &str) -> bool {
    matches!(source, "cli" | "env")
}

/// Cap rank of a repo that is active: pinned ones by their source, everything else
/// (a `signal` row, or a legacy row a signal has named again) as a signal repo.
fn live_tier(source: &str) -> u8 {
    if is_pinned(source) {
        tier(source)
    } else {
        tier("signal")
    }
}

/// The stored repos split by whether the poller and the watcher work on them.
#[derive(Debug, Default)]
pub struct Selection {
    /// Polled and watched: pinned (`cli`, `env`) or signalled inside the window,
    /// pinned first, then the newest signal, at most `max_repos`.
    pub active: Vec<StoredRepo>,
    /// Enabled, not pinned, no signal inside the window: left alone. Their views,
    /// blobs and defs stay, so the next signal rescans them warm.
    pub idle: Vec<IdleRepo>,
    /// Active by signal or pin, but past the cap.
    pub over_cap: Vec<StoredRepo>,
    /// Inside a protected folder: only a session maps these.
    pub protected: Vec<StoredRepo>,
    /// Repos with an in-window signal and no stored row at all (discovery's cap
    /// kept them out): common dir and signal time.
    pub unregistered: Vec<(String, i64)>,
}

#[derive(Debug, Clone)]
pub struct IdleRepo {
    pub repo: StoredRepo,
    /// Seconds since the epoch of the repo's latest signal, if one is on record.
    pub last_signal: Option<i64>,
}

/// Decide, from stored rows and signals only, which repos are active. No
/// filesystem call: the protected test is lexical.
pub fn select_repos(
    conn: &Connection,
    home: &Path,
    now_secs: i64,
    active_hours: u64,
    max_repos: usize,
) -> anyhow::Result<Selection> {
    let since = now_secs.saturating_sub((active_hours as i64).saturating_mul(3600));
    let signalled = store::signal_activity(conn, since)?;
    let latest = store::signal_activity(conn, 0)?;
    let all = store::list_repos(conn)?;
    let known: std::collections::HashSet<&str> =
        all.iter().map(|r| r.common_dir.as_str()).collect();
    let mut unregistered: Vec<(String, i64)> = signalled
        .iter()
        .filter(|(c, _)| !known.contains(c.as_str()))
        .map(|(c, t)| (c.clone(), *t))
        .collect();
    unregistered.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut sel = Selection {
        unregistered,
        ..Selection::default()
    };

    let mut live: Vec<(StoredRepo, i64)> = Vec::new();
    for r in all.iter().filter(|r| r.enabled) {
        if is_tcc_path(Path::new(&r.common_dir), Some(home)) {
            sel.protected.push(r.clone());
        } else if is_pinned(&r.source) || signalled.contains_key(&r.common_dir) {
            let at = signalled.get(&r.common_dir).copied().unwrap_or(0);
            live.push((r.clone(), at));
        } else {
            sel.idle.push(IdleRepo {
                repo: r.clone(),
                last_signal: latest.get(&r.common_dir).copied(),
            });
        }
    }
    live.sort_by(|(a, at), (b, bt)| {
        live_tier(&a.source)
            .cmp(&live_tier(&b.source))
            .then(bt.cmp(at))
            .then(b.added_at.cmp(&a.added_at))
            .then(a.common_dir.cmp(&b.common_dir))
    });
    let mut live: Vec<StoredRepo> = live.into_iter().map(|(r, _)| r).collect();
    if live.len() > max_repos {
        sel.over_cap = live.split_off(max_repos);
    }
    sel.active = live;
    sel.idle.sort_by(|a, b| {
        b.last_signal
            .cmp(&a.last_signal)
            .then(a.repo.common_dir.cmp(&b.repo.common_dir))
    });
    Ok(sel)
}

/// What a signal's cwd came to.
#[derive(Debug)]
enum Resolved {
    Repo(PathBuf),
    /// Why it maps nothing: `protected`, `home`, `temp`, `disabled`, `not-a-repo`
    /// or `removed`.
    Dropped(&'static str),
}

/// Resolve a session cwd to its repo. The protected-folder check is lexical and
/// runs before any filesystem call; `identify_with` resolves the path one
/// component at a time (`resolve_safe`) and refuses a symlink chain into a
/// protected folder before it stats it.
fn resolve_signal(
    cwd: &Path,
    home: &Path,
    is_temp: &dyn Fn(&Path) -> bool,
    tombstones: &[PathBuf],
) -> Resolved {
    let home_opt = Some(home);
    let cwd = normalize_lexical(cwd);
    if is_tcc_path(&cwd, home_opt) {
        return Resolved::Dropped("protected");
    }
    if cwd == normalize_lexical(home) {
        return Resolved::Dropped("home");
    }
    if is_temp(&cwd) {
        return Resolved::Dropped("temp");
    }
    match identify_with(&cwd, home_opt) {
        Ok(id) => {
            if is_tcc_path(&id.common_dir, home_opt) || is_tcc_path(&id.worktree_root, home_opt) {
                Resolved::Dropped("protected")
            } else if contains_tcc_root(&id.worktree_root, home_opt)
                || only_excluded_views(&id.common_dir, home_opt)
            {
                // A repo rooted at the home folder (or above, or at a folder that
                // holds a protected one), or one whose every view is: a scan would
                // walk all of $HOME. A repo with a linked worktree elsewhere keeps
                // that view; `list_views_with` drops the others.
                Resolved::Dropped("home")
            } else if is_temp(&id.common_dir) || is_temp(&id.worktree_root) {
                Resolved::Dropped("temp")
            } else if tombstones.contains(&id.common_dir) {
                Resolved::Dropped("disabled")
            } else {
                Resolved::Repo(id.common_dir)
            }
        }
        Err(e) if is_protected_error(&e) => Resolved::Dropped("protected"),
        Err(_) if is_dir_safe(&cwd, home_opt) => Resolved::Dropped("not-a-repo"),
        Err(_) => Resolved::Dropped("removed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codemap::testutil::{db, Fx};

    const NOW: i64 = 1_800_000_000;
    const H: i64 = 3600;

    fn no_temp(_: &Path) -> bool {
        false
    }

    fn run(
        conn: &Connection,
        fx: &Fx,
        env: Option<&str>,
        max: usize,
        hours: u64,
        now: i64,
    ) -> Vec<Candidate> {
        discover_impl(conn, &fx.home, env, max, hours, now, &no_temp)
    }

    fn signal(conn: &Connection, dir: &Path, source: &str, at: i64) {
        store::signal_upsert(conn, &dir.to_string_lossy(), source, at).unwrap();
    }

    fn status_of(conn: &Connection, dir: &Path) -> (Option<String>, Option<String>) {
        let s = store::list_signals(conn, 0)
            .unwrap()
            .into_iter()
            .find(|s| s.cwd == dir.to_string_lossy())
            .expect("signal row");
        (s.status, s.common_dir)
    }

    #[test]
    fn a_signal_in_the_window_makes_the_repo_a_candidate_from_any_worktree_or_subdirectory() {
        let fx = Fx::new();
        let conn = db();
        let main = fx.init_repo("r");
        std::fs::create_dir_all(main.join("src/deep")).unwrap();
        crate::codemap::testutil::write(&main, "a.py", "def a(): pass\n");
        fx.commit_all(&main, "one");
        let wt = fx.add_worktree(&main, &fx.root.join("r-wt"), "wt");

        signal(&conn, &main.join("src/deep"), "post_tool_use", NOW - 5 * H);
        signal(&conn, &wt, "session_start", NOW - H);
        let got = run(&conn, &fx, None, 16, 24, NOW);
        assert_eq!(
            got.len(),
            1,
            "one repo, whichever worktree signalled: {got:?}"
        );
        assert_eq!(got[0].source, "signal");
        assert_eq!(got[0].common_dir, main.join(".git"));
        assert_eq!(got[0].main_path.as_deref(), Some(main.as_path()));
        assert_eq!(got[0].recent_ts, NOW - H, "the newest signal of the repo");
        assert_eq!(
            status_of(&conn, &wt),
            (
                Some("repo".into()),
                Some(main.join(".git").to_string_lossy().into())
            ),
            "the answer is written back to the signal row"
        );
    }

    #[test]
    fn a_signal_older_than_the_window_is_not_a_candidate_and_cli_and_env_repos_always_are() {
        let fx = Fx::new();
        let conn = db();
        let stale = fx.init_repo("stale");
        let pinned_cli = fx.init_repo("cli");
        let pinned_env = fx.init_repo("env");
        signal(&conn, &stale, "session_start", NOW - 25 * H);
        signal(&conn, &pinned_cli, "session_start", NOW - 900 * H);
        store::upsert_repo(
            &conn,
            &pinned_cli.join(".git").to_string_lossy(),
            Some(&pinned_cli.to_string_lossy()),
            "cli",
            true,
        )
        .unwrap();
        let env = pinned_env.to_string_lossy().to_string();
        let got = run(&conn, &fx, Some(&env), 16, 24, NOW);
        let names: Vec<(&str, &Path)> = got
            .iter()
            .map(|c| (c.source, c.common_dir.as_path()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("cli", pinned_cli.join(".git").as_path()),
                ("env", pinned_env.join(".git").as_path()),
            ],
            "{got:?}"
        );
        // A window of 48 hours brings the 25-hour-old signal back.
        let got = run(&conn, &fx, Some(&env), 16, 48, NOW);
        assert_eq!(got.len(), 3);
        assert_eq!(got[2].source, "signal");
        assert_eq!(got[2].common_dir, stale.join(".git"));
    }

    #[test]
    fn the_cap_keeps_cli_then_env_then_the_most_recent_signals() {
        let fx = Fx::new();
        let conn = db();
        let mut sig = Vec::new();
        for i in 0..4 {
            let r = fx.init_repo(&format!("s{i}"));
            // s3 is the newest signal, s0 the oldest.
            signal(&conn, &r, "post_tool_use", NOW - (10 - i as i64) * H);
            sig.push(r);
        }
        let env_repo = fx.init_repo("e");
        let cli_repo = fx.init_repo("c");
        store::upsert_repo(
            &conn,
            &cli_repo.join(".git").to_string_lossy(),
            Some(&cli_repo.to_string_lossy()),
            "cli",
            true,
        )
        .unwrap();
        let env = env_repo.to_string_lossy().to_string();

        let got = run(&conn, &fx, Some(&env), 3, 24, NOW);
        let order: Vec<&Path> = got.iter().map(|c| c.common_dir.as_path()).collect();
        assert_eq!(
            order,
            vec![
                cli_repo.join(".git").as_path(),
                env_repo.join(".git").as_path(),
                sig[3].join(".git").as_path(),
            ]
        );
        let got = run(&conn, &fx, Some(&env), 4, 24, NOW);
        assert_eq!(
            got[3].common_dir,
            sig[2].join(".git"),
            "then the next newest"
        );
    }

    #[test]
    fn rows_an_older_binary_registered_from_history_or_the_code_graph_name_no_repo() {
        let fx = Fx::new();
        let conn = db();
        let hist = fx.init_repo("hist");
        let nodes = fx.init_repo("nodes");
        let both = fx.init_repo("both");
        for (r, source) in [
            (&hist, "history"),
            (&nodes, "code_nodes"),
            (&both, "history"),
        ] {
            store::upsert_repo(
                &conn,
                &r.join(".git").to_string_lossy(),
                Some(&r.to_string_lossy()),
                source,
                true,
            )
            .unwrap();
        }
        // The same repo with a fresh signal is a candidate again, as a signal repo.
        signal(&conn, &both, "session_start", NOW - H);
        let got = run(&conn, &fx, None, 16, 24, NOW);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].common_dir, both.join(".git"));
        assert_eq!(got[0].source, "signal");
    }

    #[test]
    fn nothing_reads_history_jsonl_or_the_code_graph_any_more() {
        let fx = Fx::new();
        let conn = db();
        let repo = fx.init_repo("r");
        std::fs::create_dir_all(fx.home.join(".claude")).unwrap();
        std::fs::write(
            fx.home.join(".claude/history.jsonl"),
            format!(
                "{{\"timestamp\":{},\"project\":\"{}\"}}\n",
                NOW * 1000 - 1000,
                repo.display()
            ),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO code_nodes (id, file, kind, name, repo_root)
             VALUES ('n1', ?1, 'function', 'f', ?2)",
            rusqlite::params![repo.join("a.py").to_string_lossy(), repo.to_string_lossy()],
        )
        .unwrap();
        assert!(run(&conn, &fx, None, 16, 24, NOW).is_empty());
    }

    #[test]
    fn temp_roots_are_refused_unless_registered_and_disabled_repos_stay_out() {
        let fx = Fx::new();
        let conn = db();
        let repo = fx.init_repo("r1");
        signal(&conn, &repo, "session_start", NOW - H);
        // The real temp rule: the fixture lives under the system temp dir.
        let real = discover_with(&conn, &fx.home, None, 16, 24, NOW);
        assert!(real.is_empty(), "a signal from a temp root is dropped");
        assert_eq!(status_of(&conn, &repo).0.as_deref(), Some("temp"));

        // The same path in CSR_CODEMAP_REPOS is kept.
        let env = repo.to_string_lossy().to_string();
        let got = discover_with(&conn, &fx.home, Some(&env), 16, 24, NOW);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].source, "env");
        assert_eq!(got[0].common_dir, repo.join(".git"));

        // A cli-registered row survives without env or signal.
        store::upsert_repo(
            &conn,
            &repo.join(".git").to_string_lossy(),
            Some(&repo.to_string_lossy()),
            "cli",
            true,
        )
        .unwrap();
        let got = discover_with(&conn, &fx.home, None, 16, 24, NOW);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].source, "cli");

        // enabled = 0 keeps it out of every source, env and signals included.
        store::disable_repo(&conn, &repo.join(".git").to_string_lossy(), None).unwrap();
        assert!(discover_impl(&conn, &fx.home, Some(&env), 16, 24, NOW, &no_temp).is_empty());
        assert_eq!(status_of(&conn, &repo).0.as_deref(), Some("disabled"));
    }

    #[test]
    fn a_repo_whose_root_holds_the_home_folder_is_dropped_as_home() {
        let fx = Fx::new();
        let conn = db();
        // A repo whose worktree root is an ANCESTOR of the home folder (`/` or
        // `/Users` with a .git): a session started outside $HOME but inside that
        // repo would have the scan walk all of $HOME.
        let top = fx.home.parent().unwrap().to_path_buf();
        fx.git(&top, &["init", "-q", "-b", "main"]);
        signal(&conn, &fx.root, "session_start", NOW - H);
        assert!(run(&conn, &fx, None, 16, 24, NOW).is_empty());
        assert_eq!(status_of(&conn, &fx.root).0.as_deref(), Some("home"));
        let resolved = resolve_signal(&fx.root, &fx.home, &no_temp, &[]);
        assert!(
            matches!(resolved, Resolved::Dropped("home")),
            "{resolved:?}"
        );
    }

    #[test]
    fn a_repo_rooted_at_the_home_folder_is_never_registered() {
        let fx = Fx::new();
        // `git init ~`: `identify_with` stops at $HOME, so the walk up from a
        // subfolder never returns it ("not inside a git repository below $HOME").
        fx.git(&fx.home, &["init", "-q", "-b", "main"]);
        let sub = fx.home.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let resolved = resolve_signal(&sub, &fx.home, &no_temp, &[]);
        assert!(
            matches!(resolved, Resolved::Dropped("not-a-repo")),
            "{resolved:?}"
        );
    }

    #[test]
    fn a_home_rooted_repo_is_reached_through_its_linked_worktree_only_for_that_worktree() {
        use crate::codemap::testutil::{py, write};
        let fx = Fx::new();
        fx.git(&fx.home, &["init", "-q", "-b", "main"]);
        write(&fx.home, "rc.py", &py(&["rc"]));
        fx.commit_all(&fx.home, "init");
        let wt = fx.add_worktree(&fx.home, &fx.home.join("projects/dotfiles"), "dots");
        // The linked worktree is a view; the repo is registered for it alone
        // (`list_views_with` drops the home view, so the scan never walks `$HOME`).
        let resolved = resolve_signal(&wt, &fx.home, &no_temp, &[]);
        assert!(matches!(resolved, Resolved::Repo(_)), "{resolved:?}");
        let listing = crate::codemap::identity::list_views_with(
            &fx.home.join(".git"),
            Some(fx.home.as_path()),
        )
        .unwrap();
        assert_eq!(
            (listing.views.len(), listing.excluded),
            (1, 1),
            "{listing:?}"
        );
        // A linked worktree that is itself `<home>/Library`: nothing to walk.
        let lib = fx.add_worktree(&fx.home, &fx.home.join("Library"), "lib");
        let resolved = resolve_signal(&lib, &fx.home, &no_temp, &[]);
        assert!(
            matches!(resolved, Resolved::Dropped("home")),
            "{resolved:?}"
        );
    }

    #[test]
    fn home_missing_and_non_repo_signals_get_their_own_status_and_map_nothing() {
        let fx = Fx::new();
        let conn = db();
        let plain = fx.root.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let gone = fx.root.join("gone");
        signal(&conn, &fx.home, "session_start", NOW - H);
        signal(&conn, &plain, "session_start", NOW - H);
        signal(&conn, &gone, "session_start", NOW - H);
        assert!(run(&conn, &fx, None, 16, 24, NOW).is_empty());
        assert_eq!(status_of(&conn, &fx.home).0.as_deref(), Some("home"));
        assert_eq!(status_of(&conn, &plain).0.as_deref(), Some("not-a-repo"));
        assert_eq!(status_of(&conn, &gone).0.as_deref(), Some("removed"));
    }

    #[test]
    fn a_second_pass_over_unchanged_signals_writes_nothing_and_old_signals_are_dropped() {
        let fx = Fx::new();
        let conn = db();
        let repo = fx.init_repo("r");
        signal(&conn, &repo, "session_start", NOW - H);
        signal(
            &conn,
            &fx.root.join("ancient"),
            "session_start",
            NOW - 30 * 24 * H,
        );
        run(&conn, &fx, None, 16, 24, NOW);
        let before = conn.total_changes();
        run(&conn, &fx, None, 16, 24, NOW);
        assert_eq!(conn.total_changes(), before, "steady state costs no write");
        let left = store::list_signals(&conn, 0).unwrap();
        assert_eq!(left.len(), 1, "the 30-day-old signal is garbage-collected");
        assert_eq!(left[0].cwd, repo.to_string_lossy());
    }

    #[test]
    fn a_protected_cwd_is_skipped_with_the_protected_status_and_never_followed() {
        let fx = Fx::new();
        let conn = db();
        // A real repo inside a fake ~/Documents: a stat-and-walk would find it.
        let docs = fx.home.join("Documents");
        let proj = docs.join("probe");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        // A symlink chain from an ordinary place into it.
        let hop2 = fx.root.join("hop2");
        let hop1 = fx.root.join("hop1");
        std::os::unix::fs::symlink(&proj, &hop2).unwrap();
        std::os::unix::fs::symlink(&hop2, &hop1).unwrap();
        signal(&conn, &proj, "session_start", NOW - H);
        signal(&conn, &proj.join("src"), "post_tool_use", NOW - H);
        signal(&conn, &hop1, "session_start", NOW - H);
        assert!(run(&conn, &fx, None, 16, 24, NOW).is_empty());
        for cwd in [&proj, &proj.join("src"), &hop1] {
            assert_eq!(
                status_of(&conn, cwd),
                (Some("protected".into()), None),
                "{}",
                cwd.display()
            );
        }
    }

    #[test]
    fn the_daemon_resolves_a_protected_cwd_without_a_filesystem_call_under_it() {
        use std::os::unix::fs::PermissionsExt;
        let fx = Fx::new();
        let conn = db();
        let docs = fx.home.join("Documents");
        std::fs::create_dir_all(docs.join("probe/.git")).unwrap();
        // Mode 000: any stat, open or read_dir of an entry below it fails with
        // EACCES. A resolver that touched the path would classify the cwd as
        // `removed` or `not-a-repo` from that error; the lexical rule answers
        // `protected` first. (This shows the classification does not depend on
        // the filesystem; it cannot observe a stat that discards its result.)
        std::fs::set_permissions(&docs, std::fs::Permissions::from_mode(0o000)).unwrap();
        let cwd = docs.join("probe/src");
        signal(&conn, &cwd, "post_tool_use", NOW - H);
        let got = run(&conn, &fx, None, 16, 24, NOW);
        std::fs::set_permissions(&docs, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(got.is_empty());
        assert_eq!(status_of(&conn, &cwd).0.as_deref(), Some("protected"));
    }

    fn register(conn: &Connection, common: &str, source: &str) {
        store::upsert_repo(conn, common, None, source, true).unwrap();
    }

    fn signalled(conn: &Connection, common: &str, source: &str, at: i64) {
        register(conn, common, source);
        let cwd = common.trim_end_matches("/.git");
        store::signal_upsert(conn, cwd, "session_start", at).unwrap();
        store::signal_resolve(conn, cwd, Some(common), "repo").unwrap();
    }

    fn commons(repos: &[StoredRepo]) -> Vec<&str> {
        repos.iter().map(|r| r.common_dir.as_str()).collect()
    }

    #[test]
    fn selection_splits_active_idle_capped_protected_and_unregistered_repos() {
        let conn = db();
        let home = Path::new("/Users/nobody");
        // Pinned repos never idle; a signal in the window activates a signal row
        // or a legacy row; one past the window is idle; the cap orders by pin, then
        // newest signal.
        register(&conn, "/w/cli/.git", "cli");
        register(&conn, "/w/env/.git", "env");
        signalled(&conn, "/w/new/.git", "signal", NOW - H);
        signalled(&conn, "/w/mid/.git", "history", NOW - 5 * H);
        signalled(&conn, "/w/old/.git", "signal", NOW - 30 * H);
        register(&conn, "/w/legacy/.git", "code_nodes");
        register(&conn, "/Users/nobody/Documents/p/.git", "cli");
        // A signal for a repo with no row (discovery's cap kept it out).
        store::signal_upsert(&conn, "/w/lost", "session_start", NOW - 2 * H).unwrap();
        store::signal_resolve(&conn, "/w/lost", Some("/w/lost/.git"), "repo").unwrap();

        let sel = select_repos(&conn, home, NOW, 24, 16).unwrap();
        assert_eq!(
            commons(&sel.active),
            ["/w/cli/.git", "/w/env/.git", "/w/new/.git", "/w/mid/.git"]
        );
        let idle: Vec<(&str, Option<i64>)> = sel
            .idle
            .iter()
            .map(|i| (i.repo.common_dir.as_str(), i.last_signal))
            .collect();
        assert_eq!(
            idle,
            [
                ("/w/old/.git", Some(NOW - 30 * H)),
                ("/w/legacy/.git", None)
            ]
        );
        assert_eq!(commons(&sel.protected), ["/Users/nobody/Documents/p/.git"]);
        assert_eq!(
            sel.unregistered,
            [("/w/lost/.git".to_string(), NOW - 2 * H)]
        );
        assert!(sel.over_cap.is_empty());

        // Cap 3: pinned first, then the newest signal; the rest are over the cap.
        let sel = select_repos(&conn, home, NOW, 24, 3).unwrap();
        assert_eq!(
            commons(&sel.active),
            ["/w/cli/.git", "/w/env/.git", "/w/new/.git"]
        );
        assert_eq!(commons(&sel.over_cap), ["/w/mid/.git"]);

        // A 48-hour window turns the 30-hour-old repo active.
        let sel = select_repos(&conn, home, NOW, 48, 16).unwrap();
        assert!(commons(&sel.active).contains(&"/w/old/.git"));
        // A disabled row is in none of the lists.
        store::disable_repo(&conn, "/w/new/.git", None).unwrap();
        let sel = select_repos(&conn, home, NOW, 24, 16).unwrap();
        assert!(!commons(&sel.active).contains(&"/w/new/.git"));
        assert!(sel.idle.iter().all(|i| i.repo.common_dir != "/w/new/.git"));
    }

    #[test]
    fn selection_reads_no_filesystem_and_survives_a_database_without_signals() {
        let conn = db();
        register(&conn, "/nonexistent/x/.git", "cli");
        conn.execute_batch("DROP TABLE codemap_signals").unwrap();
        let sel = select_repos(&conn, Path::new("/Users/nobody"), NOW, 24, 16).unwrap();
        assert_eq!(commons(&sel.active), ["/nonexistent/x/.git"]);
    }

    #[test]
    fn active_hours_reads_a_positive_number_and_defaults_to_a_day() {
        assert_eq!(active_hours_from_value(None), 24);
        assert_eq!(active_hours_from_value(Some("")), 24);
        assert_eq!(active_hours_from_value(Some("0")), 24);
        assert_eq!(active_hours_from_value(Some("nope")), 24);
        assert_eq!(active_hours_from_value(Some(" 72 ")), 72);
    }
}
