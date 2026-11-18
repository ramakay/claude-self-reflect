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
use std::collections::HashSet;

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

/// The project's co-edit counter (`code_graph_project_coedit`, 0 before any
/// co-edit row for one of its files).
fn coedit_gen(conn: &Connection, project: &str) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT gen FROM code_graph_project_coedit WHERE project = ?1",
            params![project],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// The project's resolve-input counter (0 before any write): its own row, the
/// every-project row (`'*'`, which no trigger bumps now) and its co-edit row.
pub fn project_gen(conn: &Connection, project: &str) -> Result<i64> {
    let rows: i64 = conn.query_row(
        "SELECT COALESCE(SUM(gen), 0) FROM code_graph_project_gen
         WHERE project = ?1 OR project = '*'",
        params![project],
        |r| r.get(0),
    )?;
    Ok(rows + coedit_gen(conn, project)?)
}

/// Unix seconds now.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// A project's resolve-input counter, split: its own row, and the rows that
/// only ask for a pass after a cooldown (the every-project row `'*'` and the
/// project's co-edit row), added together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GenParts {
    pub own: i64,
    pub star: i64,
}

fn gen_row(conn: &Connection, project: &str) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT gen FROM code_graph_project_gen WHERE project = ?1",
            params![project],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// [`project_gen`] as its two parts.
pub fn project_gen_parts(conn: &Connection, project: &str) -> Result<GenParts> {
    Ok(GenParts {
        own: gen_row(conn, project)?,
        star: gen_row(conn, "*")? + coedit_gen(conn, project)?,
    })
}

/// Record that a resolve pass covered the project's inputs as of `covered`.
/// Only ever moves forward.
pub fn mark_project_settled(conn: &Connection, project: &str, covered: GenParts) -> Result<()> {
    conn.execute(
        "INSERT INTO code_graph_project_settled (project, own, star, settled_at)
         VALUES (?1, ?2, ?3, CAST(strftime('%s', 'now') AS INTEGER))
         ON CONFLICT(project) DO UPDATE SET
             own = MAX(own, excluded.own),
             star = MAX(star, excluded.star),
             settled_at = excluded.settled_at",
        params![project, covered.own, covered.star],
    )?;
    Ok(())
}

/// How long a project must have been settled before a change only its co-edit
/// counter saw (a `code_evolution` row for one of its files) asks for another
/// pass.
///
/// Chosen over a shorter wait because the only reader of that evidence is the
/// resolver's B3 tier (`coedit_weight`), which decides between definitions the
/// graph alone cannot tell apart and needs a 2x margin to bind: one more session
/// seldom moves it, and a project being edited gets passes for its own edits
/// anyway. An in-flight plan is not affected by this wait: the co-edit counter is
/// part of `project_gen`, so a plan made before such a row is dropped and made
/// again. At 60 s, the previous value, an actively edited project was re-resolved
/// every minute for evidence that rarely changed an answer.
pub const STAR_ONLY_COOLDOWN_SECS: i64 = 900;

/// Projects whose resolve inputs changed since the last pass that covered them
/// and that no pending file or queued snapshot accounts for: the counter of the
/// project's own row is ahead of the settled one, or its co-edit/every-project
/// counter is and `STAR_ONLY_COOLDOWN_SECS` have passed since the project last
/// settled.
pub fn dirty_projects(conn: &Connection, now_secs: i64) -> Result<Vec<String>> {
    let star = gen_row(conn, "*")?;
    let mut stmt = conn.prepare(
        "SELECT g.project, g.gen, COALESCE(s.own, 0), COALESCE(s.star, 0), COALESCE(s.settled_at, 0),
                COALESCE(c.gen, 0)
         FROM code_graph_project_gen g
         LEFT JOIN code_graph_project_settled s ON s.project = g.project
         LEFT JOIN code_graph_project_coedit c ON c.project = g.project
         WHERE g.project != '*' AND g.project != ''
         ORDER BY g.project",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (project, own, settled_own, settled_star, settled_at, coedit) = row?;
        let own_moved = own > settled_own;
        let star_moved =
            star + coedit > settled_star && now_secs - settled_at >= STAR_ONLY_COOLDOWN_SECS;
        if own_moved || star_moved {
            out.push(project);
        }
    }
    Ok(out)
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

/// Record the exact transcript chunk containing the latest tool edit that
/// produced this node. Kept outside [`NodeRow`] so historical/backfill graph
/// writers cannot accidentally overwrite live transcript attribution.
pub fn set_last_chunk_id(conn: &Connection, node_id: &str, chunk_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE code_nodes SET last_chunk_id = ?2 WHERE id = ?1",
        params![node_id, chunk_id],
    )?;
    Ok(())
}

