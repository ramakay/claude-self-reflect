use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::sync::{OwnedSemaphorePermit, RwLock, Semaphore};

use crate::embeddings::EmbeddingEngine;
use crate::import;
use crate::import::ConversationChunk;
use crate::search::SearchEngine;
use crate::storage::Storage;

/// Debounce interval for collecting file changes before processing.
const DEBOUNCE_SECS: u64 = 5;

/// Batch size for embedding during auto-import.
const BATCH_SIZE: usize = 10;

/// Minimum wall-clock gap between HNSW dumps in the watcher.
///
/// The watcher used to dump the whole index after every debounced batch. On a
/// live corpus that is ~270 dumps/hour of ~475MB data + ~167MB graph + ~15MB
/// manifest, roughly 3GB/min of writes, and it held the daemon near 30% CPU
/// indefinitely.
///
/// Deferring is safe because the dump is pure cache. `import_file` writes each
/// embedding to SQLite *before* inserting it into the in-memory index, and
/// `Engine::new` additively backfills any id present in the DB but missing from
/// the on-disk cache, in bounded batches, without a full rebuild. So a deferred
/// dump costs a slightly longer startup backfill, never data. This is the same
/// durability contract `precompact` and `session-end` already rely on.
const MIN_FLUSH_INTERVAL_SECS: u64 = 300;

/// Whether a dirty index is due to be dumped, given the last dump time.
///
/// `None` means nothing has been dumped yet this run, so dump immediately —
/// a fresh watcher should not sit on its first batch for five minutes.
fn flush_is_due(last_flush: Option<Instant>, now: Instant) -> bool {
    match last_flush {
        None => true,
        Some(last) => now.duration_since(last) >= Duration::from_secs(MIN_FLUSH_INTERVAL_SECS),
    }
}

/// The daemon's dump budget: a generation rewrite costs ~1.55 GB at the live
/// corpus size, and at `MIN_FLUSH_INTERVAL_SECS` that was ~20 GB/h for a
/// trickle of new chunks. The daemon dumps only when at least this long has
/// passed since its last dump...
const BUDGET_MIN_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// ...and the generation on disk is missing at least this many vector
/// mutations, or [`BUDGET_MIN_FRACTION`] of the index if that is fewer —
/// plus once on clean shutdown (`Daemon::run`). Skipping a dump loses
/// nothing: every vector is in SQLite first, any loader backfills ids the
/// generation lacks, and the daemon's index sync replays updates after the
/// generation's change cursor.
const BUDGET_MIN_NEW: u64 = 2_000;
const BUDGET_MIN_FRACTION: f64 = 0.005;

/// When a watcher rewrites the index generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlushPolicy {
    /// At most once per `MIN_FLUSH_INTERVAL_SECS`, the first batch at once:
    /// a foreground `--watch` importer.
    Interval,
    /// The daemon: [`BUDGET_MIN_INTERVAL`] and [`budget_threshold`] both met.
    Budget,
}

/// Unpublished mutations that justify rewriting an index of `slots` points.
fn budget_threshold(slots: usize) -> u64 {
    let fraction = (slots as f64 * BUDGET_MIN_FRACTION).ceil() as u64;
    fraction.clamp(1, BUDGET_MIN_NEW)
}

/// Whether the daemon's budget allows a dump now.
fn budget_flush_due(last_flush: Instant, now: Instant, unpublished: u64, slots: usize) -> bool {
    now.duration_since(last_flush) >= BUDGET_MIN_INTERVAL && unpublished >= budget_threshold(slots)
}

/// Backoff schedule (seconds) for a file whose import keeps failing with
/// `database is locked`/`database is busy`. Index is `attempts - 1`; once
/// `attempts` exceeds this schedule's length the file is dropped with a WARN
/// instead of retried again — a session's final append must eventually be
/// declared lost rather than retried forever against a database another
/// process holds indefinitely.
const BUSY_RETRY_BACKOFF_SECS: [u64; 5] = [5, 15, 45, 120, 300];

/// Meta counter keys surfaced by `csr-engine status`. See `Storage::bump_meta_counter_by`.
pub(crate) const BUSY_RETRIES_COUNTER: &str = "watcher_import_busy_retries";
pub(crate) const BUSY_GAVE_UP_COUNTER: &str = "watcher_import_busy_gave_up";

/// Per-file retry bookkeeping for the watcher's busy-import backoff.
///
/// `next_at` deliberately uses `tokio::time::Instant`, not `std::time::Instant`
/// (unlike `last_flush`/`flush_is_due` elsewhere in this file): it is compared
/// against and slept on via `tokio::time::sleep_until` in the same select loop,
/// and using tokio's clock end-to-end is what lets `tokio::time::pause` +
/// `advance` drive both the sleep and the due-check deterministically in
/// tests, instead of racing a paused virtual clock against the real OS clock.
#[derive(Debug, Clone)]
pub(crate) struct RetryState {
    /// Number of busy failures seen for this path so far.
    attempts: u32,
    /// When this path becomes due for its next retry attempt.
    next_at: tokio::time::Instant,
}

