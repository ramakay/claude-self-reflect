//! Which repos to map.
//!
//! Sources, in priority order:
//!
//! 1. enabled rows of `codemap_repos`, ordered by how they were registered:
//!    `cli`, `env`, `code_nodes`, `history`;
//! 2. `CSR_CODEMAP_REPOS` (`:`-separated paths);
//! 3. distinct `code_nodes.repo_root` values;
//! 4. distinct `project` paths in `<home>/.claude/history.jsonl` lines from the
//!    last 30 days (`session_registry` only keeps basenames).
//!
//! Every path is mapped to its git common dir and deduplicated. Dropped: paths
//! that are missing or not in git, `$HOME` itself, protected folders, temp roots
//! (unless registered by `cli` or `env`), and any repo with an `enabled = 0` row
//! (`codemap remove` leaves that tombstone so discovery never re-adds it). The
//! result is capped at `CSR_CODEMAP_MAX_REPOS` (default 16), keeping priority
//! order and then most recent activity.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Deserialize;

use super::identity::{identify_with, is_tcc_path, is_temp_path, normalize_lexical};
use super::store;

pub const DEFAULT_MAX_REPOS: usize = 16;
const HISTORY_WINDOW_MS: i64 = 30 * 24 * 3600 * 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub common_dir: PathBuf,
    /// The main worktree, when the common dir is a normal `<main>/.git`.
    pub main_path: Option<PathBuf>,
    /// cli | env | code_nodes | history
    pub source: &'static str,
    /// Latest session activity (ms since epoch) seen for the repo, 0 if none.
    pub recent_ts: i64,
}

fn tier(source: &str) -> u8 {
    match source {
        "cli" => 0,
        "env" => 1,
        "code_nodes" => 2,
        _ => 3,
    }
}

fn static_source(source: &str) -> &'static str {
    match source {
        "cli" => "cli",
        "env" => "env",
        "code_nodes" => "code_nodes",
        _ => "history",
    }
}

/// Discover the repos to map, reading `CSR_CODEMAP_REPOS` and
/// `CSR_CODEMAP_MAX_REPOS` from the environment.
pub fn discover(conn: &Connection, home: &Path) -> Vec<Candidate> {
    let env_repos = std::env::var("CSR_CODEMAP_REPOS").ok();
    let max = std::env::var("CSR_CODEMAP_MAX_REPOS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_REPOS);
    let now_ms = super::now_secs() * 1000;
    discover_with(conn, home, env_repos.as_deref(), max, now_ms)
}

struct Input {
    source: &'static str,
    /// Registered through `cli` or `env`: exempt from the temp-root rule.
    exempt_from_temp: bool,
    /// A stored repo row already names the common dir.
    path: PathBuf,
    is_common_dir: bool,
    ts: i64,
}

/// `discover` with its environment inputs passed in (testable).
pub fn discover_with(
    conn: &Connection,
    home: &Path,
    env_repos: Option<&str>,
    max_repos: usize,
    now_ms: i64,
) -> Vec<Candidate> {
    let mut inputs: Vec<Input> = Vec::new();
    let mut tombstones: Vec<PathBuf> = Vec::new();

    if store::tables_ready(conn) {
        if let Ok(rows) = store::list_repos(conn) {
            for r in rows {
                let common = PathBuf::from(&r.common_dir);
                if !r.enabled {
                    tombstones.push(common);
                    continue;
                }
                let source = static_source(&r.source);
                inputs.push(Input {
                    source,
                    exempt_from_temp: matches!(source, "cli" | "env"),
                    path: common,
                    is_common_dir: true,
                    ts: 0,
                });
            }
        }
    }
    for p in env_repos.unwrap_or("").split(':').filter(|p| !p.is_empty()) {
        inputs.push(Input {
            source: "env",
            exempt_from_temp: true,
            path: PathBuf::from(p),
            is_common_dir: false,
            ts: 0,
        });
    }
    if let Ok(mut stmt) = conn.prepare(
        "SELECT DISTINCT repo_root FROM code_nodes
         WHERE repo_root IS NOT NULL AND repo_root != ''",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            for p in rows.flatten() {
                inputs.push(Input {
                    source: "code_nodes",
                    exempt_from_temp: false,
                    path: PathBuf::from(p),
                    is_common_dir: false,
                    ts: 0,
                });
            }
        }
    }
    for (project, ts) in history_projects(home, now_ms) {
        inputs.push(Input {
            source: "history",
            exempt_from_temp: false,
            path: PathBuf::from(project),
            is_common_dir: false,
            ts,
        });
    }

    // Activity per repo from history, whichever source named it.
    let mut by_common: HashMap<PathBuf, Candidate> = HashMap::new();
    let mut resolved: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    let mut activity: HashMap<PathBuf, i64> = HashMap::new();
    let home_opt = Some(home);
    for input in &inputs {
        let path = normalize_lexical(&input.path);
        if is_tcc_path(&path, home_opt) {
            continue;
        }
        let common = if input.is_common_dir {
            if !path.is_dir() {
                continue;
            }
            Some(path.clone())
        } else {
            resolved
                .entry(path.clone())
                .or_insert_with(|| identify_with(&path, home_opt).ok().map(|id| id.common_dir))
                .clone()
        };
        let Some(common) = common else { continue };
        if is_tcc_path(&common, home_opt) {
            continue;
        }
        // Activity counts even when this input is later dropped as a temp
        // path: a registered repo still shows when it was last worked in.
        let seen = activity.entry(common.clone()).or_insert(0);
        *seen = (*seen).max(input.ts);
        if !input.exempt_from_temp && (is_temp_path(&path) || is_temp_path(&common)) {
            continue;
        }
        if tombstones.contains(&common) {
            continue;
        }
        let main_path = (common.file_name().is_some_and(|n| n == ".git"))
            .then(|| common.parent().map(Path::to_path_buf))
            .flatten();
        by_common
            .entry(common.clone())
            .and_modify(|c| {
                if tier(input.source) < tier(c.source) {
                    c.source = input.source;
                }
            })
            .or_insert(Candidate {
                common_dir: common,
                main_path,
                source: input.source,
                recent_ts: input.ts,
            });
    }

    let mut out: Vec<Candidate> = by_common
        .into_values()
        .map(|mut c| {
            c.recent_ts = activity.get(&c.common_dir).copied().unwrap_or(0);
            c
        })
        .collect();
    out.sort_by(|a, b| {
        tier(a.source)
            .cmp(&tier(b.source))
            .then(b.recent_ts.cmp(&a.recent_ts))
            .then(a.common_dir.cmp(&b.common_dir))
    });
    out.truncate(max_repos);
    out
}

