//! Code-property-graph storage (v9.4).
//!
//! Conversation-provenance code graph: nodes (symbols), edges (calls/defines/
//! imports), per-file extraction state, and degree-based rank. Every node and
//! edge carries `conv_id` / `session_id` provenance — that is the moat.
//!
//! Schema lives in `migrations.rs` (tables `code_nodes`, `code_edges`,
//! `code_graph_file_state`, `code_node_rank`). This module owns the queries.

use anyhow::Result;
use rusqlite::{params, Connection};

/// A graph node row (a symbol seen in code). Used for both writes and reads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeRow {
    pub id: String,
    pub repo: String,
    pub project: String,
    pub file: String,
    pub lang: String,
    /// module | function | type | import (canonical)
    pub kind: String,
    pub name: String,
    pub fqname: String,
    pub body_hash: String,
    pub span_start: i64,
    pub span_end: i64,
    pub first_conv_id: String,
    pub last_conv_id: String,
    pub last_session_id: String,
    /// true when this row is an unverified name-collision guess (matched via
    /// the `name:<bare>` placeholder or an unresolved edge), false when it is
    /// backed by a real resolved `code_nodes` definition.
    pub name_only: bool,
}

/// A graph edge row (a relation between nodes).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EdgeRow {
    pub src_id: String,
    /// May be a `name:<symbol>` placeholder until the resolver repoints it.
    pub dst_id: String,
    /// calls | imports | references | defines
    pub kind: String,
    pub src_file: String,
    pub resolved: i64,
    pub weight: f64,
    pub conv_id: String,
    pub session_id: String,
    /// '' | 'direct' | 'method' — how the callee was syntactically invoked,
    /// captured at extraction time before `bare_callee()` strips the receiver.
    /// Only set on `calls` edges; '' for `imports`/`defines`.
    pub callee_kind: String,
    /// '' | 'external' | 'method' — set by the resolver (Phase 2+), not extraction.
    pub boundary: String,
    /// Resolver evidence for the edge's resolution decision (Phase 2+); '' until then.
    pub evidence: String,
    /// Immutable per-edge write-time provenance (WCR truth pass, Codex round
    /// 7 adversarial review): the whole-file content hash
    /// (`extraction::codegraph::body_hash` of the SAME source that produced
    /// this edge), stamped once at extraction time by `extract_inner`'s
    /// `add_edge` closure. Deliberately independent of `code_nodes.body_hash`
    /// — that column is refreshed by every `upsert_node` call, in a SEPARATE
    /// transaction from the edge replace, so it can silently drift out of
    /// sync with a stale edge that never got re-extracted (a partial write —
    /// nodes refreshed, edge replace failed or simply never ran — falsely
    /// "authenticates" the stale edge if the gate trusts node state instead
    /// of the edge's own). '' means absent — a legacy edge written before
    /// this column existed. The WCR re-point gate
    /// (`eval::codegraph::historical_src_content_unchanged`) treats '' as
    /// categorically ineligible for re-pointing, never a guess.
    pub src_content_hash: String,
}

/// One neighbour edge in a `query_neighbors` result.
#[derive(Debug, Clone)]
pub struct NeighborEdge {
    /// "out" (node calls/defines other) or "in" (other points at node).
    pub direction: String,
    pub edge_kind: String,
    pub resolved: bool,
    /// The endpoint on the other side of the edge.
    pub node: NodeRow,
}

/// A timeline entry in a file ledger (from `code_evolution`).
#[derive(Debug, Clone)]
pub struct LedgerTimelineEntry {
    pub session_id: String,
    pub timestamp: String,
    pub tool_name: String,
    pub functions_added: String,
    pub functions_removed: String,
    pub types_added: String,
    pub types_removed: String,
    pub imports_added: String,
}

