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
            chunk.message_count,
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
    let mut chunks = Vec::new();
    for id in ids {
        let mut stmt = conn.prepare(
            "SELECT id, conversation_id, project_name, timestamp, content, message_count
             FROM chunks WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok(ConversationChunk {
                id: row.get(0)?,
                conversation_id: row.get(1)?,
                project_name: row.get(2)?,
                timestamp: row.get(3)?,
                content: row.get(4)?,
                message_count: row.get::<_, i64>(5)? as usize,
            })
        })?;
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

pub fn is_file_imported(conn: &Connection, path: &Path) -> Result<bool> {
    let path_str = path.to_string_lossy().to_string();
    let mut stmt = conn.prepare("SELECT 1 FROM import_state WHERE file_path = ?1")?;
    Ok(stmt.exists(params![path_str])?)
}

pub fn mark_file_imported(conn: &Connection, path: &Path, chunks: usize) -> Result<()> {
    let path_str = path.to_string_lossy().to_string();
    conn.execute(
        "INSERT OR REPLACE INTO import_state (file_path, chunks_imported) VALUES (?1, ?2)",
        params![path_str, chunks as i64],
    )?;
    Ok(())
}
