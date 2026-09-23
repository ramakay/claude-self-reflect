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
#[derive(Debug, Clone)]
pub(crate) struct RetryState {
    /// Number of busy failures seen for this path so far.
    attempts: u32,
    /// When this path becomes due for its next retry attempt.
    next_at: Instant,
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
                    next_at: Instant::now(),
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
                    entry.next_at = Instant::now() + Duration::from_secs(backoff);
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
fn earliest_retry(retry_map: &HashMap<PathBuf, RetryState>) -> Option<Instant> {
    retry_map.values().map(|state| state.next_at).min()
}

/// Sleep until `deadline`, or block forever when `None` — lets the watcher's
/// `tokio::select!` treat "no retries pending" and "waiting for a real
/// retry" uniformly against the same branch.
async fn sleep_until_or_pending(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
        None => std::future::pending::<()>().await,
    }
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
        }
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

        // Debounce loop: collect changed files over DEBOUNCE_SECS, then process
        let mut last_flush: Option<Instant> = None;
        let mut retry_map: HashMap<PathBuf, RetryState> = HashMap::new();
        loop {
            let mut pending: HashSet<PathBuf> = HashSet::new();

            // Wait for the first event OR the earliest due busy-retry — a
            // retry must run even when no new notify event ever arrives
            // (the case that used to lose a session's last append for good).
            let retry_deadline = earliest_retry(&retry_map);
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
                            break;
                        }
                    }
                }
                _ = sleep_until_or_pending(retry_deadline) => {
                    // Woken purely to service a due retry — merged in below.
                }
            }

            // Merge in any busy-retry paths that are due now, whether or not
            // a fresh event also arrived this iteration.
            let now_instant = Instant::now();
            for (path, state) in &retry_map {
                if state.next_at <= now_instant {
                    pending.insert(path.clone());
                }
            }

            // Process all pending files
            if !pending.is_empty() {
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

                // Flush the HNSW index at most once per MIN_FLUSH_INTERVAL_SECS.
                // See the const for why deferring the dump is safe.
                let now = Instant::now();
                if flush_is_due(last_flush, now) {
                    let mut idx = self.search.write().await;
                    if idx.is_dirty() {
                        let chunk_count = self.storage.count_chunk_embeddings().unwrap_or(0);
                        let refl_count = self.storage.count_reflection_embeddings().unwrap_or(0);
                        if let Err(e) = idx.dump_to_disk(&self.index_dir, chunk_count, refl_count) {
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
                    drop(idx);
                }
            }
        }

        // Channel closed. Dump once on the way out so a deferred batch is not
        // left for the next startup to backfill.
        let mut idx = self.search.write().await;
        if idx.is_dirty() {
            let chunk_count = self.storage.count_chunk_embeddings().unwrap_or(0);
            let refl_count = self.storage.count_reflection_embeddings().unwrap_or(0);
            if let Err(e) = idx.dump_to_disk(&self.index_dir, chunk_count, refl_count) {
                tracing::warn!(error = %e, "failed to flush HNSW index on watcher shutdown");
            }
        }
        drop(idx);

        Ok(())
    }

    /// Import a single JSONL file: parse → embed → store → index.
    async fn import_file(&self, file_path: &Path) -> Result<()> {
        // Security: resolve symlinks and verify the file is within our projects directory
        let canonical = file_path
            .canonicalize()
            .context("failed to canonicalize import path")?;
        let canonical_base = self
            .projects_dir
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
            self.storage.rescope_sidechain_conversation(
                &conv_id,
                &attribution.project_name,
                parent,
            )?;
        }

        // Skip if already imported
        if self.storage.is_file_imported(file_path)? {
            return Ok(());
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
            self.storage.set_conversation_entrypoint(&conv_id, ep)?;
        }
        if chunks.is_empty() {
            // Record the skip so this file isn't re-parsed every watcher pass
            // and import_percent counts it as processed.
            self.storage
                .mark_file_imported_with_suppression(file_path, 0, suppression)?;
            return Ok(());
        }

        let chunk_count = chunks.len();

        // Journal v4 P4b: bind any pasted dream prompt to the dream that
        // produced it (same contract as `engine::import_file_with_attribution`
        // — marker present or nothing is written).
        import::dream_marker::bind_markers(&self.storage, &conv_id, &chunks);

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
            let stored: HashMap<String, ConversationChunk> = self
                .storage
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
            let emb = self.embeddings.clone();
            let embeddings = tokio::task::spawn_blocking(move || {
                let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
                emb.embed(&refs)
            })
            .await??;

            let mut idx = self.search.write().await;
            for (chunk, embedding) in pending.iter().zip(embeddings) {
                self.storage
                    .insert_chunk_with_source(chunk, &embedding, attribution.source)?;
                if let Err(error) = self.storage.insert_chunk_provenance(
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
                    self.storage.replace_chunk_evidence(chunk_evidence)?;
                }
                idx.insert_chunk(chunk.id.clone(), embedding);
            }
        }

        self.storage
            .mark_file_imported_with_suppression(file_path, chunk_count, suppression)?;
        self.storage.relink_conversation(&conv_id)?;

        // Layer 1: Heuristic enrichment (inline, instant, free)
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
}
