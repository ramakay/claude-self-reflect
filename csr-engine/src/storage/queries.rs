use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use chrono;
use rusqlite::{params, Connection, OptionalExtension};

use crate::import::{ConversationChunk, CsrSuppressionStats};
use crate::provenance::{ChunkProvenance, Speaker};

/// Upsert provenance for a chunk (who authored it, source conv, supersession).
pub fn insert_chunk_provenance(
    conn: &Connection,
    chunk_id: &str,
    prov: &ChunkProvenance,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO chunk_provenance (chunk_id, author, source_conv_id, supersedes)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            chunk_id,
            prov.author.as_str(),
            prov.source_conv_id,
            prov.supersedes,
        ],
    )?;
    Ok(())
}

/// Upsert a derivation-ledger entry (Pillar 1). `times_reused` is preserved on
/// conflict so a re-import never resets reuse counts.
pub fn upsert_ledger_entry(conn: &Connection, e: &crate::ledger::LedgerEntry) -> Result<()> {
    conn.execute(
        "INSERT INTO derivation_ledger
             (id, content, anchor, cost_bucket, inferability, confidence, times_reused, repo, branch, user)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id, repo, branch, user) DO UPDATE SET
             content=excluded.content, anchor=excluded.anchor,
             cost_bucket=excluded.cost_bucket, inferability=excluded.inferability,
             confidence=excluded.confidence",
        rusqlite::params![
            e.id,
            e.content,
            e.anchor,
            e.cost_bucket.as_str(),
            e.inferability,
            e.confidence,
            e.times_reused as i64,
            e.scope.repo,
            e.scope.branch,
            e.scope.user,
        ],
    )?;
    Ok(())
}

/// Fetch ledger entries for an exact {repo, branch, user} scope, newest first.
pub fn get_ledger_entries(
    conn: &Connection,
    scope: &crate::ledger::Scope,
    limit: i64,
) -> Result<Vec<crate::ledger::LedgerEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, content, anchor, cost_bucket, inferability, confidence, times_reused,
                repo, branch, user
         FROM derivation_ledger
         WHERE repo = ?1 AND branch = ?2 AND user = ?3
         ORDER BY created_at DESC, id DESC LIMIT ?4",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![scope.repo, scope.branch, scope.user, limit],
        |r| {
            Ok(crate::ledger::LedgerEntry {
                id: r.get(0)?,
                content: r.get(1)?,
                anchor: r.get(2)?,
                cost_bucket: crate::ledger::CostBucket::from_str_lossy(&r.get::<_, String>(3)?),
                inferability: r.get(4)?,
                confidence: r.get(5)?,
                times_reused: r.get::<_, i64>(6)? as u32,
                scope: crate::ledger::Scope {
                    repo: r.get(7)?,
                    branch: r.get(8)?,
                    user: r.get(9)?,
                },
            })
        },
    )?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Increment the reuse counter for a ledger entry (governor signal, Pillar 4).
pub fn increment_ledger_reuse(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE derivation_ledger SET times_reused = times_reused + 1 WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Fetch provenance for a chunk, if any. Unknown author tokens degrade to
/// `ToolResult` (non-authoritative) rather than failing the read.
pub fn get_chunk_provenance(conn: &Connection, chunk_id: &str) -> Result<Option<ChunkProvenance>> {
    let mut stmt = conn.prepare(
        "SELECT author, source_conv_id, supersedes FROM chunk_provenance WHERE chunk_id = ?1",
    )?;
    let mut rows = stmt.query(params![chunk_id])?;
    if let Some(row) = rows.next()? {
        let author_str: String = row.get(0)?;
        Ok(Some(ChunkProvenance {
            author: author_str.parse().unwrap_or(Speaker::ToolResult),
            source_conv_id: row.get(1)?,
            supersedes: row.get(2)?,
        }))
    } else {
        Ok(None)
    }
}

/// A reflection row: (id, content, tags, timestamp).
pub type ReflectionRow = (String, String, Vec<String>, String);

/// A session_registry row projection: (project, first_ts, last_ts). Timestamps are
/// nullable in the schema (registry rows can be created before a session closes).
pub type SessionWindowRow = (String, Option<String>, Option<String>);

/// Aggregated session info for timeline display.
/// JOINs chunk data with enrichment reflections for rich context.
pub struct SessionInfo {
    pub conversation_id: String,
    pub project_name: String,
    pub timestamp: String,
    pub total_messages: usize,
    pub chunk_count: usize,
    pub summary: Option<String>,
    pub enrichment: Option<String>,
}

/// Serialize a f32 vector to bytes (little-endian).
fn vec_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Deserialize bytes back to f32 vector.
fn bytes_to_vec(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

pub fn insert_chunk(conn: &Connection, chunk: &ConversationChunk, embedding: &[f32]) -> Result<()> {
    insert_chunk_with_source(conn, chunk, embedding, "conversation")
}

/// `source` is a storage-level attribute ('conversation' | 'plan'), deliberately NOT a
/// `ConversationChunk` field: ~45 construction sites would churn for a value only the
/// aux-source importers set. Readers that need it distinguish plan chunks by the
/// `conversation_id` prefix `plan:` instead.
pub fn insert_chunk_with_source(
    conn: &Connection,
    chunk: &ConversationChunk,
    embedding: &[f32],
    source: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO chunks (id, conversation_id, project_name, timestamp, content, message_count, summary, seq, is_sidechain, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            chunk.id,
            chunk.conversation_id,
            chunk.project_name,
            chunk.timestamp,
            chunk.content,
            chunk.message_count as i64,
            chunk.summary,
            chunk.seq as i64,
            chunk.is_sidechain as i64,
            source,
        ],
    )?;

    conn.execute(
        "INSERT OR REPLACE INTO chunk_embeddings (chunk_id, embedding) VALUES (?1, ?2)",
        params![chunk.id, vec_to_bytes(embedding)],
    )?;

    // Insert into FTS index
    conn.execute(
        "INSERT OR REPLACE INTO chunks_fts (rowid, content) VALUES (
            (SELECT rowid FROM chunks WHERE id = ?1), ?2
        )",
        params![chunk.id, chunk.content],
    )?;

    Ok(())
}

/// Read back a chunk's storage-level `source` ('conversation' | 'plan' | ...). The
/// column is deliberately absent from `ConversationChunk` (see `insert_chunk_with_source`),
/// so aux-source adapter tests need an explicit read path to assert on it.
pub fn get_chunk_source(conn: &Connection, id: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT source FROM chunks WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Repair sidechain rows imported by the recursive watcher before it understood
/// the nested path layout. Additive-safe and idempotent; called only by explicit
/// sidechain discovery/reimport, never by the normal storage startup path.
pub fn rescope_sidechain_conversation(
    conn: &mut Connection,
    conversation_id: &str,
    project_name: &str,
    parent_conversation_id: &str,
) -> Result<()> {
    let needs_repair = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM chunks c
             LEFT JOIN chunk_provenance p ON p.chunk_id = c.id
             WHERE c.conversation_id = ?1
               AND (c.project_name IS NOT ?2
                    OR c.source IS NOT 'sidechain'
                    OR c.is_sidechain != 1
                    OR p.source_conv_id IS NOT ?3)
         )",
        params![conversation_id, project_name, parent_conversation_id],
        |row| row.get::<_, i64>(0),
    )? != 0;
    if !needs_repair {
        return Ok(());
    }
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE chunks SET project_name = ?2, source = 'sidechain', is_sidechain = 1
         WHERE conversation_id = ?1",
        params![conversation_id, project_name],
    )?;
    tx.execute(
        "INSERT INTO chunk_provenance (chunk_id, author, source_conv_id, supersedes)
         SELECT c.id, COALESCE(p.author, 'assistant'), ?2, p.supersedes
         FROM chunks c LEFT JOIN chunk_provenance p ON p.chunk_id = c.id
         WHERE c.conversation_id = ?1
         ON CONFLICT(chunk_id) DO UPDATE SET source_conv_id = excluded.source_conv_id",
        params![conversation_id, parent_conversation_id],
    )?;
    tx.commit()?;
    Ok(())
}

/// Delete a whole conversation's chunks + their embeddings, FTS rows, and provenance
/// edges. Aux-source adapters (plans, tasks, ...) reimport a whole document on content
/// change, and the document can shrink — overwriting by deterministic chunk id alone
/// would leave stale tail chunks (and their embeddings) orphaned in search forever, so
/// a full wipe-then-rebuild is required for idempotent reimport.
pub fn delete_chunks_for_conversation(conn: &Connection, conversation_id: &str) -> Result<()> {
    let mut stmt = conn.prepare("SELECT id FROM chunks WHERE conversation_id = ?1")?;
    let ids: Vec<String> = stmt
        .query_map(params![conversation_id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);

    for id in &ids {
        // chunks_fts has no FK on chunks.id (it's rowid-addressed), so its row must be
        // dropped before the owning chunks row disappears and the rowid lookup goes stale.
        conn.execute(
            "DELETE FROM chunks_fts WHERE rowid = (SELECT rowid FROM chunks WHERE id = ?1)",
            params![id],
        )?;
        conn.execute(
            "DELETE FROM chunk_embeddings WHERE chunk_id = ?1",
            params![id],
        )?;
        conn.execute(
            "DELETE FROM chunk_provenance WHERE chunk_id = ?1",
            params![id],
        )?;
    }
    conn.execute(
        "DELETE FROM chunks WHERE conversation_id = ?1",
        params![conversation_id],
    )?;
    Ok(())
}

pub fn load_all_chunk_vectors(conn: &Connection) -> Result<Vec<(String, Vec<f32>)>> {
    let mut stmt = conn.prepare("SELECT chunk_id, embedding FROM chunk_embeddings")?;
    let rows = stmt.query_map([], |row| {
        let id: String = row.get(0)?;
        let bytes: Vec<u8> = row.get(1)?;
        Ok((id, bytes_to_vec(&bytes)))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

pub fn get_chunks_by_ids(conn: &Connection, ids: &[String]) -> Result<Vec<ConversationChunk>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, project_name, timestamp, content, message_count, summary, is_sidechain
         FROM chunks WHERE id = ?1",
    )?;
    let mut chunks = Vec::new();
    for id in ids {
        let mut rows = stmt.query_map(params![id], row_to_chunk)?;
        if let Some(row) = rows.next() {
            chunks.push(row?);
        }
    }
    Ok(chunks)
}

/// Like [`get_chunks_by_ids`], but resolves each chunk's true author via a
/// `LEFT JOIN` on `chunk_provenance` instead of defaulting every chunk to
/// `Speaker::ToolResult`. `get_chunks_by_ids` intentionally does not carry
/// author (it's cheaper and most callers don't need it — reinstatement graph
/// walks, MCP display, eval metadata); this variant exists for callers that
/// filter on `ConversationChunk::author`, e.g. ratification's `build_digest`,
/// which prioritizes `Speaker::User` turns. Chunks with no provenance row, or
/// an unrecognized author token, degrade to `Speaker::ToolResult` — same
/// fallback as [`get_chunk_provenance`].
pub fn get_chunks_by_ids_with_provenance(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<ConversationChunk>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT c.id, c.conversation_id, c.project_name, c.timestamp, c.content,
                c.message_count, c.summary, c.is_sidechain, cp.author
         FROM chunks c LEFT JOIN chunk_provenance cp ON cp.chunk_id = c.id
         WHERE c.id = ?1",
    )?;
    let mut chunks = Vec::new();
    for id in ids {
        let mut rows = stmt.query_map(params![id], row_to_chunk_with_author)?;
        if let Some(row) = rows.next() {
            chunks.push(row?);
        }
    }
    Ok(chunks)
}

pub fn insert_reflection(
    conn: &Connection,
    id: &str,
    content: &str,
    tags: &[String],
    embedding: &[f32],
) -> Result<()> {
    let tags_json = serde_json::to_string(tags)?;
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "INSERT OR REPLACE INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, ?3, ?4)",
        params![id, content, tags_json, now],
    )?;

    conn.execute(
        "INSERT OR REPLACE INTO reflection_embeddings (reflection_id, embedding) VALUES (?1, ?2)",
        params![id, vec_to_bytes(embedding)],
    )?;

    Ok(())
}

