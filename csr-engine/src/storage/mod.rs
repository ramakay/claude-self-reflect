pub mod ancestry;
pub mod chunk_binding;
pub mod codegraph;
pub mod migrations;
pub mod queries;
pub mod recap_feeds;
pub mod witness_ledger;
pub mod witness_verdicts;

pub use queries::{
    NarrativeUsageRow, NarrativeUsageSummary, RatificationScoreRow, ResolutionEntry,
};

use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use rusqlite::Connection;

use crate::import::{ConversationChunk, CsrSuppressionStats};

/// SQLite storage with FTS5 for full-text search.
/// Thread-safe via Mutex around the Connection.
pub struct Storage {
    conn: Mutex<Connection>,
}

impl Storage {
    /// Run an internal read or write operation while holding the SQLite mutex.
    ///
    /// Kept crate-private so diagnostics can take consistent multi-table
    /// snapshots without exposing the raw connection as part of the public API.
    pub(crate) fn with_connection<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T>,
    ) -> Result<T> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        operation(&conn)
    }

    /// Open (or create) the database at the given path.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        // foreign_keys=ON is explicit, not build-flag-dependent (Codex MEDIUM):
        // makes the declared chunk_provenance FK enforced deterministically.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;",
        )?;
        migrations::run(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open an in-memory database (for tests).
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
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

    /// Upsert provenance (author, source conv, supersession) for a chunk.
    pub fn insert_chunk_provenance(
        &self,
        chunk_id: &str,
        prov: &crate::provenance::ChunkProvenance,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::insert_chunk_provenance(&conn, chunk_id, prov)
    }

    /// Fetch provenance for a chunk, if recorded.
    pub fn get_chunk_provenance(
        &self,
        chunk_id: &str,
    ) -> Result<Option<crate::provenance::ChunkProvenance>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_chunk_provenance(&conn, chunk_id)
    }

    /// Upsert a derivation-ledger entry (Pillar 1).
    pub fn upsert_ledger_entry(&self, entry: &crate::ledger::LedgerEntry) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::upsert_ledger_entry(&conn, entry)
    }

    /// Fetch ledger entries for an exact {repo, branch, user} scope.
    pub fn get_ledger_entries(
        &self,
        scope: &crate::ledger::Scope,
        limit: usize,
    ) -> Result<Vec<crate::ledger::LedgerEntry>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_ledger_entries(&conn, scope, limit as i64)
    }

    /// Increment a ledger entry's reuse counter (governor signal, Pillar 4).
    pub fn increment_ledger_reuse(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::increment_ledger_reuse(&conn, id)
    }

    pub fn get_chunks_by_ids(&self, ids: &[String]) -> Result<Vec<ConversationChunk>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_chunks_by_ids(&conn, ids)
    }

    /// Like [`Self::get_chunks_by_ids`], but resolves each chunk's true author
    /// via a `LEFT JOIN` on `chunk_provenance` instead of defaulting to
    /// `Speaker::ToolResult`. Needed by callers that filter on
    /// `ConversationChunk::author` — e.g. ratification's `build_digest`, which
    /// prioritizes `Speaker::User` turns and silently degraded to head/tail
    /// sampling in production without this (author lives outside `chunks`).
    pub fn get_chunks_by_ids_with_provenance(
        &self,
        ids: &[String],
    ) -> Result<Vec<ConversationChunk>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_chunks_by_ids_with_provenance(&conn, ids)
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

    pub fn load_all_chunk_ids(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::load_all_chunk_ids(&conn)
    }

    pub fn load_all_reflection_ids(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::load_all_reflection_ids(&conn)
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

    pub fn get_most_recent_session(&self, project: &str) -> Result<Option<queries::SessionInfo>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_most_recent_session(&conn, project)
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

    pub fn get_reflections_by_two_tags(
        &self,
        tag_a: &str,
        tag_b: &str,
        limit: usize,
    ) -> Result<Vec<queries::ReflectionRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_reflections_by_two_tags(&conn, tag_a, tag_b, limit)
    }

    pub fn conversation_message_count(&self, conversation_id: &str) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::conversation_message_count(&conn, conversation_id)
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

    pub fn mark_enrichment_unavailable(
        &self,
        conversation_id: &str,
        enrichment_type: &str,
        reason: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::mark_enrichment_unavailable(&conn, conversation_id, enrichment_type, reason)
    }

    pub fn reset_enrichment(&self, conversation_id: &str, enrichment_type: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::reset_enrichment(&conn, conversation_id, enrichment_type)
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

    // ─── Consolidation queries (v9 Dreamer) ───

    pub fn get_unconsolidated_conversations(&self, limit: usize) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_unconsolidated_conversations(&conn, limit)
    }

    pub fn mark_consolidated(&self, conversation_id: &str, reflection_id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::mark_consolidated(&conn, conversation_id, reflection_id)
    }

    pub fn mark_consolidated_skipped(&self, conversation_id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::mark_consolidated_skipped(&conn, conversation_id)
    }

    pub fn search_consolidated_facts(
        &self,
        project_name: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::search_consolidated_facts(&conn, project_name, limit)
    }

    // ─── Story backfill queries ───

    pub fn get_conversations_missing_stories(
        &self,
        min_messages: usize,
    ) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_conversations_missing_stories(&conn, min_messages)
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

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_meta(&conn, key)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::set_meta(&conn, key, value)
    }

    /// Increment the schema-miss counter for an aux corpus source (tasks/plans/history…).
    /// Silence was the failure mode that let the TodoWrite→TaskCreate rename rot episode
    /// extraction for weeks — every aux adapter counts what it fails to parse.
    pub fn bump_aux_counter(&self, source: &str) -> Result<()> {
        self.bump_aux_counter_by(source, 1)
    }

    /// Increment an aux schema-miss counter by the number of quarantined raw lines.
    pub fn bump_aux_counter_by(&self, source: &str, count: usize) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let key = format!("aux_schema_miss:{source}");
        let current: i64 = queries::get_meta(&conn, &key)?
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        queries::set_meta(&conn, &key, &(current + count as i64).to_string())
    }

    /// All aux schema-miss counters as (source, count), for status surfacing.
    pub fn get_aux_counters(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let mut stmt =
            conn.prepare("SELECT key, value FROM meta WHERE key LIKE 'aux_schema_miss:%'")?;
        let rows = stmt.query_map([], |row| {
            let key: String = row.get(0)?;
            let value: String = row.get(1)?;
            Ok((key, value))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (key, value) = row?;
            let source = key.trim_start_matches("aux_schema_miss:").to_string();
            out.push((source, value.parse().unwrap_or(0)));
        }
        Ok(out)
    }

    pub fn get_csr_tool_blocks_suppressed(&self) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let split_value = queries::get_meta(&conn, "csr_tool_blocks_suppressed")?;
        let value = match split_value {
            Some(value) => Some(value),
            None => queries::get_meta(&conn, "csr_self_suppressed")?,
        };
        Ok(value.and_then(|value| value.parse().ok()).unwrap_or(0))
    }

    pub fn get_csr_hook_wrappers_scrubbed(&self) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        Ok(queries::get_meta(&conn, "csr_hook_wrappers_scrubbed")?
            .and_then(|value| value.parse().ok())
            .unwrap_or(0))
    }

    /// Backward-compatible aggregate surfaced by status: tool blocks + wrappers.
    pub fn get_csr_self_suppressed(&self) -> Result<i64> {
        Ok(self.get_csr_tool_blocks_suppressed()? + self.get_csr_hook_wrappers_scrubbed()?)
    }

    pub fn insert_chunk_with_source(
        &self,
        chunk: &ConversationChunk,
        embedding: &[f32],
        source: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::insert_chunk_with_source(&conn, chunk, embedding, source)
    }

    /// Record a task-derived resolution proposal. Proposals are NOT verdicts:
    /// they live in their own table, invisible to search annotation, until a
    /// human promotes one via csr_resolve (Codex adversarial review — automatic
    /// ledger rows would be indistinguishable from human verdicts at read time).
    /// Idempotent per (chunk_id, session_id).
    pub fn insert_resolution_proposal(
        &self,
        chunk_id: &str,
        claim: Option<&str>,
        evidence: &str,
        session_id: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        conn.execute(
            "INSERT OR IGNORE INTO resolution_proposals (chunk_id, claim, evidence, session_id)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![chunk_id, claim, evidence, session_id],
        )?;
        Ok(())
    }

    pub fn count_resolution_proposals(&self) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        Ok(
            conn.query_row("SELECT COUNT(*) FROM resolution_proposals", [], |r| {
                r.get(0)
            })?,
        )
    }

    pub fn count_resolution_verdicts(&self) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        Ok(conn.query_row("SELECT COUNT(*) FROM resolution_ledger", [], |r| r.get(0))?)
    }

    /// Plan-corpus counts: (docs, chunks, unscoped_docs). Plan chunks are
    /// identified by their `plan:` conversation-id prefix, not `chunks.source`,
    /// matching how every reader distinguishes them.
    pub fn plan_source_counts(&self) -> Result<(i64, i64, i64)> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        conn.query_row(
            "SELECT COUNT(DISTINCT conversation_id), COUNT(*),
                    COUNT(DISTINCT CASE WHEN project_name = '_unscoped'
                          THEN conversation_id END)
             FROM chunks WHERE conversation_id LIKE 'plan:%'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(Into::into)
    }

    /// Read back a chunk's storage-level `source` attribute (see
    /// `insert_chunk_with_source`) — used by aux-source adapter tests.
    pub fn get_chunk_source(&self, id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_chunk_source(&conn, id)
    }

    pub fn rescope_sidechain_conversation(
        &self,
        conversation_id: &str,
        project_name: &str,
        parent_conversation_id: &str,
    ) -> Result<()> {
        let mut conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::rescope_sidechain_conversation(
            &mut conn,
            conversation_id,
            project_name,
            parent_conversation_id,
        )
    }

    /// Wipe a conversation's chunks + embeddings + FTS rows + provenance edges, so an
    /// aux-source adapter can rebuild it from scratch on reimport (idempotent even when
    /// the source document shrinks). See `queries::delete_chunks_for_conversation`.
    pub fn delete_chunks_for_conversation(&self, conversation_id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::delete_chunks_for_conversation(&conn, conversation_id)
    }

    pub fn record_narrative_usage(&self, row: &NarrativeUsageRow) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::record_narrative_usage(&conn, row)
    }

    pub fn narrative_usage_summary(&self) -> Result<NarrativeUsageSummary> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::narrative_usage_summary(&conn)
    }

    pub fn upsert_ratification_score(&self, row: &RatificationScoreRow) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::upsert_ratification_score(&conn, row)
    }

    pub fn get_ratification_score(&self, conversation_id: &str) -> Result<Option<f32>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_ratification_score(&conn, conversation_id)
    }

    pub fn get_ratification_scores(
        &self,
        ids: &[String],
    ) -> Result<std::collections::HashMap<String, f32>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_ratification_scores(&conn, ids)
    }

    pub fn ratification_summary(&self) -> Result<(i64, f64)> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::ratification_summary(&conn)
    }

    /// Integrity check with an app-level cache. SQLite recomputes
    /// `PRAGMA integrity_check` from scratch on every call — ~10s of CPU on a
    /// multi-GB DB (it walks every btree, including FTS5) — so the result is
    /// persisted in `meta` and reused.
    ///
    /// - Cache younger than `ttl_hours`: cached verdict, no recompute.
    /// - Cache stale: recomputes only when `refresh_if_stale` is true (daemon
    ///   ticks and `status --deep`); other callers keep serving the stale
    ///   verdict so a statusline poll never eats the 10s hit.
    /// - No cache yet: computes once and stores (fresh installs are small/fast).
    pub fn integrity_check_cached(&self, ttl_hours: i64, refresh_if_stale: bool) -> Result<bool> {
        let cached = self.get_meta("integrity_ok")?;
        let checked_at = self
            .get_meta("integrity_checked_at")?
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|t| t.with_timezone(&chrono::Utc));

        if let (Some(ok), Some(ts)) = (&cached, checked_at) {
            let age = chrono::Utc::now().signed_duration_since(ts);
            let fresh = age < chrono::Duration::hours(ttl_hours) && age.num_seconds() >= 0;
            if fresh || !refresh_if_stale {
                return Ok(ok == "1");
            }
        }

        let ok = self.integrity_check()?;
        self.set_meta("integrity_ok", if ok { "1" } else { "0" })?;
        self.set_meta("integrity_checked_at", &chrono::Utc::now().to_rfc3339())?;
        Ok(ok)
    }

    /// Attempt a WAL checkpoint+truncate. Long-lived MCP readers can hold the
    /// WAL open indefinitely, letting it grow to hundreds of MB; a periodic
    /// TRUNCATE attempt from the daemon reclaims it whenever readers allow.
    /// Best-effort: returns Ok(false) when readers blocked the truncate.
    pub fn checkpoint_wal(&self) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let (busy, _log, _ckpt): (i64, i64, i64) =
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
        Ok(busy == 0)
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

    pub(crate) fn mark_file_imported_with_suppression(
        &self,
        path: &Path,
        chunks: usize,
        suppression: CsrSuppressionStats,
    ) -> Result<()> {
        let mut conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::mark_file_imported_with_suppression(&mut conn, path, chunks, suppression)
    }

    /// Read an import_state mtime keyed by a synthetic (non-filesystem) `file_path`,
    /// for aux-source adapters. See `queries::get_import_state_mtime`.
    pub fn get_import_state_mtime(&self, file_path: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_import_state_mtime(&conn, file_path)
    }

    /// Upsert an import_state row with an explicit mtime, for aux-source adapters. See
    /// `queries::upsert_import_state_explicit`.
    pub fn upsert_import_state_explicit(
        &self,
        file_path: &str,
        conversation_id: &str,
        chunks: usize,
        mtime: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::upsert_import_state_explicit(&conn, file_path, conversation_id, chunks, mtime)
    }

    // ─── Session registry queries (multi-source corpus, v9.4) ───

    /// (first_ts, last_ts) for one session, if registered. See
    /// `queries::get_session_registry_window`.
    pub fn get_session_registry_window(
        &self,
        session_id: &str,
    ) -> Result<Option<(Option<String>, Option<String>)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_session_registry_window(&conn, session_id)
    }

    /// All (project, first_ts, last_ts) rows. See
    /// `queries::list_session_registry_windows`.
    pub fn list_session_registry_windows(&self) -> Result<Vec<queries::SessionWindowRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::list_session_registry_windows(&conn)
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

    // ─── Outcome scoring: retrieval stats ───

    pub fn update_retrieval_stats_for_session(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::update_retrieval_stats_for_session(&conn, session_id)
    }

    pub fn get_outcome_stats_batch(
        &self,
        memory_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, (i64, i64)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_outcome_stats_batch(&conn, memory_ids)
    }

    // ─── Completion queries (v8.3.0) ───

    pub fn list_project_names(&self, prefix: &str, limit: usize) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::list_project_names(&conn, prefix, limit)
    }

    pub fn list_file_paths(&self, prefix: &str, limit: usize) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::list_file_paths(&conn, prefix, limit)
    }

    pub fn list_session_ids(&self, prefix: &str, limit: usize) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::list_session_ids(&conn, prefix, limit)
    }

    // ─── Code evolution queries (v9) ───

    #[allow(clippy::too_many_arguments)]
    pub fn insert_code_evolution(
        &self,
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
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::insert_code_evolution(
            &conn,
            session_id,
            project_name,
            file_path,
            language,
            tool_name,
            functions_added,
            functions_removed,
            types_added,
            types_removed,
            imports_added,
            imports_removed,
            repo_root,
        )
    }

    /// Insert a backfilled code-evolution row (`csr-engine backfill-coedit`).
    /// Idempotent (`INSERT OR IGNORE` on the `id` PRIMARY KEY). Returns `true`
    /// if a new row was written, `false` if `id` already existed.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_code_evolution_backfill(
        &self,
        id: &str,
        session_id: &str,
        project_name: &str,
        file_path: &str,
        language: &str,
        tool_name: &str,
        timestamp: &str,
        repo_root: Option<&str>,
    ) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::insert_code_evolution_backfill(
            &conn,
            id,
            session_id,
            project_name,
            file_path,
            language,
            tool_name,
            timestamp,
            repo_root,
        )
    }

    /// True if a `code_evolution` row with this `id` already exists.
    pub fn code_evolution_id_exists(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::code_evolution_id_exists(&conn, id)
    }

    pub fn get_recent_code_evolution(
        &self,
        file_path: &str,
        project_name: &str,
        limit: usize,
    ) -> Result<Vec<queries::CodeEvolutionRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_recent_code_evolution(&conn, file_path, project_name, limit)
    }

    pub fn get_session_code_evolution(
        &self,
        session_id: &str,
    ) -> Result<Vec<queries::SessionCodeEvolutionRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_session_code_evolution(&conn, session_id)
    }

    // ─── Saga Phase 1 (WS1) ───

    pub fn get_chunk_ids_for_conversation(&self, conversation_id: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_chunk_ids_for_conversation(&conn, conversation_id)
    }

    pub fn get_chunk_vectors_by_ids(&self, ids: &[String]) -> Result<Vec<(String, Vec<f32>)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_chunk_vectors_by_ids(&conn, ids)
    }

    pub fn files_for_session(&self, session_id: &str, limit: usize) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::files_for_session(&conn, session_id, limit)
    }

    pub fn sessions_for_file(
        &self,
        file_path: &str,
        exclude_session: &str,
        project: Option<&str>,
        limit: usize,
    ) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::sessions_for_file(&conn, file_path, exclude_session, project, limit)
    }

    pub fn set_chunk_saga_columns(
        &self,
        chunk_id: &str,
        seq: usize,
        is_sidechain: bool,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::set_chunk_saga_columns(&conn, chunk_id, seq, is_sidechain)
    }

    pub fn list_all_import_state_file_paths(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::list_all_import_state_file_paths(&conn)
    }

    pub fn ground_truth_sessions_for_target(
        &self,
        target: &str,
    ) -> Result<std::collections::HashSet<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::ground_truth_sessions_for_target(&conn, target)
    }

    // ─── Episode anchors (v9.3) ───

    /// Replace all anchors for a session (delete-then-insert upsert).
    pub fn replace_session_anchors(
        &self,
        session_id: &str,
        project: &str,
        anchors: &[crate::extraction::anchors::FunctionAnchor],
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::replace_session_anchors(&conn, session_id, project, anchors)
    }

    /// Most-recent-first anchors for a project: `(session_id, anchor)`.
    pub fn get_project_anchors(
        &self,
        project: &str,
        limit: usize,
    ) -> Result<Vec<(String, crate::extraction::anchors::FunctionAnchor)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_project_anchors(&conn, project, limit as i64)
    }

    // ─── Code property graph (v9.4) ───

    pub fn upsert_code_node(&self, node: &codegraph::NodeRow) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::upsert_node(&conn, node)
    }

    pub fn replace_code_file_edges(
        &self,
        project: &str,
        src_file: &str,
        edges: &[codegraph::EdgeRow],
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::replace_file_edges(&conn, project, src_file, edges)
    }

    /// Distinct `code_nodes.file` values still missing `repo_root` (WP2 Stage 1 backfill).
    pub fn code_node_files_missing_repo_root(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::code_node_files_missing_repo_root(&conn)
    }

    /// Set `repo_root` on every `code_nodes` row matching `file` currently NULL.
    pub fn set_repo_root_for_file(&self, file: &str, repo_root: &str) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::set_repo_root_for_file(&conn, file, repo_root)
    }

    /// The `code_nodes`-stored `repo_root` for `file`, if any resolved node
    /// exists for it. `None` does not mean unresolvable — callers typically
    /// fall back to `extraction::repo_root::repo_root_for_file` (a live git
    /// walk) when this returns `None`, same as `dream`'s own join.
    pub fn stored_repo_root_for_file(&self, file: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::stored_repo_root_for_file(&conn, file)
    }

    /// Distinct `code_evolution.file_path` values still missing `repo_root` (WP2 Stage 1 backfill).
    pub fn code_evolution_files_missing_repo_root(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::code_evolution_files_missing_repo_root(&conn)
    }

    /// Set `repo_root` on every `code_evolution` row matching `file_path` currently NULL.
    pub fn set_repo_root_for_evolution_file(
        &self,
        file_path: &str,
        repo_root: &str,
    ) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::set_repo_root_for_evolution_file(&conn, file_path, repo_root)
    }

    /// Replace-per-file upsert of repo_defs (name, kind, lang) for `(project, file)`.
    pub fn upsert_repo_defs(
        &self,
        project: &str,
        file: &str,
        defs: &[(String, String, String)],
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::upsert_repo_defs(&conn, project, file, defs)
    }

    /// Definition sites for `name` within `project`: `(file, kind)`.
    pub fn lookup_repo_defs(&self, project: &str, name: &str) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::lookup_repo_defs(&conn, project, name)
    }

    pub fn upsert_code_file_state(
        &self,
        project: &str,
        file: &str,
        content_hash: &str,
        dirty: bool,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::upsert_file_state(&conn, project, file, content_hash, dirty)
    }

    pub fn mark_code_file_dirty(&self, project: &str, file: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::mark_file_dirty(&conn, project, file)
    }

    /// Record that `file` was seen by an extraction write path but its
    /// extension is outside the six AST-supported languages (WP2 Stage 3,
    /// H8 innovation — receipt R4). See `codegraph::mark_file_unsupported`.
    pub fn mark_code_file_unsupported(&self, project: &str, file: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::mark_file_unsupported(&conn, project, file)
    }

    /// Re-resolve placeholder edges for a project (two-pass name resolution).
    /// Uses a direct-stat default for the WCR Phase 6 TASK C stale-file
    /// check (`canonical_repo_path(file).is_file()`), suitable for hooks and
    /// backfill callers that resolve a handful of files at a time. Callers
    /// that will check many distinct files in one pass (e.g. the live eval
    /// gate, scanning the whole corpus) should precompute an existence set
    /// once and call `resolve_code_edges_with_fs_check` instead — see that
    /// method's doc comment.
    pub fn resolve_code_edges(
        &self,
        project: &str,
    ) -> Result<crate::extraction::resolver::ResolveStats> {
        self.resolve_code_edges_with_fs_check(project, &|file: &str| {
            crate::extraction::repo_path::canonical_repo_path(std::path::Path::new(file)).is_file()
        })
    }

    /// Same as `resolve_code_edges`, but with the WCR Phase 6 TASK C
    /// stale-file existence check supplied by the caller instead of
    /// defaulted. `file_exists` receives a `Pending::src_file` as stored
    /// (raw, not canonicalized) and must apply `canonical_repo_path` itself
    /// before checking — see `extraction::resolver::resolve_edges`'s doc
    /// comment. Exists so a caller resolving many projects/files in one pass
    /// (the live eval gate) can precompute a canonicalized existence set
    /// once up front and close over it, rather than re-stat'ing the
    /// filesystem per pending edge.
    pub fn resolve_code_edges_with_fs_check(
        &self,
        project: &str,
        file_exists: &dyn Fn(&str) -> bool,
    ) -> Result<crate::extraction::resolver::ResolveStats> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        crate::extraction::resolver::resolve_edges(&conn, project, file_exists)
    }

    /// Recompute degree ranks for a project.
    pub fn compute_code_rank(&self, project: &str) -> Result<crate::search::code_rank::RankStats> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        crate::search::code_rank::compute_code_rank(&conn, project)
    }

    pub fn code_file_ledger(&self, project: &str, file: &str) -> Result<codegraph::FileLedger> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::file_ledger(&conn, project, file)
    }

    pub fn code_query_callers(
        &self,
        name_or_id: &str,
        project: &str,
        limit: usize,
    ) -> Result<Vec<codegraph::NodeRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::query_callers(&conn, name_or_id, project, limit)
    }

    pub fn code_query_callees(
        &self,
        node_id: &str,
        limit: usize,
    ) -> Result<Vec<codegraph::NodeRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::query_callees(&conn, node_id, limit)
    }

    pub fn code_query_neighbors(
        &self,
        node_id: &str,
        kind_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<codegraph::NeighborEdge>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::query_neighbors(&conn, node_id, kind_filter, limit)
    }

    pub fn code_nodes_by_name(
        &self,
        name: &str,
        project: &str,
        limit: usize,
    ) -> Result<Vec<codegraph::NodeRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::nodes_by_name(&conn, name, project, limit)
    }

    pub fn code_get_node_rank(&self, id: &str) -> Result<Option<(f64, i64, i64)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::get_node_rank(&conn, id)
    }

    /// All `code_nodes` rows (WP2 Stage 2 attribution backfill).
    pub fn all_code_nodes(&self) -> Result<Vec<codegraph::NodeRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::all_nodes(&conn)
    }

    /// Every `code_evolution` event, oldest-first (WP2 Stage 2 transcript-channel backfill).
    pub fn all_code_evolution_events_ordered(&self) -> Result<Vec<queries::CodeEvolutionEventRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::all_code_evolution_events_ordered(&conn)
    }

    /// Upsert one attribution channel row (WP2 Stage 2).
    pub fn upsert_code_attribution(&self, row: &codegraph::AttributionRow) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::upsert_attribution(&conn, row)
    }

    /// Raw attribution rows for a node (0-2 rows, one per channel).
    pub fn code_attribution_rows(&self, node_id: &str) -> Result<Vec<codegraph::AttributionRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::get_attribution(&conn, node_id)
    }

    /// Rendered attribution summary for a node — what `csr_code_graph` /
    /// `csr_search_by_file` display. Never falls back to `first_conv_id`.
    pub fn code_attribution_for_node(&self, node_id: &str) -> Result<String> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        codegraph::attribution_for_node(&conn, node_id)
    }

    // ─── Resolution ledger ───

    /// Append resolution verdicts for one or more chunk ids.
    pub fn insert_resolutions(
        &self,
        chunk_ids: &[String],
        status: &str,
        evidence: &str,
        claim: Option<&str>,
        source: &str,
    ) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::insert_resolutions(&conn, chunk_ids, status, evidence, claim, source)
    }

    /// Batch-fetch latest resolution entries keyed by chunk_id.
    pub fn get_resolutions_batch(
        &self,
        chunk_ids: &[String],
    ) -> Result<std::collections::HashMap<String, queries::ResolutionEntry>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::get_resolutions_batch(&conn, chunk_ids)
    }

    // ─── Dream verdicts (v10) ───

    /// Batch-resolve dream-verdict hits for a set of conversation ids — see
    /// `chunk_binding::witness_verdict_for_chunks`'s two-channel contract.
    /// Consumed by the search validity partition (`mcp::tools`) to
    /// demote/annotate chunks whose underlying code claim the `dream` cycle
    /// has determined is stale (`Demote`) or has evolved (`Annotate`). One
    /// batched query per call — zero git access, purely a read over
    /// precomputed `witness_verdicts` events.
    pub fn witness_verdicts_for_conversations(
        &self,
        conversation_ids: &[String],
    ) -> Result<std::collections::BTreeMap<String, Vec<chunk_binding::ChunkWitnessVerdict>>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        chunk_binding::witness_verdict_for_chunks(&conn, conversation_ids)
    }

    // ─── Session registry / aux coverage ───

    /// Runs `f` with the raw connection inside a single SQLite transaction.
    /// Used by registry ingest to make "upsert rows" + "advance checkpoint"
    /// atomic — a crash between the two is otherwise how a batch could be
    /// replayed or silently dropped. Enforced via this single
    /// `with_transaction` call wrapping both the row upserts and the meta
    /// checkpoint writes in `import::registry::ingest_history`.
    pub fn with_transaction<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let tx = conn.unchecked_transaction()?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    /// (sessions_seen, sessions_imported, gap) from session_registry vs chunks.
    pub fn coverage_stats(&self) -> anyhow::Result<(i64, i64, i64)> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::coverage_stats(&conn)
    }

    /// Subset of candidate session ids present in session_registry or chunks.
    pub fn known_session_ids(
        &self,
        candidates: &[String],
    ) -> anyhow::Result<std::collections::HashSet<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        queries::known_session_ids(&conn, candidates)
    }

    // ─── Witness ledger (v10 "dreaming" substrate) ───
    //
    // APPEND-ONLY: insert + query only, no update/delete — see
    // `witness_ledger`'s module doc for the full invariant.

    /// Append one witness row. Duplicates (symbol-level AND whole-file
    /// NULL-key rows alike) are a silent no-op: `INSERT OR IGNORE` against
    /// the COALESCE-based `idx_witness_ledger_identity` UNIQUE index.
    pub fn insert_witness(&self, row: &witness_ledger::WitnessLedgerRow) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        witness_ledger::insert_witness(&conn, row)
    }

    /// Full append-only history of witnesses for `(project, file)`, oldest-first.
    pub fn witnesses_for_file(
        &self,
        project: &str,
        file: &str,
    ) -> Result<Vec<witness_ledger::WitnessLedgerRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        witness_ledger::witnesses_for_file(&conn, project, file)
    }

    /// Most recently inserted witness for `(project, file, symbol)`;
    /// `symbol = None` selects the whole-file witness.
    pub fn latest_witness_for_symbol(
        &self,
        project: &str,
        file: &str,
        symbol: Option<&str>,
    ) -> Result<Option<witness_ledger::WitnessLedgerRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        witness_ledger::latest_witness_for_symbol(&conn, project, file, symbol)
    }

    /// Count of ledger rows for `(project, file)` (idempotency checks).
    pub fn count_witnesses_for_file(&self, project: &str, file: &str) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        witness_ledger::count_witnesses_for_file(&conn, project, file)
    }

    /// Every `tier = 'committed'` witness row, grouped by `(project, file,
    /// symbol)`. Feeds `dream`'s successor join.
    pub fn all_committed_witnesses(&self) -> Result<Vec<witness_ledger::WitnessLedgerRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        witness_ledger::all_committed_witnesses(&conn)
    }

    /// A single witness row by its primary key.
    pub fn witness_by_id(&self, id: i64) -> Result<Option<witness_ledger::WitnessLedgerRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        witness_ledger::witness_by_id(&conn, id)
    }

    // ─── Witness verdicts (v10 "dreaming" — see `crate::dream`) ───
    //
    // APPEND-ONLY: insert + query only, no update/delete — see
    // `witness_verdicts`'s module doc for the full invariant.

    /// The latest recorded verdict event for a specific `witness_ledger.id`.
    pub fn latest_witness_verdict(
        &self,
        witness_id: i64,
    ) -> Result<Option<witness_verdicts::WitnessVerdictRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        witness_verdicts::latest_event(&conn, witness_id)
    }

    /// Insert a verdict event unless it is identical to the latest recorded
    /// event for that witness (see `witness_verdicts::is_new_event`).
    /// Returns whether a new row was actually written.
    pub fn insert_witness_verdict(
        &self,
        row: &witness_verdicts::WitnessVerdictRow,
    ) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        witness_verdicts::insert_verdict_if_changed(&conn, row)
    }

    /// Order-independent symbol-level current state: the resolved
    /// `Demote`/`Annotate` channel plus representative negative event iff
    /// the `(project, file, symbol)` anchor carries an uncancelled negative
    /// verdict — see `witness_verdicts`'s "Symbol-level current state"
    /// module doc for the two-channel rule chunk binding relies on.
    pub fn symbol_verdict_state(
        &self,
        project: &str,
        file: &str,
        symbol: Option<&str>,
    ) -> Result<Option<witness_verdicts::SymbolVerdictState>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        witness_verdicts::symbol_verdict_state(&conn, project, file, symbol)
    }

    /// Every `witness_verdicts` event ever recorded, newest-first, joined to
    /// its witness's anchor identity — the dream report's timeline. See
    /// `witness_verdicts::all_events_with_anchor`.
    pub fn all_dream_events(&self) -> Result<Vec<witness_verdicts::DreamEventRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        witness_verdicts::all_events_with_anchor(&conn)
    }

    /// The most recent dream cycle observed anywhere in the ledger:
    /// `(observed_head_oid, created_at)` of the globally newest event.
    /// `None` if `dream` has never written an event.
    pub fn last_dream_run(&self) -> Result<Option<(String, String)>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        witness_verdicts::last_dream_run(&conn)
    }

    /// Totals `(obsolete, superseded, reinstated)` across every event ever
    /// recorded — feeds `status`'s `dream.by_verdict` and the report header.
    pub fn dream_event_totals(&self) -> Result<(i64, i64, i64)> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        witness_verdicts::event_totals_by_verdict(&conn)
    }

    /// Every `(project, file, symbol)` anchor currently on the `Demote`
    /// channel — "what CSR forgot". See `witness_verdicts::all_demoted_symbols`.
    pub fn all_demoted_symbols(&self) -> Result<Vec<witness_verdicts::DemotedSymbol>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        witness_verdicts::all_demoted_symbols(&conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_anchors_roundtrip() {
        let storage = Storage::open_memory().unwrap();
        let a = crate::extraction::anchors::FunctionAnchor {
            file: "src/auth.rs".into(),
            node_kind: "function_item".into(),
            name: "validate_token".into(),
            body_hash: "9f3a1c2e44b8d701".into(),
        };
        storage
            .replace_session_anchors("sess-1", "proj", std::slice::from_ref(&a))
            .unwrap();
        // Upsert: replacing again must not duplicate
        storage
            .replace_session_anchors("sess-1", "proj", std::slice::from_ref(&a))
            .unwrap();
        let got = storage.get_project_anchors("proj", 100).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1.name, "validate_token");
        assert_eq!(got[0].0, "sess-1"); // session_id comes back too
    }

    #[test]
    fn chunk_provenance_roundtrip() {
        use crate::import::ConversationChunk;
        use crate::provenance::{ChunkProvenance, Speaker};
        let storage = Storage::open_memory().unwrap();

        // Provenance references a chunk row (FK enforced).
        let chunk = ConversationChunk {
            id: "chunk-1".into(),
            conversation_id: "0bab445f".into(),
            project_name: "proj".into(),
            timestamp: "2026-06-10T12:00:00Z".into(),
            content: "the vision".into(),
            message_count: 1,
            summary: None,
            author: Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[0.0; 384]).unwrap();

        let p = ChunkProvenance {
            author: Speaker::User,
            source_conv_id: "0bab445f".into(),
            supersedes: Some("behavioral continuity".into()),
        };
        storage.insert_chunk_provenance("chunk-1", &p).unwrap();
        // Upsert: inserting again must not duplicate or error.
        storage.insert_chunk_provenance("chunk-1", &p).unwrap();

        let got = storage
            .get_chunk_provenance("chunk-1")
            .unwrap()
            .expect("provenance present");
        assert_eq!(got.author, Speaker::User);
        assert_eq!(got.source_conv_id, "0bab445f");
        assert_eq!(got.supersedes.as_deref(), Some("behavioral continuity"));

        assert!(storage.get_chunk_provenance("missing").unwrap().is_none());
    }

    #[test]
    fn chunk_source_persists_and_defaults() {
        use crate::provenance::Speaker;
        let storage = Storage::open_memory().unwrap();
        let mk = |id: &str| ConversationChunk {
            id: id.into(),
            conversation_id: format!("conv-{id}"),
            project_name: "proj".into(),
            timestamp: "2026-07-27T12:00:00Z".into(),
            content: "content".into(),
            message_count: 1,
            summary: None,
            author: Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&mk("a"), &[0.0; 4]).unwrap();
        storage
            .insert_chunk_with_source(&mk("b"), &[0.0; 4], "plan")
            .unwrap();
        let conn = storage.conn.lock().unwrap();
        let src = |id: &str| -> String {
            conn.query_row("SELECT source FROM chunks WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
        };
        assert_eq!(src("a"), "conversation");
        assert_eq!(src("b"), "plan");
    }

    #[test]
    fn aux_counters_increment_and_list() {
        let storage = Storage::open_memory().unwrap();
        assert!(storage.get_aux_counters().unwrap().is_empty());
        storage.bump_aux_counter("tasks").unwrap();
        storage.bump_aux_counter("tasks").unwrap();
        storage.bump_aux_counter("history").unwrap();
        let mut counters = storage.get_aux_counters().unwrap();
        counters.sort();
        assert_eq!(
            counters,
            vec![("history".to_string(), 1), ("tasks".to_string(), 2)]
        );
    }

    #[test]
    fn aux_counter_can_quarantine_multiple_raw_lines() {
        let storage = Storage::open_memory().unwrap();
        storage.bump_aux_counter_by("codex_rollout", 3).unwrap();
        assert_eq!(
            storage.get_aux_counters().unwrap(),
            vec![("codex_rollout".to_string(), 3)]
        );
    }

    #[test]
    fn sidechain_rescope_repairs_project_source_and_parent_link_idempotently() {
        use crate::provenance::Speaker;
        let storage = Storage::open_memory().unwrap();
        let chunk = ConversationChunk {
            id: "sidechunk-1".into(),
            conversation_id: "agent-child".into(),
            project_name: "subagents".into(),
            timestamp: "2026-08-06T12:00:00Z".into(),
            content: "sidechain evidence".into(),
            message_count: 1,
            summary: None,
            author: Speaker::Assistant,
            seq: 0,
            is_sidechain: true,
        };
        storage.insert_chunk(&chunk, &[0.0; 4]).unwrap();

        storage
            .rescope_sidechain_conversation("agent-child", "real-project", "parent-session")
            .unwrap();
        let changes_after_first = storage.conn.lock().unwrap().total_changes();
        storage
            .rescope_sidechain_conversation("agent-child", "real-project", "parent-session")
            .unwrap();
        let changes_after_second = storage.conn.lock().unwrap().total_changes();
        assert_eq!(
            changes_after_second, changes_after_first,
            "an already-correct discovery scan must perform zero writes"
        );

        let conn = storage.conn.lock().unwrap();
        let row: (String, String) = conn
            .query_row(
                "SELECT project_name, source FROM chunks WHERE id = 'sidechunk-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        drop(conn);
        assert_eq!(row, ("real-project".to_string(), "sidechain".to_string()));
        let provenance = storage
            .get_chunk_provenance("sidechunk-1")
            .unwrap()
            .unwrap();
        assert_eq!(provenance.author, Speaker::Assistant);
        assert_eq!(provenance.source_conv_id, "parent-session");
    }

    #[test]
    fn get_chunk_vectors_by_ids_roundtrip() {
        use crate::import::ConversationChunk;
        use crate::provenance::Speaker;
        use std::collections::HashMap;

        let storage = Storage::open_memory().unwrap();
        let id1 = "vec-chunk-1".to_string();
        let id2 = "vec-chunk-2".to_string();
        let emb1 = vec![0.1; 384];
        let emb2 = vec![0.2; 384];
        storage
            .insert_chunk(
                &ConversationChunk {
                    id: id1.clone(),
                    conversation_id: "conv-v".into(),
                    project_name: "proj".into(),
                    timestamp: "2026-06-10T12:00:00Z".into(),
                    content: "one".into(),
                    message_count: 1,
                    summary: None,
                    author: Speaker::User,
                    seq: 0,
                    is_sidechain: false,
                },
                &emb1,
            )
            .unwrap();
        storage
            .insert_chunk(
                &ConversationChunk {
                    id: id2.clone(),
                    conversation_id: "conv-v".into(),
                    project_name: "proj".into(),
                    timestamp: "2026-06-10T12:00:00Z".into(),
                    content: "two".into(),
                    message_count: 1,
                    summary: None,
                    author: Speaker::User,
                    seq: 1,
                    is_sidechain: false,
                },
                &emb2,
            )
            .unwrap();

        let got = storage
            .get_chunk_vectors_by_ids(&[id1.clone(), id2.clone(), "nonexistent".to_string()])
            .unwrap();
        assert_eq!(got.len(), 2);
        let map: HashMap<String, Vec<f32>> = got.into_iter().collect();
        assert!((map[&id1][0] - 0.1).abs() < 1e-6);
        assert!((map[&id2][0] - 0.2).abs() < 1e-6);
        assert!(!map.contains_key("nonexistent"));
    }

    #[test]
    fn derivation_ledger_roundtrip_and_reuse() {
        use crate::ledger::{CostBucket, LedgerEntry, Scope};
        let storage = Storage::open_memory().unwrap();
        let scope = Scope {
            repo: "csr".into(),
            branch: "main".into(),
            user: "rama".into(),
        };
        let e = LedgerEntry {
            id: "f1".into(),
            content: "epistemic continuity supersedes behavioral".into(),
            anchor: Some("validate_token".into()),
            cost_bucket: CostBucket::Expensive,
            inferability: 0.1,
            confidence: 0.9,
            times_reused: 0,
            scope: scope.clone(),
        };
        storage.upsert_ledger_entry(&e).unwrap();
        storage.upsert_ledger_entry(&e).unwrap(); // upsert: no dup

        let got = storage.get_ledger_entries(&scope, 100).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].cost_bucket, CostBucket::Expensive);
        assert_eq!(got[0].anchor.as_deref(), Some("validate_token"));

        storage.increment_ledger_reuse("f1").unwrap();
        let got = storage.get_ledger_entries(&scope, 100).unwrap();
        assert_eq!(got[0].times_reused, 1);

        // Scope isolation: a different scope sees nothing.
        let other = Scope {
            repo: "csr".into(),
            branch: "feature".into(),
            user: "rama".into(),
        };
        assert!(storage.get_ledger_entries(&other, 100).unwrap().is_empty());
    }

    #[test]
    fn ledger_same_id_different_scope_no_crosstalk() {
        // Codex HIGH: an id reused across scopes must NOT clobber the other scope.
        use crate::ledger::{CostBucket, LedgerEntry, Scope};
        let storage = Storage::open_memory().unwrap();
        let mk = |branch: &str, content: &str| LedgerEntry {
            id: "shared".into(),
            content: content.into(),
            anchor: None,
            cost_bucket: CostBucket::Moderate,
            inferability: 0.2,
            confidence: 0.8,
            times_reused: 0,
            scope: Scope {
                repo: "csr".into(),
                branch: branch.into(),
                user: "rama".into(),
            },
        };
        let a = mk("main", "main-scope fact");
        let b = mk("feature", "feature-scope fact");
        storage.upsert_ledger_entry(&a).unwrap();
        storage.upsert_ledger_entry(&b).unwrap();

        let got_a = storage.get_ledger_entries(&a.scope, 100).unwrap();
        let got_b = storage.get_ledger_entries(&b.scope, 100).unwrap();
        assert_eq!(got_a.len(), 1, "main scope keeps its own row");
        assert_eq!(got_a[0].content, "main-scope fact");
        assert_eq!(got_b.len(), 1, "feature scope keeps its own row");
        assert_eq!(got_b[0].content, "feature-scope fact");
    }

    #[test]
    fn meta_kv_roundtrip() {
        let storage = Storage::open_memory().unwrap();
        assert_eq!(storage.get_meta("missing").unwrap(), None);
        storage.set_meta("k", "v1").unwrap();
        assert_eq!(storage.get_meta("k").unwrap().as_deref(), Some("v1"));
        storage.set_meta("k", "v2").unwrap();
        assert_eq!(storage.get_meta("k").unwrap().as_deref(), Some("v2"));
    }

    #[test]
    fn integrity_cached_no_cache_computes_and_stores() {
        let storage = Storage::open_memory().unwrap();
        assert!(storage.integrity_check_cached(24, false).unwrap());
        assert_eq!(
            storage.get_meta("integrity_ok").unwrap().as_deref(),
            Some("1")
        );
        assert!(storage.get_meta("integrity_checked_at").unwrap().is_some());
    }

    #[test]
    fn integrity_cached_fresh_serves_cache_without_recompute() {
        let storage = Storage::open_memory().unwrap();
        // Plant a fresh-but-wrong verdict: if the cache is honored, we get the
        // planted value back instead of the real (healthy) recompute.
        storage.set_meta("integrity_ok", "0").unwrap();
        storage
            .set_meta("integrity_checked_at", &chrono::Utc::now().to_rfc3339())
            .unwrap();
        assert!(!storage.integrity_check_cached(24, true).unwrap());
        assert!(!storage.integrity_check_cached(24, false).unwrap());
    }

    #[test]
    fn integrity_cached_stale_behavior_depends_on_refresh_flag() {
        let storage = Storage::open_memory().unwrap();
        let stale = chrono::Utc::now() - chrono::Duration::hours(48);
        storage.set_meta("integrity_ok", "0").unwrap();
        storage
            .set_meta("integrity_checked_at", &stale.to_rfc3339())
            .unwrap();
        // Stale + no refresh: keep serving the stale verdict (statusline path).
        assert!(!storage.integrity_check_cached(24, false).unwrap());
        // Stale + refresh: recompute (daemon path) and update the cache.
        assert!(storage.integrity_check_cached(24, true).unwrap());
        assert_eq!(
            storage.get_meta("integrity_ok").unwrap().as_deref(),
            Some("1")
        );
    }

    #[test]
    fn checkpoint_wal_is_safe_on_memory_db() {
        let storage = Storage::open_memory().unwrap();
        // In-memory DBs have no WAL — the pragma must not error.
        let _ = storage.checkpoint_wal();
    }

    #[test]
    fn resolution_ledger_insert_and_batch_read() {
        let storage = Storage::open_memory().unwrap();
        let n = storage
            .insert_resolutions(
                &["c1".to_string(), "c2".to_string()],
                "resolved",
                "test evidence",
                None,
                "agent",
            )
            .unwrap();
        assert_eq!(n, 2);

        let map = storage
            .get_resolutions_batch(&["c1".to_string(), "c2".to_string()])
            .unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("c1").unwrap().status, "resolved");
        assert_eq!(map.get("c2").unwrap().status, "resolved");
        assert_eq!(map.get("c1").unwrap().evidence, "test evidence");
    }

    #[test]
    fn resolution_ledger_latest_wins() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_resolutions(&["c1".to_string()], "resolved", "first", None, "agent")
            .unwrap();
        storage
            .insert_resolutions(&["c1".to_string()], "regressed", "later", None, "agent")
            .unwrap();
        storage
            .insert_resolutions(&["c2".to_string()], "resolved", "ok", None, "agent")
            .unwrap();

        let map = storage
            .get_resolutions_batch(&["c1".to_string(), "c2".to_string()])
            .unwrap();
        assert_eq!(map.get("c1").unwrap().status, "regressed");
        assert_eq!(map.get("c2").unwrap().status, "resolved");
    }

    #[test]
    fn resolution_ledger_unknown_id_absent() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_resolutions(&["c1".to_string()], "resolved", "evidence", None, "agent")
            .unwrap();

        let map = storage
            .get_resolutions_batch(&["c1".to_string(), "unknown-id".to_string()])
            .unwrap();
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("c1"));
        assert!(!map.contains_key("unknown-id"));
    }

    #[test]
    fn resolution_ledger_empty_batch() {
        let storage = Storage::open_memory().unwrap();
        let map = storage.get_resolutions_batch(&[]).unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn resolution_ledger_invalid_status_errors() {
        let storage = Storage::open_memory().unwrap();
        let result = storage.insert_resolutions(
            &["c1".to_string()],
            "bogus_status",
            "evidence",
            None,
            "agent",
        );
        assert!(result.is_err());
    }

    #[test]
    fn coverage_stats_math() {
        use crate::import::ConversationChunk;
        use crate::provenance::Speaker;

        let storage = Storage::open_memory().unwrap();
        // Seed 3 session_registry rows directly.
        {
            let conn = storage.conn.lock().unwrap();
            for (id, proj) in [("sr1", "p"), ("sr2", "p"), ("sr3", "p")] {
                conn.execute(
                    "INSERT INTO session_registry (session_id, project, first_prompt, first_ts, last_ts, prompt_count)
                     VALUES (?1, ?2, 'x', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 1)",
                    rusqlite::params![id, proj],
                )
                .unwrap();
            }
        }
        // Chunks for exactly 1 of those 3 conversation_ids.
        storage
            .insert_chunk(
                &ConversationChunk {
                    id: "chunk-sr1".into(),
                    conversation_id: "sr1".into(),
                    project_name: "p".into(),
                    timestamp: "2026-01-01T00:00:00Z".into(),
                    content: "c".into(),
                    message_count: 1,
                    summary: None,
                    author: Speaker::User,
                    seq: 0,
                    is_sidechain: false,
                },
                &[0.0; 4],
            )
            .unwrap();

        assert_eq!(storage.coverage_stats().unwrap(), (3, 1, 2));
    }

    #[test]
    fn per_file_csr_suppression_counters_only_apply_new_deltas() {
        let storage = Storage::open_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("counter.jsonl");
        let initial = [
            serde_json::json!({
                "type": "assistant",
                "message": {"content": [
                    {"type": "tool_use", "id": "csr-1", "name": "csr_reflect_on_past", "input": {"query": "old"}},
                    {"type": "text", "text": "ordinary assistant prose"}
                ]}
            }),
            serde_json::json!({
                "type": "user",
                "message": {"content": [
                    {"type": "text", "text": "before <system-reminder>CSR PICKUP — injected</system-reminder> after"},
                    {"type": "tool_result", "tool_use_id": "csr-1", "content": "old result"}
                ]}
            }),
        ];
        std::fs::write(
            &path,
            initial
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();

        let parsed = crate::import::parse_jsonl_file_with_stats(&path, "test").unwrap();
        storage
            .mark_file_imported_with_suppression(&path, parsed.chunks.len(), parsed.suppression)
            .unwrap();
        assert_eq!(storage.get_csr_tool_blocks_suppressed().unwrap(), 2);
        assert_eq!(storage.get_csr_hook_wrappers_scrubbed().unwrap(), 1);
        assert_eq!(storage.get_csr_self_suppressed().unwrap(), 3);

        // Same full-file totals must not recount history.
        storage
            .mark_file_imported_with_suppression(&path, parsed.chunks.len(), parsed.suppression)
            .unwrap();
        assert_eq!(storage.get_csr_self_suppressed().unwrap(), 3);

        // Appending only ordinary content leaves both suppression totals unchanged.
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str(&format!(
            "\n{}",
            serde_json::json!({
                "type": "user",
                "message": {"content": [{"type": "text", "text": "ordinary append"}]}
            })
        ));
        std::fs::write(&path, &content).unwrap();
        let parsed = crate::import::parse_jsonl_file_with_stats(&path, "test").unwrap();
        storage
            .mark_file_imported_with_suppression(&path, parsed.chunks.len(), parsed.suppression)
            .unwrap();
        assert_eq!(storage.get_csr_self_suppressed().unwrap(), 3);

        // A newly appended correlated CSR pair contributes exactly two tool blocks.
        content.push_str(
            &format!(
                "\n{}\n{}",
                serde_json::json!({
                    "type": "assistant",
                    "message": {"content": [{"type": "tool_use", "id": "csr-2", "name": "csr_quick_check", "input": {"query": "new"}}]}
                }),
                serde_json::json!({
                    "type": "user",
                    "message": {"content": [{"type": "tool_result", "tool_use_id": "csr-2", "content": "new result"}]}
                })
            ),
        );
        std::fs::write(&path, content).unwrap();
        let parsed = crate::import::parse_jsonl_file_with_stats(&path, "test").unwrap();
        storage
            .mark_file_imported_with_suppression(&path, parsed.chunks.len(), parsed.suppression)
            .unwrap();

        assert_eq!(storage.get_csr_tool_blocks_suppressed().unwrap(), 4);
        assert_eq!(storage.get_csr_hook_wrappers_scrubbed().unwrap(), 1);
        assert_eq!(storage.get_csr_self_suppressed().unwrap(), 5);
    }

    #[test]
    fn migrated_import_row_baselines_history_before_applying_new_suppression() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("legacy.db");
        let transcript_path = dir.path().join("existing.jsonl");
        let pair = |id: &str| {
            [
                serde_json::json!({
                    "type": "assistant",
                    "message": {"content": [{
                        "type": "tool_use", "id": id, "name": "csr_reflect_on_past",
                        "input": {"query": "history"}
                    }]}
                }),
                serde_json::json!({
                    "type": "user",
                    "message": {"content": [{
                        "type": "tool_result", "tool_use_id": id, "content": "historical result"
                    }]}
                }),
            ]
        };
        let mut messages = pair("csr-1").to_vec();
        std::fs::write(
            &transcript_path,
            messages
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();

        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE import_state (
                    file_path TEXT PRIMARY KEY,
                    conversation_id TEXT,
                    chunks_imported INTEGER,
                    imported_at TEXT DEFAULT (datetime('now')),
                    file_mtime TEXT
                 );
                 CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO import_state (file_path, conversation_id, chunks_imported, file_mtime) VALUES (?1, 'existing', 1, 'legacy')",
                [transcript_path.to_string_lossy().as_ref()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('csr_self_suppressed', '2')",
                [],
            )
            .unwrap();
        }

        let storage = Storage::open(&db_path).unwrap();
        let parsed = crate::import::parse_jsonl_file_with_stats(&transcript_path, "test").unwrap();
        storage
            .mark_file_imported_with_suppression(
                &transcript_path,
                parsed.chunks.len(),
                parsed.suppression,
            )
            .unwrap();
        assert_eq!(storage.get_csr_self_suppressed().unwrap(), 2);

        messages.extend(pair("csr-2"));
        std::fs::write(
            &transcript_path,
            messages
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let parsed = crate::import::parse_jsonl_file_with_stats(&transcript_path, "test").unwrap();
        storage
            .mark_file_imported_with_suppression(
                &transcript_path,
                parsed.chunks.len(),
                parsed.suppression,
            )
            .unwrap();
        assert_eq!(storage.get_csr_self_suppressed().unwrap(), 4);

        storage
            .mark_file_imported_with_suppression(
                &transcript_path,
                parsed.chunks.len(),
                parsed.suppression,
            )
            .unwrap();
        assert_eq!(storage.get_csr_self_suppressed().unwrap(), 4);
    }
}
