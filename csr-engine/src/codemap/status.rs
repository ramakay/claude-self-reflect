//! Read-only report of what the code map holds. `csr-engine codemap status`
//! prints it; `csr-engine status` reuses `status_report`.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

use super::store;

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
    /// cli | env | code_nodes | history | unregistered
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

#[derive(Debug, Clone, Serialize)]
pub struct CodemapStatus {
    /// False when the code map tables do not exist yet.
    pub initialised: bool,
    pub disabled: bool,
    pub extractor_version: String,
    pub repos: Vec<RepoStatus>,
    pub totals: StatusTotals,
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0)
}

/// Everything the code map holds, per repo and view, plus totals.
pub fn status_report(conn: &Connection) -> Result<CodemapStatus> {
    let mut out = CodemapStatus {
        initialised: store::tables_ready(conn),
        disabled: super::disabled(),
        extractor_version: super::extractor_version().to_string(),
        repos: Vec::new(),
        totals: StatusTotals::default(),
    };
    if !out.initialised {
        return Ok(out);
    }
    let now = super::now_secs();
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
