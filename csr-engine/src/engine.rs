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

/// Initial HNSW capacity for an import-only engine's empty in-memory index.
/// Only a construction hint — hnsw_rs grows as needed — sized to comfortably
/// hold one session's worth of new chunks without reallocation.
const IMPORT_ONLY_INDEX_CAPACITY: usize = 4096;

/// Orchestrates all subsystems: storage, embeddings, search, import, and MCP.
#[derive(Clone)]
pub struct Engine {
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    projects_dir: PathBuf,
    index_dir: PathBuf,
    /// When true, the engine never loads the on-disk HNSW cache at construction
    /// and `flush_index` is a no-op. Used by write-only hooks (precompact,
    /// session-end) that import a transcript but never search: they pay neither
    /// the ~O(corpus) `HnswIo::load_hnsw` at startup nor a full re-dump. New
    /// embeddings still land in SQLite; the next search-constructing process
    /// reconciles them via the additive-backfill path in `Engine::new`.
    ///
    /// The flush guard is the critical invariant: without it, dispatch_hook's
    /// unconditional `flush_index` would dump the near-empty in-memory index
    /// with a manifest count equal to the FULL DB count, which the next
    /// `load_from_disk` accepts as current — replacing the real many-node graph
    /// with a handful of points. Recall is not permanently lost (the next
    /// `Engine::new` compares DB ids against the cache and backfills the missing
    /// ones), but recovery costs a full re-insert of the corpus, which defeats
    /// the entire point of the import-only path.
    skip_index_persistence: bool,
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
            skip_index_persistence: false,
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
            // Track the additive-backfill delta and the loaded cache's
            // on-disk age purely as a staleness diagnostic (logged below) —
            // how far behind SQLite the on-disk HNSW cache was at load time,
            // and how long it's been since anything last flushed it. Local
            // to this branch; nothing downstream of `Engine::new` reads
            // either value (there is no cache staleness to report on the
            // rebuild path below — a fresh dump has zero drift and zero age
            // by construction).
            let mut backfill_added = 0usize;
            let cache_age = std::fs::metadata(index_dir.join("manifest.json"))
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok());

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

                // Backfill reflections that exist in DB but not in HNSW
                let missing: Vec<&str> = db_id_set
                    .iter()
                    .filter(|id| !search.has_reflection(id))
                    .copied()
                    .collect();
                if !missing.is_empty() {
                    if let Ok(all_vecs) = storage.load_all_reflection_vectors() {
                        let mut added = 0;
                        for (id, vec) in &all_vecs {
                            if missing.contains(&id.as_str()) {
                                search.insert_reflection(id.clone(), vec.clone());
                                added += 1;
                            }
                        }
                        if added > 0 {
                            tracing::info!(added, "backfilled missing reflections into HNSW cache");
                        }
                        backfill_added += added;
                    }
                }
            }

            // Backfill chunks added to DB since the last dump (additive drift).
            // Cheap ID probe first; only load the (large) vector set if something is
            // actually missing. This is what lets a stale-but-additive cache load in
            // ~ms instead of triggering a full HNSW rebuild (~tens of seconds).
            if let Ok(db_chunk_ids) = storage.load_all_chunk_ids() {
                let missing: std::collections::HashSet<&str> = db_chunk_ids
                    .iter()
                    .filter(|id| !search.has_chunk(id))
                    .map(|s| s.as_str())
                    .collect();
                if !missing.is_empty() {
                    if let Ok(all_vecs) = storage.load_all_chunk_vectors() {
                        let mut added = 0;
                        for (id, vec) in &all_vecs {
                            if missing.contains(id.as_str()) {
                                search.insert_chunk(id.clone(), vec.clone());
                                added += 1;
                            }
                        }
                        if added > 0 {
                            tracing::info!(added, "backfilled missing chunks into HNSW cache");
                        }
                        backfill_added += added;
                    }
                }
            }
            if backfill_added > 0 {
                tracing::info!(
                    backfill_added,
                    cache_age_secs = cache_age.map(|d| d.as_secs()),
                    "HNSW cache behind DB at startup — additive backfill applied"
                );
            }
            search
        } else {
            // Cache miss — rebuild from SQLite vectors. The dump below writes a
            // brand-new manifest, so there is no drift and no stale age to
            // report.
            tracing::info!("building search index from stored vectors");

            let chunk_vecs = storage.load_all_chunk_vectors()?;
            let t_load = t0.elapsed();

            let estimated_size = (chunk_vecs.len() + 1000).max(10_000);
            let mut search = SearchEngine::new(estimated_size);
            for (id, vec) in &chunk_vecs {
                search.insert_chunk(id.clone(), vec.clone());
            }
            let reflection_vecs = storage.load_all_reflection_vectors()?;
            for (id, vec) in &reflection_vecs {
                search.insert_reflection(id.clone(), vec.clone());
            }
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
                chunks = chunk_vecs.len(),
                reflections = reflection_vecs.len(),
                "search index rebuilt and cached"
            );
            let startup_line = format!(
                "CSR startup: storage={:.0}ms embed={:.0}ms vectors={:.0}ms hnsw={:.0}ms dump={:.0}ms total={:.0}ms ({} chunks, rebuilt)",
                t_storage.as_secs_f64() * 1000.0,
                (t_embed - t_storage).as_secs_f64() * 1000.0,
                (t_load - t_count).as_secs_f64() * 1000.0,
                (t_hnsw - t_load).as_secs_f64() * 1000.0,
                (t_total - t_hnsw).as_secs_f64() * 1000.0,
                t_total.as_secs_f64() * 1000.0,
                chunk_vecs.len(),
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
            skip_index_persistence: false,
        })
    }

    /// Construct an engine for write-only hooks (precompact, session-end) that
    /// import a transcript but never search.
    ///
    /// Unlike [`Engine::new`], this skips loading the on-disk HNSW cache
    /// entirely — the `HnswIo::load_hnsw` that walks every point in the graph
    /// (the "setting number of points …" pass, ~O(corpus)). The in-memory index
    /// starts empty and, together with the `flush_index` guard, is never dumped,
    /// so the on-disk cache is left untouched. New embeddings are written to
    /// SQLite; the next search-constructing process picks them up via the
    /// additive-backfill path in [`Engine::new`].
    pub fn new_import_only(db_path: &Path, projects_dir: &Path) -> Result<Self> {
        let storage = Arc::new(Storage::open(db_path)?);
        let embeddings = Arc::new(EmbeddingEngine::new()?);
        let index_dir = db_path.parent().unwrap_or(Path::new(".")).join("index");
        // Small empty index: import inserts land here and are discarded on
        // process exit. Never dumped, so no on-disk cache is touched.
        let search = SearchEngine::new(IMPORT_ONLY_INDEX_CAPACITY);
        Ok(Self {
            storage,
            embeddings,
            search: Arc::new(RwLock::new(search)),
            projects_dir: projects_dir.to_path_buf(),
            index_dir,
            skip_index_persistence: true,
        })
    }

    /// Import conversations from the Claude projects directory.
    /// Uses batch embedding for ~3.4x speedup over single embeds.
    pub async fn import_conversations(&self, limit: Option<usize>) -> Result<usize> {
        let projects = import::discover_projects(&self.projects_dir)?;
        let mut total = 0usize;

        for (dir, project_name) in &projects {
            let files = import::list_conversation_jsonl_files(dir)?;
            for file_path in &files {
                let attribution =
                    import::derive_conversation_attribution(&self.projects_dir, file_path);
                debug_assert_eq!(&attribution.project_name, project_name);
                total += self
                    .import_file_with_attribution(file_path, &attribution)
                    .await?;

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
    /// Incremental — when a transcript grows mid-session only the chunks whose
    /// content actually changed are re-embedded, which includes the trailing
    /// chunk from the previous pass because it was flushed partial.
    /// Returns the number of chunks written (0 if nothing changed).
    pub async fn import_file(&self, file_path: &Path, project_name: &str) -> Result<usize> {
        let attribution = import::ConversationAttribution {
            project_name: project_name.to_string(),
            source: "conversation",
            parent_conversation_id: None,
        };
        self.import_file_with_attribution(file_path, &attribution)
            .await
    }

    async fn import_file_with_attribution(
        &self,
        file_path: &Path,
        attribution: &import::ConversationAttribution,
    ) -> Result<usize> {
        let conversation_id = file_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if let Some(parent) = attribution.parent_conversation_id.as_deref() {
            self.storage.rescope_sidechain_conversation(
                &conversation_id,
                &attribution.project_name,
                parent,
            )?;
        }

        let ctx = import::incremental::ImportContext {
            storage: &self.storage,
            embeddings: &self.embeddings,
            search: &self.search,
        };
        // Driven by the Stop hook and by bulk import, where the transcript is
        // final — so the trailing chunk is sealed and indexed in the same pass.
        let outcome = import::incremental::import_file_incremental(
            &ctx,
            file_path,
            attribution,
            import::incremental::SealPolicy::SealAll,
        )
        .await?;

        if outcome.unchanged {
            return Ok(0);
        }

        import::incremental::maybe_enrich(&ctx, &outcome, file_path, attribution).await;

        Ok(outcome.written_chunks)
    }

    /// Backfill missing import_state rows and run heuristic enrichment for all unenriched conversations.
    /// This repairs the enrichment pipeline when conversations exist in `chunks` but lack import_state entries.
    pub async fn backfill_and_enrich(&self) -> Result<(usize, usize)> {
        // Build conv_id -> (file_path, project_name) map once for both steps
        let projects = import::discover_projects(&self.projects_dir)?;
        let mut conv_to_file: std::collections::HashMap<String, (std::path::PathBuf, String)> =
            std::collections::HashMap::new();

        for (dir, project_name) in &projects {
            if let Ok(files) = import::list_conversation_jsonl_files(dir) {
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
        // Write-only engines (import-only hooks) never persist the index. The
        // in-memory index holds at most this session's chunks against an empty
        // base; dumping it would overwrite the real on-disk cache with a
        // near-empty graph carrying a full-count manifest. The next load would
        // accept it as current and (via the id-backfill in `Engine::new`)
        // recover only by re-inserting the whole corpus — the expensive rebuild
        // this path exists to avoid. See `skip_index_persistence`.
        if self.skip_index_persistence {
            return;
        }
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
    ///
    /// `stdout` is fd 1 already claimed for the JSON-RPC transport by
    /// `main.rs`, before this engine's own construction had a chance to run
    /// `hnsw_rs` through its index rebuild path. `Some` on unix routes the
    /// transport's write half through that claimed descriptor instead of
    /// `rmcp::transport::io::stdio()`'s own fd 1, so anything the process
    /// still writes to fd 1 directly (the enrichment loops and `--watch`
    /// importer started below, both of which insert into the same HNSW index)
    /// lands on stderr rather than corrupting the protocol stream. `None`
    /// (claim failed, or non-unix) falls back to the unmodified stdio
    /// transport — the pre-existing behaviour.
    pub async fn serve_mcp(self, stdout: Option<crate::hooks::ClaimedStdout>) -> Result<()> {
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
        #[cfg(unix)]
        let service = match stdout {
            Some(claimed) => {
                server
                    .serve((tokio::io::stdin(), claimed.into_async_writer()))
                    .await?
            }
            None => server.serve(rmcp::transport::io::stdio()).await?,
        };
        #[cfg(not(unix))]
        let service = {
            let _ = stdout;
            server.serve(rmcp::transport::io::stdio()).await?
        };
        service.waiting().await?;

        // Clean up enrichment loops on MCP server exit
        for handle in enrichment_handles {
            handle.abort();
        }

        Ok(())
    }
}
