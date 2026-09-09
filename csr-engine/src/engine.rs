use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use rmcp::ServiceExt;
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::import;
use crate::import::watcher::FileWatcher;
use crate::mcp::CsrServer;
use crate::search::SearchEngine;
use crate::storage::Storage;

/// Append a timing line to ~/.claude-self-reflect/hook-timing.log.
fn log_timing(line: &str) {
    crate::telemetry::append_timing_line(line);
}

/// Orchestrates all subsystems: storage, embeddings, search, import, and MCP.
#[derive(Clone)]
pub struct Engine {
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    projects_dir: PathBuf,
    index_dir: PathBuf,
}

impl Engine {
    /// Create an engine from pre-built components (for testing).
    pub fn from_parts(
        storage: Arc<Storage>,
        embeddings: Arc<EmbeddingEngine>,
        search: Arc<RwLock<SearchEngine>>,
        projects_dir: PathBuf,
    ) -> Self {
        // Derive index_dir from projects_dir for test constructor
        let index_dir = projects_dir.join("index");
        Self {
            storage,
            embeddings,
            search,
            projects_dir,
            index_dir,
        }
    }

    pub fn new(db_path: &Path, projects_dir: &Path) -> Result<Self> {
        let t0 = std::time::Instant::now();

        tracing::info!(?db_path, "opening storage");
        let storage = Arc::new(Storage::open(db_path)?);
        let t_storage = t0.elapsed();

        tracing::info!("initializing embedding engine");
        let embeddings = Arc::new(EmbeddingEngine::new()?);
        let t_embed = t0.elapsed();

        // Compute index cache directory alongside the database
        let index_dir = db_path.parent().unwrap_or(Path::new(".")).join("index");

        // Fast O(1) counts for staleness check (~1ms)
        let chunk_count = storage.count_chunk_embeddings()?;
        let reflection_count = storage.count_reflection_embeddings()?;
        let t_count = t0.elapsed();

        // Try loading from disk cache first
        let search = if let Some(cached) =
            SearchEngine::load_from_disk(&index_dir, chunk_count, reflection_count)
        {
            let t_total = t0.elapsed();
            let startup_line = format!(
                "CSR startup: storage={:.0}ms embed={:.0}ms cache_load={:.0}ms total={:.0}ms ({} chunks, cached)",
                t_storage.as_secs_f64() * 1000.0,
                (t_embed - t_storage).as_secs_f64() * 1000.0,
                (t_total - t_count).as_secs_f64() * 1000.0,
                t_total.as_secs_f64() * 1000.0,
                chunk_count,
            );
            eprintln!("{}", startup_line);
            log_timing(&startup_line);
            tracing::info!(
                chunks = chunk_count,
                reflections = reflection_count,
                "search index loaded from cache"
            );
            // Reconcile HNSW cache with DB:
            // 1. Blank orphan entries (deleted from DB since last dump)
            // 2. Backfill missing entries (added to DB since last dump)
            let mut search = cached;
            if let Ok(db_ids) = storage.load_all_reflection_ids() {
                let db_id_set: std::collections::HashSet<&str> =
                    db_ids.iter().map(|s| s.as_str()).collect();
                let blanked = search.blank_orphan_reflections(&db_id_set);
                if blanked > 0 {
                    tracing::info!(blanked, "blanked orphan reflection entries in HNSW cache");
                }

                // Backfill reflections that exist in DB but not in HNSW.
                // Fetch only the missing ids, in bounded batches — never the
                // whole reflection table.
                let missing: Vec<String> = db_id_set
                    .iter()
                    .filter(|id| !search.has_reflection(id))
                    .map(|s| s.to_string())
                    .collect();
                if !missing.is_empty() {
                    if let Ok(added) = backfill_missing_reflections(&storage, &mut search, &missing)
                    {
                        if added > 0 {
                            tracing::info!(added, "backfilled missing reflections into HNSW cache");
                        }
                    }
                }
            }

            // Backfill chunks added to DB since the last dump (additive drift).
            // Cheap ID probe first; only fetch the vectors that are actually
            // missing (in bounded batches) if something is missing. This is what
            // lets a stale-but-additive cache load in ~ms instead of triggering a
            // full HNSW rebuild (~tens of seconds), while keeping peak transient
            // memory O(batch) rather than O(corpus).
            if let Ok(db_chunk_ids) = storage.load_all_chunk_ids() {
                let missing: Vec<String> = db_chunk_ids
                    .into_iter()
                    .filter(|id| !search.has_chunk(id))
                    .collect();
                if !missing.is_empty() {
                    if let Ok(added) = backfill_missing_chunks(&storage, &mut search, &missing) {
                        if added > 0 {
                            tracing::info!(added, "backfilled missing chunks into HNSW cache");
                        }
                    }
                }
            }
            search
        } else {
            // Cache miss — rebuild from SQLite vectors, streaming in bounded
            // batches so peak transient memory is O(batch) instead of
            // O(corpus) (no `load_all_chunk_vectors()` + clone-into-index).
            tracing::info!("building search index from stored vectors");

            let (mut search, chunk_total, reflection_total) =
                rebuild_search_index_streaming(&storage, RECONCILE_BATCH)?;
            let t_hnsw = t0.elapsed();

            // Re-query counts right before dump to minimize staleness window (E-1)
            let chunk_count = storage.count_chunk_embeddings().unwrap_or(chunk_count);
            let reflection_count = storage
                .count_reflection_embeddings()
                .unwrap_or(reflection_count);
            // Dump to disk for next startup
            if let Err(e) = search.dump_to_disk(&index_dir, chunk_count, reflection_count) {
                tracing::warn!(error = %e, "failed to cache HNSW index (non-fatal)");
            }
            let t_total = t0.elapsed();

            tracing::info!(
                chunks = chunk_total,
                reflections = reflection_total,
                "search index rebuilt and cached"
            );
            let startup_line = format!(
                "CSR startup: storage={:.0}ms embed={:.0}ms hnsw={:.0}ms dump={:.0}ms total={:.0}ms ({} chunks, rebuilt)",
                t_storage.as_secs_f64() * 1000.0,
                (t_embed - t_storage).as_secs_f64() * 1000.0,
                (t_hnsw - t_count).as_secs_f64() * 1000.0,
                (t_total - t_hnsw).as_secs_f64() * 1000.0,
                t_total.as_secs_f64() * 1000.0,
                chunk_total,
            );
            eprintln!("{}", startup_line);
            log_timing(&startup_line);

            search
        };

        // Clean stale numbered HNSW files from previous sessions (crash resilience).
        // Acquire exclusive lock to avoid racing with concurrent dump_to_disk writers.
        if let Ok(lock_file) = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(index_dir.join("index.lock"))
        {
            if fs2::FileExt::lock_exclusive(&lock_file).is_ok() {
                crate::search::cleanup_stale_index_files(&index_dir);
            }
            // lock released on drop
        }

        Ok(Self {
            storage,
            embeddings,
            search: Arc::new(RwLock::new(search)),
            projects_dir: projects_dir.to_path_buf(),
            index_dir,
        })
    }