/// The deterministic, append-only dossier for a single file (§8b).
#[derive(Debug, Clone)]
pub struct FileLedger {
    pub file: String,
    pub symbols: Vec<NodeRow>,
    pub timeline: Vec<LedgerTimelineEntry>,
    /// (caller_name, caller_file) for symbols that depend on this file.
    pub callers: Vec<(String, String)>,
    /// true when `code_graph_file_state` has a row for this file — i.e. the
    /// file went through extraction, even if it produced zero symbols.
    /// Distinguishes "never extracted" from "indexed and confirmed empty".
    pub indexed: bool,
}

/// All `code_nodes` columns, in a stable order shared by every read.
const NODE_COLS: &str = "id, repo, project, file, lang, kind, name, fqname, body_hash, \
     span_start, span_end, first_conv_id, last_conv_id, last_session_id";

fn row_to_node(row: &rusqlite::Row) -> rusqlite::Result<NodeRow> {
    Ok(NodeRow {
        id: row.get(0)?,
        repo: row.get(1)?,
        project: row.get(2)?,
        file: row.get(3)?,
        lang: row.get(4)?,
        kind: row.get(5)?,
        name: row.get(6)?,
        fqname: row.get(7)?,
        body_hash: row.get(8)?,
        span_start: row.get(9)?,
        span_end: row.get(10)?,
        first_conv_id: row.get(11)?,
        last_conv_id: row.get(12)?,
        last_session_id: row.get(13)?,
        name_only: false,
    })
}

/// Upsert a node. `first_conv_id` is preserved on an existing row (immutable
/// history); `last_conv_id` / `last_session_id` / `body_hash` / span / timestamps
/// are refreshed to the latest sighting.
pub fn upsert_node(conn: &Connection, n: &NodeRow) -> Result<()> {
    conn.execute(
        "INSERT INTO code_nodes
            (id, repo, project, file, lang, kind, name, fqname, body_hash,
             span_start, span_end, first_conv_id, last_conv_id, last_session_id,
             last_seen, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 datetime('now'), datetime('now'))
         ON CONFLICT(id) DO UPDATE SET
             repo = excluded.repo,
             project = excluded.project,
             file = excluded.file,
             lang = excluded.lang,
             kind = excluded.kind,
             name = excluded.name,
             fqname = excluded.fqname,
             body_hash = excluded.body_hash,
             span_start = excluded.span_start,
             span_end = excluded.span_end,
             -- first_conv_id is immutable: keep the original unless it was blank.
             first_conv_id = CASE
                 WHEN code_nodes.first_conv_id = '' THEN excluded.first_conv_id
                 ELSE code_nodes.first_conv_id END,
             last_conv_id = excluded.last_conv_id,
             last_session_id = excluded.last_session_id,
             last_seen = datetime('now'),
             updated_at = datetime('now')",
        params![
            n.id,
            n.repo,
            n.project,
            n.file,
            n.lang,
            n.kind,
            n.name,
            n.fqname,
            n.body_hash,
            n.span_start,
            n.span_end,
            n.first_conv_id,
            n.last_conv_id,
            n.last_session_id,
        ],
    )?;
    Ok(())
}