/// Outcome of one `process_batch` pass, for the caller to persist counters
/// and log — kept structured rather than side-effecting so the function stays
/// unit-testable without a `Storage`.
#[derive(Debug, Default)]
pub(crate) struct BatchOutcome {
    /// Paths that imported successfully this pass (including ones that had
    /// previously been retried).
    pub imported: Vec<PathBuf>,
    /// Busy-failure count observed this pass (may exceed `imported.len()` +
    /// `gave_up.len()` since a path can fail more than once across the whole
    /// retry lifetime, just not within a single pass).
    pub busy_retries: u64,
    /// Paths that exhausted `BUSY_RETRY_BACKOFF_SECS` and were dropped.
    pub gave_up: Vec<PathBuf>,
}

/// Import every path in `paths` via `import`, applying the busy-retry
/// backoff policy to `retry_map`. A path that fails with
/// `scrub::is_database_busy` is requeued with exponential backoff (see
/// `BUSY_RETRY_BACKOFF_SECS`) instead of being dropped on first failure — the
/// bug this closes: a sidechain's last append hitting `SQLITE_BUSY` used to
/// be gone for good, because no later notify event would ever re-trigger it.
/// Any other error keeps the previous log-and-drop behaviour.
///
/// `import` is injected so this is testable without spinning up a real
/// `Storage`/`EmbeddingEngine`/`SearchEngine` — the production caller passes
/// `|path| self.import_file(&path)`.
pub(crate) async fn process_batch<F, Fut>(
    paths: impl IntoIterator<Item = PathBuf>,
    mut import: F,
    retry_map: &mut HashMap<PathBuf, RetryState>,
) -> BatchOutcome
where
    F: FnMut(PathBuf) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let mut outcome = BatchOutcome::default();
    for path in paths {
        match import(path.clone()).await {
            Ok(()) => {
                retry_map.remove(&path);
                outcome.imported.push(path);
            }
            Err(e) if crate::import::scrub::is_database_busy(&e) => {
                outcome.busy_retries += 1;
                let entry = retry_map.entry(path.clone()).or_insert(RetryState {
                    attempts: 0,
                    next_at: tokio::time::Instant::now(),
                });
                entry.attempts += 1;
                if entry.attempts as usize > BUSY_RETRY_BACKOFF_SECS.len() {
                    tracing::warn!(
                        file = %path.display(),
                        attempts = entry.attempts,
                        "giving up on busy import after max retries"
                    );
                    retry_map.remove(&path);
                    outcome.gave_up.push(path);
                } else {
                    let backoff = BUSY_RETRY_BACKOFF_SECS[entry.attempts as usize - 1];
                    entry.next_at = tokio::time::Instant::now() + Duration::from_secs(backoff);
                    tracing::debug!(
                        file = %path.display(),
                        attempts = entry.attempts,
                        backoff_secs = backoff,
                        "requeueing import after database busy"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    file = %path.display(),
                    error = %e,
                    "failed to import file"
                );
                retry_map.remove(&path);
            }
        }
    }
    outcome
}

/// The earliest `next_at` across every path still pending retry, or `None`
/// when nothing is queued — the watcher loop's `tokio::select!` sleeps until
/// this so a due retry runs even without a fresh notify event.
fn earliest_retry(retry_map: &HashMap<PathBuf, RetryState>) -> Option<tokio::time::Instant> {
    retry_map.values().map(|state| state.next_at).min()
}