    /// Import conversations from the Claude projects directory.
    /// Uses batch embedding for ~3.4x speedup over single embeds.
    pub async fn import_conversations(&self, limit: Option<usize>) -> Result<usize> {
        let projects = import::discover_projects(&self.projects_dir)?;
        let mut total = 0usize;

        for (dir, project_name) in &projects {
            let files = import::list_jsonl_files(dir)?;
            for file_path in &files {
                if self.storage.is_file_imported(file_path)? {
                    continue;
                }
                total += self.import_file(file_path, project_name).await?;

                if let Some(lim) = limit {
                    if total >= lim {
                        return Ok(total);
                    }
                }
            }
        }

        self.flush_index().await;
        Ok(total)
    }

    /// Import a single JSONL file: parse, embed, store chunks, enrich.
    /// Supports incremental import — when a transcript grows mid-session,
    /// only new chunks are embedded (chunks beyond prev_count are new).
    /// Returns the number of NEW chunks imported (0 if nothing new).
    pub async fn import_file(&self, file_path: &Path, project_name: &str) -> Result<usize> {
        // Check if file is unchanged (mtime match = fully imported, nothing new)
        if self.storage.is_file_imported(file_path)? {
            return Ok(0);
        }

        let chunks = import::parse_jsonl_file(file_path, project_name)?;
        if chunks.is_empty() {
            // Record the skip (agent transcripts, empty conversations) so the
            // watcher doesn't re-parse the file every pass and import_percent
            // counts it as processed instead of silently under-reporting.
            self.storage.mark_file_imported(file_path, 0)?;
            return Ok(0);
        }

        // Incremental: skip chunks we already embedded
        let prev_count = self.storage.get_imported_chunk_count(file_path)?;
        if chunks.len() <= prev_count {
            // File parsed to same/fewer chunks — just update mtime
            self.storage.mark_file_imported(file_path, chunks.len())?;
            return Ok(0);
        }

        let new_chunks = &chunks[prev_count..];
        const BATCH_SIZE: usize = 10;

        for batch in new_chunks.chunks(BATCH_SIZE) {
            let texts: Vec<String> = batch.iter().map(|c| c.content.clone()).collect();
            let emb = self.embeddings.clone();
            let embeddings = tokio::task::spawn_blocking(move || {
                let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
                emb.embed(&refs)
            })
            .await??;

            let mut idx = self.search.write().await;
            for (chunk, embedding) in batch.iter().zip(embeddings) {
                self.storage.insert_chunk(chunk, &embedding)?;
                // Persist provenance: who authored this chunk + its source conv.
                // Supersession detection is deferred (None) — recall still gains
                // from author-authority weighting. Non-fatal on error.
                if let Err(e) = self.storage.insert_chunk_provenance(
                    &chunk.id,
                    &crate::provenance::ChunkProvenance {
                        author: chunk.author,
                        source_conv_id: chunk.conversation_id.clone(),
                        supersedes: None,
                    },
                ) {
                    eprintln!("CSR: chunk provenance persist error (non-fatal): {e}");
                }
                idx.insert_chunk(chunk.id.clone(), embedding);
            }
        }

        self.storage.mark_file_imported(file_path, chunks.len())?;

        // Layer 1: Heuristic enrichment only on first import (not incremental updates)
        if prev_count == 0 {
            let conv_id = file_path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if !self
                .storage
                .is_conversation_enriched(&conv_id, "heuristic")
                .unwrap_or(false)
            {
                if let Err(e) = crate::extraction::heuristic::enrich_conversation(
                    file_path,
                    &conv_id,
                    project_name,
                    &self.storage,
                    &self.embeddings,
                    &self.search,
                )
                .await
                {
                    tracing::warn!(
                        conv = %conv_id,
                        error = %e,
                        "heuristic enrichment failed (non-fatal)"
                    );
                }
            }
        }

        Ok(new_chunks.len())
    }

