//! Daemon module — background processing for progressive enrichment.
//!
//! Runs eight background tasks:
//! 1. File watcher (existing) — auto-import new JSONL files
//! 2. Extraction loop (Layer 2) — V3 extraction on imported conversations
//! 3. Narrator loop (Layer 3) — AI batch narrative generation (if API key set)
//! 4. Consolidation loop (Layer 4) — Dreamer v1 typed fact extraction from narratives
//! 5. Dream loop (v10) — periodic `dream_cadence::dream_loop` cycle over the
//!    witness ledger (see that module for cadence/persistence/cost-discipline)
//! 6. Release-ancestry loop — precomputes deterministic TAD v2 episode labels
//! 7. Memory registry loop — periodic metadata-only scan of native memory files (`~/.claude/projects/*/memory/*.md`) into `memory_registry`; never embeds or injects memory content
//! 8. Provenance backfill loop — one idle-gated structural batch at a time

pub mod consolidation;
pub mod dream_cadence;
pub mod ratification;
pub mod trained_rerank;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use sha2::{Digest, Sha256};
use tokio::sync::{RwLock, Semaphore};

use crate::api::types::BatchRequest;
use crate::api::{AnthropicClient, BatchClient};
use crate::embeddings::EmbeddingEngine;
use crate::extraction;
use crate::import;
use crate::search::SearchEngine;
use crate::storage::Storage;

/// `CSR_NO_MEMORY_REGISTRY` kill switch — same "1"/"true" (case-insensitive) idiom
/// as `crate::daemon::dream_cadence::dreaming_disabled`.
pub fn memory_registry_disabled() -> bool {
    std::env::var("CSR_NO_MEMORY_REGISTRY")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn provenance_backfill_disabled() -> bool {
    std::env::var("CSR_NO_PROVENANCE_BACKFILL")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Configuration for the daemon loops.
pub struct DaemonConfig {
    pub extraction_interval_secs: u64,
    pub batch_size_trigger: usize,
    pub batch_time_trigger_secs: u64,
    pub batch_poll_interval_secs: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            extraction_interval_secs: 30,
            batch_size_trigger: 10,
            batch_time_trigger_secs: 1800,
            batch_poll_interval_secs: 60,
        }
    }
}

/// The daemon orchestrates file watching + enrichment loops.
pub struct Daemon {
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    projects_dir: PathBuf,
    index_dir: PathBuf,
    config: DaemonConfig,
    api_client: Option<AnthropicClient>,
}

impl Daemon {
    pub fn new(
        storage: Arc<Storage>,
        embeddings: Arc<EmbeddingEngine>,
        search: Arc<RwLock<SearchEngine>>,
        projects_dir: PathBuf,
        index_dir: PathBuf,
        config: DaemonConfig,
        enable_ai: bool,
    ) -> Self {
        let api_client = if enable_ai {
            AnthropicClient::from_env()
        } else {
            None
        };
        Self {
            storage,
            embeddings,
            search,
            projects_dir,
            index_dir,
            config,
            api_client,
        }
    }