/// Replace edges from `src_file` whose source nodes belong to `project`, then
/// insert the fresh set. Sweep source-less edges on the file as before. An
/// empty project replaces the entire file for the eval snapshot copy.
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
    project: &str,
    src_file: &str,
    edges: &[EdgeRow],
) -> Result<()> {
    in_tx(conn, |tx| {
        replace_file_edges_in(tx, project, src_file, edges)
    })
}

/// Run `f` in a transaction of its own, or inside the caller's when `conn`
/// already has one open (so a caller can make several of these writes atomic).
fn in_tx<T>(conn: &Connection, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    if !conn.is_autocommit() {
        return f(conn);
    }
    // IMMEDIATE: several of these start with a read, and a deferred transaction
    // that reads and then writes fails outright when another writer committed in
    // between instead of waiting for the lock.
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let out = f(&tx)?;
    tx.commit()?;
    Ok(out)
}

fn replace_file_edges_in(
    tx: &Connection,
    project: &str,
    src_file: &str,
    edges: &[EdgeRow],
) -> Result<()> {
    tx.execute(
        "DELETE FROM code_edges
         WHERE src_file = ?1 AND (?2 = '' OR NOT EXISTS (
             SELECT 1 FROM code_nodes n WHERE n.id = code_edges.src_id AND n.project != ?2
         ))",
        params![src_file, project],
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
    in_tx(conn, |tx| {
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
        Ok(())
    })
}

/// Retire `code_nodes` rows for `(project, file)` whose id is not in
/// `seen_ids` after a fresh extraction. Extraction has REPLACE
/// semantics per file (D6): a node absent from the current fragment
/// (renamed, deleted, or its `kind` changed) must not survive as a
/// stale row with a stale span. No retired/tombstone column exists on
/// `code_nodes` (checked `migrations.rs`), so this hard-deletes — but
/// ONLY rows scoped to this exact (project, file), and ONLY ids absent
/// from `seen_ids`. `code_node_rank` FK-references `code_nodes.id`
/// with no cascade, so its row for each retired id is deleted first,
/// in the same transaction; `code_node_attribution` has no FK but is
/// cleaned up too so it never dangles.
pub fn retire_missing_nodes(
    conn: &Connection,
    project: &str,
    file: &str,
    seen_ids: &[String],
) -> Result<usize> {
    // Fail-open on an empty observation. A re-extraction that yielded no nodes
    // is far more likely a parse failure — a file caught mid-edit in a
    // syntactically broken state, an unsupported construct, an ast-grep miss —
    // than a file that genuinely lost every symbol it had. Retiring against an
    // empty set would treat every existing node as stale and hard-delete the
    // whole file's `code_node_attribution` provenance, which is the memory this
    // system exists to keep. A stale span is a far cheaper failure than erased
    // provenance, so an empty extraction retires nothing.
    if seen_ids.is_empty() {
        return Ok(0);
    }

    let seen: std::collections::HashSet<&str> = seen_ids.iter().map(String::as_str).collect();

    let mut stmt = conn.prepare("SELECT id FROM code_nodes WHERE project = ?1 AND file = ?2")?;
    let existing: Vec<String> = stmt
        .query_map(params![project, file], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let stale_ids: Vec<String> = existing
        .into_iter()
        .filter(|id| !seen.contains(id.as_str()))
        .collect();

    if stale_ids.is_empty() {
        return Ok(0);
    }

    in_tx(conn, |tx| {
        for id in &stale_ids {
            tx.execute("DELETE FROM code_node_rank WHERE node_id = ?1", params![id])?;
            tx.execute(
                "DELETE FROM code_node_attribution WHERE node_id = ?1",
                params![id],
            )?;
            let gone = tx.execute(
                "DELETE FROM code_nodes WHERE id = ?1 AND project = ?2 AND file = ?3",
                params![id, project, file],
            )?;
            if gone > 0 {
                // The node's edges go with it: left behind they dangle (nothing
                // resolves them, and a resolved one still counts toward its
                // destination's in-degree) until the file is extracted again.
                tx.execute(
                    "DELETE FROM code_edges WHERE src_id = ?1
                       AND NOT EXISTS (SELECT 1 FROM code_nodes WHERE id = ?1)",
                    params![id],
                )?;
            }
        }
        Ok(stale_ids.len())
    })
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

/// `upsert_file_state(.., dirty = false)` that also bumps the file's
/// `graph_gen`, in the same statement: the file's edges changed, so the
/// project-wide resolve has work for it. The last write of an extraction.
pub fn upsert_file_state_pending(
    conn: &Connection,
    project: &str,
    file: &str,
    content_hash: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO code_graph_file_state
            (project, file, content_hash, dirty, extracted_at, ast_status, graph_gen)
         VALUES (?1, ?2, ?3, 0, datetime('now'), 'supported', 1)
         ON CONFLICT(project, file) DO UPDATE SET
             content_hash = excluded.content_hash,
             dirty = 0,
             extracted_at = datetime('now'),
             ast_status = 'supported',
             graph_gen = graph_gen + 1",
        params![project, file, content_hash],
    )?;
    Ok(())
}

/// One file's extraction, to be stored as a unit by [`store_extraction`].
pub struct Extraction<'a> {
    pub project: &'a str,
    pub file: &'a str,
    pub content_hash: &'a str,
    /// Nodes the file defines now, already stamped with their repo root.
    pub nodes: &'a [NodeRow],
    /// The transcript chunk of the edit, to attribute changed nodes to.
    pub source_chunk_id: Option<&'a str>,
    /// Only a clean parse may retire nodes and replace edges; `None` for a
    /// partial parse, which must leave both alone.
    pub edges: Option<&'a [EdgeRow]>,
    /// The snapshot (`seq`) this extraction answers: consumed in the same
    /// transaction, and the extraction is not stored at all when a newer
    /// snapshot of the file has been consumed already.
    pub claim: i64,
}

/// The most snapshots one file may have waiting. A drain that is not running
/// must not let a hammered file pile up 256 KB per edit. Past this, an arrival
/// older than the newest queued is dropped and a newer one replaces the newest,
/// so the queue always ends on the file's newest version; only an intermediate
/// edit loses its attribution (`last_chunk_id` credit), never the end state.
pub const MAX_SNAPSHOTS_PER_FILE: i64 = 64;

/// `kind` of a snapshot of a file the extractor cannot parse: the drain records
/// the file-level `ast_status = 'unsupported'` row, the hook writes no graph row.
pub const SNAPSHOT_UNSUPPORTED: &str = "unsupported";
/// `kind` of a snapshot to extract.
pub const SNAPSHOT_EXTRACT: &str = "extract";

/// An edit as a hook saw it, before the drain extracts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Capture time in unix nanoseconds (the order the drain extracts in).
    pub seq: i64,
    pub project: String,
    /// Canonical stored path.
    pub file: String,
    /// The path the tool saw; read from disk only for a legacy row without content.
    pub read_path: String,
    pub kind: String,
    pub content: Option<String>,
    pub conv_id: String,
    pub session_id: String,
    pub source_chunk_id: Option<String>,
    /// The spool file this came from, if it came through the spool.
    pub origin: Option<String>,
}