    /// Backfill missing import_state rows and run heuristic enrichment for all unenriched conversations.
    /// This repairs the enrichment pipeline when conversations exist in `chunks` but lack import_state entries.
    pub async fn backfill_and_enrich(&self) -> Result<(usize, usize)> {
        // Build conv_id -> (file_path, project_name) map once for both steps
        let projects = import::discover_projects(&self.projects_dir)?;
        let mut conv_to_file: std::collections::HashMap<String, (std::path::PathBuf, String)> =
            std::collections::HashMap::new();

        for (dir, project_name) in &projects {
            if let Ok(files) = import::list_jsonl_files(dir) {
                for file_path in files {
                    let conv_id = file_path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    conv_to_file.insert(conv_id, (file_path, project_name.clone()));
                }
            }
        }

        // Step 1: Backfill missing import_state rows
        let missing = self.storage.get_conversations_missing_import_state()?;
        let mut backfilled = 0usize;

        if !missing.is_empty() {
            eprintln!(
                "CSR: found {} conversations missing import_state rows",
                missing.len()
            );

            for conv_id in &missing {
                if let Some((file_path, _)) = conv_to_file.get(conv_id) {
                    if let Err(e) = self.storage.mark_file_imported(file_path, 0) {
                        tracing::warn!(conv = %conv_id, error = %e, "failed to backfill import_state");
                    } else {
                        backfilled += 1;
                    }
                }
            }
            eprintln!("CSR: backfilled {} import_state rows", backfilled);
        }

        // Step 2: Heuristic enrichment for all unenriched conversations
        let needing = self.storage.get_conversations_needing_heuristic()?;
        let mut enriched = 0usize;

        if !needing.is_empty() {
            eprintln!(
                "CSR: found {} conversations needing heuristic enrichment",
                needing.len()
            );

            for (conv_id, project_name) in &needing {
                if let Some((file_path, _)) = conv_to_file.get(conv_id) {
                    if let Err(e) = crate::extraction::heuristic::enrich_conversation(
                        file_path,
                        conv_id,
                        project_name,
                        &self.storage,
                        &self.embeddings,
                        &self.search,
                    )
                    .await
                    {
                        tracing::warn!(
                            conv = %conv_id,
                            error = %e,
                            "heuristic enrichment failed (non-fatal)"
                        );
                    } else {
                        enriched += 1;
                        if enriched.is_multiple_of(100) {
                            eprintln!(
                                "CSR: enriched {}/{} conversations...",
                                enriched,
                                needing.len()
                            );
                            self.flush_index().await;
                        }
                    }
                }
            }
            eprintln!(
                "CSR: heuristic enrichment complete: {}/{}",
                enriched,
                needing.len()
            );
        }

        // Final flush to persist any remaining vectors
        self.flush_index().await;

        Ok((backfilled, enriched))
    }

