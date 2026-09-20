use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
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
            // Orphan share of each index, measured against the id set read for
            // this reconciliation (not the earlier count, which writers can move).
            let mut orphans_material = false;
            if let Ok(db_ids) = storage.load_all_reflection_ids() {
                let db_id_set: std::collections::HashSet<&str> =
                    db_ids.iter().map(|s| s.as_str()).collect();
                let blanked = search.blank_orphan_reflections(&db_id_set);
                orphans_material |= blanked * ORPHAN_REBUILD_DIVISOR > db_id_set.len().max(1);
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
                        backfill_added += added;
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
                // The manifest count is written by whoever dumps, from the DB at
                // dump time. A long-running process that loaded before a purge
                // and dumps after it persists every purged vector under a count
                // that matches the DB, so the count check above accepts it.
                let db_id_set: std::collections::HashSet<&str> =
                    db_chunk_ids.iter().map(|s| s.as_str()).collect();
                let orphan_chunks = search.blank_orphan_chunks(&db_id_set);
                orphans_material |= orphan_chunks * ORPHAN_REBUILD_DIVISOR > db_id_set.len().max(1);
                if orphan_chunks > 0 {
                    tracing::info!(orphan_chunks, "blanked orphan chunk entries in HNSW cache");
                }
                let missing: Vec<String> = db_chunk_ids
                    .into_iter()
                    .filter(|id| !search.has_chunk(id))
                    .collect();
                if !missing.is_empty() {
                    if let Ok(added) = backfill_missing_chunks(&storage, &mut search, &missing) {
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
            // hnsw_rs cannot delete. A blanked slot still takes a neighbour
            // position and search does not over-fetch, so a cache that is
            // materially tombstones returns short result lists: rebuild it.
            if orphans_material {
                tracing::info!("HNSW cache holds purged vectors — rebuilding");
                let (mut fresh, _, _) = rebuild_search_index_streaming(&storage, RECONCILE_BATCH)?;
                let chunk_count = storage.count_chunk_embeddings().unwrap_or(chunk_count);
                let reflection_count = storage
                    .count_reflection_embeddings()
                    .unwrap_or(reflection_count);
                if let Err(e) = fresh.dump_to_disk(&index_dir, chunk_count, reflection_count) {
                    tracing::warn!(error = %e, "failed to cache HNSW index (non-fatal)");
                }
                search = fresh;
            }
            search
        } else {
            // Cache miss — rebuild from SQLite vectors, streaming in bounded
            // batches so peak transient memory is O(batch) instead of
            // O(corpus) (no `load_all_chunk_vectors()` + clone-into-index).
            // The dump below writes a brand-new manifest, so there is no drift
            // and no stale age to report.
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

        // The index is built and reconciled; hand the allocator's freed transient
        // pages back to the OS so this server's steady-state footprint reflects live
        // memory rather than retained slack (macOS-only; no-op elsewhere).
        crate::runtime::release_freed_pages();

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
    /// Supports incremental import — when a transcript grows mid-session,
    /// only new chunks are embedded (chunks beyond prev_count are new).
    /// Returns the number of NEW chunks imported (0 if nothing new).
    pub async fn import_file(&self, file_path: &Path, project_name: &str) -> Result<usize> {
        let mut attribution =
            import::derive_conversation_attribution(&self.projects_dir, file_path);
        if attribution.parent_conversation_id.is_none() {
            attribution.project_name = project_name.into();
        }
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
        import::record_conversation_scope(&self.storage, file_path, &conversation_id);
        // Check if file is unchanged (mtime match = fully imported, nothing new)
        if self.storage.is_file_imported(file_path)? {
            return Ok(0);
        }

        let parent_context = attribution
            .parent_conversation_id
            .as_deref()
            .map(|parent_id| {
                let message_key = import::sidechain_parent_message_key(file_path);
                self.storage
                    .parent_provenance_context(parent_id, message_key.as_deref())
            })
            .transpose()?;
        let parsed = import::parse_jsonl_file_with_stats_and_parent(
            file_path,
            &attribution.project_name,
            parent_context.as_ref(),
        )?;
        let import::ParsedConversation {
            chunks,
            suppression,
            evidence,
        } = parsed;
        if chunks.is_empty() {
            // Record the skip (agent transcripts, empty conversations) so the
            // watcher doesn't re-parse the file every pass and import_percent
            // counts it as processed instead of silently under-reporting.
            self.storage
                .mark_file_imported_with_suppression(file_path, 0, suppression)?;
            return Ok(0);
        }

        // Journal v4 P4b: bind any pasted dream prompt to the dream that
        // produced it. Runs over the FULL chunk list (not just the new tail)
        // and before the incremental early-return, so a marker that arrives
        // in a later pass still binds. `INSERT OR IGNORE` makes the repeat
        // scan free. Never fatal — losing an attribution costs a metric,
        // failing the import costs the corpus.
        import::dream_marker::bind_markers(&self.storage, &conversation_id, &chunks);

        // Incremental: skip chunks we already embedded
        let prev_count = self.storage.get_imported_chunk_count(file_path)?;
        if chunks.len() <= prev_count {
            // File parsed to same/fewer chunks — just update mtime
            self.storage.mark_file_imported_with_suppression(
                file_path,
                chunks.len(),
                suppression,
            )?;
            self.storage.relink_conversation(&conversation_id)?;
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
                self.storage
                    .insert_chunk_with_source(chunk, &embedding, attribution.source)?;
                // Persist provenance: who authored this chunk + its source conv.
                // Supersession detection is deferred (None) — recall still gains
                // from author-authority weighting. Non-fatal on error.
                if let Err(e) = self.storage.insert_chunk_provenance(
                    &chunk.id,
                    &crate::provenance::ChunkProvenance {
                        author: chunk.author,
                        source_conv_id: attribution
                            .parent_conversation_id
                            .clone()
                            .unwrap_or_else(|| chunk.conversation_id.clone()),
                        supersedes: None,
                    },
                ) {
                    eprintln!("CSR: chunk provenance persist error (non-fatal): {e}");
                }
                if let Some(chunk_evidence) = evidence.get(&chunk.id) {
                    self.storage.replace_chunk_evidence(chunk_evidence)?;
                }
                idx.insert_chunk(chunk.id.clone(), embedding);
            }
        }

        self.storage
            .mark_file_imported_with_suppression(file_path, chunks.len(), suppression)?;
        self.storage.relink_conversation(&conversation_id)?;

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
                    &attribution.project_name,
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
        if let Err(e) = self.flush_index_checked().await {
            tracing::warn!(error = %e, "failed to flush HNSW index (non-fatal)");
        }
    }

    /// Flush the HNSW index and propagate persistence failures to the caller.
    /// Maintenance commands use this so they cannot report success while the
    /// durable index still contains stale vectors.
    pub async fn flush_index_checked(&self) -> Result<()> {
        // Write-only engines (import-only hooks) never persist the index. The
        // in-memory index holds at most this session's chunks against an empty
        // base; dumping it would overwrite the real on-disk cache with a
        // near-empty graph carrying a full-count manifest. The next load would
        // accept it as current and (via the id-backfill in `Engine::new`)
        // recover only by re-inserting the whole corpus — the expensive rebuild
        // this path exists to avoid. See `skip_index_persistence`.
        if self.skip_index_persistence {
            return Ok(());
        }
        let mut idx = self.search.write().await;
        if idx.is_dirty() {
            // Query current DB counts for staleness-correct manifest
            let chunk_count = self.storage.count_chunk_embeddings()?;
            let refl_count = self.storage.count_reflection_embeddings()?;
            idx.dump_to_disk(&self.index_dir, chunk_count, refl_count)
                .with_context(|| {
                    format!("persisting HNSW index to {}", self.index_dir.display())
                })?;
        }
        Ok(())
    }

    /// Rebuild the in-memory search index from the vectors stored in SQLite
    /// (chunks and reflections), swap it in, and persist it. Maintenance
    /// commands that blank many HNSW slots (`backfill scrub`) call this at the
    /// end so the persisted index is compact: search never has to over-fetch
    /// past tombstones, and the next process start loads from cache instead
    /// of rebuilding. Returns `(chunks, reflections)` indexed.
    pub async fn rebuild_search_index(&self) -> Result<(usize, usize)> {
        let (fresh, chunk_count, reflection_count) =
            rebuild_search_index_streaming(&self.storage, RECONCILE_BATCH)?;
        *self.search.write().await = fresh;
        self.flush_index_checked().await?;
        Ok((chunk_count, reflection_count))
    }

    /// Remove the persisted manifest before a maintenance operation mutates
    /// vectors. If the operation is interrupted or its final dump fails, the
    /// next process must rebuild from SQLite instead of accepting stale files
    /// whose row counts happen to match.
    pub fn invalidate_index_manifest(&self) -> Result<()> {
        let path = self.index_dir.join("manifest.json");
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error)
                .with_context(|| format!("invalidating HNSW manifest at {}", path.display())),
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
        // Record which build is serving, so `status` can tell the user when a
        // newer binary was installed underneath this connection.
        crate::binary_stamp::record_serving_binary();
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

/// A loaded cache is rebuilt when orphan slots in either index exceed 1/N of
/// its live rows (5%). Below that the blanked slots cost less than a rebuild.
const ORPHAN_REBUILD_DIVISOR: usize = 20;

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