    /// Acquire a lockfile to prevent multiple daemon instances.
    /// Uses OS-level advisory locking (flock) via the fs2 crate.
    pub fn acquire_lock() -> Result<std::fs::File> {
        use fs2::FileExt;

        let lock_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".claude-self-reflect");
        std::fs::create_dir_all(&lock_dir)?;
        let lock_path = lock_dir.join("daemon.lock");

        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&lock_path)?;

        // Try non-blocking exclusive lock — fails if another daemon holds it
        file.try_lock_exclusive().map_err(|_| {
            anyhow::anyhow!(
                "another daemon instance is already running (lockfile: {})",
                lock_path.display()
            )
        })?;

        // Write PID for diagnostics (after acquiring lock)
        use std::io::Write;
        let mut locked = &file;
        write!(locked, "{}", std::process::id())?;

        Ok(file)
    }

    /// Run the daemon: file watcher + extraction loop + narrator loop.
    pub async fn run(self) -> Result<()> {
        let _lock = Self::acquire_lock()?;
        tracing::info!("daemon started, acquired lockfile");

        // Shared shutdown flag for graceful termination (D-8)
        let shutdown = Arc::new(AtomicBool::new(false));

        // Shared one-owner permit for watcher batches, plan imports, and
        // dream cycles. Owned permits provide RAII release on every exit.
        let heavy_work = Arc::new(Semaphore::new(1));

        // Start file watcher
        let watcher = crate::import::watcher::FileWatcher::new(
            self.projects_dir.clone(),
            self.storage.clone(),
            self.embeddings.clone(),
            self.search.clone(),
            self.index_dir.clone(),
        )
        .with_heavy_work_permit(heavy_work.clone());
        let watcher_handle = watcher.spawn();
        tracing::info!("file watcher started");

        // Start extraction loop (Layer 2 — always runs, free)
        let extraction_handle = {
            let storage = self.storage.clone();
            let embeddings = self.embeddings.clone();
            let search = self.search.clone();
            let interval = self.config.extraction_interval_secs;
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                extraction_loop(storage, embeddings, search, interval, shutdown).await;
            })
        };
        tracing::info!("extraction loop started (Layer 2)");

        // Start narrator loop (Layer 3 — only if API key set)
        let narrator_handle = if let Some(client) = self.api_client {
            let storage = self.storage.clone();
            let embeddings = self.embeddings.clone();
            let search = self.search.clone();
            let config_batch_size = self.config.batch_size_trigger;
            let config_batch_time = self.config.batch_time_trigger_secs;
            let config_poll = self.config.batch_poll_interval_secs;
            let shutdown = shutdown.clone();
            let handle = tokio::spawn(async move {
                narrator_loop(
                    storage,
                    embeddings,
                    search,
                    Arc::new(client),
                    config_batch_size,
                    config_batch_time,
                    config_poll,
                    shutdown,
                )
                .await;
            });
            tracing::info!("narrator loop started (Layer 3 — AI narrative)");
            Some(handle)
        } else {
            tracing::info!("narrator loop skipped (no ANTHROPIC_API_KEY)");
            None
        };

        // Start consolidation loop (Layer 4 — Dreamer v1, always runs, free)
        let consolidation_handle = {
            let storage = self.storage.clone();
            let embeddings = self.embeddings.clone();
            let search = self.search.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                consolidation_loop(storage, embeddings, search, shutdown).await;
            })
        };
        tracing::info!("consolidation loop started (Layer 4 — Dreamer v1)");

        // Maintenance loop: hourly, refreshes the cached integrity verdict
        // (24h TTL — the daemon absorbs the ~10s full check so status calls
        // never do) and attempts a WAL checkpoint+truncate (long-lived MCP
        // readers let the WAL grow unbounded otherwise).
        let maintenance_handle = {
            let storage = self.storage.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                loop {
                    for _ in 0..360 {
                        if shutdown.load(Ordering::SeqCst) {
                            return;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                    }
                    let s = storage.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        match s.integrity_check_cached(24, true) {
                            Ok(ok) => tracing::debug!(healthy = ok, "integrity verdict refreshed"),
                            Err(e) => tracing::warn!("integrity refresh failed: {e}"),
                        }
                        match s.checkpoint_wal() {
                            Ok(true) => tracing::debug!("WAL checkpoint+truncate succeeded"),
                            Ok(false) => tracing::debug!("WAL checkpoint blocked by readers"),
                            Err(e) => tracing::warn!("WAL checkpoint failed: {e}"),
                        }
                    })
                    .await;
                }
            })
        };
        tracing::info!("maintenance loop started (integrity cache + WAL checkpoint)");

        // Registry loop: every 10 minutes, incrementally ingest ~/.claude/history.jsonl
        // into session_registry (coverage spine — never embedded, never injected).
        // Daemon is the ONLY caller by design: single-writer serialization is part of
        // the checkpoint's crash-safety contract (Codex adversarial review).
        let registry_handle = {
            let storage = self.storage.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                loop {
                    for _ in 0..60 {
                        if shutdown.load(Ordering::SeqCst) {
                            return;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                    }
                    let s = storage.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        match s.refresh_contamination_cache() {
                            Ok(measurement) => tracing::debug!(
                                conversations = measurement.conversations,
                                total_conversations = measurement.total_conversations,
                                pct = measurement.pct,
                                "contamination measurement refreshed"
                            ),
                            Err(e) => tracing::warn!("contamination refresh failed: {e}"),
                        }
                        let Some(home) = dirs::home_dir() else { return };
                        let path = home.join(".claude/history.jsonl");
                        match crate::import::registry::ingest_history(&s, &path) {
                            Ok(stats) => tracing::debug!(
                                lines = stats.lines_read,
                                sessions = stats.sessions_upserted,
                                errors = stats.parse_errors,
                                "session registry ingested"
                            ),
                            Err(e) => tracing::warn!("registry ingest failed: {e}"),
                        }
                    })
                    .await;
                }
            })
        };
        tracing::info!("registry loop started (history.jsonl → session_registry)");

        // Plans loop: every 30 minutes, (re)import changed ~/.claude/plans/*.md docs
        // as source='plan' chunks. Runs AFTER registry ingest has had a chance to
        // populate session windows (correlation's Strategy 2 needs them, but waives
        // gracefully when absent). mtime-keyed import_state makes each pass cheap.
        let plans_handle = {
            // Daemon holds the parts, not an Engine — assemble one for import_plan
            // (same Arcs, so storage/index writes land in the shared instances).
            let engine = Arc::new(crate::engine::Engine::from_parts(
                self.storage.clone(),
                self.embeddings.clone(),
                self.search.clone(),
                self.projects_dir.clone(),
            ));
            let shutdown = shutdown.clone();
            let heavy_work = heavy_work.clone();
            tokio::spawn(async move {
                loop {
                    for _ in 0..180 {
                        if shutdown.load(Ordering::SeqCst) {
                            return;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                    }
                    let eng = engine.clone();
                    let shutdown_inner = shutdown.clone();
                    let permit = match heavy_work.clone().acquire_owned().await {
                        Ok(permit) => permit,
                        Err(_) => return,
                    };
                    let _ = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        let Some(home) = dirs::home_dir() else { return };
                        let plans_dir = home.join(".claude/plans");
                        let plans =
                            match crate::import::plans::discover_plans(&plans_dir, eng.storage()) {
                                Ok(p) => p,
                                Err(e) => {
                                    tracing::warn!("plan discovery failed: {e}");
                                    return;
                                }
                            };
                        for plan in plans {
                            // Imports mutate chunks + the shared HNSW index; a batch
                            // that straddles shutdown would race the final
                            // dump_to_disk (CodeRabbit). Stop between plans — each
                            // single import is bounded and awaited below.
                            if shutdown_inner.load(Ordering::SeqCst) {
                                return;
                            }
                            match crate::import::plans::import_plan(&eng, &plan) {
                                Ok(n) => {
                                    tracing::debug!(slug = %plan.slug, chunks = n, "plan imported")
                                }
                                Err(e) => {
                                    tracing::warn!(slug = %plan.slug, "plan import failed: {e}")
                                }
                            }
                        }
                    })
                    .await;
                }
            })
        };
        tracing::info!("plans loop started (~/.claude/plans → source='plan' chunks)");

        // Memory registry loop: every 30 minutes, scan <projects_root>/*/memory/*.md
        // and upsert metadata-only rows into memory_registry. Never embeds or
        // injects memory file bodies — scan_memory_dirs already enforces that.
        let memory_registry_handle = {
            let storage = self.storage.clone();
            let projects_root = self.projects_dir.clone();
            let shutdown = shutdown.clone();
            let heavy_work = heavy_work.clone();
            tokio::spawn(async move {
                if memory_registry_disabled() {
                    tracing::info!("memory registry loop disabled via CSR_NO_MEMORY_REGISTRY");
                    return;
                }
                loop {
                    for _ in 0..180 {
                        if shutdown.load(Ordering::SeqCst) {
                            return;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                    }
                    let s = storage.clone();
                    let projects_root = projects_root.clone();
                    let permit = match heavy_work.clone().acquire_owned().await {
                        Ok(permit) => permit,
                        Err(_) => return,
                    };
                    let _ = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        match crate::import::memory_registry::scan_memory_dirs(&s, &projects_root) {
                            Ok(stats) => tracing::debug!(
                                files_seen = stats.files_seen,
                                upserted = stats.upserted,
                                deleted = stats.deleted,
                                schema_misses = stats.schema_misses,
                                "memory registry scanned"
                            ),
                            Err(e) => tracing::warn!("memory registry scan failed: {e}"),
                        }
                    })
                    .await;
                }
            })
        };
        if !memory_registry_disabled() {
            tracing::info!(
                "memory registry loop started (<projects_root>/*/memory/*.md → memory_registry)"
            );
        }

        // Populate one structural-provenance batch per idle window. Parsing
        // happens before the bounded write transaction and the shared permit
        // keeps it out of the watch/import and dream critical sections.
        let provenance_backfill_handle = {
            let storage = self.storage.clone();
            let projects_root = self.projects_dir.clone();
            let shutdown = shutdown.clone();
            let heavy_work = heavy_work.clone();
            tokio::spawn(async move {
                loop {
                    for _ in 0..30 {
                        if shutdown.load(Ordering::SeqCst) {
                            return;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                    }
                    if provenance_backfill_disabled()
                        || !dream_cadence::is_idle(
                            dream_cadence::last_activity_at(&storage),
                            chrono::Utc::now(),
                            dream_cadence::idle_secs(),
                        )
                    {
                        continue;
                    }
                    let Some(permit) =
                        acquire_heavy_work_unless_shutdown(heavy_work.clone(), shutdown.as_ref())
                            .await
                    else {
                        return;
                    };
                    if provenance_backfill_disabled()
                        || !dream_cadence::is_idle(
                            dream_cadence::last_activity_at(&storage),
                            chrono::Utc::now(),
                            dream_cadence::idle_secs(),
                        )
                    {
                        drop(permit);
                        continue;
                    }
                    let storage = storage.clone();
                    let projects_root = projects_root.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        match crate::import::provenance_backfill::backfill_incremental(
                            &storage,
                            &projects_root,
                            crate::import::provenance_backfill::DEFAULT_BATCH_SIZE,
                        ) {
                            Ok(stats) => tracing::debug!(
                                scanned = stats.chunks_scanned,
                                reconstructed = stats.chunks_reconstructed,
                                unknown = stats.chunks_unreconstructible,
                                "provenance backfill batch completed"
                            ),
                            Err(error) => tracing::warn!(%error, "provenance backfill failed"),
                        }
                    })
                    .await;
                }
            })
        };

        // Optional Codex rollout loop. It runs once immediately and then every
        // 30 minutes. Missing ~/.codex/sessions is deliberately silent, and the
        // directory is re-checked each cycle so a later installation is detected.
        let codex_rollout_handle = {
            let engine = Arc::new(crate::engine::Engine::from_parts(
                self.storage.clone(),
                self.embeddings.clone(),
                self.search.clone(),
                self.projects_dir.clone(),
            ));
            let shutdown = shutdown.clone();
            let heavy_work = heavy_work.clone();
            tokio::spawn(async move {
                loop {
                    if shutdown.load(Ordering::SeqCst) {
                        return;
                    }
                    let codex_root = dirs::home_dir().map(|home| home.join(".codex/sessions"));
                    if let Some(root) = codex_root.filter(|root| root.exists()) {
                        let permit = match heavy_work.clone().acquire_owned().await {
                            Ok(permit) => permit,
                            Err(_) => return,
                        };
                        let adapter_engine = engine.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            let _permit = permit;
                            match crate::import::codex_rollout::import_changed_rollouts(
                                &adapter_engine,
                                &root,
                            ) {
                                Ok(stats) => tracing::debug!(
                                    discovered = stats.files_discovered,
                                    files = stats.files_imported,
                                    chunks = stats.chunks_imported,
                                    vanished = stats.vanished,
                                    schema_misses = stats.schema_misses,
                                    csr_tool_blocks_suppressed = stats.csr_tool_blocks_suppressed,
                                    csr_hook_wrappers_scrubbed = stats.csr_hook_wrappers_scrubbed,
                                    "Codex rollouts imported"
                                ),
                                Err(error) => {
                                    tracing::warn!(%error, "Codex rollout import failed")
                                }
                            }
                        })
                        .await;
                    }
                    for _ in 0..180 {
                        if shutdown.load(Ordering::SeqCst) {
                            return;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                    }
                }
            })
        };

        // Ratification loop — dialog-act scoring (interval via CSR_RATIFICATION_INTERVAL_SECS)
        let ratification_handle = {
            let storage = self.storage.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                ratification_loop(storage, shutdown).await;
            })
        };
        tracing::info!("ratification loop started");

        // Dream loop (v10) — periodic cadence-gated `dream_cadence::dream_loop`
        // cycle over the witness ledger. See that module's doc for cadence,
        // persistence, cancellation, monotonic scheduling, and the heavy
        // work permit it shares with the watcher and plans loop above.
        let dream_handle = {
            let engine = Arc::new(crate::engine::Engine::from_parts(
                self.storage.clone(),
                self.embeddings.clone(),
                self.search.clone(),
                self.projects_dir.clone(),
            ));
            let heavy_work = heavy_work.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                dream_cadence::dream_loop(engine, heavy_work, shutdown).await;
            })
        };
        tracing::info!("dream loop started (v10 \"dreaming\")");

        // TAD v2 release ancestry: git traversal is daemon-only and shares
        // the single heavy-work permit with watcher/plans/dream cycles.
        let ancestry_handle = {
            let storage = self.storage.clone();
            let heavy_work = heavy_work.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                ancestry_refresh_loop(storage, heavy_work, shutdown).await;
            })
        };
        tracing::info!("release-ancestry refresh loop started (TAD v2)");

        // Trained re-ranker: deterministic nightly label harvest, fit, and
        // chronological gate. It shares the single heavy-work permit and does
        // not invoke the narrative/LLM path.
        let trained_rerank_handle = {
            let storage = self.storage.clone();
            let embeddings = self.embeddings.clone();
            let search = self.search.clone();
            let heavy_work = heavy_work.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                trained_rerank::nightly_loop(storage, embeddings, search, heavy_work, shutdown)
                    .await;
            })
        };
        tracing::info!("trained re-ranker nightly loop started");

        // Journal v4 dream server (locked decision 7: daemon-hosted, always
        // on, stable loopback port, bookmarkable). It binds 127.0.0.1 only
        // and serves read-only routes. `spawn_for_daemon` returns `None`
        // when `CSR_NO_JOURNAL_SERVER=1`, and the task it does spawn logs
        // and returns on any bind/serve failure rather than propagating —
        // a busy port must never take the daemon down. It shares the same
        // `Arc<AtomicBool>` every other loop uses, so shutdown is one flag.
        let journal_handle =
            crate::journal::spawn_for_daemon(self.storage.clone(), shutdown.clone());
        if journal_handle.is_some() {
            tracing::info!("journal server started (v10.1 journal v4)");
        }

        // Wait for Ctrl+C
        tokio::signal::ctrl_c().await?;
        tracing::info!("shutting down daemon gracefully");

        // Signal all loops to stop
        shutdown.store(true, Ordering::SeqCst);

        // Give loops time to finish current work (5s timeout)
        let timeout = tokio::time::Duration::from_secs(5);
        let _ = tokio::time::timeout(timeout, extraction_handle).await;
        if let Some(h) = narrator_handle {
            let _ = tokio::time::timeout(timeout, h).await;
        }
        let _ = tokio::time::timeout(timeout, consolidation_handle).await;
        let _ = tokio::time::timeout(timeout, maintenance_handle).await;
        let _ = tokio::time::timeout(timeout, registry_handle).await;
        // Plans imports write the HNSW index the flush below persists. A timeout
        // here would DETACH the task, not stop it — it could then mutate the
        // index after dump_to_disk read its state (Codex). The loop stops
        // between plans on shutdown, so at most one bounded import is in
        // flight: await it fully.
        let _ = plans_handle.await;
        // Same index-mutation contract as plans: never detach an in-flight import.
        let _ = codex_rollout_handle.await;
        // Memory registry only upserts its own SQLite table transactionally —
        // no HNSW mutation — so a timeout here cannot corrupt the search index.
        let _ = tokio::time::timeout(timeout, memory_registry_handle).await;
        let _ = tokio::time::timeout(timeout, provenance_backfill_handle).await;
        let _ = tokio::time::timeout(timeout, ratification_handle).await;
        // The dream loop's tick awaits its `spawn_blocking` cycle directly
        // (see `dream_cadence::tick`) and checks the shutdown flag between
        // repos/anchors, so the timeout below bounds a cycle that ignores
        // cancellation. Dream writes are single-statement inserts — an abrupt
        // exit mid-cycle loses at most that cycle's remaining events, never
        // corrupts state.
        let _ = tokio::time::timeout(timeout, dream_handle).await;
        // An ancestry refresh owns the same heavy-work permit and awaits its
        // blocking git/cache publication. Timing out would detach that task,
        // allowing a cache mutation after `run` returned. The loop abstains
        // immediately when shutdown arrives while it waits for the permit;
        // if publication already began, await that bounded cycle fully.
        let _ = ancestry_handle.await;
        // A cycle holds the shared heavy-work permit and appends its model row
        // transactionally. Await it so shutdown cannot detach a half-harvested
        // training attempt.
        let _ = trained_rerank_handle.await;
        // The journal server stops on the same flag (its graceful-shutdown
        // future polls it). Bounded by the shared timeout: an in-flight
        // request only reads, so a detached one cannot corrupt anything.
        if let Some(h) = journal_handle {
            let _ = tokio::time::timeout(timeout, h).await;
        }
        watcher_handle.abort(); // Watcher uses notify which doesn't check shutdown flag

        // Flush HNSW index to disk before exit
        let mut idx = self.search.write().await;
        if idx.is_dirty() {
            let chunk_count = self.storage.count_chunk_embeddings().unwrap_or(0);
            let refl_count = self.storage.count_reflection_embeddings().unwrap_or(0);
            if let Err(e) = idx.dump_to_disk(&self.index_dir, chunk_count, refl_count) {
                tracing::warn!(error = %e, "failed to flush HNSW index on shutdown");
            } else {
                tracing::info!("HNSW index flushed to disk on shutdown");
            }
        }

        tracing::info!("daemon stopped");
        Ok(())
    }
}