/// Sleep until `deadline`, or block forever when `None` — lets the watcher's
/// `tokio::select!` treat "no retries pending" and "waiting for a real
/// retry" uniformly against the same branch.
async fn sleep_until_or_pending(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

/// Waits for either a fresh notify event (then collects more within the
/// `DEBOUNCE_SECS` window) or the earliest due busy-retry deadline, then
/// merges any `retry_map` paths whose `next_at` has elapsed into the
/// returned batch — including when no fresh notify event arrived at all.
///
/// This merge-on-wake is the mechanism that lets a busy retry fire without a
/// new file write (the gap-G1 bug: a session's last append hitting
/// `SQLITE_BUSY` used to be gone for good, because nothing else would ever
/// touch that file again to re-trigger the notify path). Factored out of
/// `run` so it is unit-testable under `tokio::time::pause` without a real
/// `notify::Watcher` — see `wake_on_retry_without_new_notify_event` below.
///
/// Returns `None` when the notify channel has closed (the caller should
/// stop the watcher loop).
async fn wait_for_batch(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<PathBuf>,
    retry_map: &HashMap<PathBuf, RetryState>,
) -> Option<HashSet<PathBuf>> {
    let mut pending: HashSet<PathBuf> = HashSet::new();

    // Wait for the first event OR the earliest due busy-retry — a retry
    // must run even when no new notify event ever arrives.
    let retry_deadline = earliest_retry(retry_map);
    tokio::select! {
        received = rx.recv() => {
            match received {
                Some(path) => {
                    pending.insert(path);

                    // Collect more events within the debounce window
                    let deadline = tokio::time::Instant::now()
                        + tokio::time::Duration::from_secs(DEBOUNCE_SECS);
                    loop {
                        let timeout = tokio::time::timeout_at(deadline, rx.recv());
                        match timeout.await {
                            Ok(Some(path)) => {
                                pending.insert(path);
                            }
                            Ok(None) => {
                                // Channel closed
                                break;
                            }
                            Err(_) => {
                                // Timeout — debounce window expired
                                break;
                            }
                        }
                    }
                }
                None => {
                    tracing::info!("file watcher channel closed, stopping");
                    return None;
                }
            }
        }
        _ = sleep_until_or_pending(retry_deadline) => {
            // Woken purely to service a due retry — merged in below.
        }
    }

    // Merge in any busy-retry paths that are due now, whether or not a
    // fresh event also arrived this iteration.
    let now_instant = tokio::time::Instant::now();
    for (path, state) in retry_map {
        if state.next_at <= now_instant {
            pending.insert(path.clone());
        }
    }

    Some(pending)
}

/// Watches the Claude projects directory for new JSONL files and auto-imports them.
pub struct FileWatcher {
    projects_dir: PathBuf,
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    index_dir: PathBuf,
    /// Optional shared one-owner permit, held while importing a debounced
    /// batch of files. `None` for callers
    /// that don't need the signal (e.g. the MCP-embedded watcher started by
    /// `Engine::start_watcher`) — opt-in via [`Self::with_heavy_work_permit`] so
    /// this stays additive for every existing caller.
    heavy_work: Option<Arc<Semaphore>>,
    flush_policy: FlushPolicy,
}

impl FileWatcher {
    pub fn new(
        projects_dir: PathBuf,
        storage: Arc<Storage>,
        embeddings: Arc<EmbeddingEngine>,
        search: Arc<RwLock<SearchEngine>>,
        index_dir: PathBuf,
    ) -> Self {
        Self {
            projects_dir,
            storage,
            embeddings,
            search,
            index_dir,
            heavy_work: None,
            flush_policy: FlushPolicy::Interval,
        }
    }

    /// Dump under the daemon's budget ([`FlushPolicy::Budget`]) instead of
    /// every `MIN_FLUSH_INTERVAL_SECS`.
    pub fn with_flush_budget(mut self) -> Self {
        self.flush_policy = FlushPolicy::Budget;
        self
    }

    /// Share the daemon's exclusive heavy-work semaphore. The watcher waits
    /// for and retains an owned RAII permit across import and index flush.
    pub fn with_heavy_work_permit(mut self, permit: Arc<Semaphore>) -> Self {
        self.heavy_work = Some(permit);
        self
    }

    /// Spawn the watcher as a background tokio task. Returns a JoinHandle.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = self.run().await {
                tracing::error!(error = %e, "file watcher stopped with error");
            }
        })
    }

    /// Main watcher loop: watches for new JSONL files, debounces, and imports.
    async fn run(&self) -> Result<()> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();

        // Create the notify watcher — sends events via a closure that bridges to tokio channel
        let sender = tx.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                match event.kind {
                    EventKind::Create(_) | EventKind::Modify(_) => {
                        for path in event.paths {
                            if path.extension().is_some_and(|ext| ext == "jsonl") {
                                let _ = sender.send(path);
                            }
                        }
                    }
                    _ => {}
                }
            }
        })?;

        // Watch the projects directory recursively
        watcher.watch(&self.projects_dir, RecursiveMode::Recursive)?;
        tracing::info!(dir = %self.projects_dir.display(), "file watcher started");

        // Debounce loop: collect changed files over DEBOUNCE_SECS, then process.
        // Under the budget the generation just loaded counts as a dump: the
        // first one waits for the interval and the unpublished threshold.
        let mut last_flush: Option<Instant> = match self.flush_policy {
            FlushPolicy::Interval => None,
            FlushPolicy::Budget => Some(Instant::now()),
        };
        let mut retry_map: HashMap<PathBuf, RetryState> = HashMap::new();
        loop {
            let Some(pending) = wait_for_batch(&mut rx, &retry_map).await else {
                break;
            };

            // Process all pending files
            if !pending.is_empty() {
                // A maintenance run holds the database: the batch waits for it.
                if let Some(data_dir) = self.index_dir.parent() {
                    crate::maintenance::sleep_while_active(data_dir).await;
                }
                let _heavy_permit = match &self.heavy_work {
                    Some(heavy_work) => Some(acquire_heavy_work_permit(heavy_work.clone()).await),
                    None => None,
                };
                tracing::info!(count = pending.len(), "processing new/modified JSONL files");
                let outcome = process_batch(
                    pending.iter().cloned(),
                    |path| async move { self.import_file(&path).await },
                    &mut retry_map,
                )
                .await;
                if outcome.busy_retries > 0 {
                    if let Err(e) = self
                        .storage
                        .bump_meta_counter_by(BUSY_RETRIES_COUNTER, outcome.busy_retries)
                    {
                        tracing::warn!(error = %e, "failed to persist watcher busy-retry counter");
                    }
                }
                if !outcome.gave_up.is_empty() {
                    if let Err(e) = self
                        .storage
                        .bump_meta_counter_by(BUSY_GAVE_UP_COUNTER, outcome.gave_up.len() as u64)
                    {
                        tracing::warn!(error = %e, "failed to persist watcher busy-gave-up counter");
                    }
                }

                // See the consts for why deferring the dump is safe.
                let now = Instant::now();
                let due = match self.flush_policy {
                    FlushPolicy::Interval => flush_is_due(last_flush, now),
                    FlushPolicy::Budget => {
                        let index = self.search.read().await;
                        budget_flush_due(
                            last_flush.unwrap_or(now),
                            now,
                            index.unpublished_changes(),
                            index.chunk_slot_count() + index.reflection_slot_count(),
                        )
                    }
                };
                if due {
                    match self.flush_index().await {
                        None => {}
                        Some(result) => {
                            if let Err(e) = result {
                                tracing::warn!(
                                    error = %e,
                                    "failed to flush HNSW index after watcher batch"
                                );
                            }
                            // Recorded even when the dump failed: the index stays
                            // dirty and will retry at the next interval, but a
                            // failing dump must not reinstate the per-batch storm.
                            last_flush = Some(now);
                        }
                    }
                }
            }
        }

        // Channel closed. Dump once on the way out so a deferred batch is not
        // left for the next startup to backfill.
        if let Some(Err(e)) = self.flush_index().await {
            tracing::warn!(error = %e, "failed to flush HNSW index on watcher shutdown");
        }

        Ok(())
    }

    /// [`flush_index_blocking`] on the blocking pool. `None`: nothing was dirty.
    /// Under the daemon's budget a successful dump is followed by
    /// [`crate::engine::remap_published_generation`], still inside the
    /// caller's heavy-work permit: the daemon then serves the vectors it
    /// just wrote from the mapped file instead of keeping heap copies.
    async fn flush_index(&self) -> Option<Result<()>> {
        let storage = Arc::clone(&self.storage);
        let search = Arc::clone(&self.search);
        let index_dir = self.index_dir.clone();
        let remap = self.flush_policy == FlushPolicy::Budget;
        tokio::task::spawn_blocking(move || {
            let flushed = flush_index_blocking(&storage, &search, &index_dir, false);
            if remap && matches!(flushed, Some(Ok(()))) {
                if let Err(e) =
                    crate::engine::remap_published_generation(&storage, &search, &index_dir)
                {
                    tracing::warn!(error = %e, "index remap after dump failed (non-fatal)");
                }
            }
            flushed
        })
        .await
        .unwrap_or_else(|e| Some(Err(anyhow::anyhow!("index flush task failed: {e}"))))
    }

    /// Import a single JSONL file: parse → embed → store → index.
    ///
    /// Thin wrapper over [`import_file_content_diff`] — see that function's
    /// docs for why the diff shape matters (gap-G1 high finding: the
    /// catch-up sweep must use this exact path, not `Engine::import_file`'s
    /// count-incremental one, or a busy-dropped final append that only grows
    /// the last chunk gets stamped as reimported without ever being
    /// re-embedded).
    async fn import_file(&self, file_path: &Path) -> Result<()> {
        import_file_content_diff_off_worker(
            Arc::clone(&self.storage),
            Arc::clone(&self.embeddings),
            Arc::clone(&self.search),
            self.projects_dir.clone(),
            file_path.to_path_buf(),
        )
        .await
    }
}