/// Unix nanoseconds now: the clock `Snapshot::seq` is on. Strictly increasing
/// within a process, so two snapshots one hook takes in a row (a Bash call's
/// files) never share a number, or a spool file name.
pub fn now_ns() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static LAST: AtomicI64 = AtomicI64::new(0);
    let clock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as i64);
    let mut last = LAST.load(Ordering::Relaxed);
    loop {
        let next = clock.max(last + 1);
        match LAST.compare_exchange_weak(last, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(seen) => last = seen,
        }
    }
}

/// Add `s` to the snapshot queue. It takes `s.seq`, or the next free number
/// when another snapshot holds it, and returns the `seq` it got (`s.seq` for a
/// snapshot that was dropped on arrival, see below). Writes no graph row: this
/// is the hook's only graph write.
///
/// Dropped on arrival: a snapshot at or below the file's high-water mark, and,
/// when the file already has [`MAX_SNAPSHOTS_PER_FILE`] queued, one older than
/// the newest queued.
pub fn insert_snapshot(conn: &Connection, s: &Snapshot) -> Result<i64> {
    in_tx(conn, |tx| {
        if let Some(origin) = &s.origin {
            let known: Option<i64> = tx
                .query_row(
                    "SELECT seq FROM code_graph_snapshots WHERE origin = ?1",
                    params![origin],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(seq) = known {
                return Ok(seq);
            }
        }
        // Superseded before it arrived: the file's consumed high-water mark is
        // at or past it (a delayed hook, a spool record replayed after its
        // snapshot was extracted). The extractor would drop it; so does the queue,
        // before anything is evicted for it.
        if s.seq <= snapshot_high_water(tx, &s.project, &s.file)? {
            return Ok(s.seq);
        }
        let (depth, newest): (i64, Option<i64>) = tx.query_row(
            "SELECT COUNT(*), MAX(seq) FROM code_graph_snapshots WHERE project = ?1 AND file = ?2",
            params![s.project, s.file],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if depth >= MAX_SNAPSHOTS_PER_FILE {
            // Full. The cap exists to bound a pile-up behind a dead drain, and
            // what survives it must still end on the file's newest version, so:
            // an arrival older than the newest queued is dropped (a delayed
            // intermediate version; the newest decides the end state), and an
            // arrival newer than it replaces the newest (the one version the
            // drain skips is an intermediate one). The newest is never evicted
            // for anything older.
            if newest.is_some_and(|n| s.seq < n) {
                return Ok(s.seq);
            }
            tx.execute(
                "DELETE FROM code_graph_snapshots WHERE seq =
                     (SELECT MAX(seq) FROM code_graph_snapshots WHERE project = ?1 AND file = ?2)",
                params![s.project, s.file],
            )?;
        }
        let mut seq = s.seq;
        while tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM code_graph_snapshots WHERE seq = ?1)",
            params![seq],
            |r| r.get::<_, bool>(0),
        )? {
            seq += 1;
        }
        tx.execute(
            "INSERT INTO code_graph_snapshots
                (seq, project, file, read_path, kind, content, conv_id, session_id,
                 source_chunk_id, origin)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                seq,
                s.project,
                s.file,
                s.read_path,
                s.kind,
                s.content,
                s.conv_id,
                s.session_id,
                s.source_chunk_id,
                s.origin
            ],
        )?;
        Ok(seq)
    })
}