/// Acquire the daemon's serialized heavy-work slot, then recheck shutdown at
/// the handoff boundary. A loop may begin waiting before shutdown and acquire
/// only afterward; returning `None` drops that permit without starting work.
async fn acquire_heavy_work_unless_shutdown(
    heavy_work: Arc<Semaphore>,
    shutdown: &AtomicBool,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    let permit = heavy_work.acquire_owned().await.ok()?;
    if shutdown.load(Ordering::SeqCst) {
        return None;
    }
    Some(permit)
}

fn finish_ancestry_refresh<T>(
    storage: &Storage,
    result: std::result::Result<anyhow::Result<T>, tokio::task::JoinError>,
) -> anyhow::Result<T> {
    match result {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => {
            crate::storage::ancestry::invalidate_ancestry_cache(storage)?;
            Err(error)
        }
        Err(error) => {
            crate::storage::ancestry::invalidate_ancestry_cache(storage)?;
            Err(error.into())
        }
    }
}

async fn ancestry_refresh_once(
    storage: Arc<Storage>,
    heavy_work: Arc<Semaphore>,
    shutdown: &AtomicBool,
) -> anyhow::Result<Option<usize>> {
    if shutdown.load(Ordering::SeqCst) {
        return Ok(None);
    }

    let refresh_storage = storage.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        crate::storage::ancestry::prepare_ancestry_refresh(
            &refresh_storage,
            &chrono::Utc::now().to_rfc3339(),
        )
    })
    .await;
    let mut refresh = finish_ancestry_refresh(&storage, prepared)?;

    for repository in refresh.repositories() {
        if shutdown.load(Ordering::SeqCst) {
            crate::storage::ancestry::invalidate_ancestry_cache(&storage)?;
            return Ok(None);
        }
        let Some(permit) = acquire_heavy_work_unless_shutdown(heavy_work.clone(), shutdown).await
        else {
            crate::storage::ancestry::invalidate_ancestry_cache(&storage)?;
            return Ok(None);
        };
        let walked = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            Ok(refresh.walk_repository(&repository))
        })
        .await;
        refresh = finish_ancestry_refresh(&storage, walked)?;
    }

    if shutdown.load(Ordering::SeqCst) {
        crate::storage::ancestry::invalidate_ancestry_cache(&storage)?;
        return Ok(None);
    }
    let publish_storage = storage.clone();
    let published = tokio::task::spawn_blocking(move || refresh.publish(&publish_storage)).await;
    finish_ancestry_refresh(&storage, published).map(Some)
}