pub fn load_all_reflection_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT reflection_id FROM reflection_embeddings")?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    rows.collect::<std::result::Result<Vec<String>, _>>()
        .map_err(|e| anyhow::anyhow!("{}", e))
}

/// Load just the chunk IDs (no vectors) — cheap probe for incremental backfill.
pub fn load_all_chunk_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT chunk_id FROM chunk_embeddings")?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    rows.collect::<std::result::Result<Vec<String>, _>>()
        .map_err(|e| anyhow::anyhow!("{}", e))
}

pub fn load_all_reflection_vectors(conn: &Connection) -> Result<Vec<(String, Vec<f32>)>> {
    let mut stmt = conn.prepare("SELECT reflection_id, embedding FROM reflection_embeddings")?;
    let rows = stmt.query_map([], |row| {
        let id: String = row.get(0)?;
        let bytes: Vec<u8> = row.get(1)?;
        Ok((id, bytes_to_vec(&bytes)))
    })?;

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

pub fn get_reflection_by_id(
    conn: &Connection,
    id: &str,
) -> Result<Option<(String, Vec<String>, String)>> {
    let mut stmt =
        conn.prepare("SELECT content, tags, timestamp FROM reflections WHERE id = ?1")?;
    let mut rows = stmt.query_map(params![id], |row| {
        let content: String = row.get(0)?;
        let tags_json: String = row.get(1)?;
        let timestamp: String = row.get(2)?;
        Ok((content, tags_json, timestamp))
    })?;

    if let Some(row) = rows.next() {
        let (content, tags_json, timestamp) = row?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        Ok(Some((content, tags, timestamp)))
    } else {
        Ok(None)
    }
}

// ─── Project filtering queries ───

/// Get all chunk IDs belonging to a specific project.
pub fn get_chunk_ids_for_project(conn: &Connection, project: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT id FROM chunks WHERE project_name = ?1")?;
    let rows = stmt.query_map(params![project], |row| row.get::<_, String>(0))?;
    let mut ids = Vec::new();
    for row in rows {
        ids.push(row?);
    }
    Ok(ids)
}

/// Get chunks belonging to a specific project, ordered by timestamp descending.
pub fn get_chunks_by_project(
    conn: &Connection,
    project: &str,
    limit: usize,
) -> Result<Vec<ConversationChunk>> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, project_name, timestamp, content, message_count, summary, is_sidechain
         FROM chunks WHERE project_name = ?1 ORDER BY timestamp DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![project, limit as i64], row_to_chunk)?;
    let mut chunks = Vec::new();
    for row in rows {
        chunks.push(row?);
    }
    Ok(chunks)
}

// ─── Temporal queries ───

/// Get recent chunks ordered by timestamp descending.
/// Returns one representative chunk per unique conversation (the most recent chunk),
/// ensuring the limit applies to distinct conversations, not raw chunks.
pub fn get_recent_chunks(
    conn: &Connection,
    limit: usize,
    project: Option<&str>,
) -> Result<Vec<ConversationChunk>> {
    // Pick one representative chunk per conversation (first chunk = best summary)
    // and order by last-activity timestamp.
    let mut stmt;
    let rows = if let Some(p) = project.filter(|p| *p != "all") {
        stmt = conn.prepare(
            "SELECT c.id, c.conversation_id, c.project_name, c.timestamp, c.content, c.message_count, c.summary, c.is_sidechain
             FROM chunks c
             WHERE c.project_name = ?1
               AND c.rowid = (SELECT MIN(c2.rowid) FROM chunks c2 WHERE c2.conversation_id = c.conversation_id)
             ORDER BY c.timestamp DESC LIMIT ?2",
        )?;
        stmt.query_map(params![p, limit as i64], row_to_chunk)?
    } else {
        stmt = conn.prepare(
            "SELECT c.id, c.conversation_id, c.project_name, c.timestamp, c.content, c.message_count, c.summary, c.is_sidechain
             FROM chunks c
             WHERE c.rowid = (SELECT MIN(c2.rowid) FROM chunks c2 WHERE c2.conversation_id = c.conversation_id)
             ORDER BY c.timestamp DESC LIMIT ?1",
        )?;
        stmt.query_map(params![limit as i64], row_to_chunk)?
    };

    let chunks: Vec<ConversationChunk> = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(chunks)
}

/// Get chunks within a time range, optionally filtered by project.
pub fn get_chunks_in_timerange(
    conn: &Connection,
    start: &str,
    end: &str,
    project: Option<&str>,
) -> Result<Vec<ConversationChunk>> {
    let chunks = if let Some(p) = project.filter(|p| *p != "all") {
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, project_name, timestamp, content, message_count, summary, is_sidechain
             FROM chunks WHERE timestamp BETWEEN ?1 AND ?2 AND project_name = ?3
             ORDER BY timestamp DESC",
        )?;
        let rows = stmt.query_map(params![start, end, p], row_to_chunk)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, project_name, timestamp, content, message_count, summary, is_sidechain
             FROM chunks WHERE timestamp BETWEEN ?1 AND ?2
             ORDER BY timestamp DESC",
        )?;
        let rows = stmt.query_map(params![start, end], row_to_chunk)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(chunks)
}

/// Get chunk IDs within a time range (for filtered HNSW search).
pub fn get_chunk_ids_in_timerange(
    conn: &Connection,
    start: &str,
    end: &str,
    project: Option<&str>,
) -> Result<Vec<String>> {
    let ids = if let Some(p) = project.filter(|p| *p != "all") {
        let mut stmt = conn.prepare(
            "SELECT id FROM chunks WHERE timestamp BETWEEN ?1 AND ?2 AND project_name = ?3",
        )?;
        let rows = stmt.query_map(params![start, end, p], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut stmt = conn.prepare("SELECT id FROM chunks WHERE timestamp BETWEEN ?1 AND ?2")?;
        let rows = stmt.query_map(params![start, end], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(ids)
}

// ─── FTS5 full-text search ───

/// Search chunks using FTS5 full-text search (for file path lookup etc.).
pub fn fts5_search(
    conn: &Connection,
    query: &str,
    limit: usize,
    project: Option<&str>,
) -> Result<Vec<ConversationChunk>> {
    // Sanitize for FTS5: split into OR-joined quoted words
    // "Apify runaway cost" → '"apify" OR "runaway" OR "cost"' (matches any word)
    // Quoting prevents hyphens/special chars from being parsed as FTS5 operators
    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
                .collect::<String>()
        })
        .filter(|w| w.len() >= 2)
        .collect();
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let fts_query = words
        .iter()
        .map(|w| format!("\"{}\"", w))
        .collect::<Vec<_>>()
        .join(" OR ");

    let chunks = if let Some(p) = project.filter(|p| *p != "all") {
        let mut stmt = conn.prepare(
            "SELECT c.id, c.conversation_id, c.project_name, c.timestamp, c.content, c.message_count, c.summary, c.is_sidechain
             FROM chunks c
             JOIN chunks_fts fts ON fts.rowid = c.rowid
             WHERE chunks_fts MATCH ?1 AND c.project_name = ?2
             ORDER BY fts.rank
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![fts_query, p, limit as i64], row_to_chunk)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut stmt = conn.prepare(
            "SELECT c.id, c.conversation_id, c.project_name, c.timestamp, c.content, c.message_count, c.summary, c.is_sidechain
             FROM chunks c
             JOIN chunks_fts fts ON fts.rowid = c.rowid
             WHERE chunks_fts MATCH ?1
             ORDER BY fts.rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![fts_query, limit as i64], row_to_chunk)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(chunks)
}

// ─── Reflection tag queries ───

/// Get reflections matching a specific tag substring (for session learnings).
pub fn get_reflections_by_tag(
    conn: &Connection,
    tag: &str,
    limit: usize,
) -> Result<Vec<ReflectionRow>> {
    // Escape LIKE wildcards to prevent injection
    let escaped_tag = tag.replace('%', "\\%").replace('_', "\\_");
    let pattern = format!("%{}%", escaped_tag);
    let mut stmt = conn.prepare(
        "SELECT id, content, tags, timestamp FROM reflections
         WHERE tags LIKE ?1 ESCAPE '\\' ORDER BY timestamp DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![pattern, limit as i64], |row| {
        let id: String = row.get(0)?;
        let content: String = row.get(1)?;
        let tags_json: String = row.get(2)?;
        let timestamp: String = row.get(3)?;
        Ok((id, content, tags_json, timestamp))
    })?;

    let mut results = Vec::new();
    for row in rows {
        let (id, content, tags_json, timestamp) = row?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        results.push((id, content, tags, timestamp));
    }
    Ok(results)
}

/// Like `get_reflections_by_tag` but requires BOTH tags to be present. Used by the
/// session-briefing hook to fetch a project's recent episodes directly
/// (`project_<name>` AND `session_episode`) so the LIMIT applies AFTER both filters
/// — a project can't be starved by its own non-episode reflections crowding a
/// pre-filter window. Caller should still exact-match the project tag (LIKE can
/// substring-match `project_foo` inside `project_foo-bar`).
pub fn get_reflections_by_two_tags(
    conn: &Connection,
    tag_a: &str,
    tag_b: &str,
    limit: usize,
) -> Result<Vec<ReflectionRow>> {
    // Escape backslash FIRST (it's the ESCAPE char), then LIKE wildcards. Wrap the
    // tag in quotes so the pattern matches a whole JSON-array element — otherwise
    // `session_episode` would substring-match a tag like `not_session_episode`.
    let esc = |t: &str| {
        t.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    };
    let pat_a = format!("%\"{}\"%", esc(tag_a));
    let pat_b = format!("%\"{}\"%", esc(tag_b));
    let mut stmt = conn.prepare(
        "SELECT id, content, tags, timestamp FROM reflections
         WHERE tags LIKE ?1 ESCAPE '\\' AND tags LIKE ?2 ESCAPE '\\'
         ORDER BY timestamp DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![pat_a, pat_b, limit as i64], |row| {
        let id: String = row.get(0)?;
        let content: String = row.get(1)?;
        let tags_json: String = row.get(2)?;
        let timestamp: String = row.get(3)?;
        Ok((id, content, tags_json, timestamp))
    })?;

    let mut results = Vec::new();
    for row in rows {
        let (id, content, tags_json, timestamp) = row?;
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        results.push((id, content, tags, timestamp));
    }
    Ok(results)
}

/// Helper: map a row to ConversationChunk.
/// Expects columns: id, conversation_id, project_name, timestamp, content,
/// message_count, summary, is_sidechain.
fn row_to_chunk(row: &rusqlite::Row) -> rusqlite::Result<ConversationChunk> {
    Ok(ConversationChunk {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        project_name: row.get(2)?,
        timestamp: row.get(3)?,
        content: row.get(4)?,
        message_count: row.get::<_, i64>(5)? as usize,
        summary: row.get(6)?,
        // Author is stored separately in chunk_provenance, not the chunks table;
        // reconstructed chunks default to non-authoritative. Live recall reads
        // provenance explicitly via get_chunk_provenance, or use
        // get_chunks_by_ids_with_provenance / row_to_chunk_with_author below.
        author: crate::provenance::Speaker::ToolResult,
        // Sequence is not needed on these read paths; sidechain identity is live
        // search metadata and must survive hydration for parent-origin dedupe.
        seq: 0,
        is_sidechain: row.get::<_, i64>(7)? != 0,
    })
}

/// Helper: map a row to `ConversationChunk` with a resolved author, for queries
/// that `LEFT JOIN chunk_provenance` (8th column: nullable `author` TEXT).
/// See `get_chunks_by_ids_with_provenance`.
fn row_to_chunk_with_author(row: &rusqlite::Row) -> rusqlite::Result<ConversationChunk> {
    let author_str: Option<String> = row.get(8)?;
    let author = author_str
        .and_then(|s| s.parse::<Speaker>().ok())
        .unwrap_or(Speaker::ToolResult);
    Ok(ConversationChunk {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        project_name: row.get(2)?,
        timestamp: row.get(3)?,
        content: row.get(4)?,
        message_count: row.get::<_, i64>(5)? as usize,
        summary: row.get(6)?,
        author,
        // Sequence is not needed on these read paths.
        seq: 0,
        is_sidechain: row.get::<_, i64>(7)? != 0,
    })
}

// ─── Enrichment state queries ───

/// Check if a conversation has been enriched with a specific type.
pub fn is_conversation_enriched(
    conn: &Connection,
    conversation_id: &str,
    enrichment_type: &str,
) -> Result<bool> {
    let mut stmt = conn.prepare(
        "SELECT 1 FROM enrichment_state
         WHERE conversation_id = ?1 AND enrichment_type = ?2 AND status = 'completed'",
    )?;
    Ok(stmt.exists(params![conversation_id, enrichment_type])?)
}

/// Mark enrichment as completed and link to the stored reflection.
pub fn mark_enrichment_completed(
    conn: &Connection,
    conversation_id: &str,
    enrichment_type: &str,
    reflection_id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO enrichment_state (conversation_id, enrichment_type, status, reflection_id, updated_at)
         VALUES (?1, ?2, 'completed', ?3, datetime('now'))
         ON CONFLICT(conversation_id, enrichment_type) DO UPDATE SET
             status = 'completed', reflection_id = ?3, updated_at = datetime('now')",
        params![conversation_id, enrichment_type, reflection_id],
    )?;
    Ok(())
}

/// Mark enrichment as failed with an error message.
pub fn mark_enrichment_failed(
    conn: &Connection,
    conversation_id: &str,
    enrichment_type: &str,
    error: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO enrichment_state (conversation_id, enrichment_type, status, error_message, updated_at)
         VALUES (?1, ?2, 'failed', ?3, datetime('now'))
         ON CONFLICT(conversation_id, enrichment_type) DO UPDATE SET
             status = 'failed', error_message = ?3, updated_at = datetime('now')",
        params![conversation_id, enrichment_type, error],
    )?;
    Ok(())
}