/// The transcript chunk a conversation's newest edit belongs to: its latest
/// chunk. `None` before the transcript has been imported.
pub fn latest_chunk_id(conn: &Connection, conv_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT id FROM chunks WHERE conversation_id = ?1
             ORDER BY COALESCE(seq, -1) DESC, rowid DESC LIMIT 1",
            params![conv_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Every queued snapshot, in the order to extract them.
pub fn queued_snapshots(conn: &Connection) -> Result<Vec<Snapshot>> {
    let mut stmt = conn.prepare(
        "SELECT seq, project, file, read_path, kind, content, conv_id, session_id,
                source_chunk_id, origin
         FROM code_graph_snapshots ORDER BY seq",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Snapshot {
            seq: r.get(0)?,
            project: r.get(1)?,
            file: r.get(2)?,
            read_path: r.get(3)?,
            kind: r.get(4)?,
            content: r.get(5)?,
            conv_id: r.get(6)?,
            session_id: r.get(7)?,
            source_chunk_id: r.get(8)?,
            origin: r.get(9)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Whether any snapshot waits for the drain.
pub fn has_queued_snapshots(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM code_graph_snapshots)",
        [],
        |r| r.get(0),
    )?)
}

/// Whether a snapshot of a file in `project` waits for the drain.
pub fn has_queued_snapshots_for(conn: &Connection, project: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM code_graph_snapshots WHERE project = ?1)",
        params![project],
        |r| r.get(0),
    )?)
}

/// The highest snapshot `seq` consumed for the file (0 before any).
pub fn snapshot_high_water(conn: &Connection, project: &str, file: &str) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT seq FROM code_graph_snap_hw WHERE project = ?1 AND file = ?2",
            params![project, file],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// Whether the live hook path has touched `file` since `since_ns`: a snapshot
/// of it is waiting, or one captured after that time has been consumed. A
/// historical replay that read the file earlier must not write over it.
pub fn live_since(conn: &Connection, project: &str, file: &str, since_ns: i64) -> Result<bool> {
    if snapshot_high_water(conn, project, file)? > since_ns {
        return Ok(true);
    }
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM code_graph_snapshots WHERE project = ?1 AND file = ?2)",
        params![project, file],
        |r| r.get(0),
    )?)
}

