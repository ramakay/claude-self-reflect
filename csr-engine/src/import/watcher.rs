use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::sync::{OwnedSemaphorePermit, RwLock, Semaphore};

use crate::embeddings::EmbeddingEngine;
use crate::import;
use crate::search::SearchEngine;
use crate::storage::Storage;

/// Debounce interval for collecting file changes before processing.
const DEBOUNCE_SECS: u64 = 5;

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
        loop {
            let mut pending: HashSet<PathBuf> = HashSet::new();

            // Wait for the first event
            match rx.recv().await {
                Some(path) => {
                    pending.insert(path);
                }
                None => {
                    tracing::info!("file watcher channel closed, stopping");
                    break;
                }
            }

            // Collect more events within the debounce window
            let deadline =
                tokio::time::Instant::now() + tokio::time::Duration::from_secs(DEBOUNCE_SECS);
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

            // Process all pending files
            if !pending.is_empty() {
                let _heavy_permit = match &self.heavy_work {
                    Some(heavy_work) => Some(acquire_heavy_work_permit(heavy_work.clone()).await),
                    None => None,
                };
                tracing::info!(count = pending.len(), "processing new/modified JSONL files");
                for file_path in &pending {
                    if let Err(e) = self.import_file(file_path).await {
                        tracing::warn!(
                            file = %file_path.display(),
                            error = %e,
                            "failed to import file"
                        );
                    }
                }

                // Flush HNSW index to disk after processing batch
                let mut idx = self.search.write().await;
                if idx.is_dirty() {
                    let chunk_count = self.storage.count_chunk_embeddings().unwrap_or(0);
                    let refl_count = self.storage.count_reflection_embeddings().unwrap_or(0);
                    if let Err(e) = idx.dump_to_disk(&self.index_dir, chunk_count, refl_count) {
                        tracing::warn!(error = %e, "failed to flush HNSW index after watcher batch");
                    }
                }
                drop(idx);
            }
        }

        Ok(())
    }

    /// Import a single JSONL file. Parse, embed, store and index all live in
    /// [`import::incremental`], shared with `Engine::import_file` so the two
    /// copies of this routine cannot drift apart again.
    pub(crate) async fn import_file(&self, file_path: &Path) -> Result<()> {
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

        let ctx = import::incremental::ImportContext {
            storage: &self.storage,
            embeddings: &self.embeddings,
            search: &self.search,
        };
        // A watched transcript belongs to a live session, so its trailing chunk is
        // still growing. Its content reaches SQLite and FTS immediately, but it
        // stays out of HNSW until it stops changing — indexing it early would
        // freeze a vector representing only its first fragment.
        let outcome = import::incremental::import_file_incremental(
            &ctx,
            file_path,
            &attribution,
            import::incremental::SealPolicy::DeferTrailing,
        )
        .await?;

        if outcome.unchanged {
            return Ok(());
        }

        import::incremental::maybe_enrich(&ctx, &outcome, file_path, &attribution).await;

        tracing::info!(
            file = %file_path.display(),
            chunks = outcome.total_chunks,
            written = outcome.written_chunks,
            indexed = outcome.indexed_chunks,
            project = %attribution.project_name,
            source = attribution.source,
            "auto-imported conversation"
        );

        Ok(())
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
