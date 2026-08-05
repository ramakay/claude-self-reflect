//! Code-property-graph storage (v9.4).
//!
//! Conversation-provenance code graph: nodes (symbols), edges (calls/defines/
//! imports), per-file extraction state, and degree-based rank. Every node and
//! edge carries `conv_id` / `session_id` provenance — that is the moat.
//!
//! Schema lives in `migrations.rs` (tables `code_nodes`, `code_edges`,
//! `code_graph_file_state`, `code_node_rank`). This module owns the queries.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

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
    /// Git toplevel of `file` at write time (WP2 Stage 1, H8 finding — see
    /// `extraction::repo_root::repo_root_for_file`). `None` for non-git
    /// files or when the repo can no longer be resolved. Independent of
    /// `project` (the session-cwd tag) — never overwrites it, never derived
    /// from it.
    pub repo_root: Option<String>,
    /// true when this row is an unverified name-collision guess (matched via
    /// the `name:<bare>` placeholder or an unresolved edge), false when it is
    /// backed by a real resolved `code_nodes` definition.
    pub name_only: bool,
    /// Rendered two-channel attribution summary (WP2 Stage 2 — see
    /// `format_attribution`): `"unattributed"`, `"transcript:<8>"`,
    /// `"git:<8>"`, or both combined (optionally flagged as a disagreement).
    /// Empty string means "not attached yet" — callers that render a node
    /// must attach it explicitly (`attribution_for_node` / the MCP tool
    /// layer); it is never derived from `first_conv_id`.
    pub attribution: String,
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
     span_start, span_end, first_conv_id, last_conv_id, last_session_id, repo_root";

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
        repo_root: row.get(14)?,
        name_only: false,
        attribution: String::new(),
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
             repo_root, last_seen, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
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
             -- repo_root: refresh when this sighting resolved one; keep the
             -- prior value when it didn't (a transient `git` failure must
             -- never silently blank out a previously-known repo identity).
             repo_root = COALESCE(excluded.repo_root, code_nodes.repo_root),
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
            n.repo_root,
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