    // ─── Accessors for hooks ───

    pub fn storage(&self) -> &Arc<Storage> {
        &self.storage
    }

    pub fn embeddings(&self) -> &Arc<EmbeddingEngine> {
        &self.embeddings
    }

    pub fn search(&self) -> &Arc<RwLock<SearchEngine>> {
        &self.search
    }

    pub fn projects_dir(&self) -> &Path {
        &self.projects_dir
    }

    pub fn index_dir(&self) -> &Path {
        &self.index_dir
    }

    /// Flush the HNSW index to disk if it has been modified.
    /// Safe to call multiple times — skips if not dirty.
    pub async fn flush_index(&self) {
        let mut idx = self.search.write().await;
        if idx.is_dirty() {
            // Query current DB counts for staleness-correct manifest
            let chunk_count = self.storage.count_chunk_embeddings().unwrap_or(0);
            let refl_count = self.storage.count_reflection_embeddings().unwrap_or(0);
            if let Err(e) = idx.dump_to_disk(&self.index_dir, chunk_count, refl_count) {
                tracing::warn!(error = %e, "failed to flush HNSW index (non-fatal)");
            }
        }
    }

    /// Start the file system watcher as a background task.
    /// Returns a JoinHandle that can be awaited or dropped.
    pub fn start_watcher(&self) -> tokio::task::JoinHandle<()> {
        let watcher = FileWatcher::new(
            self.projects_dir.clone(),
            self.storage.clone(),
            self.embeddings.clone(),
            self.search.clone(),
            self.index_dir.to_path_buf(),
        );
        watcher.spawn()
    }

    /// Start the MCP server on stdio with background enrichment loops.
    pub async fn serve_mcp(self) -> Result<()> {
        // Spawn enrichment loops as background tasks (extraction, narrator, consolidation)
        let enrichment_handles = crate::daemon::spawn_enrichment_loops(
            self.storage.clone(),
            self.embeddings.clone(),
            self.search.clone(),
        );

        let server = CsrServer::new(
            self.storage,
            self.embeddings,
            self.search,
            self.projects_dir,
            self.index_dir,
        );
        let service = server.serve(rmcp::transport::io::stdio()).await?;
        service.waiting().await?;

        // Clean up enrichment loops on MCP server exit
        for handle in enrichment_handles {
            handle.abort();
        }

        Ok(())
    }
}

