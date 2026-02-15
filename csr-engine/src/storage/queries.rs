use std::path::Path;

use anyhow::Result;
use rusqlite::{params, Connection};

use crate::import::ConversationChunk;

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
        "INSERT OR REPLACE INTO chunks (id, conversation_id, project_name, timestamp, content, message_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            chunk.id,
            chunk.conversation_id,
            chunk.project_name,
            chunk.timestamp,
            chunk.content,
            chunk.message_count as i64,
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
        "SELECT id, conversation_id, project_name, timestamp, content, message_count
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
        "SELECT id, conversation_id, project_name, timestamp, content, message_count
         FROM chunks WHERE project_name = ?1 ORDER BY timestamp DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![project, limit as i64], |row| {
        Ok(ConversationChunk {
            id: row.get(0)?,
            conversation_id: row.get(1)?,
            project_name: row.get(2)?,
            timestamp: row.get(3)?,
            content: row.get(4)?,
            message_count: row.get::<_, i64>(5)? as usize,
        })
    })?;
    let mut chunks = Vec::new();
    for row in rows {
        chunks.push(row?);
    }
    Ok(chunks)
}

// ─── Temporal queries ───

/// Get recent chunks ordered by timestamp descending.
pub fn get_recent_chunks(
    conn: &Connection,
    limit: usize,
    project: Option<&str>,
) -> Result<Vec<ConversationChunk>> {
    let mut stmt;
    let rows = if let Some(p) = project.filter(|p| *p != "all") {
        stmt = conn.prepare(
            "SELECT id, conversation_id, project_name, timestamp, content, message_count
             FROM chunks WHERE project_name = ?1 ORDER BY timestamp DESC LIMIT ?2",
        )?;
        stmt.query_map(params![p, limit as i64], row_to_chunk)?
    } else {
        stmt = conn.prepare(
            "SELECT id, conversation_id, project_name, timestamp, content, message_count
             FROM chunks ORDER BY timestamp DESC LIMIT ?1",
        )?;
        stmt.query_map(params![limit as i64], row_to_chunk)?
    };

    let mut chunks = Vec::new();
    for row in rows {
        chunks.push(row?);
    }
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
            "SELECT id, conversation_id, project_name, timestamp, content, message_count
             FROM chunks WHERE timestamp BETWEEN ?1 AND ?2 AND project_name = ?3
             ORDER BY timestamp DESC",
        )?;
        let rows = stmt.query_map(params![start, end, p], row_to_chunk)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, project_name, timestamp, content, message_count
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
        let mut stmt = conn.prepare(
            "SELECT id FROM chunks WHERE timestamp BETWEEN ?1 AND ?2",
        )?;
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
    // Sanitize for FTS5: remove control chars, escape double quotes, wrap as phrase
    let sanitized: String = query
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .replace('"', "\"\"");
    let fts_query = format!("\"{}\"", sanitized);

    let chunks = if let Some(p) = project.filter(|p| *p != "all") {
        let mut stmt = conn.prepare(
            "SELECT c.id, c.conversation_id, c.project_name, c.timestamp, c.content, c.message_count
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
            "SELECT c.id, c.conversation_id, c.project_name, c.timestamp, c.content, c.message_count
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
) -> Result<Vec<(String, String, Vec<String>, String)>> {
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
fn row_to_chunk(row: &rusqlite::Row) -> rusqlite::Result<ConversationChunk> {
    Ok(ConversationChunk {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        project_name: row.get(2)?,
        timestamp: row.get(3)?,
        content: row.get(4)?,
        message_count: row.get::<_, i64>(5)? as usize,
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
pub fn get_conversations_by_batch(
    conn: &Connection,
    batch_id: &str,
) -> Result<Vec<String>> {
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
    conn.execute("DELETE FROM reflection_embeddings WHERE reflection_id = ?1", params![id])?;
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

// ─── Import state queries ───

pub fn is_file_imported(conn: &Connection, path: &Path) -> Result<bool> {
    let path_str = path.to_string_lossy().to_string();
    let mut stmt = conn.prepare("SELECT 1 FROM import_state WHERE file_path = ?1")?;
    Ok(stmt.exists(params![path_str])?)
}

pub fn mark_file_imported(conn: &Connection, path: &Path, chunks: usize) -> Result<()> {
    let path_str = path.to_string_lossy().to_string();
    // Extract conversation_id from filename (stem of the JSONL file)
    let conv_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    conn.execute(
        "INSERT OR REPLACE INTO import_state (file_path, conversation_id, chunks_imported) VALUES (?1, ?2, ?3)",
        params![path_str, conv_id, chunks as i64],
    )?;
    Ok(())
}