/// Upsert per-file extraction state (content hash + dirty flag). Every
/// caller of this function has, by construction, just run a real extraction
/// against `file` — so `ast_status` is always stamped `'supported'` here,
/// overwriting any stale `'unsupported'` left by an earlier sighting of the
/// same path under a different extension resolution (rare, but honest).
pub fn upsert_file_state(
    conn: &Connection,
    project: &str,
    file: &str,
    content_hash: &str,
    dirty: bool,
) -> Result<()> {
    conn.execute(
        "INSERT INTO code_graph_file_state (project, file, content_hash, dirty, extracted_at, ast_status)
         VALUES (?1, ?2, ?3, ?4, datetime('now'), 'supported')
         ON CONFLICT(project, file) DO UPDATE SET
             content_hash = excluded.content_hash,
             dirty = excluded.dirty,
             extracted_at = datetime('now'),
             ast_status = 'supported'",
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

/// Record that `file` was seen by an extraction write path (hook
/// `update_code_graph`, `import::backfill`) but skipped because its
/// extension is outside the six AST-supported languages (WP2 Stage 3, H8
/// innovation — receipt R4 in
/// `.plans/2026-07-31-codegraph-shipping-plan.md`). Never dirty (there is
/// nothing to re-extract) and never touches `content_hash` — this is
/// file-level provenance only ("we looked, it's out of scope"), not a
/// pending-extraction marker.
pub fn mark_file_unsupported(conn: &Connection, project: &str, file: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO code_graph_file_state (project, file, dirty, ast_status)
         VALUES (?1, ?2, 0, 'unsupported')
         ON CONFLICT(project, file) DO UPDATE SET
             dirty = 0,
             ast_status = 'unsupported',
             extracted_at = datetime('now')",
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
                repo_root: row.get(16)?,
                name_only: resolved == 0,
                attribution: String::new(),
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
        repo_root: row.get(16)?,
        name_only: false,
        attribution: String::new(),
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

/// Distinct `code_nodes.file` values still missing `repo_root` — feeds the
/// WP2 Stage 1 backfill (`import::backfill::backfill_repo_root`).
pub fn code_node_files_missing_repo_root(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT DISTINCT file FROM code_nodes WHERE repo_root IS NULL AND file != ''")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// The `repo_root` already stored on ANY `code_nodes` row for `file` —
/// mirrors `import::backfill::stamp_spans_into`'s own resolution preference
/// (prefer a value already on the node) so `dream`'s successor join
/// resolves the identical repo for a file without duplicating the stamping
/// pass's own per-node lookup. `None` when no row for `file` has a
/// `repo_root` yet (caller falls back to
/// `extraction::repo_root::repo_root_for_file`, exactly like
/// `stamp_spans_into` does).
pub fn stored_repo_root_for_file(conn: &Connection, file: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT repo_root FROM code_nodes WHERE file = ?1 AND repo_root IS NOT NULL LIMIT 1",
        params![file],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Set `repo_root` on every `code_nodes` row matching `file` that is
/// currently NULL. Never overwrites an already-resolved value (idempotent,
/// re-runnable). Returns the number of rows changed.
pub fn set_repo_root_for_file(conn: &Connection, file: &str, repo_root: &str) -> Result<usize> {
    let n = conn.execute(
        "UPDATE code_nodes SET repo_root = ?1 WHERE file = ?2 AND repo_root IS NULL",
        params![repo_root, file],
    )?;
    Ok(n)
}

/// All `code_nodes` rows (WP2 Stage 2 attribution backfill —
/// `import::backfill::backfill_attribution` needs the full symbol set to
/// join against `code_evolution` and to walk `git log -L`). Ordered by `id`
/// for a deterministic backfill pass.
pub fn all_nodes(conn: &Connection) -> Result<Vec<NodeRow>> {
    let sql = format!("SELECT {NODE_COLS} FROM code_nodes ORDER BY id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_node)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Every non-module `code_nodes` row attributed (via `first_conv_id` OR
/// `last_conv_id` — the direct conversation<->symbol link the code graph
/// carries; see `storage::chunk_binding`'s module doc) to one of
/// `conversation_ids`. Feeds v10 "dreaming" chunk binding: a search result's
/// conversation ids resolve to the `(project, file, name)` symbols that
/// conversation introduced or last touched, which chunk binding then tries
/// to join against `witness_ledger`.
///
/// Batched ≤400 ids per statement (two `IN` lists per batch, so this stays
/// well under `SQLITE_MAX_VARIABLE_NUMBER` — same reasoning as
/// `queries::known_session_ids`). Uses `idx_code_nodes_first_conv` /
/// `idx_code_nodes_last_conv` (see `migrations::run`) — a full scan of
/// `code_nodes` would otherwise be paid on every search.
pub fn nodes_for_conversations(
    conn: &Connection,
    conversation_ids: &[String],
) -> Result<Vec<NodeRow>> {
    if conversation_ids.is_empty() {
        return Ok(Vec::new());
    }
    const BATCH: usize = 400;
    let mut out = Vec::new();
    for batch in conversation_ids.chunks(BATCH) {
        let placeholders: Vec<String> = (1..=batch.len()).map(|i| format!("?{i}")).collect();
        let in_list = placeholders.join(", ");
        let sql = format!(
            "SELECT {NODE_COLS} FROM code_nodes
             WHERE kind != 'module'
               AND (first_conv_id IN ({in_list}) OR last_conv_id IN ({in_list}))
             ORDER BY id"
        );
        let params: Vec<&dyn rusqlite::types::ToSql> = batch
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), row_to_node)?;
        for row in rows {
            out.push(row?);
        }
    }
    Ok(out)
}

// ─── Two-channel symbol attribution (WP2 Stage 2) ───
//
// H4 (receipt R2 in `.plans/2026-07-31-codegraph-shipping-plan.md`) measured
// `code_nodes.first_conv_id` at 50.7% agreement with the evidence-bearing
// join below — it is a file-level projection (the file's first-touching
// conversation), not a real per-symbol fact. `code_node_attribution` records
// two independent, NEVER-merged provenance channels instead:
//   - `transcript`: the agent conversation whose `code_evolution` event
//     first named this symbol (earliest by (timestamp, rowid) — H5,
//     receipt R3). `source_id` = session id, `evidence` = "coedit_event".
//   - `git`: the commit that introduced the symbol's current line span, via
//     `git log -L<span>,<span>:<file> --reverse` (H6, receipt R8).
//     `source_id` = commit hash, `evidence` = "git_log_L".
// `first_conv_id` stays in the schema for compat but is no longer presented
// as introduction evidence by any consumer surface (`csr_code_graph` /
// `csr_search_by_file`) — they render `format_attribution`'s output instead.

/// One provenance channel's evidence for a `code_nodes` symbol.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttributionRow {
    pub node_id: String,
    /// "transcript" | "git" (DB-enforced by the `code_node_attribution.channel` CHECK).
    pub channel: String,
    /// Session id (transcript channel) or commit hash (git channel).
    pub source_id: String,
    /// ISO8601 event/commit timestamp, when known.
    pub observed_ts: Option<String>,
    /// Short machine tag naming the derivation ("coedit_event" | "git_log_L").
    pub evidence: String,
}

/// Idempotent upsert of one attribution channel row. PRIMARY KEY(node_id,
/// channel) means a re-run of the backfill (e.g. after a later, earlier-
/// timestamped event is discovered) simply replaces the prior value for that
/// channel — never accumulates duplicates.
pub fn upsert_attribution(conn: &Connection, row: &AttributionRow) -> Result<()> {
    conn.execute(
        "INSERT INTO code_node_attribution (node_id, channel, source_id, observed_ts, evidence)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(node_id, channel) DO UPDATE SET
             source_id = excluded.source_id,
             observed_ts = excluded.observed_ts,
             evidence = excluded.evidence",
        params![
            row.node_id,
            row.channel,
            row.source_id,
            row.observed_ts,
            row.evidence
        ],
    )?;
    Ok(())
}

/// Fetch every attribution row for one node (0, 1, or 2 rows — one per channel).
pub fn get_attribution(conn: &Connection, node_id: &str) -> Result<Vec<AttributionRow>> {
    let mut stmt = conn.prepare(
        "SELECT node_id, channel, source_id, observed_ts, evidence
         FROM code_node_attribution WHERE node_id = ?1 ORDER BY channel",
    )?;
    let rows = stmt.query_map(params![node_id], |row| {
        Ok(AttributionRow {
            node_id: row.get(0)?,
            channel: row.get(1)?,
            source_id: row.get(2)?,
            observed_ts: row.get(3)?,
            evidence: row.get(4)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Render the two-channel attribution state for one node's rows (WP2 Stage
/// 2, plan section "Consumer surfaces"):
///   - neither channel  -> "unattributed"
///   - one channel only -> "transcript:<8>" or "git:<8>"
///   - both, agreeing (or timestamps not comparable) -> "transcript:<8> + git:<8>"
///   - both, timestamps >48h apart -> the same string plus a disagreement
///     label — the two values are always shown side by side, never merged
///     into one.
///
/// `<8>` is the first 8 characters of `source_id` (a session uuid or a
/// commit hash), matching the receipt table's `transcript:<uuid8>` /
/// `git:<commit8>` shorthand.
pub fn format_attribution(rows: &[AttributionRow]) -> String {
    let transcript = rows.iter().find(|r| r.channel == "transcript");
    let git = rows.iter().find(|r| r.channel == "git");
    let short = |s: &str| s.chars().take(8).collect::<String>();

    match (transcript, git) {
        (None, None) => "unattributed".to_string(),
        (Some(t), None) => format!("transcript:{}", short(&t.source_id)),
        (None, Some(g)) => format!("git:{}", short(&g.source_id)),
        (Some(t), Some(g)) => {
            let base = format!(
                "transcript:{} + git:{}",
                short(&t.source_id),
                short(&g.source_id)
            );
            let gap_hours = match (
                t.observed_ts
                    .as_deref()
                    .and_then(crate::temporal::parse_timestamp),
                g.observed_ts
                    .as_deref()
                    .and_then(crate::temporal::parse_timestamp),
            ) {
                (Some(a), Some(b)) => Some((a - b).num_hours().abs()),
                _ => None,
            };
            match gap_hours {
                Some(h) if h > 48 => format!("{base} (disagreement, {h}h apart)"),
                _ => base,
            }
        }
    }
}

/// Fetch + render a node's attribution in one call — the render helper
/// `csr_code_graph` / `csr_search_by_file` actually use.
pub fn attribution_for_node(conn: &Connection, node_id: &str) -> Result<String> {
    let rows = get_attribution(conn, node_id)?;
    Ok(format_attribution(&rows))
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
            None,
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

    // ─── WP2 Stage 2: two-channel attribution ───

    fn attr(node_id: &str, channel: &str, source_id: &str, ts: Option<&str>) -> AttributionRow {
        AttributionRow {
            node_id: node_id.into(),
            channel: channel.into(),
            source_id: source_id.into(),
            observed_ts: ts.map(|s| s.into()),
            evidence: "test".into(),
        }
    }

    #[test]
    fn format_attribution_neither_channel_is_unattributed() {
        assert_eq!(format_attribution(&[]), "unattributed");
    }

    #[test]
    fn format_attribution_transcript_only() {
        let rows = vec![attr("n1", "transcript", "70690eeb12345678", None)];
        assert_eq!(format_attribution(&rows), "transcript:70690eeb");
    }

    #[test]
    fn format_attribution_git_only() {
        let rows = vec![attr("n1", "git", "624e7229abcdef", None)];
        assert_eq!(format_attribution(&rows), "git:624e7229");
    }

    #[test]
    fn format_attribution_both_channels_agree_no_disagreement_label() {
        let rows = vec![
            attr(
                "n1",
                "transcript",
                "70690eeb12345678",
                Some("2026-07-27T10:00:00Z"),
            ),
            attr("n1", "git", "624e7229abcdef", Some("2026-07-27T11:00:00Z")),
        ];
        let out = format_attribution(&rows);
        assert_eq!(out, "transcript:70690eeb + git:624e7229");
        assert!(!out.contains("disagreement"));
    }

    #[test]
    fn format_attribution_both_channels_over_48h_apart_is_disagreement() {
        // R8 example: git `624e7229` vs transcript 2026-07-27, labeled
        // disagreement per the plan's acceptance spot-check.
        let rows = vec![
            attr(
                "n1",
                "transcript",
                "70690eeb12345678",
                Some("2026-07-27T10:00:00Z"),
            ),
            attr("n1", "git", "624e7229abcdef", Some("2026-08-01T10:00:00Z")),
        ];
        let out = format_attribution(&rows);
        assert!(out.contains("transcript:70690eeb"), "got: {out}");
        assert!(out.contains("git:624e7229"), "got: {out}");
        assert!(out.contains("disagreement"), "got: {out}");
        // Never merge into a single collapsed value — both must be visible.
        assert!(out.contains('+') || out.contains("vs"), "got: {out}");
    }

    #[test]
    fn format_attribution_both_channels_unparseable_timestamps_no_false_disagreement() {
        // Missing/garbage timestamps must never be silently treated as a
        // >48h gap — we can't prove disagreement we can't measure.
        let rows = vec![
            attr("n1", "transcript", "abc12345", None),
            attr("n1", "git", "def67890", Some("not-a-timestamp")),
        ];
        let out = format_attribution(&rows);
        assert_eq!(out, "transcript:abc12345 + git:def67890");
    }

    #[test]
    fn upsert_and_get_attribution_round_trips_and_is_idempotent() {
        let conn = mem();
        upsert_node(&conn, &node("n1", "a.rs", "function", "foo", "conv_A")).unwrap();
        let row = attr("n1", "transcript", "sess-1", Some("2026-07-27T10:00:00Z"));
        upsert_attribution(&conn, &row).unwrap();
        // Re-run with a different source_id for the same (node_id, channel):
        // must replace, never duplicate (PK is node_id+channel).
        let row2 = attr("n1", "transcript", "sess-2", Some("2026-07-28T10:00:00Z"));
        upsert_attribution(&conn, &row2).unwrap();

        let got = get_attribution(&conn, "n1").unwrap();
        assert_eq!(got.len(), 1, "one row per channel: {got:?}");
        assert_eq!(got[0].source_id, "sess-2");
    }

    #[test]
    fn code_node_attribution_check_constraint_rejects_bad_channel() {
        let conn = mem();
        let bad = conn.execute(
            "INSERT INTO code_node_attribution (node_id, channel, source_id) VALUES ('n1', 'guess', 's1')",
            [],
        );
        assert!(
            bad.is_err(),
            "CHECK(channel IN ('transcript','git')) must reject 'guess'"
        );
    }

    #[test]
    fn attribution_for_node_unattributed_when_neither_channel_present() {
        let conn = mem();
        upsert_node(&conn, &node("n1", "a.rs", "function", "foo", "conv_A")).unwrap();
        assert_eq!(attribution_for_node(&conn, "n1").unwrap(), "unattributed");
    }
}
