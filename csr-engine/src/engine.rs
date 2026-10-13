use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
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
    /// True only when `Engine::new` took the cache-miss path (no on-disk
    /// manifest, or one `load_from_disk` rejected outright — a corrupt file,
    /// not negative drift) and built a fresh index from SQLite. That index
    /// is compact by construction and already dumped+`clear_needs_compaction`ed
    /// before this field is set — see [`Self::index_freshly_rebuilt`].
    index_freshly_rebuilt: bool,
}

impl Engine {
    /// Create an engine from pre-built components (for testing).
    ///
    /// Derives `index_dir` as `projects_dir.join("index")`. That derivation
    /// is only correct when the caller's real index directory actually lives
    /// there — true for tests, but **not** for the daemon, whose real index
    /// directory sits next to the database (`~/.claude-self-reflect/index`),
    /// not under `projects_dir` (`~/.claude/projects`). A daemon-assembled
    /// engine that persists the index (`flush_index`/`dump_to_disk`/
    /// `rebuild_search_index`) must use [`Self::from_parts_with_index_dir`]
    /// instead, or it silently writes the compacted HNSW cache into the
    /// wrong directory and the real cache's `needs_compaction` marker is
    /// never cleared.
    pub fn from_parts(
        storage: Arc<Storage>,
        embeddings: Arc<EmbeddingEngine>,
        search: Arc<RwLock<SearchEngine>>,
        projects_dir: PathBuf,
    ) -> Self {
        let index_dir = projects_dir.join("index");
        Self::from_parts_with_index_dir(storage, embeddings, search, projects_dir, index_dir)
    }

    /// Create an engine from pre-built components with an explicit
    /// `index_dir`, independent of `projects_dir`. Use this whenever the
    /// caller already knows its real on-disk index directory (the daemon
    /// does — see `Daemon::index_dir`) instead of relying on `from_parts`'s
    /// `projects_dir.join("index")` derivation, which is wrong for any
    /// caller whose database and projects tree live in different places.
    pub fn from_parts_with_index_dir(
        storage: Arc<Storage>,
        embeddings: Arc<EmbeddingEngine>,
        search: Arc<RwLock<SearchEngine>>,
        projects_dir: PathBuf,
        index_dir: PathBuf,
    ) -> Self {
        Self {
            storage,
            embeddings,
            search,
            projects_dir,
            index_dir,
            skip_index_persistence: false,
            index_freshly_rebuilt: false,
        }
    }

    pub fn new(db_path: &Path, projects_dir: &Path) -> Result<Self> {
        Self::new_with_persistence(db_path, projects_dir, true)
    }

    /// Search-capable hook engine: load/reconcile in memory, never publish or
    /// clean up the daemon-owned index, including on a cache miss.
    pub fn new_for_hook(db_path: &Path, projects_dir: &Path) -> Result<Self> {
        Self::new_with_persistence(db_path, projects_dir, false)
    }