/// Refresh immediately, then once per hour. A fixed post-cycle delay makes
/// cadence deterministic and avoids overlapping refreshes. Git/repository
/// errors are handled per repository by the refresh implementation and leave
/// those conversations neutral (no cache row).
async fn ancestry_refresh_loop(
    storage: Arc<Storage>,
    heavy_work: Arc<Semaphore>,
    shutdown: Arc<AtomicBool>,
) {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        match ancestry_refresh_once(storage.clone(), heavy_work.clone(), &shutdown).await {
            Ok(Some(count)) => {
                tracing::debug!(count, "release-ancestry cache refreshed")
            }
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "release-ancestry refresh failed open")
            }
        }

        // Shutdown-aware one-hour delay, matching the daemon's existing
        // ten-second polling convention.
        for _ in 0..360 {
            if shutdown.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
        }
    }
}

/// Layer 2 extraction loop: finds conversations needing V3 extraction and processes them.
async fn extraction_loop(
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    interval_secs: u64,
    shutdown: Arc<AtomicBool>,
) {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            tracing::info!("extraction loop: shutdown signal received");
            break;
        }
        if let Err(e) = extraction_loop_inner(&storage, &embeddings, &search).await {
            tracing::warn!(error = %e, "extraction loop iteration failed (non-fatal)");
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(interval_secs)).await;
    }
}

async fn ratification_loop(storage: Arc<Storage>, shutdown: Arc<AtomicBool>) {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            tracing::info!("ratification loop: shutdown signal received");
            break;
        }
        if crate::daemon::ratification::check_disabled() {
            // logged once inside check_disabled via Once
        } else if let Err(e) = ratification_loop_inner(&storage).await {
            tracing::warn!(error = %e, "ratification loop iteration failed (non-fatal)");
        }
        let secs: u64 = std::env::var("CSR_RATIFICATION_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20);
        tokio::time::sleep(tokio::time::Duration::from_secs(secs)).await;
    }
}

async fn ratification_loop_inner(storage: &Arc<Storage>) -> Result<()> {
    let unenriched = storage.get_unenriched_conversations("ratification", 1)?;
    for (conv_id, _file_path) in &unenriched {
        if let Err(e) = crate::daemon::ratification::process_ratification(storage, conv_id).await {
            let _ = storage.mark_enrichment_failed(conv_id, "ratification", &e.to_string());
        }
    }
    Ok(())
}

async fn extraction_loop_inner(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
) -> Result<()> {
    let unenriched = storage.get_unenriched_conversations("extracted_v3", 5)?;
    if unenriched.is_empty() {
        return Ok(());
    }

    tracing::info!(count = unenriched.len(), "processing V3 extraction queue");

    for (conv_id, file_path) in &unenriched {
        // Source JSONL may have been deleted or rotated out of ~/.claude/projects.
        // Mark it permanently unavailable so it stops re-queuing every tick (retry storm).
        if !Path::new(file_path).exists() {
            tracing::debug!(conv = %conv_id, %file_path, "V3 source file missing; marking unavailable");
            let _ =
                storage.mark_enrichment_unavailable(conv_id, "extracted_v3", "source file missing");
            continue;
        }
        if let Err(e) =
            process_v3_extraction(storage, embeddings, search, conv_id, Path::new(file_path)).await
        {
            tracing::warn!(conv = %conv_id, error = %e, "V3 extraction failed");
            let _ = storage.mark_enrichment_failed(conv_id, "extracted_v3", &e.to_string());
        }
    }

    Ok(())
}

