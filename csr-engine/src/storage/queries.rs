use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use chrono;
use rusqlite::{params, Connection, OptionalExtension};

use crate::import::ConversationChunk;
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
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

pub fn insert_chunk(conn: &Connection, chunk: &ConversationChunk, embedding: &[f32]) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO chunks (id, conversation_id, project_name, timestamp, content, message_count, summary)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            chunk.id,
            chunk.conversation_id,
            chunk.project_name,
            chunk.timestamp,
            chunk.content,
            chunk.message_count as i64,
            chunk.summary,
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
        "SELECT id, conversation_id, project_name, timestamp, content, message_count, summary
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
        "SELECT id, conversation_id, project_name, timestamp, content, message_count, summary
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
            "SELECT c.id, c.conversation_id, c.project_name, c.timestamp, c.content, c.message_count, c.summary
             FROM chunks c
             WHERE c.project_name = ?1
               AND c.rowid = (SELECT MIN(c2.rowid) FROM chunks c2 WHERE c2.conversation_id = c.conversation_id)
             ORDER BY c.timestamp DESC LIMIT ?2",
        )?;
        stmt.query_map(params![p, limit as i64], row_to_chunk)?
    } else {
        stmt = conn.prepare(
            "SELECT c.id, c.conversation_id, c.project_name, c.timestamp, c.content, c.message_count, c.summary
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
            "SELECT id, conversation_id, project_name, timestamp, content, message_count, summary
             FROM chunks WHERE timestamp BETWEEN ?1 AND ?2 AND project_name = ?3
             ORDER BY timestamp DESC",
        )?;
        let rows = stmt.query_map(params![start, end, p], row_to_chunk)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, project_name, timestamp, content, message_count, summary
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
            "SELECT c.id, c.conversation_id, c.project_name, c.timestamp, c.content, c.message_count, c.summary
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
            "SELECT c.id, c.conversation_id, c.project_name, c.timestamp, c.content, c.message_count, c.summary
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
/// Expects columns: id, conversation_id, project_name, timestamp, content, message_count, summary
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
        // provenance explicitly via get_chunk_provenance.
        author: crate::provenance::Speaker::ToolResult,
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
        "INSERT OR REPLACE INTO import_state (file_path, conversation_id, chunks_imported, file_mtime) VALUES (?1, ?2, ?3, ?4)",
        params![path_str, conv_id, chunks as i64, mtime],
    )?;
    Ok(())
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
) -> Result<()> {
    let id = format!(
        "evo_{}_{}",
        &uuid::Uuid::new_v4().to_string()[..8],
        chrono::Utc::now().timestamp_millis()
    );
    conn.execute(
        "INSERT INTO code_evolution (id, session_id, project_name, file_path, language, tool_name, functions_added, functions_removed, types_added, types_removed, imports_added, imports_removed)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![id, session_id, project_name, file_path, language, tool_name, functions_added, functions_removed, types_added, types_removed, imports_added, imports_removed],
    )?;
    Ok(())
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
}
