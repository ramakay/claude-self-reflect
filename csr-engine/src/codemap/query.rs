//! Read side: keyword search over the map, grouped by (repo, path, name), with
//! per-view presence by blob.
//!
//! A definition that sits in the same file of five worktrees is one hit, not
//! five. Its presence list says which views hold the same blob (byte-identical
//! file) as the primary and which hold a different one.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Serialize;

use super::extractor_version;

/// One view that holds the hit's file.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ViewPresence {
    /// Short label: `main` for the main worktree, else the worktree directory name.
    pub view: String,
    pub worktree_path: String,
    pub branch: Option<String>,
    pub blob_oid: String,
    /// True when this view holds the same blob as the hit's primary version.
    pub same: bool,
    /// 0 = matches the index, 1 = modified in the worktree, 2 = untracked.
    pub dirty: i64,
}

/// One definition, grouped across the views that hold it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CodemapHit {
    /// The repo's git common dir.
    pub repo: String,
    /// Repo-relative path.
    pub path: String,
    pub name: String,
    pub kind: String,
    pub fqname: Option<String>,
    pub signature: String,
    /// 0-based line numbers (the extractor's convention, as in `code_nodes`).
    pub span_start: i64,
    pub span_end: i64,
    /// The blob the span and signature come from: the one most views hold.
    pub primary_blob: String,
    /// Labels of views holding the primary blob.
    pub same_in: Vec<String>,
    /// Labels of views holding a different blob of this path.
    pub differs_in: Vec<String>,
    pub presence: Vec<ViewPresence>,
}

impl CodemapHit {
    /// One line: `Name  path:63-120  same blob in a, b; differs in c`.
    pub fn render(&self) -> String {
        let mut presence = Vec::new();
        if !self.same_in.is_empty() {
            presence.push(format!("same blob in {}", self.same_in.join(", ")));
        }
        if !self.differs_in.is_empty() {
            presence.push(format!("differs in {}", self.differs_in.join(", ")));
        }
        format!(
            "{}  {}:{}-{}  {}",
            self.name,
            self.path,
            self.span_start + 1,
            self.span_end + 1,
            presence.join("; ")
        )
    }
}