/// Mark enrichment as permanently unavailable (e.g. source JSONL deleted/rotated).
/// Unlike `mark_enrichment_failed`, status `'unavailable'` is NOT re-queued by
/// `get_unenriched_conversations`, so a missing source file stops the retry storm.
pub fn mark_enrichment_unavailable(
    conn: &Connection,
    conversation_id: &str,
    enrichment_type: &str,
    reason: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO enrichment_state (conversation_id, enrichment_type, status, error_message, updated_at)
         VALUES (?1, ?2, 'unavailable', ?3, datetime('now'))
         ON CONFLICT(conversation_id, enrichment_type) DO UPDATE SET
             status = 'unavailable', error_message = ?3, updated_at = datetime('now')",
        params![conversation_id, enrichment_type, reason],
    )?;
    Ok(())
}

/// Delete the enrichment_state row for a conversation + type, making it
/// eligible again for `get_unenriched_conversations` (status IS NULL).
/// No-op (Ok) if no matching row exists.
pub fn reset_enrichment(
    conn: &Connection,
    conversation_id: &str,
    enrichment_type: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM enrichment_state WHERE conversation_id = ?1 AND enrichment_type = ?2",
        params![conversation_id, enrichment_type],
    )?;
    Ok(())
}

/// Get conversations that need enrichment of a given type.
/// Returns (conversation_id, file_path) pairs.
pub fn get_unenriched_conversations(
    conn: &Connection,
    enrichment_type: &str,
    limit: usize,
) -> Result<Vec<(String, String)>> {
    // Find conversations that have been imported (in import_state)
    // but don't have completed enrichment of this type.
    // Uses equality JOIN on conversation_id (not LIKE) for correctness and performance.
    // Ordered by most recent chunk timestamp so recent conversations are enriched first.
    let mut stmt = conn.prepare(
        "SELECT c.conversation_id, i.file_path
         FROM chunks c
         JOIN import_state i ON i.conversation_id = c.conversation_id
         LEFT JOIN enrichment_state e
             ON e.conversation_id = c.conversation_id AND e.enrichment_type = ?1
         WHERE e.status IS NULL OR e.status = 'failed'
         GROUP BY c.conversation_id, i.file_path
         ORDER BY MAX(c.timestamp) DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![enrichment_type, limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Set the Anthropic batch ID and prompt hash for a conversation's AI narrative enrichment.
pub fn set_batch_id(
    conn: &Connection,
    conversation_id: &str,
    batch_id: &str,
    prompt_hash: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO enrichment_state (conversation_id, enrichment_type, status, batch_id, prompt_hash, updated_at)
         VALUES (?1, 'ai_narrative', 'processing', ?2, ?3, datetime('now'))
         ON CONFLICT(conversation_id, enrichment_type) DO UPDATE SET
             status = 'processing', batch_id = ?2, prompt_hash = ?3, updated_at = datetime('now')",
        params![conversation_id, batch_id, prompt_hash],
    )?;
    Ok(())
}

/// Get conversations with a specific batch ID (for result retrieval).
pub fn get_conversations_by_batch(conn: &Connection, batch_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT conversation_id FROM enrichment_state
         WHERE batch_id = ?1 AND enrichment_type = 'ai_narrative'",
    )?;
    let rows = stmt.query_map(params![batch_id], |row| row.get::<_, String>(0))?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Delete a reflection by ID (for layer supersession — Layer 2 replaces Layer 1).
pub fn delete_reflection(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM reflection_embeddings WHERE reflection_id = ?1",
        params![id],
    )?;
    conn.execute("DELETE FROM reflections WHERE id = ?1", params![id])?;
    Ok(())
}

/// Replace all episode anchors for a session (delete-then-insert upsert).
pub fn replace_session_anchors(
    conn: &Connection,
    session_id: &str,
    project: &str,
    anchors: &[crate::extraction::anchors::FunctionAnchor],
) -> Result<()> {
    conn.execute(
        "DELETE FROM episode_anchors WHERE session_id = ?1",
        params![session_id],
    )?;
    let mut stmt = conn.prepare(
        "INSERT INTO episode_anchors (session_id, project, file, node_kind, name, body_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for a in anchors {
        stmt.execute(params![
            session_id,
            project,
            a.file,
            a.node_kind,
            a.name,
            a.body_hash
        ])?;
    }
    Ok(())
}

/// Most-recent-first anchors for a project: `(session_id, anchor)`.
pub fn get_project_anchors(
    conn: &Connection,
    project: &str,
    limit: i64,
) -> Result<Vec<(String, crate::extraction::anchors::FunctionAnchor)>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, file, node_kind, name, body_hash
         FROM episode_anchors WHERE project = ?1
         ORDER BY created_at DESC, id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![project, limit], |r| {
        Ok((
            r.get::<_, String>(0)?,
            crate::extraction::anchors::FunctionAnchor {
                file: r.get(1)?,
                node_kind: r.get(2)?,
                name: r.get(3)?,
                body_hash: r.get(4)?,
            },
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Get the reflection ID for a conversation's enrichment (for supersession).
pub fn get_enrichment_reflection_id(
    conn: &Connection,
    conversation_id: &str,
    enrichment_type: &str,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT reflection_id FROM enrichment_state
         WHERE conversation_id = ?1 AND enrichment_type = ?2 AND status = 'completed'",
    )?;
    let mut rows = stmt.query_map(params![conversation_id, enrichment_type], |row| {
        row.get::<_, Option<String>>(0)
    })?;
    if let Some(row) = rows.next() {
        Ok(row?)
    } else {
        Ok(None)
    }
}

// ─── Session aggregate queries (for timeline display) ───

/// Get recent sessions with aggregated message counts and enrichment data.
/// Returns one entry per conversation, ordered by last activity.
pub fn get_recent_sessions(
    conn: &Connection,
    limit: usize,
    project: Option<&str>,
) -> Result<Vec<SessionInfo>> {
    // Enrichment subquery: COALESCE across enrichment types with priority.
    // Heuristic reflection may be deleted by v3 supersession, so fall through.
    let enrichment_subquery = "COALESCE(
                    (SELECT r.content FROM enrichment_state e
                     JOIN reflections r ON r.id = e.reflection_id
                     WHERE e.conversation_id = c.conversation_id
                       AND e.enrichment_type = 'heuristic' AND e.status = 'completed'
                     LIMIT 1),
                    (SELECT r.content FROM enrichment_state e
                     JOIN reflections r ON r.id = e.reflection_id
                     WHERE e.conversation_id = c.conversation_id
                       AND e.enrichment_type = 'extracted_v3' AND e.status = 'completed'
                     LIMIT 1),
                    (SELECT r.content FROM enrichment_state e
                     JOIN reflections r ON r.id = e.reflection_id
                     WHERE e.conversation_id = c.conversation_id
                       AND e.enrichment_type = 'ai_narrative' AND e.status = 'completed'
                     LIMIT 1)
                ) as enrichment";

    let sql = if project.filter(|p| *p != "all").is_some() {
        format!(
            "SELECT c.conversation_id, c.project_name,
                MAX(c.timestamp) as last_active,
                SUM(c.message_count) as total_messages,
                COUNT(*) as chunk_count,
                (SELECT c2.summary FROM chunks c2
                 WHERE c2.conversation_id = c.conversation_id
                 ORDER BY c2.rowid ASC LIMIT 1) as summary,
                {enrichment_subquery}
         FROM chunks c
         WHERE c.project_name = ?1
         GROUP BY c.conversation_id
         ORDER BY MAX(c.timestamp) DESC LIMIT ?2"
        )
    } else {
        format!(
            "SELECT c.conversation_id, c.project_name,
                MAX(c.timestamp) as last_active,
                SUM(c.message_count) as total_messages,
                COUNT(*) as chunk_count,
                (SELECT c2.summary FROM chunks c2
                 WHERE c2.conversation_id = c.conversation_id
                 ORDER BY c2.rowid ASC LIMIT 1) as summary,
                {enrichment_subquery}
         FROM chunks c
         GROUP BY c.conversation_id
         ORDER BY MAX(c.timestamp) DESC LIMIT ?1"
        )
    };

    let mut stmt = conn.prepare(&sql)?;

    let rows = if let Some(p) = project.filter(|p| *p != "all") {
        stmt.query_map(params![p, limit as i64], row_to_session)?
    } else {
        stmt.query_map(params![limit as i64], row_to_session)?
    };

    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Helper: map a row to SessionInfo.
fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<SessionInfo> {
    Ok(SessionInfo {
        conversation_id: row.get(0)?,
        project_name: row.get(1)?,
        timestamp: row.get(2)?,
        total_messages: row.get::<_, i64>(3)? as usize,
        chunk_count: row.get::<_, i64>(4)? as usize,
        summary: row.get(5)?,
        enrichment: row.get(6)?,
    })
}

/// Get the single most recent session for a project.
/// Used by SessionStart for "CONTINUED FROM" detection and by PromptSubmit for recency boost.
pub fn get_most_recent_session(conn: &Connection, project: &str) -> Result<Option<SessionInfo>> {
    let mut sessions = get_recent_sessions(conn, 1, Some(project))?;
    Ok(sessions.pop())
}

// ─── Import state queries ───

/// Check if a file has been imported AND hasn't changed since.
/// Compares stored file_mtime against current mtime to detect grown conversations.
pub fn is_file_imported(conn: &Connection, path: &Path) -> Result<bool> {
    let path_str = path.to_string_lossy().to_string();
    let mut stmt = conn.prepare("SELECT file_mtime FROM import_state WHERE file_path = ?1")?;

    let stored_mtime: Option<String> = stmt.query_row(params![path_str], |row| row.get(0)).ok();

    let Some(stored) = stored_mtime else {
        return Ok(false); // Never imported
    };

    // Compare against current file modification time
    let current_mtime = file_mtime_str(path);
    if stored != current_mtime {
        return Ok(false); // File has changed — needs re-import
    }

    Ok(true)
}

/// Get file modification time as a string for comparison.
fn file_mtime_str(path: &Path) -> String {
    path.metadata()
        .and_then(|m| m.modified())
        .map(|t| {
            let dt: chrono::DateTime<chrono::Utc> = t.into();
            dt.to_rfc3339()
        })
        .unwrap_or_default()
}

pub fn mark_file_imported(conn: &Connection, path: &Path, chunks: usize) -> Result<()> {
    let path_str = path.to_string_lossy().to_string();
    // Extract conversation_id from filename (stem of the JSONL file)
    let conv_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let mtime = file_mtime_str(path);
    conn.execute(
        "INSERT INTO import_state (
             file_path, conversation_id, chunks_imported, file_mtime,
             csr_tool_blocks_suppressed, csr_hook_wrappers_scrubbed
         )
         VALUES (?1, ?2, ?3, ?4, 0, 0)
         ON CONFLICT(file_path) DO UPDATE SET
             conversation_id = excluded.conversation_id,
             chunks_imported = excluded.chunks_imported,
             file_mtime = excluded.file_mtime",
        params![path_str, conv_id, chunks as i64, mtime],
    )?;
    Ok(())
}

/// Atomically persist import state and apply only newly observed per-file CSR
/// suppression totals. The import-state row is written before counter deltas;
/// any failure rolls both back.
pub(crate) fn mark_file_imported_with_suppression(
    conn: &mut Connection,
    path: &Path,
    chunks: usize,
    suppression: CsrSuppressionStats,
) -> Result<()> {
    let tx = conn.transaction()?;
    let path_str = path.to_string_lossy().to_string();
    let previous = tx
        .query_row(
            "SELECT csr_tool_blocks_suppressed, csr_hook_wrappers_scrubbed
             FROM import_state WHERE file_path = ?1",
            params![path_str],
            |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .optional()?
        .unwrap_or((Some(0), Some(0)));
    let current_tool = suppression.csr_tool_blocks_suppressed as i64;
    let current_wrappers = suppression.csr_hook_wrappers_scrubbed as i64;
    let (persisted_tool, tool_delta) = previous.0.map_or((current_tool, 0), |previous| {
        let persisted = previous.max(current_tool);
        (persisted, persisted - previous)
    });
    let (persisted_wrappers, wrapper_delta) =
        previous.1.map_or((current_wrappers, 0), |previous| {
            let persisted = previous.max(current_wrappers);
            (persisted, persisted - previous)
        });
    let conv_id = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_default();
    let mtime = file_mtime_str(path);

    tx.execute(
        "INSERT OR REPLACE INTO import_state
         (file_path, conversation_id, chunks_imported, file_mtime,
          csr_tool_blocks_suppressed, csr_hook_wrappers_scrubbed)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            path_str,
            conv_id,
            chunks as i64,
            mtime,
            persisted_tool,
            persisted_wrappers
        ],
    )?;

    if tool_delta > 0 {
        let split_value = get_meta(&tx, "csr_tool_blocks_suppressed")?;
        let current = match split_value {
            Some(value) => Some(value),
            None => get_meta(&tx, "csr_self_suppressed")?,
        }
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
        set_meta(
            &tx,
            "csr_tool_blocks_suppressed",
            &(current + tool_delta).to_string(),
        )?;
    }
    if wrapper_delta > 0 {
        let current = get_meta(&tx, "csr_hook_wrappers_scrubbed")?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        set_meta(
            &tx,
            "csr_hook_wrappers_scrubbed",
            &(current + wrapper_delta).to_string(),
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// Read the stored mtime for an import_state row keyed by an arbitrary `file_path`
/// string. Aux-source adapters (plans, tasks, ...) key `import_state` by a synthetic
/// id (e.g. `"plan:<slug>"`) that isn't a real filesystem path, so `is_file_imported`
/// (which stats the path itself) can't be reused — this never touches the filesystem.
pub fn get_import_state_mtime(conn: &Connection, file_path: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT file_mtime FROM import_state WHERE file_path = ?1",
        params![file_path],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Upsert an import_state row with a caller-supplied mtime, for aux-source adapters
/// whose "file_path" key is synthetic. Mirrors `mark_file_imported`'s INSERT OR
/// REPLACE shape but never derives mtime from `Path::metadata` (see
/// `get_import_state_mtime`).
pub fn upsert_import_state_explicit(
    conn: &Connection,
    file_path: &str,
    conversation_id: &str,
    chunks: usize,
    mtime: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO import_state (file_path, conversation_id, chunks_imported, file_mtime) VALUES (?1, ?2, ?3, ?4)",
        params![file_path, conversation_id, chunks as i64, mtime],
    )?;
    Ok(())
}

// ─── Session registry queries (multi-source corpus, v9.4) ───
// The registry itself is written elsewhere (lane/p4-registry); these are read-only
// lookups for correlating an aux-source document (e.g. a plan) to the session whose
// time window contains it.

/// (first_ts, last_ts) for one session, if the registry has a row for it. The registry
/// can lag import, so callers must treat "no row" as "unknown," not "no match."
pub fn get_session_registry_window(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<(Option<String>, Option<String>)>> {
    conn.query_row(
        "SELECT first_ts, last_ts FROM session_registry WHERE session_id = ?1",
        params![session_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(Into::into)
}

/// All (project, first_ts, last_ts) rows in the registry. Small table (one row per
/// session), so callers filter by time window in Rust rather than pushing per-call
/// date arithmetic into SQL.
pub fn list_session_registry_windows(conn: &Connection) -> Result<Vec<SessionWindowRow>> {
    let mut stmt = conn.prepare("SELECT project, first_ts, last_ts FROM session_registry")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

// ─── Backfill queries (for enrichment pipeline repair) ───

/// Get conversation IDs that exist in chunks but have no import_state row.
pub fn get_conversations_missing_import_state(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT conversation_id FROM chunks
         WHERE conversation_id NOT IN (
             SELECT conversation_id FROM import_state WHERE conversation_id IS NOT NULL
         )",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get conversations that need heuristic enrichment, NOT gated by import_state.
/// Returns (conversation_id, project_name) pairs.
pub fn get_conversations_needing_heuristic(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT c.conversation_id, c.project_name FROM chunks c
         LEFT JOIN enrichment_state e
             ON e.conversation_id = c.conversation_id AND e.enrichment_type = 'heuristic'
         WHERE e.status IS NULL OR e.status = 'failed'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get conversations that have V3 or heuristic enrichment but no session_story.
/// Returns (conversation_id, enrichment_type, reflection_id) for each candidate.
pub fn get_conversations_missing_stories(
    conn: &Connection,
    min_messages: usize,
) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT e.conversation_id, e.enrichment_type, e.reflection_id
         FROM enrichment_state e
         WHERE e.enrichment_type IN ('extracted_v3', 'heuristic')
           AND e.status = 'completed'
           AND e.conversation_id NOT IN (
               SELECT conversation_id FROM enrichment_state
               WHERE enrichment_type = 'session_story' AND status = 'completed'
           )
           AND e.conversation_id IN (
               SELECT conversation_id FROM chunks
               GROUP BY conversation_id
               HAVING SUM(message_count) >= ?1
           )
         ORDER BY e.updated_at DESC",
    )?;
    let rows = stmt.query_map(params![min_messages as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get the project name for a conversation from its chunks.
pub fn get_project_for_conversation(
    conn: &Connection,
    conversation_id: &str,
) -> Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT project_name FROM chunks WHERE conversation_id = ?1 LIMIT 1")?;
    let mut rows = stmt.query_map(params![conversation_id], |row| row.get::<_, String>(0))?;
    match rows.next() {
        Some(Ok(name)) => Ok(Some(name)),
        _ => Ok(None),
    }
}

// ─── Status queries (for csr-engine status) ───

/// Count distinct conversations in the database.
/// Total messages across a conversation's imported chunks. Returns 0 when the
/// conversation has no chunks.
pub fn conversation_message_count(conn: &Connection, conversation_id: &str) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COALESCE(SUM(message_count), 0) FROM chunks WHERE conversation_id = ?1",
        params![conversation_id],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

pub fn count_conversations(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT conversation_id) FROM chunks",
        [],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Count distinct projects in the database.
pub fn count_projects(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT project_name) FROM chunks",
        [],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

/// Count imported files.
pub fn count_imported_files(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM import_state", [], |row| row.get(0))?;
    Ok(count as usize)
}

/// Get enrichment breakdown: count of conversations per enrichment type/status.
pub fn get_enrichment_breakdown(conn: &Connection) -> Result<Vec<(String, String, usize)>> {
    let mut stmt = conn.prepare(
        "SELECT enrichment_type, status, COUNT(*) FROM enrichment_state GROUP BY enrichment_type, status",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)? as usize,
        ))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Get the newest chunk timestamp (for staleness reporting).
pub fn get_newest_chunk_timestamp(conn: &Connection) -> Result<Option<String>> {
    let result: Option<String> = conn
        .query_row("SELECT MAX(timestamp) FROM chunks", [], |row| row.get(0))
        .ok();
    Ok(result)
}

/// Get the database file size in bytes.
pub fn get_db_size(conn: &Connection) -> Result<u64> {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    Ok((page_count * page_size) as u64)
}

// ─── Incremental import queries ───

/// Get the number of chunks previously imported for a file (for incremental import).
/// Returns 0 if never imported.
pub fn get_imported_chunk_count(conn: &Connection, path: &Path) -> Result<usize> {
    let path_str = path.to_string_lossy();
    let result: rusqlite::Result<i64> = conn
        .prepare("SELECT chunks_imported FROM import_state WHERE file_path = ?1")?
        .query_row(params![path_str.as_ref()], |row| row.get(0));
    match result {
        Ok(count) => Ok(count as usize),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
        Err(e) => Err(e.into()),
    }
}

/// Get chunk content by chunk ID (for post-compact context injection).
pub fn get_chunk_content(conn: &Connection, id: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT content FROM chunks WHERE id = ?1")?;
    let result: rusqlite::Result<String> = stmt.query_row(params![id], |row| row.get(0));
    match result {
        Ok(content) => Ok(Some(content)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// ─── Count queries (for HNSW persistence staleness detection) ───

/// Fast O(1) count of chunk embeddings for HNSW cache staleness check.
pub fn count_chunk_embeddings(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM chunk_embeddings", [], |row| {
        row.get(0)
    })?;
    Ok(count as usize)
}

/// Fast O(1) count of reflection embeddings for HNSW cache staleness check.
pub fn count_reflection_embeddings(conn: &Connection) -> Result<usize> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM reflection_embeddings", [], |row| {
        row.get(0)
    })?;
    Ok(count as usize)
}

// ─── Meta KV ───

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
    let mut rows = stmt.query([key])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        [key, value],
    )?;
    Ok(())
}

// ─── Narrative usage accounting ───

pub struct NarrativeUsageRow {
    pub call_site: String, // "briefing" | "story"
    pub model: String,     // resolved model id or "unknown"
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub duration_ms: i64,
    pub success: bool,
}

#[derive(Default, serde::Serialize)]
pub struct NarrativeUsageSummary {
    pub calls_today: i64,
    pub tokens_today: i64, // input + output, today (fresh tokens only)
    pub calls_total: i64,
    pub tokens_total: i64,
    pub cache_tokens_today: i64, // cache_read + cache_creation, today
    pub cache_tokens_total: i64, // cache_read + cache_creation, all time
    pub last_model: Option<String>,
}

pub fn record_narrative_usage(conn: &Connection, row: &NarrativeUsageRow) -> Result<()> {
    conn.execute(
        "INSERT INTO narrative_usage
         (call_site, model, input_tokens, output_tokens, cache_read_tokens,
          cache_creation_tokens, duration_ms, success)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            row.call_site,
            row.model,
            row.input_tokens,
            row.output_tokens,
            row.cache_read_tokens,
            row.cache_creation_tokens,
            row.duration_ms,
            row.success as i64,
        ],
    )?;
    Ok(())
}

pub fn narrative_usage_summary(conn: &Connection) -> Result<NarrativeUsageSummary> {
    let (
        calls_total,
        tokens_total,
        cache_tokens_total,
        calls_today,
        tokens_today,
        cache_tokens_today,
    ) = conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(input_tokens + output_tokens), 0),
                COALESCE(SUM(cache_read_tokens + cache_creation_tokens), 0),
                COALESCE(SUM(CASE WHEN ts >= date('now') THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN ts >= date('now') THEN input_tokens + output_tokens ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN ts >= date('now') THEN cache_read_tokens + cache_creation_tokens ELSE 0 END), 0)
         FROM narrative_usage",
        [],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
            ))
        },
    )?;
    let last_model = conn
        .query_row(
            "SELECT model FROM narrative_usage ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(NarrativeUsageSummary {
        calls_today,
        tokens_today,
        calls_total,
        tokens_total,
        cache_tokens_today,
        cache_tokens_total,
        last_model,
    })
}