#[derive(Deserialize)]
struct HistoryLine {
    timestamp: Option<i64>,
    project: Option<String>,
}

/// Distinct `project` paths from the last 30 days of `history.jsonl`, with the
/// latest timestamp for each.
fn history_projects(home: &Path, now_ms: i64) -> Vec<(String, i64)> {
    let path = home.join(".claude").join("history.jsonl");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let cutoff = now_ms - HISTORY_WINDOW_MS;
    let mut latest: HashMap<String, i64> = HashMap::new();
    for line in text.lines() {
        let Ok(h) = serde_json::from_str::<HistoryLine>(line) else {
            continue;
        };
        let (Some(ts), Some(project)) = (h.timestamp, h.project) else {
            continue;
        };
        if ts < cutoff || project.is_empty() {
            continue;
        }
        let e = latest.entry(project).or_insert(ts);
        *e = (*e).max(ts);
    }
    latest.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        conn
    }

    fn git_repo(dir: &Path) {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
    }

    #[test]
    fn temp_root_is_refused_unless_registered_and_disabled_rows_stay_out() {
        let conn = db();
        let home = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("r1");
        git_repo(&repo);
        let canon = std::fs::canonicalize(&repo).unwrap();
        let now_ms = 1_800_000_000_000;

        // A history hit in a temp root is dropped.
        std::fs::create_dir_all(home.path().join(".claude")).unwrap();
        std::fs::write(
            home.path().join(".claude/history.jsonl"),
            format!(
                "{{\"timestamp\":{},\"project\":\"{}\"}}\n",
                now_ms - 1000,
                canon.display()
            ),
        )
        .unwrap();
        assert!(discover_with(&conn, home.path(), None, 16, now_ms).is_empty());

        // The same path in CSR_CODEMAP_REPOS is kept.
        let env = canon.to_string_lossy().to_string();
        let got = discover_with(&conn, home.path(), Some(&env), 16, now_ms);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].source, "env");
        assert_eq!(got[0].common_dir, canon.join(".git"));
        assert!(got[0].recent_ts > 0, "history activity is merged in");

        // A cli-registered row survives without env or history.
        std::fs::remove_file(home.path().join(".claude/history.jsonl")).unwrap();
        store::upsert_repo(
            &conn,
            &canon.join(".git").to_string_lossy(),
            Some(&canon.to_string_lossy()),
            "cli",
            true,
        )
        .unwrap();
        let got = discover_with(&conn, home.path(), None, 16, now_ms);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].source, "cli");

        // enabled = 0 keeps it out of every source, env included.
        store::disable_repo(&conn, &canon.join(".git").to_string_lossy(), None).unwrap();
        assert!(discover_with(&conn, home.path(), Some(&env), 16, now_ms).is_empty());
    }

    #[test]
    fn stale_history_protected_folders_and_home_are_dropped_and_the_cap_applies() {
        let conn = db();
        let home = tempfile::tempdir().unwrap();
        let now_ms = 1_800_000_000_000;
        // Repos outside temp roots are needed; /Users/... style paths under a
        // fake home would be temp too, so exercise the filters on input paths.
        let lines = [
            (
                now_ms - 40 * 24 * 3600 * 1000,
                "/Users/nobody-here/projects/old",
            ),
            (now_ms - 1000, "/Users/nobody-here/Desktop/proj"),
            (now_ms - 1000, "/Users/nobody-here/Documents/proj"),
        ];
        std::fs::create_dir_all(home.path().join(".claude")).unwrap();
        let body: String = lines
            .iter()
            .map(|(ts, p)| format!("{{\"timestamp\":{ts},\"project\":\"{p}\"}}\n"))
            .collect();
        std::fs::write(home.path().join(".claude/history.jsonl"), body).unwrap();
        assert!(discover_with(&conn, home.path(), None, 16, now_ms).is_empty());

        let tmp = tempfile::tempdir().unwrap();
        let mut env = Vec::new();
        for i in 0..5 {
            let r = tmp.path().join(format!("r{i}"));
            git_repo(&r);
            env.push(
                std::fs::canonicalize(&r)
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
            );
        }
        let got = discover_with(&conn, home.path(), Some(&env.join(":")), 3, now_ms);
        assert_eq!(got.len(), 3, "CSR_CODEMAP_MAX_REPOS caps the list");
    }
}