/// Batch size for reconciling the HNSW cache against SQLite: both the
/// additive-drift backfill (missing ids only) and the cache-miss full
/// rebuild fetch vectors this many ids at a time, so peak transient memory
/// is O(batch) instead of O(corpus). See src/engine.rs:126-165 history.
const RECONCILE_BATCH: usize = 2_000;

/// Fetch and insert only the given missing chunk ids, in bounded batches.
/// Never materializes more than `batch_size` vectors at once. Returns the
/// number of points actually inserted.
fn backfill_missing_chunks_batched(
    storage: &Storage,
    search: &mut SearchEngine,
    missing_ids: &[String],
    batch_size: usize,
) -> Result<usize> {
    let mut added = 0;
    for batch in missing_ids.chunks(batch_size.max(1)) {
        let vecs = storage.get_chunk_vectors_by_ids(batch)?;
        added += vecs.len();
        for (id, vec) in vecs {
            search.insert_chunk(id, vec);
        }
    }
    Ok(added)
}

fn backfill_missing_chunks(
    storage: &Storage,
    search: &mut SearchEngine,
    missing_ids: &[String],
) -> Result<usize> {
    backfill_missing_chunks_batched(storage, search, missing_ids, RECONCILE_BATCH)
}

/// Reflection counterpart of [`backfill_missing_chunks_batched`].
fn backfill_missing_reflections_batched(
    storage: &Storage,
    search: &mut SearchEngine,
    missing_ids: &[String],
    batch_size: usize,
) -> Result<usize> {
    let mut added = 0;
    for batch in missing_ids.chunks(batch_size.max(1)) {
        let vecs = storage.get_reflection_vectors_by_ids(batch)?;
        added += vecs.len();
        for (id, vec) in vecs {
            search.insert_reflection(id, vec);
        }
    }
    Ok(added)
}

fn backfill_missing_reflections(
    storage: &Storage,
    search: &mut SearchEngine,
    missing_ids: &[String],
) -> Result<usize> {
    backfill_missing_reflections_batched(storage, search, missing_ids, RECONCILE_BATCH)
}