// ─── Ratification scores ───

pub struct RatificationScoreRow {
    pub conversation_id: String,
    pub score: f32,
    pub acts_json: String,
    pub ledger_refs: Option<String>,
    pub extractor_version: String,
}

pub fn upsert_ratification_score(conn: &Connection, row: &RatificationScoreRow) -> Result<()> {
    conn.execute(
        "INSERT INTO ratification_scores
         (conversation_id, score, acts_json, ledger_refs, extractor_version, extracted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, strftime('%s','now'))
         ON CONFLICT(conversation_id) DO UPDATE SET
             score=?2, acts_json=?3, ledger_refs=?4, extractor_version=?5,
             extracted_at=strftime('%s','now')",
        params![
            row.conversation_id,
            row.score,
            row.acts_json,
            row.ledger_refs,
            row.extractor_version,
        ],
    )?;
    Ok(())
}

pub fn get_ratification_score(conn: &Connection, conversation_id: &str) -> Result<Option<f32>> {
    conn.query_row(
        "SELECT score FROM ratification_scores WHERE conversation_id = ?1",
        params![conversation_id],
        |r| r.get::<_, f32>(0),
    )
    .optional()
    .map_err(Into::into)
}

pub fn get_ratification_scores(conn: &Connection, ids: &[String]) -> Result<HashMap<String, f32>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    const BATCH: usize = 500;
    let mut results = HashMap::new();
    for batch in ids.chunks(BATCH) {
        if batch.is_empty() {
            continue;
        }
        let placeholders = std::iter::repeat_n("?", batch.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT conversation_id, score FROM ratification_scores WHERE conversation_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let bound: Vec<&dyn rusqlite::ToSql> =
            batch.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(bound.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f32>(1)?))
        })?;
        for row in rows {
            let (id, score) = row?;
            results.insert(id, score);
        }
    }
    Ok(results)
}