/// FTS5 expression: each alphanumeric run becomes a prefix term, all required.
fn fts_expression(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

struct Holder {
    common_dir: String,
    worktree_path: String,
    branch: Option<String>,
    blob_oid: String,
    dirty: i64,
}

fn holders_of_path(conn: &Connection, path: &str) -> Result<Vec<Holder>> {
    let mut stmt = conn.prepare_cached(
        "SELECT v.common_dir, v.worktree_path, v.branch, f.blob_oid, f.dirty
         FROM codemap_view_files f JOIN codemap_views v ON v.id = f.view_id
         WHERE f.path = ?1 ORDER BY v.worktree_path",
    )?;
    let rows = stmt
        .query_map(params![path], |r| {
            Ok(Holder {
                common_dir: r.get(0)?,
                worktree_path: r.get(1)?,
                branch: r.get(2)?,
                blob_oid: r.get(3)?,
                dirty: r.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn view_label(common_dir: &str, worktree_path: &str) -> String {
    let main = std::path::Path::new(common_dir)
        .file_name()
        .is_some_and(|n| n == ".git")
        .then(|| std::path::Path::new(common_dir).parent())
        .flatten();
    if main.is_some_and(|m| m == std::path::Path::new(worktree_path)) {
        return "main".to_string();
    }
    std::path::Path::new(worktree_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| worktree_path.to_string())
}

/// Raw matches: (name, signature, path, blob), best first.
fn raw_matches(
    conn: &Connection,
    text: &str,
    limit: usize,
) -> Result<Vec<(String, String, String, String)>> {
    let mut out = Vec::new();
    if let Some(expr) = fts_expression(text) {
        let mut stmt = conn.prepare(
            "SELECT name, signature, path, blob_oid FROM codemap_fts
             WHERE codemap_fts MATCH ?1 ORDER BY bm25(codemap_fts) LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![expr, (limit * 8).clamp(50, 4000) as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        for row in rows {
            out.push(row?);
        }
    }
    // Mid-word fragments ("Card" in StationCard) are not FTS prefixes.
    let needle: String = text.trim().to_string();
    if !needle.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT d.name, COALESCE(d.signature, ''), f.path, d.blob_oid
             FROM codemap_defs d JOIN codemap_view_files f ON f.blob_oid = d.blob_oid
             WHERE d.extractor_version = ?1 AND d.name LIKE ?2 ESCAPE '\\'
             GROUP BY d.blob_oid, d.name, f.path LIMIT ?3",
        )?;
        let escaped = needle
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let rows = stmt.query_map(
            params![
                extractor_version(),
                format!("%{escaped}%"),
                (limit * 8) as i64
            ],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        for row in rows {
            out.push(row?);
        }
    }
    Ok(out)
}

/// Search the map. Returns at most `limit` definitions, best match first.
pub fn query(conn: &Connection, text: &str, limit: usize) -> Result<Vec<CodemapHit>> {
    if !super::store::tables_ready(conn) || limit == 0 {
        return Ok(Vec::new());
    }
    // (common_dir, path, name) -> blobs the matches named, in first-seen order.
    let mut order: Vec<(String, String, String)> = Vec::new();
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    let mut holders_cache: HashMap<String, Vec<Holder>> = HashMap::new();
    for (name, _signature, path, blob) in raw_matches(conn, text, limit)? {
        if !holders_cache.contains_key(&path) {
            holders_cache.insert(path.clone(), holders_of_path(conn, &path)?);
        }
        let mut commons: Vec<&str> = holders_cache[&path]
            .iter()
            .filter(|h| h.blob_oid == blob)
            .map(|h| h.common_dir.as_str())
            .collect();
        commons.sort_unstable();
        commons.dedup();
        for common in commons {
            let key = (common.to_string(), path.clone(), name.clone());
            if seen.insert(key.clone()) {
                order.push(key);
            }
        }
    }

    let mut hits = Vec::new();
    for key in order {
        if hits.len() >= limit {
            break;
        }
        let (common, path, name) = key;
        let holders: Vec<&Holder> = holders_cache[&path]
            .iter()
            .filter(|h| h.common_dir == common)
            .collect();
        // Primary blob: the one the most views hold; ties go to the first view.
        let mut votes: BTreeMap<&str, usize> = BTreeMap::new();
        for h in &holders {
            *votes.entry(h.blob_oid.as_str()).or_insert(0) += 1;
        }
        let best = votes.values().copied().max().unwrap_or(0);
        let Some(primary) = holders
            .iter()
            .find(|h| votes[h.blob_oid.as_str()] == best && has_def(conn, &h.blob_oid, &name))
            .or_else(|| holders.iter().find(|h| has_def(conn, &h.blob_oid, &name)))
            .map(|h| h.blob_oid.clone())
        else {
            continue;
        };
        let def = conn.query_row(
            "SELECT kind, fqname, COALESCE(signature, ''), span_start, span_end
             FROM codemap_defs WHERE blob_oid = ?1 AND name = ?2 AND extractor_version = ?3
             ORDER BY span_start LIMIT 1",
            params![primary, name, extractor_version()],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            },
        )?;
        let presence: Vec<ViewPresence> = holders
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
        let label = |p: &ViewPresence| match p.dirty {
            1 => format!("{}*", p.view),
            2 => format!("{}+", p.view),
            _ => p.view.clone(),
        };
        hits.push(CodemapHit {
            repo: common,
            path,
            name,
            kind: def.0,
            fqname: def.1,
            signature: def.2,
            span_start: def.3,
            span_end: def.4,
            same_in: presence.iter().filter(|p| p.same).map(label).collect(),
            differs_in: presence.iter().filter(|p| !p.same).map(label).collect(),
            primary_blob: primary,
            presence,
        });
    }
    Ok(hits)
}

fn has_def(conn: &Connection, blob: &str, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM codemap_defs WHERE blob_oid = ?1 AND name = ?2 AND extractor_version = ?3 LIMIT 1",
        params![blob, name, extractor_version()],
        |_| Ok(()),
    )
    .is_ok()
}
