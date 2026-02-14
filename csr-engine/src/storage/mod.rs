pub mod migrations;
pub mod queries;

use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use rusqlite::Connection;

use crate::import::ConversationChunk;

/// SQLite storage with FTS5 for full-text search.
/// Thread-safe via Mutex around the Connection.
pub struct Storage {
    conn: Mutex<Connection>,
}

impl Storage {
    /// Open (or create) the database at the given path.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        migrations::run(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory database (for tests).
    #[cfg(test)]
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        migrations::run(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // ─── Chunk operations ───

    pub fn insert_chunk(&self, chunk: &ConversationChunk, embedding: &[f32]) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::insert_chunk(&conn, chunk, embedding)
    }

    pub fn load_all_chunk_vectors(&self) -> Result<Vec<(String, Vec<f32>)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::load_all_chunk_vectors(&conn)
    }

    pub fn get_chunks_by_ids(&self, ids: &[String]) -> Result<Vec<ConversationChunk>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_chunks_by_ids(&conn, ids)
    }

    // ─── Reflection operations ───

    pub fn insert_reflection(
        &self,
        id: &str,
        content: &str,
        tags: &[String],
        embedding: &[f32],
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::insert_reflection(&conn, id, content, tags, embedding)
    }

    pub fn load_all_reflection_vectors(&self) -> Result<Vec<(String, Vec<f32>)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::load_all_reflection_vectors(&conn)
    }

    pub fn get_reflection_by_id(
        &self,
        id: &str,
    ) -> Result<Option<(String, Vec<String>, String)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_reflection_by_id(&conn, id)
    }

    // ─── Import state ───

    pub fn is_file_imported(&self, path: &Path) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::is_file_imported(&conn, path)
    }

    pub fn mark_file_imported(&self, path: &Path, chunks: usize) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::mark_file_imported(&conn, path, chunks)
    }
}