pub fn ratification_summary(conn: &Connection) -> Result<(i64, f64)> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(AVG(score), 0.0) FROM ratification_scores",
        [],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)),
    )
    .map_err(Into::into)
}

// ─── TAD: Retrieval Event Tracking ───

/// Log a retrieval event (memory was surfaced during a hook).
pub fn log_retrieval_event(
    conn: &Connection,
    memory_id: &str,
    memory_type: &str,
    hook_phase: &str,
    session_id: &str,
) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let id = format!(
        "ret_{}_{}_{}",
        &session_id[..8.min(session_id.len())],
        chrono::Utc::now().timestamp_millis(),
        seq
    );
    conn.execute(
        "INSERT INTO retrieval_events (id, memory_id, memory_type, retrieved_at, hook_phase, session_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            memory_id,
            memory_type,
            chrono::Utc::now().to_rfc3339(),
            hook_phase,
            session_id,
        ],
    )?;
    Ok(())
}

/// Update session outcome for all retrieval events in a session.
pub fn update_session_outcome(conn: &Connection, session_id: &str, outcome: &str) -> Result<usize> {
    let updated = conn.execute(
        "UPDATE retrieval_events SET session_outcome = ?1 WHERE session_id = ?2",
        params![outcome, session_id],
    )?;
    Ok(updated)
}

