//! Read side, by path: which views hold a file, which blobs they hold, and the
//! definitions of the blob most views share.
//!
//! `csr_search_by_file` and `csr_code_graph` (with a `file` argument) resolve
//! their argument here. Read-only; every function answers "nothing" when the
//! code map tables do not exist.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;

use super::extractor_version;
use super::query::{holders_of_path, view_label, ViewPresence};

/// One definition of a file's primary blob.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FileDef {
    pub name: String,
    pub kind: String,
    /// 0-based line numbers (the extractor's convention).
    pub span_start: i64,
    pub span_end: i64,
}

/// One repo-relative path and what the map knows of it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CodemapFile {
    /// The repo's git common dir.
    pub repo: String,
    /// Repo-relative path.
    pub path: String,
    /// The blob most views hold (ties go to the main worktree).
    pub primary_blob: String,
    /// Every view holding the path, `same` meaning the primary blob. The main
    /// worktree first, then by worktree path.
    pub presence: Vec<ViewPresence>,
    /// Definitions of the primary blob in file order (at most [`MAX_FILE_DEFS`]).
    pub defs: Vec<FileDef>,
    /// How many definitions the primary blob has in all.
    pub defs_total: usize,
}

impl CodemapFile {
    /// Views holding this path.
    pub fn view_count(&self) -> usize {
        self.presence.len()
    }
}

/// What [`lookup_files`] found: at most `limit` files, and how many paths matched.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct FilesLookup {
    pub files: Vec<CodemapFile>,
    /// Matching (repo, path) pairs before the cap.
    pub total_paths: usize,
}

/// Definitions kept per file. The renderers trim further to their byte budget.
pub const MAX_FILE_DEFS: usize = 40;

/// Short repo name for output: the checkout directory (`anukriti` for
/// `/Users/x/anukriti/.git`).
pub fn repo_label(common_dir: &str) -> String {
    let path = std::path::Path::new(common_dir);
    let named = if path.file_name().is_some_and(|n| n == ".git") {
        path.parent()
    } else {
        Some(path)
    };
    named
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| common_dir.to_string())
}

fn view_tag(p: &ViewPresence) -> String {
    match p.dirty {
        1 => format!("{} (dirty)", p.view),
        2 => format!("{} (untracked)", p.view),
        _ => p.view.clone(),
    }
}

/// Views named per group before `+N more`.
const MAX_VIEWS_LISTED: usize = 6;

fn list_views(views: &[&ViewPresence]) -> String {
    let mut names: Vec<String> = views
        .iter()
        .take(MAX_VIEWS_LISTED)
        .map(|p| view_tag(p))
        .collect();
    if views.len() > MAX_VIEWS_LISTED {
        names.push(format!("+{} more", views.len() - MAX_VIEWS_LISTED));
    }
    names.join(", ")
}

/// One line saying which views hold the same blob and which differ:
/// `same blob in main, a; differs in b (dirty)`. Several differing blobs are
/// listed as separate `differs in` groups, each tagged with its blob prefix.
pub fn presence_summary(presence: &[ViewPresence]) -> String {
    let same: Vec<&ViewPresence> = presence.iter().filter(|p| p.same).collect();
    let mut groups: Vec<(&str, Vec<&ViewPresence>)> = Vec::new();
    for p in presence.iter().filter(|p| !p.same) {
        match groups.iter_mut().find(|(blob, _)| *blob == p.blob_oid) {
            Some((_, views)) => views.push(p),
            None => groups.push((p.blob_oid.as_str(), vec![p])),
        }
    }
    let mut parts = Vec::new();
    if !same.is_empty() {
        parts.push(if groups.is_empty() && same.len() == 1 {
            format!("only in {}", list_views(&same))
        } else {
            format!("same blob in {}", list_views(&same))
        });
    }
    let multi = groups.len() > 1;
    for (blob, views) in &groups {
        let tag = if multi {
            format!(" [{}]", blob.chars().take(7).collect::<String>())
        } else {
            String::new()
        };
        parts.push(format!("differs in {}{tag}", list_views(views)));
    }
    parts.join("; ")
}