/// Rebuild the HNSW index from SQLite, streaming vectors in bounded batches
/// instead of materializing `load_all_chunk_vectors()` (the whole corpus) as
/// one `Vec<(String, Vec<f32>)>` before inserting. Cheap id probes
/// (`load_all_chunk_ids` / `load_all_reflection_ids`) drive the batching;
/// each batch's vectors are moved (not cloned) into the index and dropped
/// before the next batch is fetched. Returns the built index plus the
/// (chunks, reflections) counts actually inserted.
fn rebuild_search_index_streaming(
    storage: &Storage,
    batch_size: usize,
) -> Result<(SearchEngine, usize, usize)> {
    let chunk_ids = storage.load_all_chunk_ids()?;
    let reflection_ids = storage.load_all_reflection_ids()?;

    let estimated_size = (chunk_ids.len() + 1000).max(10_000);
    let mut search = SearchEngine::new(estimated_size);

    let mut chunk_total = 0usize;
    for batch in chunk_ids.chunks(batch_size.max(1)) {
        let vecs = storage.get_chunk_vectors_by_ids(batch)?;
        chunk_total += vecs.len();
        for (id, vec) in vecs {
            search.insert_chunk(id, vec);
        }
    }

    let mut reflection_total = 0usize;
    for batch in reflection_ids.chunks(batch_size.max(1)) {
        let vecs = storage.get_reflection_vectors_by_ids(batch)?;
        reflection_total += vecs.len();
        for (id, vec) in vecs {
            search.insert_reflection(id, vec);
        }
    }

    Ok((search, chunk_total, reflection_total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::ConversationChunk;
    use crate::provenance::Speaker;

    fn insert_test_chunk(storage: &Storage, id: &str) {
        storage
            .insert_chunk(
                &ConversationChunk {
                    id: id.into(),
                    conversation_id: "conv".into(),
                    project_name: "proj".into(),
                    timestamp: "2026-09-08T00:00:00Z".into(),
                    content: "x".into(),
                    message_count: 1,
                    summary: None,
                    author: Speaker::User,
                    seq: 0,
                    is_sidechain: false,
                },
                &[0.1_f32; 384],
            )
            .unwrap();
    }

    #[test]
    fn backfill_missing_chunks_inserts_exactly_the_missing_ids_in_batches() {
        let storage = Storage::open_memory().unwrap();
        for i in 0..10 {
            insert_test_chunk(&storage, &format!("chunk-{i}"));
        }
        let mut search = SearchEngine::new(100);
        // Pre-seed half of them as already-present — these must be skipped,
        // not re-fetched (proves we fetch only the missing ids, not the corpus).
        for i in 0..5 {
            search.insert_chunk(format!("chunk-{i}"), vec![0.1_f32; 384]);
        }

        let db_ids = storage.load_all_chunk_ids().unwrap();
        let missing: Vec<String> = db_ids
            .into_iter()
            .filter(|id| !search.has_chunk(id))
            .collect();
        assert_eq!(missing.len(), 5, "sanity: half should be missing");

        // batch_size=2 over 5 missing ids forces 3 separate batched lookups
        // (2, 2, 1) — exercises the bounded-batch path, not a single big fetch.
        let added = backfill_missing_chunks_batched(&storage, &mut search, &missing, 2).unwrap();

        assert_eq!(
            added, 5,
            "must insert exactly the missing ids, not the whole corpus"
        );
        for i in 5..10 {
            assert!(search.has_chunk(&format!("chunk-{i}")));
        }
    }

    #[test]
    fn backfill_missing_reflections_inserts_exactly_the_missing_ids_in_batches() {
        let storage = Storage::open_memory().unwrap();
        for i in 0..7 {
            storage
                .insert_reflection(&format!("refl-{i}"), "content", &[], &[0.2_f32; 384])
                .unwrap();
        }
        let mut search = SearchEngine::new(100);
        for i in 0..3 {
            search.insert_reflection(format!("refl-{i}"), vec![0.2_f32; 384]);
        }

        let db_ids = storage.load_all_reflection_ids().unwrap();
        let missing: Vec<String> = db_ids
            .into_iter()
            .filter(|id| !search.has_reflection(id))
            .collect();
        assert_eq!(missing.len(), 4, "sanity: 4 should be missing");

        let added =
            backfill_missing_reflections_batched(&storage, &mut search, &missing, 3).unwrap();

        assert_eq!(added, 4);
        for i in 3..7 {
            assert!(search.has_reflection(&format!("refl-{i}")));
        }
    }

    #[test]
    fn rebuild_search_index_streaming_inserts_all_corpus_points_via_batches() {
        let storage = Storage::open_memory().unwrap();
        for i in 0..7 {
            insert_test_chunk(&storage, &format!("chunk-{i}"));
        }
        for i in 0..3 {
            storage
                .insert_reflection(&format!("refl-{i}"), "content", &[], &[0.2_f32; 384])
                .unwrap();
        }

        // batch_size=2 forces multiple batches for both chunks (7) and
        // reflections (3) — proves the rebuild path never needs a single
        // corpus-sized fetch to produce a correct index.
        let (search, chunk_total, reflection_total) =
            rebuild_search_index_streaming(&storage, 2).unwrap();

        assert_eq!(chunk_total, 7);
        assert_eq!(reflection_total, 3);
        for i in 0..7 {
            assert!(search.has_chunk(&format!("chunk-{i}")));
        }
        for i in 0..3 {
            assert!(search.has_reflection(&format!("refl-{i}")));
        }
    }
}