/// Take snapshot `seq` off the queue and advance the file's high-water mark.
/// `true` when it is fresh, i.e. nothing at or after it has been consumed for
/// this file: a `false` snapshot arrived late (or is a replay) and must not be
/// stored.
pub fn consume_snapshot(conn: &Connection, project: &str, file: &str, seq: i64) -> Result<bool> {
    in_tx(conn, |tx| {
        let seen = snapshot_high_water(tx, project, file)?;
        tx.execute(
            "DELETE FROM code_graph_snapshots WHERE seq = ?1 AND project = ?2 AND file = ?3",
            params![seq, project, file],
        )?;
        if seq <= seen {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO code_graph_snap_hw (project, file, seq) VALUES (?1, ?2, ?3)
             ON CONFLICT(project, file) DO UPDATE SET seq = excluded.seq",
            params![project, file, seq],
        )?;
        Ok(true)
    })
}

/// Store an extraction in ONE transaction: the snapshot it answers is consumed,
/// then node upserts, retirement of the symbols the file lost, the file's edge
/// replacement, and the `graph_gen` bump that queues the project-wide resolve.
/// A process killed anywhere in it leaves the graph and the queue as they were;
/// there is no state with new edges and an unchanged generation.
///
/// Returns `false`, storing nothing, when a snapshot of the file at or after
/// `x.claim` was consumed already.
pub fn store_extraction(conn: &Connection, x: &Extraction<'_>) -> Result<bool> {
    in_tx(conn, |tx| {
        if !consume_snapshot(tx, x.project, x.file, x.claim)? {
            return Ok(false);
        }
        for node in x.nodes {
            let changed =
                get_node(tx, &node.id)?.is_none_or(|stored| stored.body_hash != node.body_hash);
            upsert_node(tx, node)?;
            if changed {
                match x.source_chunk_id {
                    Some(chunk_id) => set_last_chunk_id(tx, &node.id, chunk_id)?,
                    // This edit changed the body and its chunk is unknown (the
                    // hook spooled it without a database, or the transcript is
                    // not imported yet). The credit left on the node is an
                    // earlier edit's, which no longer describes the body: unknown
                    // is NULL, not that chunk.
                    None => tx
                        .execute(
                            "UPDATE code_nodes SET last_chunk_id = NULL WHERE id = ?1",
                            params![node.id],
                        )
                        .map(drop)?,
                }
            }
        }
        if let Some(edges) = x.edges {
            let seen: Vec<String> = x.nodes.iter().map(|n| n.id.clone()).collect();
            retire_missing_nodes(tx, x.project, x.file, &seen)?;
            replace_file_edges(tx, x.project, x.file, edges)?;
        }
        upsert_file_state_pending(tx, x.project, x.file, x.content_hash)?;
        Ok(true)
    })
}

/// Consume an `unsupported` snapshot: record the file-level provenance row.
pub fn store_unsupported(conn: &Connection, s: &Snapshot) -> Result<()> {
    in_tx(conn, |tx| {
        if consume_snapshot(tx, &s.project, &s.file, s.seq)? {
            mark_file_unsupported(tx, &s.project, &s.file)?;
        }
        Ok(())
    })
}

/// Files whose edges changed since the last resolve that covered them:
/// `(project, file, graph_gen)`, oldest extraction first.
pub fn pending_resolve_files(conn: &Connection) -> Result<Vec<(String, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT project, file, graph_gen FROM code_graph_file_state
         WHERE graph_gen > resolved_gen
         ORDER BY project, extracted_at",
    )?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// [`pending_resolve_files`] for one project.