type PathPairs = (Vec<(String, String)>, usize);

/// Resolve `arg` to (common_dir, repo-relative path) pairs, at most `cap`, and
/// how many matched.
///
/// - an absolute path: the view whose worktree path is its longest prefix (on a
///   `/` boundary) and that holds the remainder gives the repo and the path;
/// - else an exact repo-relative path (a leading `./` is ignored);
/// - else a suffix: paths ending in `/<arg>`.
fn resolve_paths(
    conn: &Connection,
    arg: &str,
    cap: usize,
    scope: Option<&RepoScope>,
) -> Result<PathPairs> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Ok((Vec::new(), 0));
    }
    if arg.starts_with('/') {
        return resolve_absolute(conn, arg, scope);
    }

    let rel = arg.trim_start_matches("./").trim_end_matches('/');
    if rel.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let mut exact = conn.prepare_cached(
        "SELECT DISTINCT v.common_dir FROM codemap_view_files f
         JOIN codemap_views v ON v.id = f.view_id WHERE f.path = ?1 ORDER BY v.common_dir",
    )?;
    let commons = exact
        .query_map(params![rel], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|c| in_scope(scope, c))
        .collect::<Vec<_>>();
    if !commons.is_empty() {
        let total = commons.len();
        let pairs = commons
            .into_iter()
            .take(cap)
            .map(|c| (c, rel.to_string()))
            .collect();
        return Ok((pairs, total));
    }

    // LIKE is ASCII-case-insensitive; it narrows, `ends_with` below decides.
    let escaped = rel
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let mut suffix = conn.prepare_cached(
        "SELECT DISTINCT v.common_dir, f.path FROM codemap_view_files f
         JOIN codemap_views v ON v.id = f.view_id
         WHERE f.path LIKE ?1 ESCAPE '\\' ORDER BY f.path, v.common_dir",
    )?;
    let needle = format!("/{rel}");
    let all: Vec<(String, String)> = suffix
        .query_map(params![format!("%/{escaped}")], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|(common, path)| path.ends_with(&needle) && in_scope(scope, common))
        .collect();
    let total = all.len();
    Ok((all.into_iter().take(cap).collect(), total))
}

fn resolve_absolute(conn: &Connection, arg: &str, scope: Option<&RepoScope>) -> Result<PathPairs> {
    let abs = super::identity::normalize_lexical(std::path::Path::new(arg));
    let abs = abs.to_string_lossy().to_string();
    let mut stmt = conn.prepare_cached("SELECT common_dir, worktree_path FROM codemap_views")?;
    let mut views = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    // Longest worktree first: a nested worktree beats the checkout around it.
    views.sort_by_key(|(_, wt)| std::cmp::Reverse(wt.len()));
    for (common, wt) in views {
        if !in_scope(scope, &common) {
            continue;
        }
        let Some(rel) = abs
            .strip_prefix(wt.trim_end_matches('/'))
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|rel| !rel.is_empty())
        else {
            continue;
        };
        let exists = conn
            .query_row(
                "SELECT 1 FROM codemap_view_files f JOIN codemap_views v ON v.id = f.view_id
                 WHERE v.common_dir = ?1 AND f.path = ?2 LIMIT 1",
                params![common, rel],
                |_| Ok(()),
            )
            .is_ok();
        if exists {
            return Ok((vec![(common, rel.to_string())], 1));
        }
    }
    Ok((Vec::new(), 0))
}