/// Get retrieval events for a specific memory (for TAD scoring).
pub fn get_retrieval_events_for_memory(
    conn: &Connection,
    memory_id: &str,
) -> Result<Vec<(String, String, String)>> {
    // Returns (retrieved_at, session_outcome, hook_phase)
    let mut stmt = conn.prepare(
        "SELECT retrieved_at, session_outcome, hook_phase
         FROM retrieval_events
         WHERE memory_id = ?1
         ORDER BY retrieved_at DESC
         LIMIT 20",
    )?;
    let rows = stmt.query_map(params![memory_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Batch-fetch typed retrieval events for TAD scoring.
pub fn get_retrieval_events_batch(
    conn: &Connection,
    memory_ids: &[&str],
) -> Result<HashMap<String, Vec<crate::search::decay::RetrievalEvent>>> {
    use crate::search::decay::{RetrievalEvent, SessionOutcome};

    if memory_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let placeholders: Vec<String> = (1..=memory_ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "SELECT memory_id, retrieved_at, session_outcome
         FROM retrieval_events
         WHERE memory_id IN ({})
         ORDER BY retrieved_at DESC",
        placeholders.join(", ")
    );

    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> = memory_ids
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut map: HashMap<String, Vec<RetrievalEvent>> = HashMap::new();
    for row in rows {
        let (mid, ts_str, outcome_str) = row?;
        let retrieved_at = match ts_str.parse::<chrono::DateTime<chrono::Utc>>() {
            Ok(ts) => ts,
            Err(_) => continue,
        };
        let session_outcome = match outcome_str.as_str() {
            "success" => SessionOutcome::Success,
            "failed" => SessionOutcome::Failed,
            _ => SessionOutcome::Neutral,
        };
        map.entry(mid).or_default().push(RetrievalEvent {
            retrieved_at,
            session_outcome,
        });
    }
    Ok(map)
}

// ─── Completion queries (v8.3.0) ───

/// Escape SQLite LIKE wildcards (`%` and `_`) in a prefix string.
fn escape_like(prefix: &str) -> String {
    prefix.replace('%', "\\%").replace('_', "\\_")
}

/// List distinct project names matching a prefix (for MCP completions).
/// Returns up to `limit` results, sorted alphabetically.
pub fn list_project_names(conn: &Connection, prefix: &str, limit: usize) -> Result<Vec<String>> {
    let pattern = format!("{}%", escape_like(prefix));
    let mut stmt = conn.prepare(
        "SELECT DISTINCT project_name FROM chunks
         WHERE project_name LIKE ?1 ESCAPE '\\'
         ORDER BY project_name
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![pattern, limit as i64], |row| row.get(0))?;
    rows.collect::<std::result::Result<Vec<String>, _>>()
        .map_err(|e| anyhow::anyhow!("{}", e))
}

/// List distinct file paths from import_state matching a prefix.
pub fn list_file_paths(conn: &Connection, prefix: &str, limit: usize) -> Result<Vec<String>> {
    let pattern = format!("%{}%", escape_like(prefix));
    let mut stmt = conn.prepare(
        "SELECT DISTINCT file_path FROM import_state
         WHERE file_path LIKE ?1 ESCAPE '\\'
         ORDER BY file_path
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![pattern, limit as i64], |row| {
        row.get::<_, String>(0)
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

// ─── Outcome scoring: retrieval stats rollup ───

/// Compute retrieval stats rollup from events for a specific session.
pub fn update_retrieval_stats_for_session(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO retrieval_stats (memory_id, success_count, failure_count, neutral_count, last_updated)
         SELECT memory_id,
                COALESCE(SUM(CASE WHEN session_outcome = 'success' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN session_outcome IN ('stuck', 'abandoned', 'failed') THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN session_outcome = 'neutral' THEN 1 ELSE 0 END), 0),
                datetime('now')
         FROM retrieval_events
         WHERE memory_id IN (SELECT DISTINCT memory_id FROM retrieval_events WHERE session_id = ?1)
         GROUP BY memory_id",
        params![session_id],
    )?;
    Ok(())
}

/// Batch-fetch outcome stats for scoring. Returns HashMap<memory_id, (success_count, failure_count)>.
/// Only returns entries with total events >= 3 (minimum signal gate).
pub fn get_outcome_stats_batch(
    conn: &Connection,
    memory_ids: &[&str],
) -> Result<HashMap<String, (i64, i64)>> {
    let mut map = HashMap::new();
    if memory_ids.is_empty() {
        return Ok(map);
    }
    // Build IN clause
    let placeholders: Vec<String> = memory_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect();
    let sql = format!(
        "SELECT memory_id, success_count, failure_count FROM retrieval_stats WHERE memory_id IN ({}) AND (success_count + failure_count) >= 3",
        placeholders.join(",")
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> = memory_ids
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (id, s, f) = row?;
        map.insert(id, (s, f));
    }
    Ok(map)
}

// ─── Code evolution queries (v9) ───

/// A row from `get_recent_code_evolution`: (session_id, timestamp, functions_added, functions_removed, tool_name).
pub type CodeEvolutionRow = (String, String, String, String, String);

/// A row from `get_session_code_evolution`: (file_path, functions_added, functions_removed, types_added, types_removed, imports_added).
pub type SessionCodeEvolutionRow = (String, String, String, String, String, String);

/// Insert a code evolution record.
#[allow(clippy::too_many_arguments)]
pub fn insert_code_evolution(
    conn: &Connection,
    session_id: &str,
    project_name: &str,
    file_path: &str,
    language: &str,
    tool_name: &str,
    functions_added: &str,
    functions_removed: &str,
    types_added: &str,
    types_removed: &str,
    imports_added: &str,
    imports_removed: &str,
    repo_root: Option<&str>,
) -> Result<()> {
    let id = format!(
        "evo_{}_{}",
        &uuid::Uuid::new_v4().to_string()[..8],
        chrono::Utc::now().timestamp_millis()
    );
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO code_evolution (id, session_id, project_name, file_path, language, tool_name, functions_added, functions_removed, types_added, types_removed, imports_added, imports_removed, repo_root, timestamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![id, session_id, project_name, file_path, language, tool_name, functions_added, functions_removed, types_added, types_removed, imports_added, imports_removed, repo_root, now],
    )?;
    Ok(())
}

/// Insert a backfilled code-evolution row with an explicit deterministic id
/// and historical timestamp (`csr-engine backfill-coedit`). `functions_added`
/// / `functions_removed` / `types_added` / `types_removed` / `imports_added`
/// / `imports_removed` are left at their schema default (`'[]'`) — backfill
/// reconstructs co-edit *ledger* signal only, not AST diffs.
///
/// `id` is the caller's deterministic id (`bf-<sha256 prefix>`); `INSERT OR
/// IGNORE` against the `id` PRIMARY KEY makes repeated runs idempotent —
/// never touches an existing row. Returns `true` if a new row was inserted,
/// `false` if it already existed (no-op).
#[allow(clippy::too_many_arguments)]
pub fn insert_code_evolution_backfill(
    conn: &Connection,
    id: &str,
    session_id: &str,
    project_name: &str,
    file_path: &str,
    language: &str,
    tool_name: &str,
    timestamp: &str,
    repo_root: Option<&str>,
) -> Result<bool> {
    let changed = conn.execute(
        "INSERT OR IGNORE INTO code_evolution
             (id, session_id, project_name, file_path, language, tool_name, timestamp, repo_root)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            session_id,
            project_name,
            file_path,
            language,
            tool_name,
            timestamp,
            repo_root,
        ],
    )?;
    Ok(changed > 0)
}

/// Distinct `code_evolution.file_path` values still missing `repo_root` —
/// feeds the WP2 Stage 1 backfill (`import::backfill::backfill_repo_root`).
pub fn code_evolution_files_missing_repo_root(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT file_path FROM code_evolution WHERE repo_root IS NULL AND file_path != ''",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Set `repo_root` on every `code_evolution` row matching `file_path` that
/// is currently NULL. Never overwrites an already-resolved value
/// (idempotent, re-runnable). Returns the number of rows changed.
pub fn set_repo_root_for_evolution_file(
    conn: &Connection,
    file_path: &str,
    repo_root: &str,
) -> Result<usize> {
    let n = conn.execute(
        "UPDATE code_evolution SET repo_root = ?1 WHERE file_path = ?2 AND repo_root IS NULL",
        params![repo_root, file_path],
    )?;
    Ok(n)
}

/// A row from `all_code_evolution_events_ordered`:
/// (rowid, session_id, file_path, timestamp, functions_added, types_added).
pub type CodeEvolutionEventRow = (i64, String, String, String, String, String);

/// Every `code_evolution` row, oldest-first with a `rowid` tiebreak (WP2
/// Stage 2 transcript-channel attribution backfill —
/// `import::backfill::backfill_attribution`, receipt R2). Feeds the
/// earliest-by-(timestamp, rowid) join: for a symbol named in more than one
/// event, the FIRST row returned here is the one that wins. Loaded once into
/// memory — the corpus is thousands of rows, not millions, so this stays
/// cheap even at the backfill's documented "minutes-scale over ~6.7k
/// symbols" cost.
pub fn all_code_evolution_events_ordered(conn: &Connection) -> Result<Vec<CodeEvolutionEventRow>> {
    // `timestamp` holds two shapes: the schema default `datetime('now')`
    // (`YYYY-MM-DD HH:MM:SS`, legacy rows) and RFC 3339 (`YYYY-MM-DDT...`,
    // `insert_code_evolution` / backfill rows). Lexically, ' ' < 'T', so a
    // plain ORDER BY would put every legacy row of a given day before every
    // RFC 3339 row of that day regardless of the real instant — and the
    // attribution channel takes the FIRST row per symbol as the winner.
    // Normalizing the separator makes the two shapes prefix-comparable.
    let mut stmt = conn.prepare(
        "SELECT rowid, session_id, file_path, timestamp, functions_added, types_added
         FROM code_evolution
         ORDER BY replace(timestamp, 'T', ' ') ASC, rowid ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// True if a `code_evolution` row with this `id` already exists. Used by
/// `backfill-coedit --dry-run` to preview accurate would-insert counts
/// without writing anything.
pub fn code_evolution_id_exists(conn: &Connection, id: &str) -> Result<bool> {
    conn.query_row(
        "SELECT 1 FROM code_evolution WHERE id = ?1 LIMIT 1",
        params![id],
        |_| Ok(()),
    )
    .optional()
    .map(|r| r.is_some())
    .map_err(Into::into)
}

/// Get recent code evolution for a file (most recent N entries).
/// Scoped by project_name to prevent cross-project leakage (Codex M-3).
/// Returns (session_id, timestamp, functions_added, functions_removed, tool_name).
pub fn get_recent_code_evolution(
    conn: &Connection,
    file_path: &str,
    project_name: &str,
    limit: usize,
) -> Result<Vec<CodeEvolutionRow>> {
    let row_mapper = |row: &rusqlite::Row| -> rusqlite::Result<CodeEvolutionRow> {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    };

    if project_name.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT session_id, timestamp, functions_added, functions_removed, tool_name
             FROM code_evolution WHERE file_path = ?1
             ORDER BY timestamp DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![file_path, limit as i64], row_mapper)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    } else {
        let mut stmt = conn.prepare(
            "SELECT session_id, timestamp, functions_added, functions_removed, tool_name
             FROM code_evolution WHERE file_path = ?1 AND project_name = ?2
             ORDER BY timestamp DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![file_path, project_name, limit as i64], row_mapper)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

/// Get code evolution summary across all files for a session.
/// Returns (file_path, functions_added, functions_removed, types_added, types_removed, imports_added).
pub fn get_session_code_evolution(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<SessionCodeEvolutionRow>> {
    let mut stmt = conn.prepare(
        "SELECT file_path, functions_added, functions_removed, types_added, types_removed, imports_added
         FROM code_evolution WHERE session_id = ?1
         ORDER BY timestamp",
    )?;
    let rows = stmt.query_map(params![session_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

// ─── Saga Phase 1 (WS1): capture columns + provenance-walk storage helpers ───

/// All chunk ids for a conversation, insertion order (no seq dependency — old rows may
/// have seq=NULL). Used by WS2's exact per-conversation scoring (never
/// SearchEngine::search_chunks_filtered — see spec).
pub fn get_chunk_ids_for_conversation(
    conn: &Connection,
    conversation_id: &str,
) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT id FROM chunks WHERE conversation_id = ?1 ORDER BY rowid")?;
    let rows = stmt.query_map(params![conversation_id], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Embeddings for a specific set of chunk ids, chunked into ~500-id IN-clauses to stay
/// under SQLite's default parameter limit. Decodes the same little-endian f32 blob format
/// as `load_all_chunk_vectors`.
pub fn get_chunk_vectors_by_ids(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<(String, Vec<f32>)>> {
    const BATCH: usize = 500;
    let mut results = Vec::new();
    for batch in ids.chunks(BATCH) {
        if batch.is_empty() {
            continue;
        }
        let placeholders = std::iter::repeat_n("?", batch.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT chunk_id, embedding FROM chunk_embeddings WHERE chunk_id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let bound: Vec<&dyn rusqlite::ToSql> =
            batch.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(bound.as_slice(), |row| {
            let id: String = row.get(0)?;
            let bytes: Vec<u8> = row.get(1)?;
            Ok((id, bytes_to_vec(&bytes)))
        })?;
        for row in rows {
            results.push(row?);
        }
    }
    Ok(results)
}

/// Most-touched files for a session (code_evolution), highest-frequency first. Lifted from
/// the Phase 0 spike (examples/saga_spike.rs::files_for_session), now with a caller-supplied
/// limit instead of a hardcoded 4.
pub fn files_for_session(conn: &Connection, session_id: &str, limit: usize) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT file_path FROM code_evolution WHERE session_id = ?1
         GROUP BY file_path ORDER BY COUNT(*) DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![session_id, limit as i64], |row| {
        row.get::<_, String>(0)
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Other sessions that touched the same file (code_evolution), excluding one session.
/// Optionally scoped to a project (saga project-scoping fix — prevents cross-project
/// graph spread in the reinstatement walk).
/// Lifted from the Phase 0 spike (examples/saga_spike.rs::sessions_for_file), now with a
/// caller-supplied limit instead of a hardcoded 12.
pub fn sessions_for_file(
    conn: &Connection,
    file_path: &str,
    exclude_session: &str,
    project: Option<&str>,
    limit: usize,
) -> Result<Vec<String>> {
    match project {
        Some(p) => {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT session_id FROM code_evolution
                 WHERE file_path = ?1 AND session_id <> ?2 AND project_name = ?3 LIMIT ?4",
            )?;
            let rows = stmt.query_map(
                params![file_path, exclude_session, p, limit as i64],
                |row| row.get::<_, String>(0),
            )?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT session_id FROM code_evolution
                 WHERE file_path = ?1 AND session_id <> ?2 LIMIT ?3",
            )?;
            let rows = stmt
                .query_map(params![file_path, exclude_session, limit as i64], |row| {
                    row.get::<_, String>(0)
                })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        }
    }
}

/// Backfill UPDATE: set seq/is_sidechain on an existing chunk row by id.
pub fn set_chunk_saga_columns(
    conn: &Connection,
    chunk_id: &str,
    seq: usize,
    is_sidechain: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE chunks SET seq = ?1, is_sidechain = ?2 WHERE id = ?3",
        params![seq as i64, is_sidechain as i64, chunk_id],
    )?;
    Ok(())
}

/// All file paths recorded in import_state (for the saga-columns backfill to re-parse).
pub fn list_all_import_state_file_paths(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT file_path FROM import_state")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Distinct sessions that touched files matching a path suffix (hook-observed edits) —
/// ground truth for the provenance eval (`eval --provenance`). Lifted from the Phase 0
/// spike's `ground_truth`. Empty target -> empty set (judged-only queries have no GT).
pub fn ground_truth_sessions_for_target(
    conn: &Connection,
    target: &str,
) -> Result<std::collections::HashSet<String>> {
    if target.is_empty() {
        return Ok(std::collections::HashSet::new());
    }
    let mut stmt = conn
        .prepare("SELECT DISTINCT session_id FROM code_evolution WHERE file_path LIKE '%' || ?1")?;
    let rows = stmt.query_map(params![target], |r| r.get::<_, String>(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

// ─── Consolidation queries (v9 Dreamer) ───

/// Get conversations with V3 extraction or AI narrative but no consolidation.
/// Returns (conversation_id, narrative_content) pairs for the Dreamer to process.
pub fn get_unconsolidated_conversations(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<(String, String)>> {
    // Find conversations that have completed V3 extraction or AI narrative
    // but have no 'consolidated_fact' enrichment row yet.
    // Prefer AI narrative (Layer 3) over V3 extraction (Layer 2) when both exist.
    let mut stmt = conn.prepare(
        "SELECT e.conversation_id, r.content
         FROM enrichment_state e
         INNER JOIN reflections r ON r.id = e.reflection_id
         WHERE e.enrichment_type IN ('extracted_v3', 'ai_narrative')
         AND e.status = 'completed'
         AND e.conversation_id NOT IN (
             SELECT conversation_id FROM enrichment_state
             WHERE enrichment_type = 'consolidated_fact' AND status IN ('completed', 'skipped')
         )
         ORDER BY e.updated_at DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Mark a conversation as consolidated (with or without facts).
pub fn mark_consolidated(
    conn: &Connection,
    conversation_id: &str,
    reflection_id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO enrichment_state (conversation_id, enrichment_type, status, reflection_id, updated_at)
         VALUES (?1, 'consolidated_fact', 'completed', ?2, datetime('now'))
         ON CONFLICT(conversation_id, enrichment_type) DO UPDATE SET
             status = 'completed', reflection_id = ?2, updated_at = datetime('now')",
        params![conversation_id, reflection_id],
    )?;
    Ok(())
}

/// Mark a conversation as consolidated but skipped (no facts extracted).
pub fn mark_consolidated_skipped(conn: &Connection, conversation_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO enrichment_state (conversation_id, enrichment_type, status, updated_at)
         VALUES (?1, 'consolidated_fact', 'skipped', datetime('now'))
         ON CONFLICT(conversation_id, enrichment_type) DO UPDATE SET
             status = 'skipped', updated_at = datetime('now')",
        params![conversation_id],
    )?;
    Ok(())
}

/// Search consolidated facts by tag type, scoped by project (Codex M-4).
/// Returns (content, fact_type). Filters by project_ tag to prevent cross-project leakage.
pub fn search_consolidated_facts(
    conn: &Connection,
    project_name: &str,
    limit: usize,
) -> Result<Vec<(String, String)>> {
    let extract_fact_type = |row: &rusqlite::Row| -> rusqlite::Result<(String, String)> {
        let content: String = row.get(0)?;
        let tags_str: String = row.get(1)?;
        let fact_type = tags_str
            .split("fact_type_")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("unknown")
            .to_string();
        Ok((content, fact_type))
    };

    if project_name.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT content, tags FROM reflections
             WHERE tags LIKE '%consolidated_fact%'
             ORDER BY timestamp DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], extract_fact_type)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    } else {
        // Filter by project_ tag embedded in the fact's tags array
        let project_pattern = format!("%project_{}%", escape_like(project_name));
        let mut stmt = conn.prepare(
            "SELECT content, tags FROM reflections
             WHERE tags LIKE '%consolidated_fact%'
             AND tags LIKE ?1 ESCAPE '\\'
             ORDER BY timestamp DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![project_pattern, limit as i64], extract_fact_type)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

/// List distinct session IDs from iteration_learnings matching a prefix.
/// Returns empty vec if the table doesn't exist (created on first stop hook).
pub fn list_session_ids(conn: &Connection, prefix: &str, limit: usize) -> Result<Vec<String>> {
    let pattern = format!("{}%", escape_like(prefix));
    let result = conn.prepare(
        "SELECT DISTINCT session_id FROM iteration_learnings
         WHERE session_id LIKE ?1 ESCAPE '\\'
         ORDER BY session_id DESC
         LIMIT ?2",
    );
    let mut stmt = match result {
        Ok(s) => s,
        Err(e) => {
            // Only tolerate "no such table" — surface real SQL errors
            if e.to_string().contains("no such table") {
                return Ok(Vec::new());
            }
            return Err(anyhow::anyhow!("{}", e));
        }
    };
    let rows = stmt.query_map(params![pattern, limit as i64], |row| row.get(0))?;
    rows.collect::<std::result::Result<Vec<String>, _>>()
        .map_err(|e| anyhow::anyhow!("{}", e))
}

// ─── Resolution ledger ───

/// Latest explicit verdict for a chunk (or reflection) id.
#[derive(Debug, Clone)]
pub struct ResolutionEntry {
    pub status: String,
    pub evidence: String,
    pub claim: Option<String>,
    pub created_at: String,
}

/// Append resolution verdicts for one or more chunk ids in a single transaction.
/// Returns the number of rows inserted.
pub fn insert_resolutions(
    conn: &Connection,
    chunk_ids: &[String],
    status: &str,
    evidence: &str,
    claim: Option<&str>,
    source: &str,
) -> Result<usize> {
    if chunk_ids.is_empty() {
        return Ok(0);
    }

    let now = chrono::Utc::now().to_rfc3339();
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO resolution_ledger (chunk_id, status, evidence, claim, source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for chunk_id in chunk_ids {
            stmt.execute(params![chunk_id, status, evidence, claim, source, now])?;
        }
    }
    tx.commit()?;
    Ok(chunk_ids.len())
}

/// Batch-fetch the latest resolution entry per chunk_id (highest `id` wins).
/// Chunk ids with no ledger rows are absent from the returned map.
pub fn get_resolutions_batch(
    conn: &Connection,
    chunk_ids: &[String],
) -> Result<HashMap<String, ResolutionEntry>> {
    if chunk_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let placeholders: Vec<String> = (1..=chunk_ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "SELECT chunk_id, status, evidence, claim, source, created_at
         FROM resolution_ledger
         WHERE id IN (
             SELECT MAX(id) FROM resolution_ledger WHERE chunk_id IN ({}) GROUP BY chunk_id
         )",
        placeholders.join(", ")
    );

    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> = chunk_ids
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            ResolutionEntry {
                status: row.get(1)?,
                evidence: row.get(2)?,
                claim: row.get(3)?,
                created_at: row.get(5)?,
            },
        ))
    })?;

    let mut map = HashMap::new();
    for row in rows {
        let (chunk_id, entry) = row?;
        map.insert(chunk_id, entry);
    }
    Ok(map)
}

// ─── Session registry (history.jsonl spine — never embedded / never injected) ───

/// One row per session to upsert: (session_id, project, first_prompt,
/// first_ts_rfc3339, last_ts_rfc3339, prompt_count_delta).
#[derive(Debug, Clone)]
pub struct SessionRegistryRow {
    pub session_id: String,
    pub project: String,
    pub first_prompt: Option<String>,
    pub first_ts: String,
    pub last_ts: String,
    pub prompt_count_delta: i64,
}

/// Upsert session_registry rows within an existing transaction. On conflict:
/// first_prompt/first_ts kept from whichever row has the EARLIER first_ts
/// (existing row wins unless the existing row's first_ts is null/absent or
/// later than the new row's), last_ts = MAX(existing, new), prompt_count =
/// existing + delta.
///
/// Does NOT open a new transaction — caller already holds one (see
/// `Storage::with_transaction` for atomic upsert+checkpoint).
pub fn upsert_session_registry_batch(
    conn: &Connection,
    rows: &[SessionRegistryRow],
) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let mut stmt = conn.prepare(
        "INSERT INTO session_registry (session_id, project, first_prompt, first_ts, last_ts, prompt_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(session_id) DO UPDATE SET
           first_prompt = CASE
             WHEN excluded.first_ts < session_registry.first_ts
                  OR session_registry.first_ts IS NULL
             THEN excluded.first_prompt
             ELSE session_registry.first_prompt
           END,
           first_ts = CASE
             WHEN excluded.first_ts < session_registry.first_ts
                  OR session_registry.first_ts IS NULL
             THEN excluded.first_ts
             ELSE session_registry.first_ts
           END,
           last_ts = CASE
             WHEN session_registry.last_ts IS NULL
                  OR excluded.last_ts > session_registry.last_ts
             THEN excluded.last_ts
             ELSE session_registry.last_ts
           END,
           prompt_count = session_registry.prompt_count + excluded.prompt_count",
    )?;
    for row in rows {
        stmt.execute(params![
            row.session_id,
            row.project,
            row.first_prompt,
            row.first_ts,
            row.last_ts,
            row.prompt_count_delta,
        ])?;
    }
    Ok(rows.len())
}

/// (sessions_seen, sessions_imported, gap).
/// sessions_seen = COUNT(*) FROM session_registry.
/// sessions_imported = registry sessions that appear in non-plan chunks.
/// gap = sessions_seen - sessions_imported (never negative).
pub fn coverage_stats(conn: &Connection) -> Result<(i64, i64, i64)> {
    let (seen, imported): (i64, i64) = conn.query_row(
        "SELECT
           (SELECT COUNT(*) FROM session_registry) AS seen,
           (SELECT COUNT(*) FROM session_registry sr
              WHERE EXISTS (
                SELECT 1 FROM chunks c
                WHERE c.conversation_id = sr.session_id
                  AND c.conversation_id NOT LIKE 'plan:%'
              )
           ) AS imported",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let gap = (seen - imported).max(0);
    Ok((seen, imported, gap))
}

/// Subset of `candidates` present in either session_registry or chunks.conversation_id.
pub fn known_session_ids(
    conn: &Connection,
    candidates: &[String],
) -> Result<std::collections::HashSet<String>> {
    use std::collections::HashSet;
    if candidates.is_empty() {
        return Ok(HashSet::new());
    }

    // Batched ≤500 candidates per statement: one flat ?1..?N list would hit
    // SQLITE_MAX_VARIABLE_NUMBER on large transcript dirs, and the caller's
    // unwrap_or_default would silently report every session as unknown
    // (CodeRabbit). Numbered placeholders are reused on both UNION sides —
    // each candidate binds once.
    const BATCH: usize = 500;
    let mut out = HashSet::new();
    for batch in candidates.chunks(BATCH) {
        let placeholders: Vec<String> = (1..=batch.len()).map(|i| format!("?{i}")).collect();
        let in_list = placeholders.join(", ");
        let sql = format!(
            "SELECT session_id FROM session_registry WHERE session_id IN ({in_list})
             UNION
             SELECT conversation_id FROM chunks WHERE conversation_id IN ({in_list})"
        );

        let params: Vec<&dyn rusqlite::types::ToSql> = batch
            .iter()
            .map(|id| id as &dyn rusqlite::types::ToSql)
            .collect();

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
        for row in rows {
            out.insert(row?);
        }
    }
    Ok(out)
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

    #[test]
    fn test_narrative_usage_record_and_summary() {
        let conn = mem();
        let row = NarrativeUsageRow {
            call_site: "briefing".into(),
            model: "claude-haiku-4-5".into(),
            input_tokens: 1500,
            output_tokens: 300,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            duration_ms: 4200,
            success: true,
        };
        record_narrative_usage(&conn, &row).unwrap();
        record_narrative_usage(&conn, &row).unwrap();

        let s = narrative_usage_summary(&conn).unwrap();
        assert_eq!(s.calls_total, 2);
        assert_eq!(s.tokens_total, 3600);
        assert_eq!(s.calls_today, 2); // rows stamped 'now' are today
        assert_eq!(s.tokens_today, 3600);
        assert_eq!(s.cache_tokens_total, 0);
        assert_eq!(s.cache_tokens_today, 0);
        assert_eq!(s.last_model.as_deref(), Some("claude-haiku-4-5"));
    }

    #[test]
    fn test_narrative_usage_summary_empty() {
        let conn = mem();
        let s = narrative_usage_summary(&conn).unwrap();
        assert_eq!(s.calls_total, 0);
        assert_eq!(s.cache_tokens_total, 0);
        assert_eq!(s.cache_tokens_today, 0);
        assert_eq!(s.last_model, None);
    }

    #[test]
    fn test_get_ratification_scores_empty_input() {
        let conn = mem();
        let map = get_ratification_scores(&conn, &[]).unwrap();
        assert!(map.is_empty());
    }

    /// Regression: `get_chunks_by_ids` alone always defaults `author` to
    /// `ToolResult` (chunks table has no author column), which silently starved
    /// ratification's `build_digest` of operator turns in production even
    /// though import correctly wrote `Speaker::User` rows into
    /// `chunk_provenance`. `get_chunks_by_ids_with_provenance` must recover the
    /// real author via the LEFT JOIN.
    #[test]
    fn get_chunks_by_ids_with_provenance_resolves_true_author() {
        let conn = mem();
        let chunk = ConversationChunk {
            id: "c1".into(),
            conversation_id: "conv-1".into(),
            project_name: "p".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            content: "fix the import bug".into(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult, // irrelevant: insert_chunk doesn't persist author
            seq: 0,
            is_sidechain: false,
        };
        insert_chunk(&conn, &chunk, &[0.0; 4]).unwrap();
        insert_chunk_provenance(
            &conn,
            "c1",
            &ChunkProvenance {
                author: Speaker::User,
                source_conv_id: "conv-1".into(),
                supersedes: None,
            },
        )
        .unwrap();

        // Plain path (no join) always degrades to ToolResult.
        let plain = get_chunks_by_ids(&conn, &["c1".into()]).unwrap();
        assert_eq!(plain[0].author, Speaker::ToolResult);

        // Provenance-aware path recovers the true author.
        let with_prov = get_chunks_by_ids_with_provenance(&conn, &["c1".into()]).unwrap();
        assert_eq!(with_prov.len(), 1);
        assert_eq!(with_prov[0].author, Speaker::User);
        assert_eq!(with_prov[0].content, "fix the import bug");
    }

    /// A chunk with no `chunk_provenance` row at all must still degrade to
    /// `ToolResult` (not error), matching `get_chunk_provenance`'s fallback.
    #[test]
    fn get_chunks_by_ids_with_provenance_defaults_when_no_row() {
        let conn = mem();
        let chunk = ConversationChunk {
            id: "c2".into(),
            conversation_id: "conv-2".into(),
            project_name: "p".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            content: "no provenance row".into(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult,
            seq: 0,
            is_sidechain: false,
        };
        insert_chunk(&conn, &chunk, &[0.0; 4]).unwrap();

        let with_prov = get_chunks_by_ids_with_provenance(&conn, &["c2".into()]).unwrap();
        assert_eq!(with_prov.len(), 1);
        assert_eq!(with_prov[0].author, Speaker::ToolResult);
    }

    #[test]
    fn test_upsert_and_get_ratification_score() {
        let conn = mem();
        let row = RatificationScoreRow {
            conversation_id: "c1".into(),
            score: 0.6,
            acts_json: r#"{"acts":[]}"#.into(),
            ledger_refs: None,
            extractor_version: "ratification-v1".into(),
        };
        upsert_ratification_score(&conn, &row).unwrap();
        assert_eq!(get_ratification_score(&conn, "c1").unwrap(), Some(0.6));
        let (count, avg) = ratification_summary(&conn).unwrap();
        assert_eq!(count, 1);
        assert!((avg - 0.6).abs() < 1e-6);
        let map = get_ratification_scores(&conn, &["c1".into(), "missing".into()]).unwrap();
        assert_eq!(map.get("c1"), Some(&0.6));
        assert!(!map.contains_key("missing"));
    }

    #[test]
    fn test_reset_enrichment() {
        let conn = mem();
        mark_enrichment_completed(&conn, "conv-reset", "ratification", "x").unwrap();
        assert!(is_conversation_enriched(&conn, "conv-reset", "ratification").unwrap());

        reset_enrichment(&conn, "conv-reset", "ratification").unwrap();
        assert!(!is_conversation_enriched(&conn, "conv-reset", "ratification").unwrap());

        // Idempotent: deleting an absent row is Ok
        reset_enrichment(&conn, "conv-reset", "ratification").unwrap();
        assert!(!is_conversation_enriched(&conn, "conv-reset", "ratification").unwrap());
    }

    #[test]
    fn test_narrative_usage_summary_cache_tokens() {
        let conn = mem();
        let row = NarrativeUsageRow {
            call_site: "briefing".into(),
            model: "claude-haiku-4-5".into(),
            input_tokens: 100,
            output_tokens: 57,
            cache_read_tokens: 5000,
            cache_creation_tokens: 1200,
            duration_ms: 4200,
            success: true,
        };
        record_narrative_usage(&conn, &row).unwrap();

        let s = narrative_usage_summary(&conn).unwrap();
        // Fresh tokens only (input + output) — cache must not be folded in.
        assert_eq!(s.tokens_total, 157);
        assert_eq!(s.tokens_today, 157);
        // Cache read + creation tracked separately.
        assert_eq!(s.cache_tokens_total, 6200);
        assert_eq!(s.cache_tokens_today, 6200);
        assert_eq!(s.calls_total, 1);
        assert_eq!(s.calls_today, 1);
    }

    #[test]
    fn test_narrative_usage_failed_call_still_counts() {
        let conn = mem();
        let row = NarrativeUsageRow {
            call_site: "story".into(),
            model: "unknown".into(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            duration_ms: 150,
            success: false,
        };
        record_narrative_usage(&conn, &row).unwrap();

        let s = narrative_usage_summary(&conn).unwrap();
        // Failed/attempted calls still count as calls — "calls" means attempts, not successes.
        assert_eq!(s.calls_total, 1);
        assert_eq!(s.calls_today, 1);
        assert_eq!(s.tokens_total, 0);
    }

    #[test]
    fn set_chunk_saga_columns_roundtrip() {
        let conn = mem();
        let chunk = ConversationChunk {
            id: "chunk-saga-1".into(),
            conversation_id: "conv-1".into(),
            project_name: "proj".into(),
            timestamp: "2026-06-10T12:00:00Z".into(),
            content: "hello saga".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        insert_chunk(&conn, &chunk, &[0.1; 384]).unwrap();
        set_chunk_saga_columns(&conn, "chunk-saga-1", 7, true).unwrap();
        let (seq, is_sidechain): (i64, i64) = conn
            .query_row(
                "SELECT seq, is_sidechain FROM chunks WHERE id = ?1",
                params!["chunk-saga-1"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(seq, 7);
        assert_eq!(is_sidechain, 1);
    }

    /// Seed two projects that share a file path from different sessions.
    fn seed_cross_project_shared_file(conn: &Connection) {
        insert_code_evolution(
            conn,
            "sess_a",
            "proj_a",
            "shared.rs",
            "rust",
            "Edit",
            "[]",
            "[]",
            "[]",
            "[]",
            "[]",
            "[]",
            None,
        )
        .unwrap();
        insert_code_evolution(
            conn,
            "sess_b",
            "proj_b",
            "shared.rs",
            "rust",
            "Edit",
            "[]",
            "[]",
            "[]",
            "[]",
            "[]",
            "[]",
            None,
        )
        .unwrap();
    }

    #[test]
    fn sessions_for_file_filters_by_project() {
        let conn = mem();
        seed_cross_project_shared_file(&conn);

        // Scoped to proj_a: must not leak sess_b from proj_b (same file path).
        // Only other-project row exists, so filtered result is empty.
        let got = sessions_for_file(&conn, "shared.rs", "sess_a", Some("proj_a"), 10).unwrap();
        assert!(
            !got.contains(&"sess_b".to_string()),
            "project-scoped lookup must not return other-project sessions: {got:?}"
        );
    }

    #[test]
    fn sessions_for_file_none_project_is_unscoped() {
        let conn = mem();
        seed_cross_project_shared_file(&conn);

        // Unscoped (None): back-compat — can see sess_b across projects.
        let got = sessions_for_file(&conn, "shared.rs", "sess_a", None, 10).unwrap();
        assert!(
            got.contains(&"sess_b".to_string()),
            "unscoped lookup must return other-project sessions: {got:?}"
        );
    }
}
