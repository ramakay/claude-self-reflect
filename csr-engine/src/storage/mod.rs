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

    // ─── Project filtering ───

    pub fn get_chunk_ids_for_project(&self, project: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_chunk_ids_for_project(&conn, project)
    }

    pub fn get_chunks_by_project(
        &self,
        project: &str,
        limit: usize,
    ) -> Result<Vec<ConversationChunk>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_chunks_by_project(&conn, project, limit)
    }

    // ─── Temporal queries ───

    pub fn get_recent_chunks(
        &self,
        limit: usize,
        project: Option<&str>,
    ) -> Result<Vec<ConversationChunk>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_recent_chunks(&conn, limit, project)
    }

    pub fn get_chunks_in_timerange(
        &self,
        start: &str,
        end: &str,
        project: Option<&str>,
    ) -> Result<Vec<ConversationChunk>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_chunks_in_timerange(&conn, start, end, project)
    }

    pub fn get_chunk_ids_in_timerange(
        &self,
        start: &str,
        end: &str,
        project: Option<&str>,
    ) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_chunk_ids_in_timerange(&conn, start, end, project)
    }

    // ─── FTS5 search ───

    pub fn fts5_search(
        &self,
        query: &str,
        limit: usize,
        project: Option<&str>,
    ) -> Result<Vec<ConversationChunk>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::fts5_search(&conn, query, limit, project)
    }

    // ─── Reflection tag queries ───

    pub fn get_reflections_by_tag(
        &self,
        tag: &str,
        limit: usize,
    ) -> Result<Vec<(String, String, Vec<String>, String)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_reflections_by_tag(&conn, tag, limit)
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
