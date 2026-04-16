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

    pub fn get_reflection_by_id(&self, id: &str) -> Result<Option<(String, Vec<String>, String)>> {
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

    pub fn get_recent_sessions(
        &self,
        limit: usize,
        project: Option<&str>,
    ) -> Result<Vec<queries::SessionInfo>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_recent_sessions(&conn, limit, project)
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
    ) -> Result<Vec<queries::ReflectionRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_reflections_by_tag(&conn, tag, limit)
    }

    // ─── Enrichment state ───

    pub fn is_conversation_enriched(
        &self,
        conversation_id: &str,
        enrichment_type: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::is_conversation_enriched(&conn, conversation_id, enrichment_type)
    }

    pub fn mark_enrichment_completed(
        &self,
        conversation_id: &str,
        enrichment_type: &str,
        reflection_id: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::mark_enrichment_completed(&conn, conversation_id, enrichment_type, reflection_id)
    }

    pub fn mark_enrichment_failed(
        &self,
        conversation_id: &str,
        enrichment_type: &str,
        error: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::mark_enrichment_failed(&conn, conversation_id, enrichment_type, error)
    }

    pub fn get_unenriched_conversations(
        &self,
        enrichment_type: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_unenriched_conversations(&conn, enrichment_type, limit)
    }

    pub fn set_batch_id(
        &self,
        conversation_id: &str,
        batch_id: &str,
        prompt_hash: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::set_batch_id(&conn, conversation_id, batch_id, prompt_hash)
    }

    pub fn get_conversations_by_batch(&self, batch_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_conversations_by_batch(&conn, batch_id)
    }

    pub fn delete_reflection(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::delete_reflection(&conn, id)
    }

    pub fn get_enrichment_reflection_id(
        &self,
        conversation_id: &str,
        enrichment_type: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_enrichment_reflection_id(&conn, conversation_id, enrichment_type)
    }

    // ─── Story backfill queries ───

    pub fn get_conversations_missing_stories(&self) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_conversations_missing_stories(&conn)
    }

    pub fn get_project_for_conversation(&self, conversation_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_project_for_conversation(&conn, conversation_id)
    }

    // ─── Backfill queries ───

    pub fn get_conversations_missing_import_state(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_conversations_missing_import_state(&conn)
    }

    pub fn get_conversations_needing_heuristic(&self) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_conversations_needing_heuristic(&conn)
    }

    // ─── Status queries ───

    pub fn count_conversations(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::count_conversations(&conn)
    }

    pub fn count_projects(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::count_projects(&conn)
    }

    pub fn count_imported_files(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::count_imported_files(&conn)
    }

    pub fn get_enrichment_breakdown(&self) -> Result<Vec<(String, String, usize)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_enrichment_breakdown(&conn)
    }

    pub fn get_newest_chunk_timestamp(&self) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_newest_chunk_timestamp(&conn)
    }

    pub fn get_db_size(&self) -> Result<u64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_db_size(&conn)
    }

    // ─── Incremental import queries ───

    pub fn get_imported_chunk_count(&self, path: &Path) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_imported_chunk_count(&conn, path)
    }

    pub fn get_chunk_content(&self, id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_chunk_content(&conn, id)
    }

    // ─── Count queries (for HNSW cache staleness) ───

    pub fn count_chunk_embeddings(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::count_chunk_embeddings(&conn)
    }

    pub fn count_reflection_embeddings(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::count_reflection_embeddings(&conn)
    }

    /// Run SQLite integrity check. Returns true if database is healthy.
    pub fn integrity_check(&self) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let result: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(result == "ok")
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

    // ─── TAD: Retrieval Events ───

    pub fn log_retrieval_event(
        &self,
        memory_id: &str,
        memory_type: &str,
        hook_phase: &str,
        session_id: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::log_retrieval_event(&conn, memory_id, memory_type, hook_phase, session_id)
    }

    pub fn update_session_outcome(&self, session_id: &str, outcome: &str) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::update_session_outcome(&conn, session_id, outcome)
    }

    pub fn get_retrieval_events_for_memory(
        &self,
        memory_id: &str,
    ) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_retrieval_events_for_memory(&conn, memory_id)
    }

    pub fn get_retrieval_events_batch(
        &self,
        memory_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, Vec<crate::search::decay::RetrievalEvent>>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_retrieval_events_batch(&conn, memory_ids)
    }
}