    fn new_with_persistence(db_path: &Path, projects_dir: &Path, persist: bool) -> Result<Self> {
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
        let mut index_freshly_rebuilt = false;
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
            // position, so a cache that is materially tombstones eventually
            // wants a full rebuild for compactness. `Engine::new` runs in
            // foreground processes (MCP serve, hooks, CLI query commands) —
            // never synchronously here: that used to be the ~minutes-long
            // stall a fresh MCP handshake cannot afford under Claude Code's
            // 30s CONNECT_TIMEOUT. `search_index`/`search_chunks_filtered`
            // already overfetch past tombstoned slots, so this process keeps
            // serving full-page results with the cache as-is; a
            // `needs_compaction` marker tells the daemon's compaction loop
            // (or the explicit `rebuild-index` command) to do the rebuild
            // out-of-band.
            //
            // Materiality is the CUMULATIVE blank share of each id map
            // (`chunk_tombstone_count`/`reflection_tombstone_count`, both
            // O(1)) after this reconciliation, not the count of ids blanked
            // during this one load. A per-load delta resets to 0 on every
            // subsequent load/dump round trip (blanks persist through
            // `dump_to_disk`), so a corpus that loses a handful of ids every
            // few minutes would never cross the per-load threshold even as
            // its cumulative tombstone share grows without bound.
            let orphans_material = search.chunk_tombstone_count() * ORPHAN_REBUILD_DIVISOR
                > search.chunk_slot_count().max(1)
                || search.reflection_tombstone_count() * ORPHAN_REBUILD_DIVISOR
                    > search.reflection_slot_count().max(1);
            if orphans_material && persist {
                tracing::info!(
                    "HNSW cache holds purged vectors — serving with tombstones, marked needs_compaction"
                );
                crate::search::mark_needs_compaction(&index_dir);
            }
            // Everything above (blank_orphan_*/backfill_missing_*) is pure
            // DB-mirroring reconciliation, not new content this process
            // should feel responsible for publishing — clear the dirty
            // flags it set so this process's NEXT dump (a real
            // `store_reflection` insert, an import) doesn't republish this
            // possibly-stale reconciliation over a newer generation the
            // daemon's compaction loop may swap in concurrently. See
            // `clear_reconciliation_dirty`'s doc comment (WP-D medium #2).
            search.clear_reconciliation_dirty();
            search
        } else {
            // Cache miss — rebuild from SQLite vectors, streaming in bounded
            // batches so peak transient memory is O(batch) instead of
            // O(corpus) (no `load_all_chunk_vectors()` + clone-into-index).
            // The dump below writes a brand-new manifest, so there is no drift
            // and no stale age to report.
            tracing::info!("building search index from stored vectors");

            // The maintenance generation is read before the stream: a rebuild a
            // maintenance pass overtakes is refused publication and run again,
            // so it never restores rows the pass rewrote (astra r2 #4).
            let mut attempt = 1;
            let (search, chunk_total, reflection_total, t_hnsw) = loop {
                let generation = crate::search::read_maintenance_generation(&index_dir);
                let (mut search, chunk_total, reflection_total, streamed) =
                    rebuild_search_index_streaming(&storage, RECONCILE_BATCH)?;
                reconcile_fresh_against_db(&storage, &mut search, &streamed)?;
                search.mark_rebuilt_at(generation);
                let t_hnsw = t0.elapsed();
                if !persist {
                    break (search, chunk_total, reflection_total, t_hnsw);
                }
                // Re-query counts right before dump to minimize staleness window (E-1)
                let chunk_count = storage.count_chunk_embeddings().unwrap_or(chunk_count);
                let reflection_count = storage
                    .count_reflection_embeddings()
                    .unwrap_or(reflection_count);
                match search.dump_to_disk(&index_dir, chunk_count, reflection_count) {
                    Err(e)
                        if crate::search::crossed_maintenance(&e) && attempt < REBUILD_ATTEMPTS =>
                    {
                        tracing::warn!(error = %e, attempt, "startup rebuild crossed a maintenance pass; rebuilding");
                        attempt += 1;
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to cache HNSW index (non-fatal)");
                    }
                    Ok(()) => {}
                }
                crate::search::clear_needs_compaction(&index_dir);
                break (search, chunk_total, reflection_total, t_hnsw);
            };
            index_freshly_rebuilt = true;
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
        if persist {
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
            skip_index_persistence: !persist,
            index_freshly_rebuilt,
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
            index_freshly_rebuilt: false,
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
        // A maintenance run (`backfill scrub`) holds the database: write
        // nothing and leave the file's import state alone, so the watcher or
        // the next hook imports it once the run ends.
        if let Some(holder) = crate::maintenance::active(self.data_dir()) {
            tracing::info!(
                pid = holder.pid,
                file = %file_path.display(),
                "maintenance in progress; import skipped"
            );
            return Ok(0);
        }
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
            entrypoint,
        } = parsed;
        if let Some(ep) = entrypoint.as_deref() {
            self.storage
                .set_conversation_entrypoint(&conversation_id, ep)?;
        }
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

    /// The data directory: the database's own, where the index directory and
    /// the maintenance lock live.
    pub fn data_dir(&self) -> &Path {
        self.index_dir.parent().unwrap_or(&self.index_dir)
    }

    /// True when this `Engine::new` call took the cache-miss path and built
    /// a fresh, compact index from SQLite from scratch (first-ever run, or
    /// `load_from_disk` rejecting a missing/corrupt manifest — interface 4,
    /// out of WP-D's scope). That index is already dumped and has
    /// `needs_compaction` cleared before construction returns, so callers
    /// like the `rebuild-index` CLI command that would otherwise
    /// unconditionally call [`Self::rebuild_search_index`] right after
    /// constructing an `Engine` can skip a second, fully redundant ~11-min
    /// rebuild of a corpus this size.
    pub fn index_freshly_rebuilt(&self) -> bool {
        self.index_freshly_rebuilt
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
        // On the blocking pool, writing the files under a read lock: see
        // `import::watcher::flush_index_blocking`. Current DB counts make the
        // manifest staleness-correct; a failed count is an error here.
        let storage = self.storage.clone();
        let search = self.search.clone();
        let index_dir = self.index_dir.clone();
        let flushed = tokio::task::spawn_blocking(move || {
            crate::import::watcher::flush_index_blocking(&storage, &search, &index_dir, true)
        })
        .await
        .context("index flush task failed")?;
        match flushed {
            None => Ok(()),
            Some(result) => result
                .with_context(|| format!("persisting HNSW index to {}", self.index_dir.display())),
        }
    }

    /// Rebuild the in-memory search index from the vectors stored in SQLite
    /// (chunks and reflections), swap it in, and persist it. Maintenance
    /// commands that blank many HNSW slots (`backfill scrub`) call this at the
    /// end so the persisted index is compact: search never has to over-fetch
    /// past tombstones, and the next process start loads from cache instead
    /// of rebuilding. Returns `(chunks, reflections)` indexed.
    ///
    /// The streaming rebuild itself is synchronous CPU/IO work that takes
    /// minutes on the real corpus (hnsw insert is not async) — it runs on
    /// `spawn_blocking`'s dedicated thread pool rather than directly inside
    /// this `async fn`, so it never pins a tokio runtime worker for the
    /// whole rebuild and starves every other task scheduled on that thread
    /// (the daemon's compaction loop is the caller that matters here: it
    /// shares a runtime with the file watcher and every other background
    /// loop).
    ///
    /// `rebuild_search_index_streaming` reads its id lists once, at the
    /// start of a pass that then runs for minutes — a live watcher/hook can
    /// both import new rows AND delete rows (scrub, seam rewrites) in that
    /// window; none of those other processes take the daemon's heavy-work
    /// permit. Immediately after the stream completes, re-probe current DB
    /// ids and reconcile `fresh` both directions against them, the same way
    /// `Engine::new`'s cached-load path does: backfill anything the fresh
    /// index is missing (rows that landed mid-rebuild — so the swapped-in
    /// index never regresses recall for them), and blank anything `fresh`
    /// holds that the DB no longer has (rows deleted mid-rebuild — WP-D low
    /// #6: without this, a swapped-in "fresh" index could still hand back a
    /// dead id until the next `Engine::new` reconciliation, contradicting
    /// the "no dead id is ever returned" contract).
    ///
    /// The maintenance generation is read before the stream starts; a rebuild
    /// a maintenance pass overtook is refused publication and run again (up to
    /// [`REBUILD_ATTEMPTS`] times), so it never restores rows the pass rewrote.
    pub async fn rebuild_search_index(&self) -> Result<(usize, usize)> {
        let mut attempt = 1;
        loop {
            let generation = crate::search::read_maintenance_generation(&self.index_dir);
            let storage = self.storage.clone();
            let (mut fresh, chunk_count, reflection_count, streamed) =
                tokio::task::spawn_blocking(move || {
                    rebuild_search_index_streaming(&storage, RECONCILE_BATCH)
                })
                .await
                .context("rebuild_search_index_streaming task panicked")??;

            reconcile_fresh_against_db(&self.storage, &mut fresh, &streamed)?;
            fresh.mark_rebuilt_at(generation);

            *self.search.write().await = fresh;
            match self.flush_index_checked().await {
                Err(error)
                    if crate::search::crossed_maintenance(&error) && attempt < REBUILD_ATTEMPTS =>
                {
                    tracing::warn!(%error, attempt, "index rebuild crossed a maintenance pass; rebuilding");
                    attempt += 1;
                }
                Err(error) => return Err(error),
                Ok(()) => {
                    // Compact by construction — clears any pending compaction
                    // request (`needs_compaction` marker) a foreground process
                    // may have left.
                    crate::search::clear_needs_compaction(&self.index_dir);
                    return Ok((chunk_count, reflection_count));
                }
            }
        }
    }

    /// Remove the persisted manifest before a maintenance operation mutates
    /// vectors. If the operation is interrupted or its final dump fails, the
    /// next process must rebuild from SQLite instead of accepting stale files
    /// whose row counts happen to match.
    ///
    /// It also raises the index's maintenance generation (under `index.lock`),
    /// which outlives the manifest: every engine loaded before this call stays
    /// fenced out of publishing its pre-maintenance graph, even once the pass's
    /// own rebuild has published (see `search::invalidate_for_maintenance`).
    pub fn invalidate_index_manifest(&self) -> Result<()> {
        crate::search::invalidate_for_maintenance(&self.index_dir)
            .map(|_| ())
            .with_context(|| {
                format!(
                    "invalidating HNSW manifest at {}",
                    self.index_dir.join("manifest.json").display()
                )
            })
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
    ///
    /// `startup_identity`/`startup_exe_path` are this process's own exe
    /// identity and path, both captured together by `main()` before
    /// `Engine::new()` ran (storage open, HNSW load/rebuild, embeddings
    /// warm — seconds on a large corpus). Capturing either any later, here
    /// (or re-deriving the path via a fresh `current_exe()` call), would
    /// widen the window in which a replacement landing during this
    /// process's own startup goes permanently invisible to it — on Linux, a
    /// `current_exe()` call made after that window can return a `(deleted)`
    /// path (see `binary_identity::current_exe_path_and_identity`'s doc,
    /// gap-G2 review finding 4).
    pub async fn serve_mcp(
        self,
        stdout: Option<crate::hooks::ClaimedStdout>,
        startup_identity: Option<crate::binary_identity::FileIdentity>,
        startup_exe_path: Option<std::path::PathBuf>,
    ) -> Result<()> {
        // Spawn enrichment loops as background tasks (extraction, narrator, consolidation)
        let enrichment_handles = crate::daemon::spawn_enrichment_loops(
            self.storage.clone(),
            self.embeddings.clone(),
            self.search.clone(),
            self.projects_dir().to_path_buf(),
        );

        let server = CsrServer::new(
            self.storage,
            self.embeddings,
            self.search,
            self.projects_dir,
            self.index_dir,
            startup_identity,
            startup_exe_path,
        );
        // Record this process in the run registry so `status` can tell any
        // session when a newer binary was installed underneath its live
        // MCP connection (gap-G2: one record per process, not a single
        // last-writer-wins stamp — see `binary_identity`).
        let run_record_path = startup_identity
            .zip(crate::binary_identity::default_run_dir())
            .and_then(|(identity, run_dir)| {
                crate::binary_identity::write_run_record(&run_dir, "mcp_stdio", identity)
            });
        #[cfg(unix)]
        let service = match stdout {
            Some(claimed) => {
                server
                    .serve_io(tokio::io::stdin(), claimed.into_async_writer())
                    .await?
            }
            None => {
                let (stdin, stdout) = rmcp::transport::io::stdio();
                server.serve_io(stdin, stdout).await?
            }
        };
        #[cfg(not(unix))]
        let service = {
            let _ = stdout;
            let (stdin, stdout) = rmcp::transport::io::stdio();
            server.serve_io(stdin, stdout).await?
        };
        service.waiting().await?;

        // Clean up enrichment loops on MCP server exit
        for handle in enrichment_handles {
            handle.abort();
        }
        if let Some(path) = run_record_path {
            crate::binary_identity::remove_run_record(&path);
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

/// How many times a full rebuild runs when a maintenance pass keeps overtaking
/// it before it can publish.
const REBUILD_ATTEMPTS: usize = 3;

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

/// Reconcile `fresh` against the CURRENT DB state, immediately after a
/// streaming rebuild (`rebuild_search_index_streaming`) completes.
///
/// That streaming pass reads its id lists once, at the start of a pass that
/// then runs for minutes on the real corpus — a live watcher/hook can both
/// import new rows AND delete rows (scrub, seam rewrites) in that window;
/// none of those other processes take the daemon's heavy-work permit. This
/// blanks anything `fresh` still holds that the DB no longer has (rows
/// deleted mid-rebuild — WP-D low #6: the swapped-in index must never hand
/// back a dead id) and backfills anything the DB has that `fresh` is
/// missing (rows that landed mid-rebuild, so the swapped-in index never
/// regresses recall for them).
///
/// Named and called by `Engine::rebuild_search_index` specifically so a
/// test can call this exact function instead of reimplementing its logic
/// inline — a test that copies this reconciliation rather than calling it
/// keeps passing even after the real blanking/backfill calls in
/// `rebuild_search_index` are deleted (WP-D low #7).
fn reconcile_fresh_against_db(
    storage: &Storage,
    fresh: &mut SearchEngine,
    streamed: &StreamedVectors,
) -> Result<()> {
    let (chunks, reflections) =
        replace_vectors_changed_while_streaming(storage, fresh, streamed)
            .context("re-checking vectors changed while the rebuild streamed")?;
    if chunks + reflections > 0 {
        tracing::info!(
            chunks,
            reflections,
            "replaced vectors re-embedded while the rebuild was streaming"
        );
    }
    let db_ids = storage.load_all_reflection_ids()?;
    let db_id_set: std::collections::HashSet<&str> = db_ids.iter().map(|s| s.as_str()).collect();
    let blanked = fresh.blank_orphan_reflections(&db_id_set);
    if blanked > 0 {
        tracing::info!(
            blanked,
            "blanked reflections deleted while the rebuild was streaming"
        );
    }
    let missing: Vec<String> = db_id_set
        .iter()
        .filter(|id| !fresh.has_reflection(id))
        .map(|s| s.to_string())
        .collect();
    if !missing.is_empty() {
        let added = backfill_missing_reflections(storage, fresh, &missing)?;
        if added > 0 {
            tracing::info!(added, "backfilled reflections that landed mid-rebuild");
        }
    }
    let db_chunk_ids = storage.load_all_chunk_ids()?;
    let db_id_set: std::collections::HashSet<&str> =
        db_chunk_ids.iter().map(|s| s.as_str()).collect();
    let blanked = fresh.blank_orphan_chunks(&db_id_set);
    if blanked > 0 {
        tracing::info!(
            blanked,
            "blanked chunks deleted while the rebuild was streaming"
        );
    }
    let missing: Vec<String> = db_id_set
        .iter()
        .filter(|id| !fresh.has_chunk(id))
        .map(|s| s.to_string())
        .collect();
    if !missing.is_empty() {
        let added = backfill_missing_chunks(storage, fresh, &missing)?;
        if added > 0 {
            tracing::info!(added, "backfilled chunks that landed mid-rebuild");
        }
    }
    Ok(())
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
) -> Result<(SearchEngine, usize, usize, StreamedVectors)> {
    let chunk_ids = storage.load_all_chunk_ids()?;
    let reflection_ids = storage.load_all_reflection_ids()?;

    let estimated_size = (chunk_ids.len() + 1000).max(10_000);
    let mut search = SearchEngine::new(estimated_size);
    let batch_size = batch_size.max(1);

    let mut chunk_prints = vec![0_u64; chunk_ids.len()];
    let mut chunk_total = 0usize;
    for (number, batch) in chunk_ids.chunks(batch_size).enumerate() {
        let vecs = storage.get_chunk_vectors_by_ids(batch)?;
        chunk_total += vecs.len();
        let at = batch_positions(batch, number * batch_size);
        for (id, vec) in vecs {
            if let Some(&position) = at.get(id.as_str()) {
                chunk_prints[position] = vector_fingerprint(&vec);
            }
            search.insert_chunk(id, vec);
        }
    }

    let mut reflection_prints = vec![0_u64; reflection_ids.len()];
    let mut reflection_total = 0usize;
    for (number, batch) in reflection_ids.chunks(batch_size).enumerate() {
        let vecs = storage.get_reflection_vectors_by_ids(batch)?;
        reflection_total += vecs.len();
        let at = batch_positions(batch, number * batch_size);
        for (id, vec) in vecs {
            if let Some(&position) = at.get(id.as_str()) {
                reflection_prints[position] = vector_fingerprint(&vec);
            }
            search.insert_reflection(id, vec);
        }
    }

    Ok((
        search,
        chunk_total,
        reflection_total,
        StreamedVectors {
            chunk_ids,
            chunk_prints,
            reflection_ids,
            reflection_prints,
            batch_size,
        },
    ))
}

/// What the streaming rebuild read: every id with a fingerprint of the vector it
/// inserted (0 when the row was gone by its batch). Reconciliation re-reads the
/// same ids and replaces any vector whose fingerprint moved, so a row re-embedded
/// while the rebuild streamed is published at its current revision (astra #7:
/// the id-only reconciliation kept the old vector because the id still existed).
struct StreamedVectors {
    chunk_ids: Vec<String>,
    chunk_prints: Vec<u64>,
    reflection_ids: Vec<String>,
    reflection_prints: Vec<u64>,
    batch_size: usize,
}

fn batch_positions(batch: &[String], base: usize) -> std::collections::HashMap<&str, usize> {
    batch
        .iter()
        .enumerate()
        .map(|(offset, id)| (id.as_str(), base + offset))
        .collect()
}

/// FNV-1a over the vector's bits; never 0, which marks "not read".
fn vector_fingerprint(vector: &[f32]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for value in vector {
        for byte in value.to_bits().to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash.max(1)
}

/// Replace in `fresh` every streamed vector whose stored revision changed since
/// the stream read it. Returns how many chunks and reflections were replaced.
fn replace_vectors_changed_while_streaming(
    storage: &Storage,
    fresh: &mut SearchEngine,
    streamed: &StreamedVectors,
) -> Result<(usize, usize)> {
    let mut chunks = 0;
    for (number, batch) in streamed.chunk_ids.chunks(streamed.batch_size).enumerate() {
        let at = batch_positions(batch, number * streamed.batch_size);
        for (id, vec) in storage.get_chunk_vectors_by_ids(batch)? {
            let read = at
                .get(id.as_str())
                .map(|&position| streamed.chunk_prints[position]);
            if read.is_some_and(|read| read != 0 && read != vector_fingerprint(&vec))
                && fresh.has_chunk(&id)
            {
                fresh.remove_chunk(&id);
                fresh.insert_chunk(id, vec);
                chunks += 1;
            }
        }
    }
    let mut reflections = 0;
    for (number, batch) in streamed
        .reflection_ids
        .chunks(streamed.batch_size)
        .enumerate()
    {
        let at = batch_positions(batch, number * streamed.batch_size);
        for (id, vec) in storage.get_reflection_vectors_by_ids(batch)? {
            let read = at
                .get(id.as_str())
                .map(|&position| streamed.reflection_prints[position]);
            if read.is_some_and(|read| read != 0 && read != vector_fingerprint(&vec))
                && fresh.has_reflection(&id)
            {
                fresh.remove_reflection(&id);
                fresh.insert_reflection(id, vec);
                reflections += 1;
            }
        }
    }
    Ok((chunks, reflections))
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

    fn insert_test_chunk_with(storage: &Storage, id: &str, vector: &[f32]) {
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
                vector,
            )
            .unwrap();
    }

    /// A unit vector along axis `axis`: orthogonal to every other axis and far
    /// from the `[0.1; 384]` filler every test chunk carries.
    fn axis(axis: usize) -> Vec<f32> {
        let mut vector = vec![0.0_f32; 384];
        vector[axis] = 1.0;
        vector
    }

    #[test]
    fn astra_7_rebuild_reconciliation_picks_up_a_vector_replaced_while_streaming() {
        let storage = Storage::open_memory().unwrap();
        for i in 0..10 {
            insert_test_chunk(&storage, &format!("chunk-{i}"));
        }
        insert_test_chunk_with(&storage, "x", &axis(0));
        let (mut fresh, chunks, _, streamed) = rebuild_search_index_streaming(&storage, 4).unwrap();
        assert_eq!(chunks, 11);
        // Another writer re-embeds x after the stream read it.
        insert_test_chunk_with(&storage, "x", &axis(1));
        reconcile_fresh_against_db(&storage, &mut fresh, &streamed).unwrap();
        let hits = fresh.search_chunks(&axis(1), 1, 0.99);
        assert_eq!(hits.first().map(|hit| hit.id.as_str()), Some("x"));
        assert!(fresh.search_chunks(&axis(0), 1, 0.99).is_empty());
    }

    /// 40 filler chunks and `x` along `axis(n)`.
    fn graph_with_x(n: usize) -> SearchEngine {
        let mut graph = SearchEngine::new(100);
        for i in 0..40 {
            graph.insert_chunk(format!("filler-{i}"), vec![0.1; 384]);
        }
        graph.insert_chunk("x".into(), axis(n));
        graph
    }

    #[test]
    fn astra_r2_5_a_below_floor_manifest_is_never_carried_forward() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        graph_with_x(0).dump_to_disk(dir, 41, 0).unwrap();
        let old_manifest = std::fs::read(dir.join("manifest.json")).unwrap();
        let mut peer = SearchEngine::load_from_disk(dir, 41, 0).unwrap();
        crate::search::invalidate_for_maintenance(dir).unwrap();
        // An older binary, blind to the generation, puts the pre-pass manifest back.
        std::fs::write(dir.join("manifest.json"), old_manifest).unwrap();
        assert!(SearchEngine::load_from_disk(dir, 41, 0).is_none());
        // A peer of this binary loaded before the pass flushes: it must not
        // relabel those files current.
        peer.insert_chunk("y".into(), axis(2));
        peer.dump_to_disk(dir, 42, 0).unwrap();
        assert!(SearchEngine::load_from_disk(dir, 42, 0).is_none());
    }

    #[test]
    fn astra_r2_4_a_rebuild_a_maintenance_pass_overtook_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        graph_with_x(0).dump_to_disk(dir, 41, 0).unwrap();
        // Another process starts a cache-miss rebuild and reads x at v_old.
        let mut delayed = graph_with_x(0);
        delayed.mark_rebuilt_at(crate::search::read_maintenance_generation(dir));
        // The scrub invalidates, rewrites x and publishes its own rebuild.
        crate::search::invalidate_for_maintenance(dir).unwrap();
        let mut scrubbed = graph_with_x(1);
        scrubbed.mark_rebuilt_at(crate::search::read_maintenance_generation(dir));
        scrubbed.dump_to_disk(dir, 41, 0).unwrap();
        // The delayed rebuild finishes: refused, nothing written.
        let error = delayed.dump_to_disk(dir, 41, 0).unwrap_err();
        assert!(crate::search::crossed_maintenance(&error), "{error:#}");
        let loaded = SearchEngine::load_from_disk(dir, 41, 0).unwrap();
        assert_eq!(
            loaded
                .search_chunks(&axis(1), 1, 0.99)
                .first()
                .map(|hit| hit.id.as_str()),
            Some("x")
        );
        assert!(loaded.search_chunks(&axis(0), 1, 0.99).is_empty());
    }

    #[tokio::test]
    async fn astra_6_a_publisher_loaded_before_a_manifest_invalidation_stays_fenced() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let projects_dir = tmp.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();
        {
            let storage = Storage::open(&db_path).unwrap();
            for i in 0..40 {
                insert_test_chunk(&storage, &format!("chunk-{i}"));
            }
            insert_test_chunk_with(&storage, "x", &axis(0));
        }
        // Writes the first cache generation.
        {
            Engine::new(&db_path, &projects_dir).unwrap();
        }
        // Engine A: a long-lived MCP server or daemon, loaded with x at v_old.
        let eng_a = Engine::new(&db_path, &projects_dir).unwrap();
        // Scrub B: invalidates the manifest, rewrites x to v_clean, rebuilds and
        // publishes. No other publisher runs in between.
        let eng_b = Engine::new(&db_path, &projects_dir).unwrap();
        eng_b.invalidate_index_manifest().unwrap();
        // A flushing mid-pass, manifest gone, publishes nothing at all.
        insert_test_chunk(eng_a.storage(), "chunk-mid");
        eng_a
            .search
            .write()
            .await
            .insert_chunk("chunk-mid".into(), vec![0.1_f32; 384]);
        eng_a.flush_index_checked().await.unwrap();
        assert!(!eng_a.index_dir().join("manifest.json").exists());
        insert_test_chunk_with(eng_b.storage(), "x", &axis(1));
        eng_b.rebuild_search_index().await.unwrap();
        // A imports a genuine new chunk and flushes its pre-scrub graph.
        insert_test_chunk(eng_a.storage(), "chunk-new");
        eng_a
            .search
            .write()
            .await
            .insert_chunk("chunk-new".into(), vec![0.1_f32; 384]);
        eng_a.flush_index_checked().await.unwrap();

        let eng_c = Engine::new(&db_path, &projects_dir).unwrap();
        let index = eng_c.search.read().await;
        let hits = index.search_chunks(&axis(1), 1, 0.99);
        assert_eq!(
            hits.first().map(|hit| hit.id.as_str()),
            Some("x"),
            "the scrubbed vector must survive a pre-scrub publisher's flush"
        );
        assert!(index.has_chunk("chunk-new"));
        assert!(index.has_chunk("chunk-mid"));
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
        let (search, chunk_total, reflection_total, _) =
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

    // --- WP-D low #6: `rebuild_search_index`'s post-stream reconciliation
    // must blank ids the DB no longer has, not just backfill ids it gained,
    // so the swapped-in index never hands back a dead id. This exercises
    // the exact reconciliation call embedded in `rebuild_search_index`
    // (`db_id_set` from a fresh `load_all_chunk_ids` probe, then
    // `blank_orphan_chunks`) against a `fresh` engine produced by the same
    // `rebuild_search_index_streaming` it uses, with the deletion happening
    // in the window between the two — the scenario a live watcher/hook
    // (no heavy-work permit) creates during the real multi-minute rebuild. ---
    #[test]
    fn rebuild_search_index_post_stream_reconciliation_blanks_ids_deleted_during_the_window() {
        let storage = Storage::open_memory().unwrap();
        for i in 0..5 {
            insert_test_chunk(&storage, &format!("chunk-{i}"));
        }

        // Snapshot, exactly as `rebuild_search_index`'s `spawn_blocking` does.
        let (mut fresh, chunk_total, _reflection_total, streamed) =
            rebuild_search_index_streaming(&storage, RECONCILE_BATCH).unwrap();
        assert_eq!(chunk_total, 5);
        for i in 0..5 {
            assert!(fresh.has_chunk(&format!("chunk-{i}")));
        }

        // Simulate a watcher/hook deleting rows in the window while the
        // (in reality minutes-long) stream above was still running.
        storage.delete_chunk("chunk-1").unwrap();
        storage.delete_chunk("chunk-3").unwrap();

        // Call the SAME function `rebuild_search_index` calls — not a
        // reimplementation of its logic — so removing the blanking calls
        // inside `reconcile_fresh_against_db` fails this test (WP-D low #7).
        reconcile_fresh_against_db(&storage, &mut fresh, &streamed).unwrap();

        assert!(!fresh.has_chunk("chunk-1"));
        assert!(!fresh.has_chunk("chunk-3"));
        assert_eq!(
            fresh.chunk_count(),
            3,
            "the swapped-in index must never hand back a dead id — no dead \
             id is ever returned"
        );
        for i in [0, 2, 4] {
            assert!(fresh.has_chunk(&format!("chunk-{i}")));
        }
    }

    // --- WP-D: foreground `Engine::new` must not inline-rebuild on material
    // orphans; it defers to the `needs_compaction` marker instead. ---

    #[tokio::test]
    async fn orphans_material_in_foreground_marks_compaction_instead_of_rebuilding() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let projects_dir = tmp.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();
        let index_dir = tmp.path().join("index");

        // Seed 40 chunks directly (bypassing Engine, matching the pattern
        // other tests here use) so the first `Engine::new` has a real corpus
        // to build a cache from.
        {
            let storage = Storage::open(&db_path).unwrap();
            for i in 0..40 {
                insert_test_chunk(&storage, &format!("chunk-{i}"));
            }
        }

        // First construction: no cache exists yet, so this is the
        // out-of-scope "missing manifest still rebuilds" path (interface 4).
        // It writes a fresh, compact 40-point cache to disk.
        {
            let eng = Engine::new(&db_path, &projects_dir).unwrap();
            assert_eq!(eng.search.read().await.chunk_count(), 40);
        }
        assert!(
            !crate::search::needs_compaction(&index_dir),
            "a fresh full rebuild must not leave a stale compaction marker"
        );

        // Delete 30 of 40 rows (75% — comfortably over ORPHAN_REBUILD_DIVISOR's
        // 1/20 materiality threshold) to simulate the dip: DB count now below
        // what the cache/manifest describes.
        {
            let storage = Storage::open(&db_path).unwrap();
            for i in 0..30 {
                storage.delete_chunk(&format!("chunk-{i}")).unwrap();
            }
        }

        let t0 = std::time::Instant::now();
        let eng2 = Engine::new(&db_path, &projects_dir).unwrap();
        let elapsed = t0.elapsed();

        // The whole point: this must be a cache load + reconciliation, not a
        // from-scratch rebuild of the remaining 10 points. On a corpus this
        // tiny both paths are fast in wall-clock terms, so the receipt that
        // actually matters is behavioural: the marker is set and the engine
        // is still serving (never panicked/blocked on a rebuild loop).
        assert!(
            crate::search::needs_compaction(&index_dir),
            "material orphans in a foreground Engine::new must set the \
             needs_compaction marker instead of rebuilding inline"
        );
        assert_eq!(
            eng2.search.read().await.chunk_count(),
            10,
            "the 30 deleted ids must be reconciled (blanked) even without an \
             inline full rebuild"
        );
        tracing::debug!(?elapsed, "orphan-reconciliation Engine::new completed");

        // The explicit maintenance path (also what the daemon's compaction
        // loop calls) consumes and clears the marker, and produces a compact
        // index with no tombstoned slots left over.
        let (chunks, _reflections) = eng2.rebuild_search_index().await.unwrap();
        assert_eq!(chunks, 10);
        assert!(
            !crate::search::needs_compaction(&index_dir),
            "rebuild_search_index must clear the needs_compaction marker it satisfies"
        );
    }

    // --- WP-D medium #2: a pre-compaction engine's later flush must not
    // republish the orphans a concurrent daemon compaction already removed. ---

    #[tokio::test]
    async fn pre_compaction_engine_flush_does_not_republish_orphans_after_daemon_compacts() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let projects_dir = tmp.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();
        let index_dir = tmp.path().join("index");

        {
            let storage = Storage::open(&db_path).unwrap();
            for i in 0..40 {
                insert_test_chunk(&storage, &format!("chunk-{i}"));
            }
        }

        // First construction: no cache exists yet — writes a fresh, compact
        // 40-point cache to disk (out-of-scope "missing manifest" path).
        {
            Engine::new(&db_path, &projects_dir).unwrap();
        }

        // Delete 30 of 40 rows — negative drift, material orphans.
        {
            let storage = Storage::open(&db_path).unwrap();
            for i in 0..30 {
                storage.delete_chunk(&format!("chunk-{i}")).unwrap();
            }
        }

        // Engine A: a long-lived foreground process (the MCP server is the
        // real example) that loads BEFORE compaction. Its own startup
        // reconciliation blanks the 30 orphans and marks needs_compaction —
        // and, per the fix under test, must NOT leave chunk_dirty set.
        let eng_a = Engine::new(&db_path, &projects_dir).unwrap();
        assert!(crate::search::needs_compaction(&index_dir));
        assert!(
            !eng_a.search.read().await.is_chunk_dirty(),
            "pure startup reconciliation must not leave the chunk side dirty \
             (WP-D medium #2 fix)"
        );

        // Engine B: the daemon's compaction loop, consuming the marker —
        // rebuilds from SQLite (10 live chunks) and dumps a fresh, compact
        // generation, clearing needs_compaction.
        let eng_b = Engine::new(&db_path, &projects_dir).unwrap();
        let (chunks_b, _) = eng_b.rebuild_search_index().await.unwrap();
        assert_eq!(chunks_b, 10);
        assert!(!crate::search::needs_compaction(&index_dir));

        // Engine A now does the real-world thing that triggers a dump on a
        // long-lived process: a new reflection lands and gets flushed (the
        // `store_reflection`/stop-hook path). This must dump ONLY the
        // reflection side — the chunk side was never touched by genuine new
        // content, so it must carry forward from whatever is on disk NOW
        // (engine B's compact 10-point generation), not republish engine A's
        // stale pre-compaction 40-slot/30-tombstone snapshot.
        {
            let storage = Storage::open(&db_path).unwrap();
            storage
                .insert_reflection("refl-a", "content", &[], &[0.2_f32; 384])
                .unwrap();
        }
        {
            let mut idx = eng_a.search.write().await;
            idx.insert_reflection("refl-a".into(), vec![0.2_f32; 384]);
        }
        eng_a.flush_index_checked().await.unwrap();

        // The next fresh load (a new process, or a restarted daemon) must
        // see the compact 10-chunk generation with no re-triggered
        // materiality — not engine A's republished orphans.
        let eng_c = Engine::new(&db_path, &projects_dir).unwrap();
        assert_eq!(
            eng_c.search.read().await.chunk_count(),
            10,
            "engine A's flush must not have republished the 30 orphans engine \
             B already compacted away"
        );
        assert!(
            !crate::search::needs_compaction(&index_dir),
            "the next load must not be material — no ping-pong back into \
             another daemon rebuild"
        );
        assert!(
            eng_c.search.read().await.has_reflection("refl-a"),
            "engine A's genuine new reflection must still have been persisted"
        );
    }

    // --- Opus review follow-up on WP-D medium #2: `clear_reconciliation_dirty`
    // only protects a side the engine never genuinely touched. A side the
    // engine DID genuinely mutate (a real `insert_chunk`/`insert_reflection`)
    // is dirty on its own merits and was still published from memory even
    // when a concurrent compaction had already replaced that exact side with
    // a newer, compact generation — reviving the tombstones the compaction
    // just removed. The compaction-epoch guard in `dump_to_disk` closes this:
    // it refuses to publish EITHER side from memory once the on-disk epoch is
    // newer than what this engine loaded, dirty or not. ---

    #[tokio::test]
    async fn stale_engine_genuine_reflection_insert_does_not_republish_orphans_a_daemon_already_compacted(
    ) {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let projects_dir = tmp.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();
        let index_dir = tmp.path().join("index");

        {
            let storage = Storage::open(&db_path).unwrap();
            for i in 0..40 {
                storage
                    .insert_reflection(&format!("refl-{i}"), "content", &[], &[0.1_f32; 384])
                    .unwrap();
            }
        }

        // First construction: writes a fresh, compact 40-reflection cache
        // (epoch 1 — the first generation this on-disk cache ever gets).
        {
            Engine::new(&db_path, &projects_dir).unwrap();
        }

        // Delete 30 of 40 reflections — negative drift, material orphans.
        {
            let storage = Storage::open(&db_path).unwrap();
            for i in 0..30 {
                storage.delete_reflection(&format!("refl-{i}")).unwrap();
            }
        }

        // Engine A: a long-lived foreground process (an MCP server is the
        // real example) that loads BEFORE compaction, at epoch 1.
        let eng_a = Engine::new(&db_path, &projects_dir).unwrap();
        assert!(crate::search::needs_compaction(&index_dir));

        // Engine B: the daemon's compaction loop — rebuilds from SQLite (10
        // live reflections) and publishes generation epoch 2.
        let eng_b = Engine::new(&db_path, &projects_dir).unwrap();
        let (_chunks_b, reflections_b) = eng_b.rebuild_search_index().await.unwrap();
        assert_eq!(reflections_b, 10);
        assert!(!crate::search::needs_compaction(&index_dir));

        // Engine A now does exactly what `store_reflection` does in
        // production: a genuine NEW reflection lands on the exact side a
        // concurrent compaction just replaced. Its `reflection_dirty` flag
        // is legitimately true — this is not reconciliation noise, it is
        // real new content — so the pre-epoch fix still published engine
        // A's stale, 41-slot/30-tombstone in-memory reflection graph over
        // engine B's newer, compact 10-slot generation.
        {
            let storage = Storage::open(&db_path).unwrap();
            storage
                .insert_reflection("refl-a", "content", &[], &[0.2_f32; 384])
                .unwrap();
        }
        {
            let mut idx = eng_a.search.write().await;
            idx.insert_reflection("refl-a".into(), vec![0.2_f32; 384]);
        }
        eng_a.flush_index_checked().await.unwrap();

        // The next fresh load must see engine B's compact generation plus
        // engine A's genuine new reflection (recovered via SQLite
        // backfill), with no resurrected tombstones and no re-triggered
        // compaction marker.
        let eng_c = Engine::new(&db_path, &projects_dir).unwrap();
        assert_eq!(
            eng_c.search.read().await.reflection_count(),
            11,
            "engine A's stale-but-genuinely-dirty flush must not have \
             republished the 30 orphans engine B already compacted away, \
             and the genuine new reflection must still be present \
             (backfilled from SQLite on the next load)"
        );
        assert!(
            !crate::search::needs_compaction(&index_dir),
            "no ping-pong: engine A's stale-but-dirty flush must not \
             resurrect the tombstones engine B just compacted away"
        );
    }

    #[tokio::test]
    async fn stale_engine_genuine_chunk_insert_does_not_republish_orphans_a_daemon_already_compacted(
    ) {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let projects_dir = tmp.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();
        let index_dir = tmp.path().join("index");

        {
            let storage = Storage::open(&db_path).unwrap();
            for i in 0..40 {
                insert_test_chunk(&storage, &format!("chunk-{i}"));
            }
        }

        // First construction: writes a fresh, compact 40-chunk cache
        // (epoch 1).
        {
            Engine::new(&db_path, &projects_dir).unwrap();
        }

        // Delete 30 of 40 chunks — negative drift, material orphans.
        {
            let storage = Storage::open(&db_path).unwrap();
            for i in 0..30 {
                storage.delete_chunk(&format!("chunk-{i}")).unwrap();
            }
        }

        // Engine A: a long-lived foreground process that loads BEFORE
        // compaction, at epoch 1.
        let eng_a = Engine::new(&db_path, &projects_dir).unwrap();
        assert!(crate::search::needs_compaction(&index_dir));

        // Engine B: the daemon's compaction loop — rebuilds from SQLite (10
        // live chunks) and publishes generation epoch 2.
        let eng_b = Engine::new(&db_path, &projects_dir).unwrap();
        let (chunks_b, _) = eng_b.rebuild_search_index().await.unwrap();
        assert_eq!(chunks_b, 10);
        assert!(!crate::search::needs_compaction(&index_dir));

        // Engine A now does exactly what a `--import`/`--enrich`/`setup`
        // run that straddles a compaction does: a genuine NEW chunk lands
        // on the exact side a concurrent compaction just replaced.
        {
            let storage = Storage::open(&db_path).unwrap();
            insert_test_chunk(&storage, "chunk-new");
        }
        {
            let mut idx = eng_a.search.write().await;
            idx.insert_chunk("chunk-new".into(), vec![0.1_f32; 384]);
        }
        eng_a.flush_index_checked().await.unwrap();

        // The next fresh load must see engine B's compact generation plus
        // engine A's genuine new chunk (recovered via SQLite backfill),
        // with no resurrected tombstones and no re-triggered compaction
        // marker.
        let eng_c = Engine::new(&db_path, &projects_dir).unwrap();
        assert_eq!(
            eng_c.search.read().await.chunk_count(),
            11,
            "engine A's stale-but-genuinely-dirty flush must not have \
             republished the 30 orphans engine B already compacted away, \
             and the genuine new chunk must still be present (backfilled \
             from SQLite on the next load)"
        );
        assert!(
            !crate::search::needs_compaction(&index_dir),
            "no ping-pong: engine A's stale-but-dirty chunk flush must not \
             resurrect the tombstones engine B just compacted away"
        );
    }

    // WP-D medium (second review): the epoch guard above only protects the
    // FIRST post-compaction flush. `dump_to_disk` adopts the newer on-disk
    // epoch into `self.epoch` unconditionally on a stale dump, so by the
    // very next flush `self.epoch == on_disk_epoch` again and
    // `stale_generation` is false — even though this engine's in-memory
    // graph/id-maps were never reloaded and still hold the pre-compaction
    // (tombstoned) generation. A second genuine insert-and-flush on the
    // same stale engine therefore republishes that old graph over the
    // compacted one: the same ping-pong the first fix was meant to close,
    // one flush later. Must fail on the code that only guards a single
    // flush (revert to `self.epoch = new_epoch;` unconditional).
    #[tokio::test]
    async fn stale_engine_second_genuine_flush_does_not_republish_orphans_either() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let projects_dir = tmp.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();
        let index_dir = tmp.path().join("index");

        {
            let storage = Storage::open(&db_path).unwrap();
            for i in 0..40 {
                insert_test_chunk(&storage, &format!("chunk-{i}"));
            }
        }

        // First construction: writes a fresh, compact 40-chunk cache (epoch 1).
        {
            Engine::new(&db_path, &projects_dir).unwrap();
        }

        // Delete 30 of 40 chunks — negative drift, material orphans.
        {
            let storage = Storage::open(&db_path).unwrap();
            for i in 0..30 {
                storage.delete_chunk(&format!("chunk-{i}")).unwrap();
            }
        }

        // Engine A: a long-lived foreground process that loads BEFORE
        // compaction, at epoch 1.
        let eng_a = Engine::new(&db_path, &projects_dir).unwrap();
        assert!(crate::search::needs_compaction(&index_dir));

        // Engine B: the daemon's compaction loop — rebuilds from SQLite (10
        // live chunks) and publishes generation epoch 2.
        let eng_b = Engine::new(&db_path, &projects_dir).unwrap();
        let (chunks_b, _) = eng_b.rebuild_search_index().await.unwrap();
        assert_eq!(chunks_b, 10);
        assert!(!crate::search::needs_compaction(&index_dir));

        // Flush 1: engine A's first genuine post-compaction insert. This is
        // the scenario the previous test already covers, and must still
        // defer to engine B's generation.
        {
            let storage = Storage::open(&db_path).unwrap();
            insert_test_chunk(&storage, "chunk-new-1");
        }
        {
            let mut idx = eng_a.search.write().await;
            idx.insert_chunk("chunk-new-1".into(), vec![0.1_f32; 384]);
        }
        eng_a.flush_index_checked().await.unwrap();

        // Flush 2: the SAME long-lived engine A does a second genuine
        // insert-and-flush without ever reloading. If the epoch guard only
        // lasted one flush, `self.epoch` now equals the on-disk epoch again,
        // `stale_generation` reads false, and this flush republishes engine
        // A's original 40-entry (30-tombstoned) graph over the compacted
        // generation.
        {
            let storage = Storage::open(&db_path).unwrap();
            insert_test_chunk(&storage, "chunk-new-2");
        }
        {
            let mut idx = eng_a.search.write().await;
            idx.insert_chunk("chunk-new-2".into(), vec![0.1_f32; 384]);
        }
        eng_a.flush_index_checked().await.unwrap();

        // The next fresh load must see engine B's compact generation plus
        // BOTH of engine A's genuine new chunks (recovered via SQLite
        // backfill), with no resurrected tombstones and no re-triggered
        // compaction marker — on the SECOND stale flush, not just the
        // first.
        let eng_c = Engine::new(&db_path, &projects_dir).unwrap();
        assert_eq!(
            eng_c.search.read().await.chunk_count(),
            12,
            "engine A's second stale-but-genuinely-dirty flush must not \
             have republished the 30 orphans engine B already compacted \
             away, and both genuine new chunks must still be present \
             (backfilled from SQLite on the next load)"
        );
        assert!(
            !crate::search::needs_compaction(&index_dir),
            "no ping-pong on the SECOND stale flush: a stale engine must \
             never publish from memory again once it has deferred once, \
             not just on its first post-compaction flush"
        );
    }
}