fn file_for(conn: &Connection, common: &str, path: &str) -> Result<Option<CodemapFile>> {
    let mut holders: Vec<_> = holders_of_path(conn, path)?
        .into_iter()
        .filter(|h| h.common_dir == common)
        .collect();
    if holders.is_empty() {
        return Ok(None);
    }
    // The main worktree first (stable sort keeps worktree-path order after it),
    // so a tie in votes goes to it.
    holders.sort_by_key(|h| view_label(&h.common_dir, &h.worktree_path) != "main");
    let mut votes: BTreeMap<&str, usize> = BTreeMap::new();
    for h in &holders {
        *votes.entry(h.blob_oid.as_str()).or_insert(0) += 1;
    }
    let best = votes.values().copied().max().unwrap_or(0);
    let primary = holders
        .iter()
        .find(|h| votes[h.blob_oid.as_str()] == best)
        .map(|h| h.blob_oid.clone())
        .unwrap_or_default();
    let defs_total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM codemap_defs WHERE blob_oid = ?1 AND extractor_version = ?2",
        params![primary, extractor_version()],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare_cached(
        "SELECT name, kind, span_start, span_end FROM codemap_defs
         WHERE blob_oid = ?1 AND extractor_version = ?2
         ORDER BY span_start, span_end, name LIMIT ?3",
    )?;
    let defs = stmt
        .query_map(
            params![primary, extractor_version(), MAX_FILE_DEFS as i64],
            |r| {
                Ok(FileDef {
                    name: r.get(0)?,
                    kind: r.get(1)?,
                    span_start: r.get(2)?,
                    span_end: r.get(3)?,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let presence = holders
        .iter()
        .map(|h| ViewPresence {
            view: view_label(&h.common_dir, &h.worktree_path),
            worktree_path: h.worktree_path.clone(),
            branch: h.branch.clone(),
            blob_oid: h.blob_oid.clone(),
            same: h.blob_oid == primary,
            dirty: h.dirty,
        })
        .collect();
    Ok(Some(CodemapFile {
        repo: common.to_string(),
        path: path.to_string(),
        primary_blob: primary,
        presence,
        defs,
        defs_total: defs_total.max(0) as usize,
    }))
}

/// Resolve `arg` against the map's paths and return up to `limit` files with
/// their per-view presence, plus how many paths matched. Read-only.
pub fn lookup_files(conn: &Connection, arg: &str, limit: usize) -> Result<FilesLookup> {
    lookup_files_scoped(conn, arg, limit, None)
}

/// The common dirs (the repo identity) of the repos a project owns. See
/// [`repos_matching`].
pub type RepoScope = BTreeSet<String>;

pub(super) fn in_scope(scope: Option<&RepoScope>, common_dir: &str) -> bool {
    scope.is_none_or(|repos| repos.contains(common_dir))
}

/// The repos with a label that satisfies `belongs`. A repo's labels are its
/// checkout directory name ([`repo_label`]) and the directory name of each of
/// its worktrees, so a project named after a linked worktree finds its repo.
/// Empty when the code map tables do not exist.
pub fn repos_matching(conn: &Connection, belongs: &dyn Fn(&str) -> bool) -> Result<RepoScope> {
    let mut found = RepoScope::new();
    if !super::store::tables_ready(conn) {
        return Ok(found);
    }
    let mut stmt = conn.prepare_cached("SELECT common_dir, worktree_path FROM codemap_views")?;
    let views = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (common, worktree) in views {
        if found.contains(&common) {
            continue;
        }
        let worktree_name = std::path::Path::new(&worktree)
            .file_name()
            .map(|n| n.to_string_lossy().to_string());
        if belongs(&repo_label(&common)) || worktree_name.is_some_and(|n| belongs(&n)) {
            found.insert(common);
        }
    }
    Ok(found)
}

/// [`lookup_files`] limited to the repos in `scope` (`None` = every repo). The
/// scope applies before the cap, so a large unrelated repo cannot crowd out the
/// project's own paths.
pub fn lookup_files_scoped(
    conn: &Connection,
    arg: &str,
    limit: usize,
    scope: Option<&RepoScope>,
) -> Result<FilesLookup> {
    if !super::store::tables_ready(conn) || limit == 0 {
        return Ok(FilesLookup::default());
    }
    let (pairs, total_paths) = resolve_paths(conn, arg, limit, scope)?;
    let mut files = Vec::new();
    for (common, path) in pairs {
        if let Some(file) = file_for(conn, &common, &path)? {
            files.push(file);
        }
    }
    Ok(FilesLookup { files, total_paths })
}

/// [`lookup_files`] without the match count.
pub fn files_for(conn: &Connection, arg: &str, limit: usize) -> Result<Vec<CodemapFile>> {
    Ok(lookup_files(conn, arg, limit)?.files)
}