/// [`import_file_content_diff`] on the blocking pool instead of an async
/// worker. The import parses the whole transcript (a 104 MB session is
/// re-parsed after every turn) and writes SQLite synchronously between its
/// awaits; on an async worker that stalled every task queued behind it, the
/// prompt socket's accept loop included (sampled live 2026-10-11: no
/// connection accepted for >5 s while a worker sat in
/// `parse_jsonl_file_with_stats_and_parent`). Errors come back unchanged, so
/// the callers' busy-retry, gave-up and deferral handling sees what it saw.
pub(crate) async fn import_file_content_diff_off_worker(
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    projects_dir: PathBuf,
    file_path: PathBuf,
) -> Result<()> {
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        runtime.block_on(import_file_content_diff(
            &storage,
            &embeddings,
            &search,
            &projects_dir,
            &file_path,
        ))
    })
    .await
    .context("transcript import task failed")?
}

/// Import a single JSONL file: parse → embed → store → index, diffing every
/// parsed chunk against what is already stored (see [`needs_reindex`])
/// rather than trusting a stored chunk *count*.
///
/// This is deliberately a free function, not a method on `FileWatcher`: it is
/// also the closure the daemon's `catchup_loop` calls per stale file (see
/// `daemon::mod::catchup_loop`). Sharing this exact function is what makes
/// the catch-up sweep safe to reimport a file whose last busy-dropped write
/// only grew the transcript's final chunk — `Engine::import_file`'s
/// `chunks[prev_count..]` incremental path treats `chunks.len() <= prev_count`
/// as "nothing new" and just rewrites `file_mtime`, permanently stamping the
/// stale row as current without ever re-embedding the grown tail. Diffing by
/// content instead of count catches that case, because a grown final chunk's
/// `id` (`uuid_v5(conversation_id, seq)`) is unchanged but its `content` is
/// not — exactly what `needs_reindex` checks.
pub(crate) async fn import_file_content_diff(
    storage: &Storage,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    projects_dir: &Path,
    file_path: &Path,
) -> Result<()> {
    // Security: resolve symlinks and verify the file is within our projects directory
    let canonical = file_path
        .canonicalize()
        .context("failed to canonicalize import path")?;
    let canonical_base = projects_dir
        .canonicalize()
        .context("failed to canonicalize projects dir")?;
    if !canonical.starts_with(&canonical_base) {
        tracing::warn!(
            file = %file_path.display(),
            resolved = %canonical.display(),
            "refusing to import file outside projects directory (symlink or traversal)"
        );
        return Ok(());
    }

    let attribution =
        import::derive_conversation_attribution_canonical(&canonical_base, &canonical);
    let conv_id = file_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    if let Some(parent) = attribution.parent_conversation_id.as_deref() {
        storage.rescope_sidechain_conversation(&conv_id, &attribution.project_name, parent)?;
    }

    // Skip if already imported
    if storage.is_file_imported(file_path)? {
        return Ok(());
    }

    let parent_context = attribution
        .parent_conversation_id
        .as_deref()
        .map(|parent_id| {
            let message_key = import::sidechain_parent_message_key(file_path);
            storage.parent_provenance_context(parent_id, message_key.as_deref())
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
        storage.set_conversation_entrypoint(&conv_id, ep)?;
    }
    if chunks.is_empty() {
        // Record the skip so this file isn't re-parsed every watcher pass
        // and import_percent counts it as processed.
        storage.mark_file_imported_with_suppression(file_path, 0, suppression)?;
        return Ok(());
    }

    let chunk_count = chunks.len();

    // Journal v4 P4b: bind any pasted dream prompt to the dream that
    // produced it (same contract as `engine::import_file_with_attribution`
    // — marker present or nothing is written).
    import::dream_marker::bind_markers(storage, &conv_id, &chunks);

    // Batch embed and store — but only what actually changed.
    //
    // This is the live path, and it has no tail arithmetic: `is_file_imported`
    // returns false on any mtime change, and Claude Code appends to the session
    // JSONL after every message, so `parse_jsonl_file_with_stats` above re-derives
    // EVERY chunk of the file every ~5s (DEBOUNCE_SECS) for the whole life of a
    // session. Re-deriving is cheap; re-embedding and rewriting all N chunks is
    // not, and it grows with the session. Only chunks that differ from the row
    // already stored need work — appends change the final partial chunk and add
    // new ones, leaving every earlier chunk byte-identical.
    //
    // Compared fields are the ones that decide what search returns: `content`
    // (the indexed text), plus `project_name`/`is_sidechain`, which scope it.
    // `summary`/`seq`/`timestamp` are deterministic re-derivations of the same
    // prefix bytes, so they cannot drift without `content` drifting too.
    let mut reindexed = 0usize;
    let mut unchanged = 0usize;
    for batch in chunks.chunks(BATCH_SIZE) {
        let ids: Vec<String> = batch.iter().map(|c| c.id.clone()).collect();
        let stored: HashMap<String, ConversationChunk> = storage
            .get_chunks_by_ids(&ids)?
            .into_iter()
            .map(|chunk| (chunk.id.clone(), chunk))
            .collect();
        let pending: Vec<&ConversationChunk> = batch
            .iter()
            .filter(|chunk| needs_reindex(chunk, stored.get(&chunk.id)))
            .collect();
        unchanged += batch.len() - pending.len();
        reindexed += pending.len();
        if pending.is_empty() {
            continue;
        }

        let texts: Vec<String> = pending.iter().map(|c| c.content.clone()).collect();
        let emb = Arc::clone(embeddings);
        let embeddings_out = tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
            emb.embed(&refs)
        })
        .await??;

        let mut idx = search.write().await;
        for (chunk, embedding) in pending.iter().zip(embeddings_out) {
            storage.insert_chunk_with_source(chunk, &embedding, attribution.source)?;
            if let Err(error) = storage.insert_chunk_provenance(
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
                tracing::warn!(error = %error, chunk = %chunk.id, "chunk provenance persist failed");
            }
            if let Some(chunk_evidence) = evidence.get(&chunk.id) {
                storage.replace_chunk_evidence(chunk_evidence)?;
            }
            idx.insert_chunk(chunk.id.clone(), embedding);
        }
    }

    storage.mark_file_imported_with_suppression(file_path, chunk_count, suppression)?;
    storage.relink_conversation(&conv_id)?;

    // Layer 1: Heuristic enrichment (inline, instant, free)
    if !storage
        .is_conversation_enriched(&conv_id, "heuristic")
        .unwrap_or(false)
    {
        if let Err(e) = crate::extraction::heuristic::enrich_conversation(
            file_path,
            &conv_id,
            &attribution.project_name,
            storage,
            embeddings,
            search,
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

    tracing::info!(
        file = %file_path.display(),
        chunks = chunk_count,
        reindexed,
        unchanged,
        project = %attribution.project_name,
        source = attribution.source,
        "auto-imported conversation"
    );

    Ok(())
}

/// Does this freshly parsed chunk need to be embedded and written again?
///
/// `stored` is the row already in `chunks` under the same deterministic id
/// (`uuid_v5(conversation_id, seq)`), or `None` when this chunk is new.
///
/// Answering "no" is what stops the live watcher from re-embedding and
/// rewriting an entire session every debounce window. It is safe because the
/// storage write is an upsert keyed on that same id: re-running it with
/// identical values produces a byte-identical row, an identical embedding, and
/// an identical FTS document.
fn needs_reindex(parsed: &ConversationChunk, stored: Option<&ConversationChunk>) -> bool {
    match stored {
        None => true,
        Some(stored) => {
            stored.content != parsed.content
                || stored.project_name != parsed.project_name
                || stored.is_sidechain != parsed.is_sidechain
        }
    }
}

/// Dump a dirty index to `index_dir` with the write lock held only to record
/// the result. The generation files (~1.5 GB for the chunk side) are written
/// under a read lock ([`SearchEngine::publish_to_disk`]), so prompt renders
/// keep searching while it runs; [`SearchEngine::settle_publish`] then clears
/// what was published and leaves anything inserted meanwhile dirty. It used
/// to hold the write lock for the whole dump, on an async worker. Blocks the
/// calling thread: call it from the blocking pool. `None`: nothing was dirty.
/// `strict_counts` propagates a failed DB count instead of describing that
/// side as empty (the watcher's long-standing `unwrap_or(0)`).
pub(crate) fn flush_index_blocking(
    storage: &Storage,
    search: &RwLock<SearchEngine>,
    index_dir: &Path,
    strict_counts: bool,
) -> Option<Result<()>> {
    let index = search.blocking_read();
    if !index.is_dirty() {
        return None;
    }
    let counts = storage
        .count_chunk_embeddings()
        .and_then(|chunks| Ok((chunks, storage.count_reflection_embeddings()?)));
    let (chunk_count, reflection_count) = match counts {
        Ok(counts) => counts,
        Err(e) if strict_counts => return Some(Err(e)),
        Err(_) => (
            storage.count_chunk_embeddings().unwrap_or(0),
            storage.count_reflection_embeddings().unwrap_or(0),
        ),
    };
    let started = Instant::now();
    let published = match index.publish_to_disk(index_dir, chunk_count, reflection_count) {
        Ok(published) => published,
        Err(e) => return Some(Err(e)),
    };
    drop(index);
    let written = started.elapsed();
    search.blocking_write().settle_publish(published);
    tracing::info!(
        written_ms = written.as_millis() as u64,
        total_ms = started.elapsed().as_millis() as u64,
        "HNSW index flushed (files written under the read lock)"
    );
    Some(Ok(()))
}

/// Wait for exclusive ownership of daemon heavy work. The owned RAII
/// permit is held by the caller for the complete watcher batch.
pub(crate) async fn acquire_heavy_work_permit(heavy_work: Arc<Semaphore>) -> OwnedSemaphorePermit {
    heavy_work
        .acquire_owned()
        .await
        .expect("daemon heavy-work semaphore must never be closed")
}

#[cfg(test)]
mod tests {

    #[test]
    fn budget_threshold_is_two_thousand_or_half_a_percent_whichever_is_fewer() {
        assert_eq!(budget_threshold(773_457), 2_000);
        assert_eq!(budget_threshold(400_000), 2_000);
        assert_eq!(budget_threshold(100_000), 500);
        assert_eq!(budget_threshold(10), 1);
        assert_eq!(budget_threshold(0), 1);
    }

    #[test]
    fn budget_needs_both_the_interval_and_the_threshold() {
        let now = Instant::now();
        let long_ago = now.checked_sub(BUDGET_MIN_INTERVAL).unwrap();
        let recent = now
            .checked_sub(BUDGET_MIN_INTERVAL - Duration::from_secs(1))
            .unwrap();
        let slots = 773_457;
        assert!(budget_flush_due(long_ago, now, 2_000, slots));
        assert!(!budget_flush_due(long_ago, now, 1_999, slots), "too few");
        assert!(!budget_flush_due(recent, now, 50_000, slots), "too soon");
        // Five minutes of the old cadence never qualifies on its own.
        let five_minutes = now
            .checked_sub(Duration::from_secs(MIN_FLUSH_INTERVAL_SECS))
            .unwrap();
        assert!(!budget_flush_due(five_minutes, now, 50_000, slots));
    }

    #[test]
    fn first_batch_flushes_immediately() {
        assert!(flush_is_due(None, Instant::now()));
    }

    #[test]
    fn second_batch_inside_the_interval_is_skipped() {
        let now = Instant::now();
        let last = now
            .checked_sub(Duration::from_secs(MIN_FLUSH_INTERVAL_SECS - 1))
            .expect("instant underflow");
        assert!(!flush_is_due(Some(last), now));
    }

    #[test]
    fn batch_at_the_interval_boundary_flushes() {
        let now = Instant::now();
        let last = now
            .checked_sub(Duration::from_secs(MIN_FLUSH_INTERVAL_SECS))
            .expect("instant underflow");
        assert!(flush_is_due(Some(last), now));
    }

    #[test]
    fn batch_well_past_the_interval_flushes() {
        let now = Instant::now();
        let last = now
            .checked_sub(Duration::from_secs(MIN_FLUSH_INTERVAL_SECS * 4))
            .expect("instant underflow");
        assert!(flush_is_due(Some(last), now));
    }

    use super::*;
    use crate::provenance::Speaker;

    fn chunk(id: &str, content: &str) -> ConversationChunk {
        ConversationChunk {
            id: id.to_string(),
            conversation_id: "conv".to_string(),
            project_name: "proj".to_string(),
            timestamp: "2026-08-12T00:00:00Z".to_string(),
            content: content.to_string(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult,
            seq: 0,
            is_sidechain: false,
        }
    }

    #[test]
    fn unseen_chunk_is_indexed() {
        assert!(needs_reindex(&chunk("a", "hello"), None));
    }

    #[test]
    fn byte_identical_chunk_is_skipped() {
        // The whole point: an append to the transcript re-derives every earlier
        // chunk unchanged, and those must cost neither an embedding nor a write.
        let parsed = chunk("a", "hello");
        let stored = chunk("a", "hello");
        assert!(!needs_reindex(&parsed, Some(&stored)));
    }

    #[test]
    fn grown_final_chunk_is_reindexed() {
        // The last chunk of a live session is partial and gains text on append.
        let stored = chunk("a", "hello");
        let parsed = chunk("a", "hello there");
        assert!(needs_reindex(&parsed, Some(&stored)));
    }

    #[test]
    fn rescoped_chunk_is_reindexed() {
        let stored = chunk("a", "hello");
        let mut parsed = chunk("a", "hello");
        parsed.project_name = "other".to_string();
        assert!(needs_reindex(&parsed, Some(&stored)));

        let mut parsed = chunk("a", "hello");
        parsed.is_sidechain = true;
        assert!(needs_reindex(&parsed, Some(&stored)));
    }

    /// Matches `import::scrub`'s own busy-error test fixture exactly, so a
    /// future change to what counts as "busy" can't silently drift between
    /// the two call sites.
    fn busy_error() -> anyhow::Error {
        anyhow::Error::from(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".into()),
        ))
    }

    /// The bug this item closes: an import that fails with `SQLITE_BUSY` on
    /// a session's last write used to be dropped for good, because no later
    /// notify event would ever re-trigger it. Simulates three watcher passes
    /// (busy, busy, ok) over the same path and asserts the file eventually
    /// imports with exactly two recorded busy retries and an empty retry map.
    ///
    /// Deleting the requeue branch in `process_batch` (treating a busy error
    /// like any other and dropping the path on first failure) makes this
    /// fail: the third pass never runs because nothing re-adds the path to
    /// `pending`, so `imported` stays empty.
    #[tokio::test]
    async fn busy_import_is_retried_then_succeeds() {
        let path = PathBuf::from("/tmp/csr-watcher-test/conv.jsonl");
        let calls = std::cell::Cell::new(0u32);
        let mut retry_map: HashMap<PathBuf, RetryState> = HashMap::new();
        let mut total_busy_retries = 0u64;
        let mut imported = false;

        for _pass in 0..3 {
            let outcome = process_batch(
                [path.clone()],
                |_p| {
                    let n = calls.get();
                    calls.set(n + 1);
                    async move {
                        if n < 2 {
                            Err(busy_error())
                        } else {
                            Ok(())
                        }
                    }
                },
                &mut retry_map,
            )
            .await;
            total_busy_retries += outcome.busy_retries;
            if outcome.imported.contains(&path) {
                imported = true;
                break;
            }
        }

        assert!(imported, "file must eventually import once busy clears");
        assert_eq!(total_busy_retries, 2);
        assert!(
            retry_map.is_empty(),
            "successful import must clear the retry entry"
        );
    }

    /// A non-busy error (e.g. malformed JSONL) keeps the original
    /// log-and-drop behaviour: no retry entry, no further attempts.
    #[tokio::test]
    async fn non_busy_error_is_not_retried() {
        let path = PathBuf::from("/tmp/csr-watcher-test/broken.jsonl");
        let mut retry_map: HashMap<PathBuf, RetryState> = HashMap::new();

        let outcome = process_batch(
            [path.clone()],
            |_p| async move { Err(anyhow::anyhow!("malformed jsonl")) },
            &mut retry_map,
        )
        .await;

        assert!(outcome.imported.is_empty());
        assert_eq!(outcome.busy_retries, 0);
        assert!(outcome.gave_up.is_empty());
        assert!(retry_map.is_empty());
    }

    /// After exhausting `BUSY_RETRY_BACKOFF_SECS`, the path is dropped
    /// (WARN + gave_up) rather than retried forever.
    #[tokio::test]
    async fn busy_import_gives_up_after_max_retries() {
        let path = PathBuf::from("/tmp/csr-watcher-test/stuck.jsonl");
        let mut retry_map: HashMap<PathBuf, RetryState> = HashMap::new();

        for attempt in 1..=BUSY_RETRY_BACKOFF_SECS.len() {
            let outcome = process_batch(
                [path.clone()],
                |_p| async move { Err(busy_error()) },
                &mut retry_map,
            )
            .await;
            assert_eq!(outcome.busy_retries, 1);
            assert!(
                outcome.gave_up.is_empty(),
                "attempt {attempt} gave up early"
            );
        }

        // One more failure exceeds the schedule: give up.
        let outcome = process_batch(
            [path.clone()],
            |_p| async move { Err(busy_error()) },
            &mut retry_map,
        )
        .await;
        assert_eq!(outcome.gave_up, vec![path.clone()]);
        assert!(retry_map.is_empty());
    }

    /// The actual gap-G1 mechanism: `wait_for_batch` must wake and re-hand
    /// back a busy path once its backoff elapses, with **no new notify
    /// event** sent on `tx` — the exact scenario that used to lose a
    /// session's final append for good, because nothing else would ever
    /// write to that file again to trigger a fresh event.
    ///
    /// Runs under `start_paused` so the 5s backoff resolves via tokio's
    /// auto-advance rather than a real sleep. Deleting the "merge in any
    /// busy-retry paths that are due now" loop inside `wait_for_batch` makes
    /// this fail: the second `wait_for_batch` call would return an empty
    /// `HashSet` instead of `{path}`, because nothing else is ever sent to
    /// `tx` in this test.
    #[tokio::test(start_paused = true)]
    async fn wake_on_retry_without_new_notify_event() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
        let path = PathBuf::from("/tmp/csr-watcher-test/wake.jsonl");
        let mut retry_map: HashMap<PathBuf, RetryState> = HashMap::new();

        // Pass 1: a real notify event arrives, import fails busy once.
        tx.send(path.clone()).unwrap();
        let pending = wait_for_batch(&mut rx, &retry_map)
            .await
            .expect("channel still open");
        assert_eq!(pending, HashSet::from([path.clone()]));

        let outcome = process_batch(
            pending,
            |_p| async move { Err(busy_error()) },
            &mut retry_map,
        )
        .await;
        assert_eq!(outcome.busy_retries, 1);
        assert!(retry_map.contains_key(&path), "must be scheduled to retry");

        // Pass 2: nothing is ever sent to `tx` again — `wait_for_batch` must
        // still wake on its own once the 5s backoff (BUSY_RETRY_BACKOFF_SECS[0])
        // elapses, and must merge the due path into the batch.
        let pending = wait_for_batch(&mut rx, &retry_map)
            .await
            .expect("channel still open");
        assert_eq!(
            pending,
            HashSet::from([path.clone()]),
            "retry must be merged in purely from the backoff deadline, no notify event sent"
        );

        let outcome = process_batch(pending, |_p| async move { Ok(()) }, &mut retry_map).await;
        assert!(outcome.imported.contains(&path));
        assert!(retry_map.is_empty(), "successful import clears the entry");
    }

    #[tokio::test]
    async fn an_off_worker_import_returns_the_import_error_unchanged() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(10)));
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let projects = tempfile::tempdir().unwrap();
        let err = import_file_content_diff_off_worker(
            storage,
            embeddings,
            search,
            projects.path().to_path_buf(),
            projects.path().join("missing.jsonl"),
        )
        .await
        .unwrap_err();
        // The import's own error, not a wrapper: busy-retry reads this chain.
        assert_eq!(err.to_string(), "failed to canonicalize import path");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_flush_writes_under_a_read_lock_and_settles_under_the_write_lock() {
        let storage = Storage::open_memory().unwrap();
        let search = Arc::new(RwLock::new(SearchEngine::new(10)));
        let index = tempfile::tempdir().unwrap();
        let dir = index.path().to_path_buf();
        let flush = |search: Arc<RwLock<SearchEngine>>, dir: PathBuf| {
            let storage = Storage::open_memory().unwrap();
            tokio::task::spawn_blocking(move || {
                flush_index_blocking(&storage, &search, &dir, false)
            })
        };
        assert!(
            flush(search.clone(), dir.clone()).await.unwrap().is_none(),
            "clean: nothing to do"
        );
        search
            .write()
            .await
            .insert_chunk("c0".into(), vec![1.0; 384]);
        drop(storage);
        // A reader holding the lock does not keep the flush from writing.
        let reader = search.clone().read_owned().await;
        let flushed = flush(search.clone(), dir.clone());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            dir.join("manifest.json").exists(),
            "files written while a reader searched"
        );
        drop(reader);
        assert!(flushed.await.unwrap().unwrap().is_ok());
        assert!(!search.read().await.is_dirty());
    }
}
