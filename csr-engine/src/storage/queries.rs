use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use chrono;
use rusqlite::{params, Connection};

use crate::import::ConversationChunk;

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
         ORDER BY e.updated_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
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
        map.entry(mid)
            .or_default()
            .push(RetrievalEvent {
                retrieved_at,
                session_outcome,
            });
    }
    Ok(map)
}