/// Process V3 extraction for a single conversation.
async fn process_v3_extraction(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    conv_id: &str,
    file_path: &Path,
) -> Result<()> {
    let messages = import::parse_jsonl_messages_for_search(file_path)?;
    if messages.is_empty() {
        return Ok(());
    }

    let result = extraction::extract_v3(&messages);
    // Row-level support: the V3 index is a deterministic reduction of the whole
    // transcript, so its floor is the transcript's floor (Unknown if unreadable).
    let inputs = import::transcript_inputs_or_unknown(storage, file_path, conv_id);

    // Embed the search_index
    let search_index = result.search_index.clone();
    let emb = embeddings.clone();
    let embeddings_vec =
        tokio::task::spawn_blocking(move || emb.embed(&[search_index.as_str()])).await??;

    if let Some(embedding) = embeddings_vec.into_iter().next() {
        let reflection_id = format!("extracted_v3_{conv_id}");

        // Build rich content: search_index + signature metadata
        let sig_json = serde_json::to_string(&result.signature).unwrap_or_default();
        let content = format!(
            "{}\n\n---\nSignature: {}\nContext:\n{}",
            result.search_index, sig_json, result.context_cache
        );

        // Add project tag so project-scoped search can filter (Codex H-2 fix)
        let project = storage
            .get_project_for_conversation(conv_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| "unknown".to_string());
        let tags = vec![
            "narrative_extracted_v3".to_string(),
            format!("conv_{conv_id}"),
            format!("status_{}", result.signature.completion_status),
            format!("project_{}", project),
        ];

        // Store the V3 reflection with its support set
        storage.insert_derived_reflection(&reflection_id, &content, &tags, &embedding, &inputs)?;
        {
            let mut idx = search.write().await;
            idx.insert_reflection(reflection_id.clone(), embedding);

            // Supersede Layer 1: delete heuristic reflection if it exists
            if let Ok(Some(old_id)) = storage.get_enrichment_reflection_id(conv_id, "heuristic") {
                let _ = storage.delete_reflection(&old_id);
                idx.remove_reflection(&old_id);
            }
        } // Write lock released before context_cache embed

        storage.mark_enrichment_completed(conv_id, "extracted_v3", &reflection_id)?;
        tracing::debug!(conv = %conv_id, "Layer 2 V3 extraction complete (supersedes Layer 1)");

        // Persist context_cache as linked reflection for error recovery retrieval
        if !result.context_cache.trim().is_empty() {
            let cache_id = format!("v3_cache_{}", conv_id);
            let project = storage
                .get_project_for_conversation(conv_id)
                .ok()
                .flatten()
                .unwrap_or_else(|| "unknown".to_string());
            let cache_tags = vec![
                "context_cache".to_string(),
                "error_recovery".to_string(),
                format!("conv_{}", conv_id),
                format!("project_{}", project),
            ];
            let cache_emb = embeddings.clone();
            let cache_text = result.context_cache.clone();
            if let Ok(Ok(cache_embedding)) =
                tokio::task::spawn_blocking(move || cache_emb.embed(&[cache_text.as_str()])).await
            {
                if let Some(cache_vec) = cache_embedding.into_iter().next() {
                    let _ = storage.insert_derived_reflection(
                        &cache_id,
                        &result.context_cache,
                        &cache_tags,
                        &cache_vec,
                        &inputs,
                    );
                    {
                        let mut idx = search.write().await;
                        idx.insert_reflection(cache_id, cache_vec);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Minimum JSONL file size (50KB) for AI narrative generation.
/// Files smaller than this are trivial/subagent sessions not worth the ~$0.012/conversation cost.
const MIN_FILE_SIZE: u64 = 50_000;

/// Layer 3 narrator loop: accumulates conversations and submits batch API requests.
#[allow(clippy::too_many_arguments)]
async fn narrator_loop(
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    client: Arc<dyn BatchClient>,
    batch_size: usize,
    batch_time_secs: u64,
    poll_interval_secs: u64,
    shutdown: Arc<AtomicBool>,
) {
    let mut last_batch_time = tokio::time::Instant::now();

    loop {
        if shutdown.load(Ordering::SeqCst) {
            tracing::info!("narrator loop: shutdown signal received");
            break;
        }
        if let Err(e) = narrator_loop_inner(
            &storage,
            &embeddings,
            &search,
            &client,
            batch_size,
            batch_time_secs,
            poll_interval_secs,
            &mut last_batch_time,
            &shutdown,
        )
        .await
        {
            tracing::warn!(error = %e, "narrator loop iteration failed (non-fatal)");
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn narrator_loop_inner(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    client: &Arc<dyn BatchClient>,
    batch_size: usize,
    batch_time_secs: u64,
    poll_interval_secs: u64,
    last_batch_time: &mut tokio::time::Instant,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let unenriched = storage.get_unenriched_conversations("ai_narrative", batch_size)?;
    let time_elapsed = last_batch_time.elapsed().as_secs() >= batch_time_secs;

    // Submit batch when enough conversations accumulate or timeout
    if unenriched.len() >= batch_size || (time_elapsed && !unenriched.is_empty()) {
        tracing::info!(count = unenriched.len(), "submitting AI narrative batch");

        // Build batch requests
        let skill_prompt = load_skill_prompt();
        let mut requests = Vec::new();

        for (conv_id, file_path) in &unenriched {
            // Skip tiny files (subagent/initialization sessions) — not worth API cost
            let path = Path::new(file_path);
            if let Ok(meta) = std::fs::metadata(path) {
                if meta.len() < MIN_FILE_SIZE {
                    let _ = storage.mark_enrichment_completed(
                        conv_id,
                        "ai_narrative",
                        "skipped_too_small",
                    );
                    tracing::debug!(
                        conv = %conv_id,
                        size = meta.len(),
                        min = MIN_FILE_SIZE,
                        "skipping tiny conversation for AI narrative"
                    );
                    continue;
                }
            }

            let messages = import::parse_jsonl_messages_for_search(path)?;
            if messages.is_empty() {
                continue;
            }

            let prompt = build_narrative_prompt(&skill_prompt, &messages);
            // Freeze the support set now: the narrative is stored later from a
            // batch result, when the transcript may have changed or vanished.
            // A failed manifest write fails closed (the stored narrative reads
            // an incomplete manifest and lands Unknown).
            let inputs = import::transcript_inputs_or_unknown(storage, path, conv_id);
            if let Err(e) = storage.record_narrative_request_inputs(conv_id, &inputs) {
                tracing::warn!(conv = %conv_id, error = %e, "narrative request manifest not recorded");
            }
            requests.push(BatchRequest {
                custom_id: conv_id.clone(),
                prompt,
            });
        }

        if requests.is_empty() {
            return Ok(());
        }

        // Collect submitted IDs before requests is moved (Codex M-1)
        let submitted_ids: Vec<String> = requests.iter().map(|r| r.custom_id.clone()).collect();

        // Submit batch
        let batch_id = client.create_batch(requests).await?;
        tracing::info!(batch_id = %batch_id, "batch submitted");

        // Store prompt hash for re-enrichment detection (D-11)
        let hash = prompt_hash(&skill_prompt);
        for (conv_id, _) in &unenriched {
            if submitted_ids.contains(conv_id) {
                let _ = storage.set_batch_id(conv_id, &batch_id, &hash);
            }
        }

        // Spawn batch polling as a separate task (D-7: non-blocking)
        let poll_storage = storage.clone();
        let poll_embeddings = embeddings.clone();
        let poll_search = search.clone();
        let poll_client = client.clone();
        let poll_unenriched = unenriched.clone();
        let poll_shutdown = shutdown.clone();
        tokio::spawn(async move {
            if let Err(e) = poll_batch_results(
                &poll_storage,
                &poll_embeddings,
                &poll_search,
                &poll_client,
                &batch_id,
                &poll_unenriched,
                poll_interval_secs,
                &poll_shutdown,
            )
            .await
            {
                tracing::warn!(error = %e, "batch polling failed");
            }
        });

        *last_batch_time = tokio::time::Instant::now();
    }

    Ok(())
}

fn build_narrative_prompt(skill_prompt: &str, messages: &[serde_json::Value]) -> String {
    // Sample first 50 + last 50 messages to capture both context and resolution.
    let mut summary = String::new();
    let max_prompt_chars: usize = 100_000;
    let total = messages.len();
    let head = 50.min(total);
    let tail_start = if total > 100 { total - 50 } else { head };
    for message in messages[..head].iter().chain(messages[tail_start..].iter()) {
        let line = serde_json::to_string(message).unwrap_or_default();
        if summary.len() + line.len() > max_prompt_chars {
            break;
        }
        if !summary.is_empty() {
            summary.push('\n');
        }
        summary.push_str(&line);
    }

    // XML boundary tags separate system prompt from user data (D-4).
    let sanitized_summary = summary
        .replace("</conversation_data>", "&lt;/conversation_data&gt;")
        .replace("<conversation_data>", "&lt;conversation_data&gt;");
    format!(
        "{}\n\n---\n<conversation_data>\n{}\n</conversation_data>",
        skill_prompt, sanitized_summary
    )
}

/// Poll a submitted batch until completion, then store results. Runs as a separate task (D-7).
#[allow(clippy::too_many_arguments)]
async fn poll_batch_results(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    client: &Arc<dyn BatchClient>,
    batch_id: &str,
    unenriched: &[(String, String)],
    poll_interval_secs: u64,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            tracing::info!("batch polling: shutdown signal received");
            return Ok(());
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(poll_interval_secs)).await;
        let status = client.get_batch_status(batch_id).await?;

        if status.processing_status == "ended" {
            tracing::info!(batch_id = %batch_id, "batch completed");
            let results = client.get_batch_results(batch_id).await?;

            for item in &results {
                if let Err(e) = store_narrative(
                    storage,
                    embeddings,
                    search,
                    &item.custom_id,
                    &item.narrative,
                )
                .await
                {
                    tracing::warn!(
                        conv = %item.custom_id,
                        error = %e,
                        "failed to store narrative"
                    );
                }
            }
            return Ok(());
        } else if status.processing_status == "errored"
            || status.processing_status == "expired"
            || status.processing_status == "canceled"
        {
            tracing::warn!(
                batch_id = %batch_id,
                status = %status.processing_status,
                "batch failed"
            );
            for (conv_id, _) in unenriched {
                let _ = storage.mark_enrichment_failed(
                    conv_id,
                    "ai_narrative",
                    &format!("batch {}", status.processing_status),
                );
            }
            return Ok(());
        }
    }
}

/// Store an AI narrative as a reflection, superseding Layer 2.
async fn store_narrative(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    conv_id: &str,
    narrative: &str,
) -> Result<()> {
    let narrative_owned = narrative.to_string();
    let emb = embeddings.clone();
    let embeddings_vec =
        tokio::task::spawn_blocking(move || emb.embed(&[narrative_owned.as_str()])).await??;

    if let Some(embedding) = embeddings_vec.into_iter().next() {
        let reflection_id = format!("ai_narrative_{conv_id}");
        let tags = vec!["narrative_ai".to_string(), format!("conv_{conv_id}")];

        // Only the frozen request manifest supports this output; a missing or
        // partial manifest is Unknown, never today's re-read of the transcript.
        let inputs = storage
            .load_narrative_request_inputs(conv_id)
            .unwrap_or_else(|_| {
                crate::storage::artifact_provenance::InputEnvelope::new(vec![
                    crate::storage::artifact_provenance::ArtifactInput::unknown(
                        "narrative request manifest unavailable",
                    ),
                ])
            });
        storage.insert_derived_reflection(&reflection_id, narrative, &tags, &embedding, &inputs)?;
        let mut idx = search.write().await;
        idx.insert_reflection(reflection_id.clone(), embedding);

        // Supersede Layer 2: delete V3 extraction reflection if it exists
        if let Ok(Some(old_id)) = storage.get_enrichment_reflection_id(conv_id, "extracted_v3") {
            let _ = storage.delete_reflection(&old_id);
            idx.remove_reflection(&old_id);
        }

        storage.mark_enrichment_completed(conv_id, "ai_narrative", &reflection_id)?;
        tracing::debug!(conv = %conv_id, "Layer 3 AI narrative stored (supersedes Layer 2)");
    }

    Ok(())
}

/// Maximum size for a skill prompt override file (10KB).
const MAX_SKILL_PROMPT_SIZE: u64 = 10 * 1024;

/// Load the SKILL_V2 prompt with layered loading (per Codex F-5).
/// Checks for runtime override (capped at 10KB), falls back to compiled-in default.
fn load_skill_prompt() -> String {
    let override_path = dirs::home_dir()
        .unwrap_or_default()
        .join(".claude-self-reflect")
        .join("skill_v2.md");

    if override_path.exists() {
        // Check file size before reading (D-10: prevent oversized prompts)
        if let Ok(meta) = std::fs::metadata(&override_path) {
            if meta.len() > MAX_SKILL_PROMPT_SIZE {
                tracing::warn!(
                    path = %override_path.display(),
                    size = meta.len(),
                    max = MAX_SKILL_PROMPT_SIZE,
                    "custom SKILL prompt too large, using default"
                );
            } else if let Ok(content) = std::fs::read_to_string(&override_path) {
                tracing::info!(
                    path = %override_path.display(),
                    "using custom SKILL prompt"
                );
                return content;
            }
        }
    }

    include_str!("../../data/SKILL_V2.md").to_string()
}

/// Layer 4 consolidation loop: extracts typed facts from V3/AI narratives (Dreamer v1).
async fn consolidation_loop(
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    shutdown: Arc<AtomicBool>,
) {
    // Wait 120 seconds before first run — let extraction/narrator populate data first
    let interval = tokio::time::Duration::from_secs(120);
    loop {
        if shutdown.load(Ordering::SeqCst) {
            tracing::info!("consolidation loop: shutdown signal received");
            break;
        }
        if let Err(e) = run_consolidation(&storage, &embeddings, &search).await {
            tracing::warn!(error = %e, "consolidation loop iteration failed (non-fatal)");
        }
        tokio::time::sleep(interval).await;
    }
}

/// Inner consolidation step: process unconsolidated conversations and store extracted facts.
async fn run_consolidation(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
) -> Result<()> {
    let unconsolidated = storage.get_unconsolidated_conversations(10)?;
    if unconsolidated.is_empty() {
        return Ok(());
    }

    tracing::info!(
        count = unconsolidated.len(),
        "consolidating conversations (Dreamer v1)"
    );

    for (conv_id, narrative_content) in &unconsolidated {
        let facts = consolidation::extract_facts(narrative_content);
        if facts.is_empty() {
            // Mark as skipped (avoid reprocessing)
            storage.mark_consolidated_skipped(conv_id)?;
            continue;
        }

        // Look up project for cross-project scoping (Codex M-4)
        let project_name = storage
            .get_project_for_conversation(conv_id)
            .ok()
            .flatten()
            .unwrap_or_default();

        // Facts are a deterministic reduction of the narrative they were cut
        // from; the narrative row (preferring Layer 3, as the query does) is
        // their whole support set.
        let source_reflection = ["ai_narrative", "extracted_v3"].iter().find_map(|layer| {
            storage
                .get_enrichment_reflection_id(conv_id, layer)
                .ok()
                .flatten()
        });
        let fact_inputs = crate::storage::artifact_provenance::InputEnvelope::new(vec![
            match &source_reflection {
                Some(id) => storage.artifact_input_or_unknown(
                    crate::storage::artifact_provenance::ArtifactKind::Reflection,
                    id,
                ),
                None => crate::storage::artifact_provenance::ArtifactInput::unknown(
                    "narrative source reflection not recorded",
                ),
            },
        ]);

        // Store each fact as a tagged reflection with embedding (for HNSW search)
        let mut stored_ids = Vec::new();
        for (i, fact) in facts.iter().enumerate() {
            let reflection_id = format!("fact_{}_{conv_id}_{i}", fact.fact_type);
            let content = format!("[{}] {}", fact.fact_type, fact.content);
            let mut tags = vec![
                "consolidated_fact".to_string(),
                format!("fact_type_{}", fact.fact_type),
                format!("conv_{conv_id}"),
            ];
            if !project_name.is_empty() {
                tags.push(format!("project_{project_name}"));
            }

            // Embed the fact content for searchability
            let content_for_embed = content.clone();
            let emb = embeddings.clone();
            let embeddings_vec =
                tokio::task::spawn_blocking(move || emb.embed(&[content_for_embed.as_str()]))
                    .await??;

            if let Some(embedding) = embeddings_vec.into_iter().next() {
                storage.insert_derived_reflection(
                    &reflection_id,
                    &content,
                    &tags,
                    &embedding,
                    &fact_inputs,
                )?;
                let mut idx = search.write().await;
                idx.insert_reflection(reflection_id.clone(), embedding);
                stored_ids.push(reflection_id);
            }
        }

        // Mark conversation as consolidated, linking to the first fact reflection
        let link_id = stored_ids.first().cloned().unwrap_or_default();
        storage.mark_consolidated(conv_id, &link_id)?;
        tracing::debug!(
            conv = %&conv_id[..8.min(conv_id.len())],
            facts = facts.len(),
            "Layer 4 consolidation complete"
        );
    }

    Ok(())
}

/// Resolve the MCP-server enrichment loops' initial delay:
/// `CSR_ENRICH_INITIAL_DELAY_SECS` if it parses as a non-negative `u64`,
/// else the default 120s. Junk/unset falls back to the default rather than
/// erroring — this is read once at server startup and must never fail.
/// A value of `0` is honored (explicit opt-out of the delay).
fn resolve_enrich_initial_delay() -> std::time::Duration {
    const DEFAULT_SECS: u64 = 120;
    let secs = std::env::var("CSR_ENRICH_INITIAL_DELAY_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SECS);
    std::time::Duration::from_secs(secs)
}

/// Hold an MCP-side enrichment loop back for `delay` before its first tick.
/// Every loop `spawn_enrichment_loops` starts goes through this — extraction,
/// narration and consolidation all embed (consolidation runs before its
/// first sleep), so gating only one of them would still load the model at
/// t=0 whenever the others have queued work.
async fn delayed<F: std::future::Future<Output = ()>>(delay: std::time::Duration, fut: F) {
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    fut.await;
}

/// Spawn enrichment loops as background tokio tasks (for embedding in MCP server).
/// Returns join handles that can be aborted on shutdown. Does NOT acquire the daemon lockfile
/// (so the standalone `csr-engine daemon` can still run alongside if needed).
///
/// All three loops wait `CSR_ENRICH_INITIAL_DELAY_SECS` (default 120s)
/// before their first tick: this function only runs inside `csr-engine serve`
/// (src/engine.rs `serve_mcp`), where an MCP session that never calls a tool
/// would otherwise start embedding at t=0 with no user request behind it.
/// The standalone `csr-engine daemon` (`Daemon::run` above) spawns its loops
/// directly and keeps its immediate-start behavior.
pub fn spawn_enrichment_loops(
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let initial_delay = resolve_enrich_initial_delay();

    // Layer 2: V3 extraction (free, runs every 60s — less aggressive than standalone daemon)
    let ext_handle = {
        let s = storage.clone();
        let e = embeddings.clone();
        let idx = search.clone();
        let sd = shutdown.clone();
        tokio::spawn(delayed(initial_delay, extraction_loop(s, e, idx, 60, sd)))
    };

    // Layer 3: AI narrative (only if API key set)
    let narrator_handle = if AnthropicClient::from_env().is_some() {
        let client = Arc::new(AnthropicClient::from_env().unwrap());
        let s = storage.clone();
        let e = embeddings.clone();
        let idx = search.clone();
        let sd = shutdown.clone();
        Some(tokio::spawn(delayed(
            initial_delay,
            narrator_loop(s, e, idx, client, 10, 1800, 60, sd),
        )))
    } else {
        None
    };

    // Layer 4: Dreamer consolidation (free, runs every 120s)
    let consol_handle = {
        let s = storage.clone();
        let e = embeddings.clone();
        let idx = search.clone();
        let sd = shutdown;
        tokio::spawn(delayed(initial_delay, consolidation_loop(s, e, idx, sd)))
    };

    let mut handles = vec![ext_handle, consol_handle];
    if let Some(h) = narrator_handle {
        handles.push(h);
    }
    handles
}

/// Compute SHA-256 hash of prompt content (for re-enrichment detection).
pub fn prompt_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_registry_kill_switch_env() {
        // Unique to this module — no cross-module env races — so no shared
        // mutex. Keep all cases in one test so cargo's default parallel
        // runners cannot interleave set_var/remove_var on this var.
        std::env::remove_var("CSR_NO_MEMORY_REGISTRY");
        assert!(!memory_registry_disabled());
        std::env::set_var("CSR_NO_MEMORY_REGISTRY", "1");
        assert!(memory_registry_disabled());
        std::env::set_var("CSR_NO_MEMORY_REGISTRY", "true");
        assert!(memory_registry_disabled());
        std::env::set_var("CSR_NO_MEMORY_REGISTRY", "TRUE");
        assert!(memory_registry_disabled());
        std::env::set_var("CSR_NO_MEMORY_REGISTRY", "0");
        assert!(!memory_registry_disabled());
        std::env::remove_var("CSR_NO_MEMORY_REGISTRY");
    }

    #[test]
    fn enrich_initial_delay_env() {
        // Unique to this module — no shared mutex needed, same rationale as
        // memory_registry_kill_switch_env above.
        std::env::remove_var("CSR_ENRICH_INITIAL_DELAY_SECS");
        assert_eq!(
            resolve_enrich_initial_delay(),
            std::time::Duration::from_secs(120)
        );
        std::env::set_var("CSR_ENRICH_INITIAL_DELAY_SECS", "5");
        assert_eq!(
            resolve_enrich_initial_delay(),
            std::time::Duration::from_secs(5)
        );
        // 0 is a valid, explicit opt-out — must be honored, not treated as unset.
        std::env::set_var("CSR_ENRICH_INITIAL_DELAY_SECS", "0");
        assert_eq!(resolve_enrich_initial_delay(), std::time::Duration::ZERO);
        std::env::set_var("CSR_ENRICH_INITIAL_DELAY_SECS", "not-a-number");
        assert_eq!(
            resolve_enrich_initial_delay(),
            std::time::Duration::from_secs(120)
        );
        std::env::remove_var("CSR_ENRICH_INITIAL_DELAY_SECS");
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_holds_the_loop_body_until_the_delay_elapses() {
        use std::sync::Mutex;
        let start = tokio::time::Instant::now();
        let ran_at: Arc<Mutex<Option<tokio::time::Instant>>> = Arc::new(Mutex::new(None));
        let slot = ran_at.clone();
        let handle = tokio::spawn(delayed(std::time::Duration::from_secs(120), async move {
            *slot.lock().unwrap() = Some(tokio::time::Instant::now());
        }));
        // Let the spawned wrapper poll once so its sleep timer is registered
        // before the clock moves; otherwise advance() has nothing to fire.
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_secs(119)).await;
        tokio::task::yield_now().await;
        assert!(
            ran_at.lock().unwrap().is_none(),
            "body ran before the delay elapsed"
        );
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        let at = ran_at
            .lock()
            .unwrap()
            .expect("body never ran after the delay");
        let elapsed = at.duration_since(start);
        assert!(
            elapsed >= std::time::Duration::from_secs(120)
                && elapsed <= std::time::Duration::from_secs(121),
            "body ran at {elapsed:?}, expected ~120s"
        );
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_aborted_before_the_deadline_never_runs_the_body() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        let handle = tokio::spawn(delayed(std::time::Duration::from_secs(120), async move {
            flag.store(true, Ordering::SeqCst);
        }));
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        // serve_mcp aborts the enrichment handles on shutdown; an abort
        // during the delay must drop the pending loop, not run it later.
        handle.abort();
        assert!(handle.await.unwrap_err().is_cancelled());
        tokio::time::advance(std::time::Duration::from_secs(120)).await;
        tokio::task::yield_now().await;
        assert!(!ran.load(Ordering::SeqCst), "aborted body still ran");
    }

    #[tokio::test]
    async fn delayed_with_zero_delay_runs_immediately() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        delayed(std::time::Duration::ZERO, async move {
            flag.store(true, Ordering::SeqCst);
        })
        .await;
        assert!(ran.load(Ordering::SeqCst));
    }

    #[test]
    fn provenance_backfill_kill_switch_env() {
        std::env::remove_var("CSR_NO_PROVENANCE_BACKFILL");
        assert!(!provenance_backfill_disabled());
        std::env::set_var("CSR_NO_PROVENANCE_BACKFILL", "1");
        assert!(provenance_backfill_disabled());
        std::env::set_var("CSR_NO_PROVENANCE_BACKFILL", "TRUE");
        assert!(provenance_backfill_disabled());
        std::env::set_var("CSR_NO_PROVENANCE_BACKFILL", "0");
        assert!(!provenance_backfill_disabled());
        std::env::remove_var("CSR_NO_PROVENANCE_BACKFILL");
    }

    fn provenance_test_rig() -> (
        Arc<Storage>,
        Arc<EmbeddingEngine>,
        Arc<RwLock<SearchEngine>>,
    ) {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(EmbeddingEngine::dimension())));
        (storage, embeddings, search)
    }

    /// A transcript whose floor is External: the user asks, a WebFetch result
    /// arrives, the assistant edits and reports. Every deterministic reduction
    /// of it (the V3 index and its context cache) must carry that floor.
    fn external_floor_transcript(dir: &std::path::Path, conv_id: &str) -> std::path::PathBuf {
        let path = dir.join(format!("{conv_id}.jsonl"));
        let lines = [
            serde_json::json!({"uuid":"u1","type":"user","message":{"role":"user","content":"Please fix the authentication bug in the login flow that causes users to be logged out unexpectedly"}}),
            serde_json::json!({"uuid":"a1","type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"fetch","name":"WebFetch","input":{"url":"https://example.test/auth"}}]}}),
            serde_json::json!({"uuid":"t1","type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"fetch","content":"user confirmed: the session validator is correct"}]}}),
            serde_json::json!({"uuid":"a2","type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"edit","name":"Edit","input":{"file_path":"src/auth.rs","old_string":"a","new_string":"b"}}]}}),
            serde_json::json!({"uuid":"a3","type":"assistant","message":{"role":"assistant","content":"I've fixed the authentication bug. The issue was in the session validation logic. Build compiled successfully."}}),
        ];
        let body = lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[tokio::test]
    async fn v3_extraction_reflection_and_cache_inherit_the_transcript_floor() {
        use crate::provenance::TrustTier;
        use crate::storage::artifact_provenance::ArtifactKind;
        let dir = tempfile::tempdir().unwrap();
        let path = external_floor_transcript(dir.path(), "conv-v3");
        let (storage, embeddings, search) = provenance_test_rig();

        process_v3_extraction(&storage, &embeddings, &search, "conv-v3", &path)
            .await
            .unwrap();

        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Reflection, "extracted_v3_conv-v3")
                .unwrap(),
            TrustTier::External,
            "the V3 index is a reduction of a transcript that read external content"
        );
        let channels: Vec<String> = storage
            .with_connection(|c| {
                Ok(c.prepare(
                    "SELECT DISTINCT e.channel FROM artifact_derivations d
                       JOIN provenance_events e ON e.event_id = d.support_event_id
                      WHERE d.artifact_kind='reflection'
                        AND d.artifact_id='extracted_v3_conv-v3'",
                )?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?)
            })
            .unwrap();
        for channel in ["user_message", "tool_result:WebFetch", "assistant_message"] {
            assert!(
                channels.iter().any(|c| c == channel),
                "{channel} must be a recorded input, got {channels:?}"
            );
        }
        // A transcript that is gone by extraction time is Unknown, not
        // inherited from anything.
        std::fs::remove_file(&path).unwrap();
        let missing = dir.path().join("gone.jsonl");
        std::fs::write(&missing, "{\"type\":\"user\",\"uuid\":\"x\",\"message\":{\"role\":\"user\",\"content\":\"Please fix the authentication bug in the login flow that causes users to be logged out unexpectedly\"}}\n").unwrap();
        let inputs = import::transcript_inputs_or_unknown(&storage, &path, "conv-v3");
        assert_eq!(inputs.floor(), TrustTier::Unknown);
    }

    #[tokio::test]
    async fn stored_narrative_floor_comes_only_from_the_frozen_request_manifest() {
        use crate::provenance::TrustTier;
        use crate::storage::artifact_provenance::ArtifactKind;
        let dir = tempfile::tempdir().unwrap();
        let path = external_floor_transcript(dir.path(), "conv-nar");
        let (storage, embeddings, search) = provenance_test_rig();

        // Submit time: the manifest is frozen from the transcript as it was.
        let inputs = import::transcript_inputs_or_unknown(&storage, &path, "conv-nar");
        assert_eq!(inputs.floor(), TrustTier::External);
        storage
            .record_narrative_request_inputs("conv-nar", &inputs)
            .unwrap();
        // The transcript vanishes before the batch result lands.
        std::fs::remove_file(&path).unwrap();

        store_narrative(&storage, &embeddings, &search, "conv-nar", "A narrative.")
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Reflection, "ai_narrative_conv-nar")
                .unwrap(),
            TrustTier::External
        );

        // No manifest at all: Unknown, never a fresh read of anything.
        store_narrative(&storage, &embeddings, &search, "conv-none", "Another.")
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_artifact_min_trust(ArtifactKind::Reflection, "ai_narrative_conv-none")
                .unwrap(),
            TrustTier::Unknown
        );
    }

    #[tokio::test]
    async fn consolidated_facts_inherit_their_narrative_floor_and_tags_cannot_raise_it() {
        use crate::provenance::TrustTier;
        use crate::storage::artifact_provenance::{ArtifactInput, ArtifactKind, InputEnvelope};
        let (storage, embeddings, search) = provenance_test_rig();
        let narrative = "## Solution Pattern\nWe decided to use the iterator parser instead of the callback parser because re-entrancy dropped frames under load.\n";
        let external = InputEnvelope::new(vec![ArtifactInput::unknown("external context")]);
        let vector = vec![0.0; EmbeddingEngine::dimension()];
        storage
            .insert_derived_reflection(
                "ai_narrative_conv-c",
                narrative,
                &["narrative_ai".into(), "conv_conv-c".into()],
                &vector,
                &external,
            )
            .unwrap();
        storage
            .mark_enrichment_completed("conv-c", "ai_narrative", "ai_narrative_conv-c")
            .unwrap();
        // The narrative itself is Unknown-floored here (unobserved input), so
        // every fact cut from it must be Unknown too, whatever its tags say.
        run_consolidation(&storage, &embeddings, &search)
            .await
            .unwrap();
        let facts: Vec<(String, i64)> = storage
            .with_connection(|c| {
                Ok(c.prepare(
                    "SELECT id, min_trust FROM reflections WHERE tags LIKE '%consolidated_fact%'",
                )?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?)
            })
            .unwrap();
        assert!(!facts.is_empty(), "the fixture narrative must yield a fact");
        for (id, tier) in &facts {
            assert_eq!(TrustTier::from_db(Some(*tier)), TrustTier::Unknown, "{id}");
            assert_eq!(
                storage
                    .get_artifact_min_trust(ArtifactKind::Reflection, id)
                    .unwrap(),
                TrustTier::Unknown
            );
        }
        let edges: i64 = storage
            .with_connection(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM artifact_derivations d
                       JOIN provenance_events e ON e.event_id = d.support_event_id
                      WHERE d.artifact_kind='reflection' AND d.artifact_id=?1
                        AND e.channel='derived_artifact:reflection'",
                    [&facts[0].0],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(
            edges, 1,
            "a fact records its narrative row as its one input"
        );
    }

    #[test]
    fn test_prompt_hash_deterministic() {
        let h1 = prompt_hash("hello world");
        let h2 = prompt_hash("hello world");
        let h3 = prompt_hash("different");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_load_skill_prompt_fallback() {
        // Should always succeed with compiled-in default
        let prompt = load_skill_prompt();
        assert!(!prompt.is_empty());
    }

    #[test]
    fn test_daemon_config_defaults() {
        let config = DaemonConfig::default();
        assert_eq!(config.extraction_interval_secs, 30);
        assert_eq!(config.batch_size_trigger, 10);
        assert_eq!(config.batch_time_trigger_secs, 1800);
        assert_eq!(config.batch_poll_interval_secs, 60);
    }

    #[tokio::test]
    async fn ancestry_refresh_abstains_when_shutdown_arrives_while_waiting_for_permit() {
        let heavy_work = Arc::new(Semaphore::new(1));
        let held_permit = heavy_work.clone().acquire_owned().await.unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let waiter = {
            let heavy_work = heavy_work.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                acquire_heavy_work_unless_shutdown(heavy_work, &shutdown)
                    .await
                    .is_some()
            })
        };

        tokio::task::yield_now().await;
        shutdown.store(true, Ordering::SeqCst);
        drop(held_permit);

        assert!(
            !waiter.await.unwrap(),
            "a refresh unblocked after shutdown must release the permit without mutating the cache"
        );
    }

    #[tokio::test]
    async fn ancestry_refresh_join_error_invalidates_cache() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO conversation_ancestry_cache
                     (conversation_id, state, release_tag, releases_behind,
                      repository, refreshed_at)
                     VALUES ('stale', 'shipped', 'v1.0.0', 5, '/repo', ?1)",
                    [chrono::Utc::now().to_rfc3339()],
                )?;
                Ok(())
            })
            .unwrap();
        let panicked = tokio::task::spawn_blocking(|| -> anyhow::Result<usize> {
            panic!("simulated ancestry worker panic")
        })
        .await;

        assert!(finish_ancestry_refresh(&storage, panicked).is_err());
        assert_eq!(storage.ancestry_cache_count().unwrap(), 0);
    }

    #[test]
    fn test_conversation_sampling_short() {
        // For <= 100 messages, all should be included (no gap)
        let messages: Vec<i32> = (0..80).collect();
        let total = messages.len();
        let head = 50.min(total);
        let tail_start = if total > 100 { total - 50 } else { head };
        let sampled: Vec<_> = messages[..head]
            .iter()
            .chain(messages[tail_start..].iter())
            .collect();
        assert_eq!(sampled.len(), 80); // all messages included
    }

    #[test]
    fn test_conversation_sampling_long() {
        // For > 100 messages, should get first 50 + last 50
        let messages: Vec<i32> = (0..200).collect();
        let total = messages.len();
        let head = 50.min(total);
        let tail_start = if total > 100 { total - 50 } else { head };
        let sampled: Vec<_> = messages[..head]
            .iter()
            .chain(messages[tail_start..].iter())
            .collect();
        assert_eq!(sampled.len(), 100);
        assert_eq!(*sampled[0], 0);
        assert_eq!(*sampled[49], 49);
        assert_eq!(*sampled[50], 150); // first of the tail
        assert_eq!(*sampled[99], 199);
    }

    #[test]
    fn narrative_prompt_excludes_csr_material_without_losing_neighbors() {
        let messages = vec![
            serde_json::json!({
                "type": "assistant",
                "message": {"content": [
                    {
                        "type": "tool_use",
                        "id": "csr-call",
                        "name": "csr_reflect_on_past",
                        "input": {"query": "CSR QUERY MUST DISAPPEAR"}
                    },
                    {
                        "type": "tool_use",
                        "id": "read-call",
                        "name": "Read",
                        "input": {"file_path": "/repo/src/kept.rs"}
                    }
                ]}
            }),
            serde_json::json!({
                "type": "user",
                "message": {"content": [
                    {
                        "type": "text",
                        "text": "USER PROSE BEFORE <system-reminder>CSR ENDLESS MEMORY ACTIVE — CSR WRAPPER MUST DISAPPEAR</system-reminder> USER PROSE AFTER"
                    },
                    {
                        "type": "tool_result",
                        "tool_use_id": "csr-call",
                        "content": "CSR RESULT MUST DISAPPEAR"
                    },
                    {
                        "type": "tool_result",
                        "tool_use_id": "read-call",
                        "content": "SIBLING RESULT MUST STAY"
                    }
                ]}
            }),
        ];

        let (sanitized, _) = import::sanitize_messages_for_search(&messages);
        let prompt = build_narrative_prompt("NARRATIVE SKILL", &sanitized);

        for forbidden in [
            "CSR QUERY MUST DISAPPEAR",
            "CSR RESULT MUST DISAPPEAR",
            "CSR WRAPPER MUST DISAPPEAR",
        ] {
            assert!(!prompt.contains(forbidden));
        }
        for retained in [
            "USER PROSE BEFORE",
            "USER PROSE AFTER",
            "kept.rs",
            "SIBLING RESULT MUST STAY",
        ] {
            assert!(
                prompt.contains(retained),
                "narrative prompt lost {retained}"
            );
        }
    }
}
