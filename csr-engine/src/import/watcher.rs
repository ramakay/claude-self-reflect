use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::import;
use crate::search::SearchEngine;
use crate::storage::Storage;

/// Debounce interval for collecting file changes before processing.
const DEBOUNCE_SECS: u64 = 5;

/// Batch size for embedding during auto-import.
const BATCH_SIZE: usize = 10;

/// Watches the Claude projects directory for new JSONL files and auto-imports them.
pub struct FileWatcher {
    projects_dir: PathBuf,
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    index_dir: PathBuf,
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
        }
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
            }
        }

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

        // Skip if already imported
        if self.storage.is_file_imported(file_path)? {
            return Ok(());
        }

        // Infer project name from parent directory
        let project_name = file_path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| import::normalize_project_name(&n.to_string_lossy()))
            .unwrap_or_else(|| "unknown".to_string());

        let chunks = import::parse_jsonl_file(file_path, &project_name)?;
        if chunks.is_empty() {
            return Ok(());
        }

        let chunk_count = chunks.len();

        // Batch embed and store
        for batch in chunks.chunks(BATCH_SIZE) {
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
                idx.insert_chunk(chunk.id.clone(), embedding);
            }
        }

        self.storage.mark_file_imported(file_path, chunk_count)?;

        // Layer 1: Heuristic enrichment (inline, instant, free)
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
                &project_name,
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
            project = %project_name,
            "auto-imported conversation"
        );

        Ok(())
    }
}