pub fn pending_resolve_files_for(
    conn: &Connection,
    project: &str,
) -> Result<Vec<(String, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT project, file, graph_gen FROM code_graph_file_state
         WHERE project = ?1 AND graph_gen > resolved_gen
         ORDER BY extracted_at",
    )?;
    let rows = stmt.query_map(params![project], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Record that a resolve covered `file` as of generation `seen`. Compare and
/// set: a file extracted again since `seen` keeps its newer `graph_gen` and
/// stays pending. Returns whether the file was settled.
pub fn settle_resolved_gen(
    conn: &Connection,
    project: &str,
    file: &str,
    seen: i64,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE code_graph_file_state SET resolved_gen = ?3
         WHERE project = ?1 AND file = ?2 AND graph_gen = ?3 AND resolved_gen < ?3",
        params![project, file, seen],
    )?;
    Ok(changed == 1)
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

/// Distinct stored code-graph project namespaces, ordered for determinism.
pub fn project_names(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT project FROM code_nodes ORDER BY project")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Resolve a `symbol or id` to candidate node ids: exact id match first, else by name.
fn family_projects(conn: &Connection, project: &str) -> Result<Vec<String>> {
    let mut projects = project_names(conn)?;
    if !project.is_empty() {
        projects.retain(|candidate| {
            crate::search::cross_project::same_project_family(project, candidate)
        });
        if !projects.iter().any(|candidate| candidate == project) {
            projects.push(project.to_string());
        }
    }
    Ok(projects)
}

fn project_is_in(projects: &[String], candidate: &str) -> bool {
    projects.is_empty() || projects.iter().any(|project| project == candidate)
}

fn nodes_by_name_in_projects(
    conn: &Connection,
    name: &str,
    projects: &[String],
    limit: usize,
) -> Result<Vec<NodeRow>> {
    let nodes = nodes_by_name(conn, name, "", i64::MAX as usize)?;
    Ok(nodes
        .into_iter()
        .filter(|node| project_is_in(projects, &node.project))
        .take(limit)
        .collect())
}

fn resolve_target_ids_in_projects(
    conn: &Connection,
    name_or_id: &str,
    projects: &[String],
) -> Result<Vec<String>> {
    if get_node(conn, name_or_id)?.is_some() {
        return Ok(vec![name_or_id.to_string()]);
    }
    Ok(
        nodes_by_name_in_projects(conn, name_or_id, projects, i64::MAX as usize)?
            .into_iter()
            .map(|n| n.id)
            .collect(),
    )
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
    let projects = family_projects(conn, project)?;
    query_callers_in_projects(conn, name_or_id, &projects, limit)
}

/// Family-scoped variant used by code-graph queries and regression fixtures.
pub fn query_callers_in_projects(
    conn: &Connection,
    name_or_id: &str,
    projects: &[String],
    limit: usize,
) -> Result<Vec<NodeRow>> {
    if get_node(conn, name_or_id)?.is_some_and(|node| !project_is_in(projects, &node.project)) {
        return Ok(Vec::new());
    }
    let mut targets = resolve_target_ids_in_projects(conn, name_or_id, projects)?;
    // Also match the unresolved placeholder form keyed on the bare name.
    let bare = name_or_id.rsplit("::").next().unwrap_or(name_or_id);
    let placeholder = format!("name:{bare}");

    if targets.is_empty() {
        targets.push(placeholder.clone());
    } else {
        targets.push(placeholder);
    }

    let placeholders: Vec<String> = (0..targets.len()).map(|i| format!("?{}", i + 1)).collect();
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
         GROUP BY s.id
         ORDER BY s.id",
        placeholders.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let p: Vec<&dyn rusqlite::types::ToSql> = targets
        .iter()
        .map(|target| target as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt.query_map(p.as_slice(), |row| {
        let mut node = row_to_node(row)?;
        let any_resolved: i64 = row.get(node_col_count)?;
        node.name_only = any_resolved == 0;
        Ok(node)
    })?;
    let rows = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|node| project_is_in(projects, &node.project))
        .take(limit)
        .collect())
}

/// What `node_id` calls — outbound `calls` edges. Resolved edges return the real
/// dst node; unresolved `name:<x>` placeholders return a synthetic node.
pub fn query_callees(conn: &Connection, node_id: &str, limit: usize) -> Result<Vec<NodeRow>> {
    let projects = match get_node(conn, node_id)? {
        Some(node) => family_projects(conn, &node.project)?,
        None => Vec::new(),
    };
    query_callees_in_projects(conn, node_id, &projects, limit)
}

/// Return callees of the selected definition, filtering resolved destinations
/// to the supplied project family.
pub fn query_callees_in_projects(
    conn: &Connection,
    node_id: &str,
    projects: &[String],
    limit: usize,
) -> Result<Vec<NodeRow>> {
    if get_node(conn, node_id)?.is_some_and(|node| !project_is_in(projects, &node.project)) {
        return Ok(Vec::new());
    }
    let source_ids = [node_id.to_string()];
    let placeholders: Vec<String> = (0..source_ids.len())
        .map(|index| format!("?{}", index + 1))
        .collect();
    let cols = NODE_COLS
        .split(", ")
        .map(|c| format!("d.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT e.dst_id, e.resolved, {cols} FROM code_edges e
         LEFT JOIN code_nodes d ON d.id = e.dst_id
         WHERE e.kind = 'calls' AND e.src_id IN ({})
         ORDER BY e.dst_id",
        placeholders.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> = source_ids
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt.query_map(params.as_slice(), |row| {
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
    let rows = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    let mut seen = HashSet::new();
    Ok(rows
        .into_iter()
        .filter(|node| {
            (node.kind == "unresolved" || project_is_in(projects, &node.project))
                && seen.insert(node.id.clone())
        })
        .take(limit)
        .collect())
}

/// 1-hop neighbours (both directions) of `node_id`, optional edge-kind filter.
pub fn query_neighbors(
    conn: &Connection,
    node_id: &str,
    kind_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<NeighborEdge>> {
    let projects = match get_node(conn, node_id)? {
        Some(node) => family_projects(conn, &node.project)?,
        None => Vec::new(),
    };
    query_neighbors_in_projects(conn, node_id, &projects, kind_filter, limit)
}

/// Return one-hop edges for a selected definition within a project family.
/// Outbound edges stay anchored to that exact definition. Inbound edges also
/// include same-named definitions from alias namespaces, but never other
/// same-named definitions from the selected definition's own project.
pub fn query_neighbors_in_projects(
    conn: &Connection,
    node_id: &str,
    projects: &[String],
    kind_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<NeighborEdge>> {
    if get_node(conn, node_id)?.is_some_and(|node| !project_is_in(projects, &node.project)) {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let kf = kind_filter.unwrap_or("");
    let selected = get_node(conn, node_id)?;
    let inbound_target_ids = match selected.as_ref() {
        Some(node) => {
            let mut ids = vec![node.id.clone()];
            ids.extend(
                nodes_by_name_in_projects(conn, &node.name, projects, i64::MAX as usize)?
                    .into_iter()
                    .filter(|candidate| candidate.project != node.project)
                    .map(|candidate| candidate.id),
            );
            ids
        }
        None => vec![node_id.to_string()],
    };
    let inbound_placeholders: Vec<String> = (0..inbound_target_ids.len())
        .map(|index| format!("?{}", index + 2))
        .collect();

    // Outbound: node_id -> other (only resolved edges have a real other node).
    let out_cols = NODE_COLS
        .split(", ")
        .map(|c| format!("d.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let out_sql = format!(
        "SELECT e.kind, e.resolved, {out_cols} FROM code_edges e
         JOIN code_nodes d ON d.id = e.dst_id
         WHERE (?1 = '' OR e.kind = ?1) AND e.src_id = ?2
         ORDER BY e.kind, d.id",
    );
    {
        let mut stmt = conn.prepare(&out_sql)?;
        let rows = stmt.query_map(params![kf, node_id], |row| {
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
            let edge = r?;
            if project_is_in(projects, &edge.node.project) {
                out.push(edge);
            }
            if out.len() == limit {
                break;
            }
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
         WHERE (?1 = '' OR e.kind = ?1) AND e.dst_id IN ({})
         ORDER BY e.kind, s.id",
        inbound_placeholders.join(", ")
    );
    {
        let mut stmt = conn.prepare(&in_sql)?;
        let mut params: Vec<&dyn rusqlite::types::ToSql> =
            Vec::with_capacity(inbound_target_ids.len() + 1);
        params.push(&kf);
        params.extend(
            inbound_target_ids
                .iter()
                .map(|id| id as &dyn rusqlite::types::ToSql),
        );
        let rows = stmt.query_map(params.as_slice(), |row| {
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
        let mut inbound = 0;
        for r in rows {
            let edge = r?;
            if project_is_in(projects, &edge.node.project) {
                out.push(edge);
                inbound += 1;
            }
            if inbound == limit {
                break;
            }
        }
    }

    let mut seen = HashSet::new();
    out.retain(|edge| {
        seen.insert((
            edge.direction.clone(),
            edge.edge_kind.clone(),
            edge.node.id.clone(),
        ))
    });
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

/// What the prompt hook shows for a file the prompt names: the first symbols
/// and callers of [`file_ledger`], nothing else.
#[derive(Debug, Clone, Default)]
pub struct FileSlice {
    pub symbols: Vec<NodeRow>,
    /// (caller_name, caller_file), resolved `calls` edges only.
    pub callers: Vec<(String, String)>,
}

/// The two statements of [`file_slice`]: symbols, then callers. Parameters:
/// `?1` the file as named, `?2` its `%`-suffix pattern, `?3` the project or
/// `''`, `?4` its last path component, `?5` the row limit. `?4` is matched
/// against the key of `idx_code_nodes_basename`, so a statement reads only
/// the nodes of files with that name; the `LIKE '%…'` test alone cannot use
/// an index and read every node and every `calls` edge on each prompt.
pub(crate) fn file_slice_sql() -> (String, String) {
    let base = super::migrations::basename_sql("file");
    let symbols = format!(
        "SELECT {NODE_COLS} FROM code_nodes
         WHERE {base} = ?4 AND (file = ?1 OR file LIKE ?2) AND (?3 = '' OR project = ?3)
           AND kind != 'module'
         ORDER BY kind, name, id LIMIT ?5"
    );
    let base = super::migrations::basename_sql("d.file");
    let callers = format!(
        "SELECT DISTINCT s.name, s.file FROM code_nodes d
         CROSS JOIN code_edges e ON e.dst_id = d.id
         CROSS JOIN code_nodes s ON s.id = e.src_id
         WHERE {base} = ?4 AND (d.file = ?1 OR d.file LIKE ?2) AND (?3 = '' OR d.project = ?3)
           AND e.kind = 'calls' AND e.resolved = 1
         ORDER BY s.file, s.name LIMIT ?5"
    );
    (symbols, callers)
}

/// The first `limit` symbols and callers of `file`, in [`file_ledger`]'s
/// order, read through the file-name index. A stored file matches when its
/// last path component is the named file's and it ends with the named path:
/// `stop.rs` and `hooks/stop.rs` find `/repo/src/hooks/stop.rs`, but
/// `nonstop.rs` is another file, and the name is compared case for case.
pub fn file_slice(conn: &Connection, project: &str, file: &str, limit: usize) -> Result<FileSlice> {
    let suffix = format!("%{file}");
    let base = file.rsplit('/').next().unwrap_or(file);
    let limit = limit as i64;
    let (symbols_sql, callers_sql) = file_slice_sql();
    let symbols = conn
        .prepare(&symbols_sql)?
        .query_map(params![file, suffix, project, base, limit], row_to_node)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let callers = conn
        .prepare(&callers_sql)?
        .query_map(params![file, suffix, project, base, limit], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(FileSlice { symbols, callers })
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
        "SELECT repo_root FROM code_nodes WHERE file = ?1 AND repo_root IS NOT NULL
         ORDER BY id LIMIT 1",
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

/// The five `code_nodes` columns witness binding reads: where a symbol lives,
/// its bare name and the two conversations it is attributed to.
pub struct WitnessNode {
    pub project: String,
    pub file: String,
    pub name: String,
    pub first_conv_id: String,
    pub last_conv_id: String,
}

/// [`nodes_for_conversations`] narrowed to [`WitnessNode`]: the same rows in the
/// same order (`ORDER BY id`), without the eight columns the binding never
/// reads. One busy conversation holds tens of thousands of nodes, and building
/// a whole `NodeRow` for each cost more than the lookup that found them.
pub fn witness_nodes_for_conversations(
    conn: &Connection,
    conversation_ids: &[String],
) -> Result<Vec<WitnessNode>> {
    if conversation_ids.is_empty() {
        return Ok(Vec::new());
    }
    const BATCH: usize = 400;
    let mut out = Vec::new();
    for batch in conversation_ids.chunks(BATCH) {
        let placeholders: Vec<String> = (1..=batch.len()).map(|i| format!("?{i}")).collect();
        let in_list = placeholders.join(", ");
        let sql = format!(
            "SELECT project, file, name, first_conv_id, last_conv_id FROM code_nodes
             WHERE kind != 'module'
               AND (first_conv_id IN ({in_list}) OR last_conv_id IN ({in_list}))
             ORDER BY id"
        );
        let params: Vec<&dyn rusqlite::types::ToSql> = batch
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok(WitnessNode {
                project: row.get(0)?,
                file: row.get(1)?,
                name: row.get(2)?,
                first_conv_id: row.get(3)?,
                last_conv_id: row.get(4)?,
            })
        })?;
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
#[path = "codegraph_tests.rs"]
mod tests;