/// Per-file edge replace (Codex #3): delete every edge extracted from `src_file`,
/// then bulk-insert the fresh set. Single transaction — no stale fan-out.
///
/// Codex round 7 adversarial review (WCR truth pass): this transaction is
/// what makes `EdgeRow::src_content_hash` a sound re-point-eligibility
/// signal — every edge in `edges` (already stamped by `extract_inner` with
/// the SAME whole-file hash) is deleted-and-reinserted atomically with that
/// stamp, so the gate that reads it back never observes a half-written
/// state. This call is still a SEPARATE transaction from any preceding
/// `upsert_node` calls for the same file (every caller — `hooks::
/// post_tool_use::update_code_graph`, `import::backfill`,
/// `eval::codegraph::extract_and_store` — upserts nodes first, then calls
/// this), so `code_nodes.body_hash` can still drift out of sync with a
/// stale edge on a partial failure between the two calls. The fix does not
/// unify those two transactions (Codex's stated minimum bar): the gate
/// simply never reads `code_nodes.body_hash` for re-point eligibility any
/// more — `src_content_hash` lives ON the edge row itself, written
/// atomically with it right here, so a node-vs-edge transaction split can no
/// longer produce a false positive regardless of ordering.
pub fn replace_file_edges(
    conn: &Connection,
    _project: &str,
    src_file: &str,
    edges: &[EdgeRow],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM code_edges WHERE src_file = ?1",
        params![src_file],
    )?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO code_edges
                (src_id, dst_id, kind, src_file, resolved, weight, conv_id, session_id,
                 callee_kind, boundary, evidence, src_content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )?;
        for e in edges {
            stmt.execute(params![
                e.src_id,
                e.dst_id,
                e.kind,
                e.src_file,
                e.resolved,
                e.weight,
                e.conv_id,
                e.session_id,
                e.callee_kind,
                e.boundary,
                e.evidence,
                e.src_content_hash,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Replace-per-file upsert of `repo_defs` (name, kind, lang) for `(project, file)`:
/// delete existing rows then bulk-insert the fresh set. Same per-file replace
/// semantics as `replace_file_edges` — a rescanned file never leaves stale defs.
pub fn upsert_repo_defs(
    conn: &Connection,
    project: &str,
    file: &str,
    defs: &[(String, String, String)],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM repo_defs WHERE project = ?1 AND file = ?2",
        params![project, file],
    )?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO repo_defs (project, file, name, kind, lang, scanned_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
        )?;
        for (name, kind, lang) in defs {
            stmt.execute(params![project, file, name, kind, lang])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Definition sites for `name` within `project`: `(file, kind)`, deterministic order.
pub fn lookup_repo_defs(
    conn: &Connection,
    project: &str,
    name: &str,
) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT file, kind FROM repo_defs WHERE project = ?1 AND name = ?2 ORDER BY file, kind",
    )?;
    let rows = stmt.query_map(params![project, name], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Upsert per-file extraction state (content hash + dirty flag).
pub fn upsert_file_state(
    conn: &Connection,
    project: &str,
    file: &str,
    content_hash: &str,
    dirty: bool,
) -> Result<()> {
    conn.execute(
        "INSERT INTO code_graph_file_state (project, file, content_hash, dirty, extracted_at)
         VALUES (?1, ?2, ?3, ?4, datetime('now'))
         ON CONFLICT(project, file) DO UPDATE SET
             content_hash = excluded.content_hash,
             dirty = excluded.dirty,
             extracted_at = datetime('now')",
        params![project, file, content_hash, dirty as i64],
    )?;
    Ok(())
}

/// Mark a file dirty (needs re-extraction) without touching its hash.
pub fn mark_file_dirty(conn: &Connection, project: &str, file: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO code_graph_file_state (project, file, dirty)
         VALUES (?1, ?2, 1)
         ON CONFLICT(project, file) DO UPDATE SET dirty = 1",
        params![project, file],
    )?;
    Ok(())
}

/// Fetch a single node by id.
pub fn get_node(conn: &Connection, id: &str) -> Result<Option<NodeRow>> {
    let sql = format!("SELECT {NODE_COLS} FROM code_nodes WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![id], row_to_node)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// Fetch definition nodes by exact name, optionally scoped to a project.
/// Ordered by rank desc (best candidate first), then id for determinism.
pub fn nodes_by_name(
    conn: &Connection,
    name: &str,
    project: &str,
    limit: usize,
) -> Result<Vec<NodeRow>> {
    let sql = format!(
        "SELECT {cols} FROM code_nodes n
         LEFT JOIN code_node_rank r ON r.node_id = n.id
         WHERE n.name = ?1 AND (?2 = '' OR n.project = ?2)
         ORDER BY COALESCE(r.rank, 0.0) DESC, n.id
         LIMIT ?3",
        cols = NODE_COLS
            .split(", ")
            .map(|c| format!("n.{c}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![name, project, limit as i64], row_to_node)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Resolve a `symbol or id` to candidate node ids: exact id match first, else by name.
fn resolve_target_ids(conn: &Connection, name_or_id: &str, project: &str) -> Result<Vec<String>> {
    if get_node(conn, name_or_id)?.is_some() {
        return Ok(vec![name_or_id.to_string()]);
    }
    Ok(nodes_by_name(conn, name_or_id, project, 50)?
        .into_iter()
        .map(|n| n.id)
        .collect())
}

/// Who calls `name_or_id` — inbound `calls` edges (resolved id match OR the
/// `name:<symbol>` placeholder, so callers surface even before resolution).
/// The caller (source) node is scoped to `project` when non-empty — a bare
/// `name:<symbol>` placeholder is unqualified and would otherwise pull in
/// same-named callers from every other project sharing this database (e.g.
/// git worktree copies). Each row's `name_only` is true unless at least one
/// matching edge is resolved (definition-backed).
pub fn query_callers(
    conn: &Connection,
    name_or_id: &str,
    project: &str,
    limit: usize,
) -> Result<Vec<NodeRow>> {
    let mut targets = resolve_target_ids(conn, name_or_id, project)?;
    // Also match the unresolved placeholder form keyed on the bare name.
    let bare = name_or_id.rsplit("::").next().unwrap_or(name_or_id);
    let placeholder = format!("name:{bare}");

    if targets.is_empty() {
        targets.push(placeholder.clone());
    } else {
        targets.push(placeholder);
    }

    let placeholders: Vec<String> = (0..targets.len()).map(|i| format!("?{}", i + 2)).collect();
    let project_idx = targets.len() + 2;
    let cols = NODE_COLS
        .split(", ")
        .map(|c| format!("s.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let node_col_count = NODE_COLS.split(", ").count();
    let sql = format!(
        "SELECT {cols}, MAX(e.resolved) AS any_resolved FROM code_edges e
         JOIN code_nodes s ON s.id = e.src_id
         WHERE e.kind = 'calls' AND e.dst_id IN ({})
           AND (?{project_idx} = '' OR s.project = ?{project_idx})
         GROUP BY s.id
         ORDER BY s.id LIMIT ?1",
        placeholders.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut p: Vec<&dyn rusqlite::types::ToSql> = Vec::with_capacity(targets.len() + 2);
    let lim = limit as i64;
    p.push(&lim);
    for t in &targets {
        p.push(t);
    }
    p.push(&project);
    let rows = stmt.query_map(p.as_slice(), |row| {
        let mut node = row_to_node(row)?;
        let any_resolved: i64 = row.get(node_col_count)?;
        node.name_only = any_resolved == 0;
        Ok(node)
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// What `node_id` calls — outbound `calls` edges. Resolved edges return the real
/// dst node; unresolved `name:<x>` placeholders return a synthetic node.
pub fn query_callees(conn: &Connection, node_id: &str, limit: usize) -> Result<Vec<NodeRow>> {
    let cols = NODE_COLS
        .split(", ")
        .map(|c| format!("d.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT e.dst_id, e.resolved, {cols} FROM code_edges e
         LEFT JOIN code_nodes d ON d.id = e.dst_id
         WHERE e.kind = 'calls' AND e.src_id = ?1
         ORDER BY e.dst_id LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![node_id, limit as i64], |row| {
        let dst_id: String = row.get(0)?;
        let resolved: i64 = row.get(1)?;
        // Columns 2.. are the joined node (may be all NULL if unresolved).
        let id: Option<String> = row.get(2)?;
        if let Some(_id) = id {
            // Shift the node mapper by 2 columns.
            Ok(NodeRow {
                id: row.get(2)?,
                repo: row.get(3)?,
                project: row.get(4)?,
                file: row.get(5)?,
                lang: row.get(6)?,
                kind: row.get(7)?,
                name: row.get(8)?,
                fqname: row.get(9)?,
                body_hash: row.get(10)?,
                span_start: row.get(11)?,
                span_end: row.get(12)?,
                first_conv_id: row.get(13)?,
                last_conv_id: row.get(14)?,
                last_session_id: row.get(15)?,
                name_only: resolved == 0,
            })
        } else {
            // Unresolved placeholder: surface the bare name. Both signals
            // must be present — a downstream consumer keys off
            // `kind == "unresolved" || name_only`.
            let name = dst_id.strip_prefix("name:").unwrap_or(&dst_id).to_string();
            Ok(NodeRow {
                id: dst_id,
                kind: "unresolved".to_string(),
                name,
                name_only: true,
                ..NodeRow::default()
            })
        }
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// 1-hop neighbours (both directions) of `node_id`, optional edge-kind filter.
pub fn query_neighbors(
    conn: &Connection,
    node_id: &str,
    kind_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<NeighborEdge>> {
    let mut out = Vec::new();
    let kf = kind_filter.unwrap_or("");

    // Outbound: node_id -> other (only resolved edges have a real other node).
    let out_cols = NODE_COLS
        .split(", ")
        .map(|c| format!("d.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let out_sql = format!(
        "SELECT e.kind, e.resolved, {out_cols} FROM code_edges e
         JOIN code_nodes d ON d.id = e.dst_id
         WHERE e.src_id = ?1 AND (?2 = '' OR e.kind = ?2)
         ORDER BY e.kind, d.id LIMIT ?3"
    );
    {
        let mut stmt = conn.prepare(&out_sql)?;
        let rows = stmt.query_map(params![node_id, kf, limit as i64], |row| {
            let edge_kind: String = row.get(0)?;
            let resolved: i64 = row.get(1)?;
            let node = shift2_node(row)?;
            Ok(NeighborEdge {
                direction: "out".to_string(),
                edge_kind,
                resolved: resolved != 0,
                node,
            })
        })?;
        for r in rows {
            out.push(r?);
        }
    }

    // Inbound: other -> node_id.
    let in_cols = NODE_COLS
        .split(", ")
        .map(|c| format!("s.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let in_sql = format!(
        "SELECT e.kind, e.resolved, {in_cols} FROM code_edges e
         JOIN code_nodes s ON s.id = e.src_id
         WHERE e.dst_id = ?1 AND (?2 = '' OR e.kind = ?2)
         ORDER BY e.kind, s.id LIMIT ?3"
    );
    {
        let mut stmt = conn.prepare(&in_sql)?;
        let rows = stmt.query_map(params![node_id, kf, limit as i64], |row| {
            let edge_kind: String = row.get(0)?;
            let resolved: i64 = row.get(1)?;
            let node = shift2_node(row)?;
            Ok(NeighborEdge {
                direction: "in".to_string(),
                edge_kind,
                resolved: resolved != 0,
                node,
            })
        })?;
        for r in rows {
            out.push(r?);
        }
    }

    Ok(out)
}

/// Map a row whose node columns start at index 2.
fn shift2_node(row: &rusqlite::Row) -> rusqlite::Result<NodeRow> {
    Ok(NodeRow {
        id: row.get(2)?,
        repo: row.get(3)?,
        project: row.get(4)?,
        file: row.get(5)?,
        lang: row.get(6)?,
        kind: row.get(7)?,
        name: row.get(8)?,
        fqname: row.get(9)?,
        body_hash: row.get(10)?,
        span_start: row.get(11)?,
        span_end: row.get(12)?,
        first_conv_id: row.get(13)?,
        last_conv_id: row.get(14)?,
        last_session_id: row.get(15)?,
        name_only: false,
    })
}

/// The immutable per-file ledger (§8b): current symbols (+provenance), the
/// `code_evolution` timeline, and callers depending on the file. Deterministic.
///
/// `file` is matched exactly OR as a path suffix, so a relative tool path and an
/// absolute stored path reconcile.
pub fn file_ledger(conn: &Connection, project: &str, file: &str) -> Result<FileLedger> {
    let suffix = format!("%{file}");

    // 1. Current symbols (code_nodes), excluding the synthetic module node.
    let sym_sql = format!(
        "SELECT {NODE_COLS} FROM code_nodes
         WHERE (file = ?1 OR file LIKE ?2) AND (?3 = '' OR project = ?3)
           AND kind != 'module'
         ORDER BY kind, name, id"
    );
    let symbols = {
        let mut stmt = conn.prepare(&sym_sql)?;
        let rows = stmt.query_map(params![file, suffix, project], row_to_node)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    // 2. Timeline (code_evolution).
    let timeline = {
        let mut stmt = conn.prepare(
            "SELECT session_id, timestamp, tool_name, functions_added, functions_removed,
                    types_added, types_removed, imports_added
             FROM code_evolution
             WHERE (file_path = ?1 OR file_path LIKE ?2) AND (?3 = '' OR project_name = ?3)
             ORDER BY timestamp DESC, id DESC",
        )?;
        let rows = stmt.query_map(params![file, suffix, project], |row| {
            Ok(LedgerTimelineEntry {
                session_id: row.get(0)?,
                timestamp: row.get(1)?,
                tool_name: row.get(2)?,
                functions_added: row.get(3)?,
                functions_removed: row.get(4)?,
                types_added: row.get(5)?,
                types_removed: row.get(6)?,
                imports_added: row.get(7)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    // 3. Callers: resolved `calls` edges whose dst node lives in this file.
    let callers = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT s.name, s.file FROM code_edges e
             JOIN code_nodes d ON d.id = e.dst_id
             JOIN code_nodes s ON s.id = e.src_id
             WHERE e.kind = 'calls' AND e.resolved = 1
               AND (d.file = ?1 OR d.file LIKE ?2) AND (?3 = '' OR d.project = ?3)
             ORDER BY s.file, s.name",
        )?;
        let rows = stmt.query_map(params![file, suffix, project], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    // 4. Indexed flag: was this file ever run through extraction at all
    //    (regardless of whether it produced any symbols)? Distinguishes
    //    "never extracted" from "indexed and confirmed empty" — both used
    //    to render identically via the FTS5 fallback.
    let indexed = {
        let mut stmt = conn.prepare(
            "SELECT 1 FROM code_graph_file_state
             WHERE (file = ?1 OR file LIKE ?2) AND (?3 = '' OR project = ?3)
             LIMIT 1",
        )?;
        stmt.exists(params![file, suffix, project])?
    };

    Ok(FileLedger {
        file: file.to_string(),
        symbols,
        timeline,
        callers,
        indexed,
    })
}

/// Fetch a node's persisted rank: (rank, in_degree, out_degree).
pub fn get_node_rank(conn: &Connection, id: &str) -> Result<Option<(f64, i64, i64)>> {
    let mut stmt =
        conn.prepare("SELECT rank, in_degree, out_degree FROM code_node_rank WHERE node_id = ?1")?;
    let mut rows = stmt.query_map(params![id], |row| {
        Ok((
            row.get::<_, f64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::migrations;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        conn
    }

    fn node(id: &str, file: &str, kind: &str, name: &str, conv: &str) -> NodeRow {
        NodeRow {
            id: id.into(),
            repo: "repo".into(),
            project: "proj".into(),
            file: file.into(),
            lang: "rust".into(),
            kind: kind.into(),
            name: name.into(),
            body_hash: "h".into(),
            first_conv_id: conv.into(),
            last_conv_id: conv.into(),
            last_session_id: "sess".into(),
            ..NodeRow::default()
        }
    }

    #[test]
    fn upsert_preserves_first_conv_id() {
        let conn = mem();
        upsert_node(&conn, &node("n1", "a.rs", "function", "foo", "conv_A")).unwrap();
        // Re-upsert with a new conv and body — first_conv_id must stay conv_A.
        let mut n = node("n1", "a.rs", "function", "foo", "conv_B");
        n.body_hash = "h2".into();
        upsert_node(&conn, &n).unwrap();
        let got = get_node(&conn, "n1").unwrap().unwrap();
        assert_eq!(got.first_conv_id, "conv_A", "first conv is immutable");
        assert_eq!(got.last_conv_id, "conv_B", "last conv updates");
        assert_eq!(got.body_hash, "h2");
    }

    #[test]
    fn replace_file_edges_is_per_file() {
        let conn = mem();
        let e1 = EdgeRow {
            src_id: "n1".into(),
            dst_id: "name:bar".into(),
            kind: "calls".into(),
            src_file: "a.rs".into(),
            weight: 1.0,
            ..EdgeRow::default()
        };
        replace_file_edges(&conn, "proj", "a.rs", std::slice::from_ref(&e1)).unwrap();
        // Replacing a.rs's edges with an empty set wipes only a.rs.
        let e_other = EdgeRow {
            src_id: "n2".into(),
            dst_id: "name:baz".into(),
            kind: "calls".into(),
            src_file: "b.rs".into(),
            weight: 1.0,
            ..EdgeRow::default()
        };
        replace_file_edges(&conn, "proj", "b.rs", std::slice::from_ref(&e_other)).unwrap();
        replace_file_edges(&conn, "proj", "a.rs", &[]).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM code_edges", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "only b.rs edge remains");
    }

    #[test]
    fn file_ledger_holds_symbols_and_history() {
        let conn = mem();
        upsert_node(&conn, &node("n1", "a.rs", "function", "foo", "conv_A")).unwrap();
        crate::storage::queries::insert_code_evolution(
            &conn,
            "sess",
            "proj",
            "a.rs",
            "rust",
            "Edit",
            "[\"foo\"]",
            "[]",
            "[]",
            "[]",
            "[]",
            "[]",
        )
        .unwrap();
        let ledger = file_ledger(&conn, "proj", "a.rs").unwrap();
        assert_eq!(ledger.symbols.len(), 1);
        assert_eq!(ledger.symbols[0].name, "foo");
        assert_eq!(ledger.symbols[0].first_conv_id, "conv_A");
        assert_eq!(ledger.timeline.len(), 1);
        assert_eq!(ledger.timeline[0].tool_name, "Edit");
    }

    #[test]
    fn query_callers_classifies_name_only_vs_definition() {
        let conn = mem();
        upsert_node(
            &conn,
            &node("target1", "t.rs", "function", "target_fn", "conv_T"),
        )
        .unwrap();
        // Caller that resolved directly to the real id.
        upsert_node(
            &conn,
            &node("caller_resolved", "a.rs", "function", "caller_a", "conv_A"),
        )
        .unwrap();
        replace_file_edges(
            &conn,
            "proj",
            "a.rs",
            &[EdgeRow {
                src_id: "caller_resolved".into(),
                dst_id: "target1".into(),
                kind: "calls".into(),
                src_file: "a.rs".into(),
                resolved: 1,
                weight: 1.0,
                ..EdgeRow::default()
            }],
        )
        .unwrap();
        // Caller that only matched via the bare-name placeholder.
        upsert_node(
            &conn,
            &node(
                "caller_placeholder",
                "b.rs",
                "function",
                "caller_b",
                "conv_B",
            ),
        )
        .unwrap();
        replace_file_edges(
            &conn,
            "proj",
            "b.rs",
            &[EdgeRow {
                src_id: "caller_placeholder".into(),
                dst_id: "name:target_fn".into(),
                kind: "calls".into(),
                src_file: "b.rs".into(),
                resolved: 0,
                weight: 1.0,
                ..EdgeRow::default()
            }],
        )
        .unwrap();

        let callers = query_callers(&conn, "target_fn", "proj", 20).unwrap();
        let resolved_caller = callers.iter().find(|n| n.id == "caller_resolved").unwrap();
        assert!(
            !resolved_caller.name_only,
            "definition-backed match must not be name_only"
        );
        let placeholder_caller = callers
            .iter()
            .find(|n| n.id == "caller_placeholder")
            .unwrap();
        assert!(
            placeholder_caller.name_only,
            "placeholder-only match must be name_only"
        );
    }

    #[test]
    fn query_callers_scopes_caller_by_project() {
        let conn = mem();
        upsert_node(
            &conn,
            &node("target1", "t.rs", "function", "target_fn", "conv_T"),
        )
        .unwrap();
        // Caller in a DIFFERENT project — must not leak into a "proj"-scoped query.
        let mut other = node("caller_other", "x.rs", "function", "caller_x", "conv_X");
        other.project = "other".into();
        upsert_node(&conn, &other).unwrap();
        replace_file_edges(
            &conn,
            "other",
            "x.rs",
            &[EdgeRow {
                src_id: "caller_other".into(),
                dst_id: "name:target_fn".into(),
                kind: "calls".into(),
                src_file: "x.rs".into(),
                resolved: 0,
                weight: 1.0,
                ..EdgeRow::default()
            }],
        )
        .unwrap();

        let callers = query_callers(&conn, "target_fn", "proj", 20).unwrap();
        assert!(
            callers.iter().all(|n| n.id != "caller_other"),
            "caller from another project must not leak: {:?}",
            callers.iter().map(|n| &n.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn query_callees_marks_unresolved_name_only() {
        let conn = mem();
        upsert_node(
            &conn,
            &node("caller1", "a.rs", "function", "caller_fn", "conv_A"),
        )
        .unwrap();
        upsert_node(
            &conn,
            &node("callee1", "b.rs", "function", "callee_fn", "conv_B"),
        )
        .unwrap();
        replace_file_edges(
            &conn,
            "proj",
            "a.rs",
            &[
                EdgeRow {
                    src_id: "caller1".into(),
                    dst_id: "callee1".into(),
                    kind: "calls".into(),
                    src_file: "a.rs".into(),
                    resolved: 1,
                    weight: 1.0,
                    ..EdgeRow::default()
                },
                EdgeRow {
                    src_id: "caller1".into(),
                    dst_id: "name:ghost_fn".into(),
                    kind: "calls".into(),
                    src_file: "a.rs".into(),
                    resolved: 0,
                    weight: 1.0,
                    ..EdgeRow::default()
                },
            ],
        )
        .unwrap();

        let callees = query_callees(&conn, "caller1", 20).unwrap();
        let resolved = callees.iter().find(|n| n.id == "callee1").unwrap();
        assert!(!resolved.name_only, "resolved callee must not be name_only");
        let unresolved = callees.iter().find(|n| n.kind == "unresolved").unwrap();
        assert!(
            unresolved.name_only,
            "unresolved placeholder callee must be name_only"
        );
        assert_eq!(unresolved.name, "ghost_fn");
    }

    #[test]
    fn repo_defs_upsert_lookup_and_per_file_replace() {
        let conn = mem();
        upsert_repo_defs(
            &conn,
            "proj",
            "a.rs",
            &[
                ("foo".into(), "function".into(), "rust".into()),
                ("Bar".into(), "type".into(), "rust".into()),
            ],
        )
        .unwrap();
        upsert_repo_defs(
            &conn,
            "proj",
            "b.rs",
            &[("foo".into(), "function".into(), "rust".into())],
        )
        .unwrap();

        let hits = lookup_repo_defs(&conn, "proj", "foo").unwrap();
        assert_eq!(hits.len(), 2, "foo defined in both files: {hits:?}");
        assert!(hits.contains(&("a.rs".to_string(), "function".to_string())));
        assert!(hits.contains(&("b.rs".to_string(), "function".to_string())));

        // Replacing a.rs's defs with a set that drops `foo` must not leave a stale row.
        upsert_repo_defs(
            &conn,
            "proj",
            "a.rs",
            &[("Baz".into(), "type".into(), "rust".into())],
        )
        .unwrap();
        let hits = lookup_repo_defs(&conn, "proj", "foo").unwrap();
        assert_eq!(
            hits,
            vec![("b.rs".to_string(), "function".to_string())],
            "a.rs's foo def must be gone after replace: {hits:?}"
        );
    }
}
