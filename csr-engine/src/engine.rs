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

/// Orchestrates all subsystems: storage, embeddings, search, import, and MCP.
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
        let index_dir = db_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("index");

        // Fast O(1) counts for staleness check (~1ms)
        let chunk_count = storage.count_chunk_embeddings()?;
        let reflection_count = storage.count_reflection_embeddings()?;
        let t_count = t0.elapsed();

        // Try loading from disk cache first
        let search = if let Some(cached) =
            SearchEngine::load_from_disk(&index_dir, chunk_count, reflection_count)
        {
            let t_total = t0.elapsed();
            eprintln!(
                "CSR startup: storage={:.0}ms embed={:.0}ms cache_load={:.0}ms total={:.0}ms ({} chunks, cached)",
                t_storage.as_secs_f64() * 1000.0,
                (t_embed - t_storage).as_secs_f64() * 1000.0,
                (t_total - t_count).as_secs_f64() * 1000.0,
                t_total.as_secs_f64() * 1000.0,
                chunk_count,
            );
            tracing::info!(
                chunks = chunk_count,
                reflections = reflection_count,
                "search index loaded from cache"
            );
            cached
        } else {
            // Cache miss — rebuild from SQLite vectors
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
            let reflection_count = storage.count_reflection_embeddings().unwrap_or(reflection_count);
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
            eprintln!(
                "CSR startup: storage={:.0}ms embed={:.0}ms vectors={:.0}ms hnsw={:.0}ms dump={:.0}ms total={:.0}ms ({} chunks, rebuilt)",
                t_storage.as_secs_f64() * 1000.0,
                (t_embed - t_storage).as_secs_f64() * 1000.0,
                (t_load - t_count).as_secs_f64() * 1000.0,
                (t_hnsw - t_load).as_secs_f64() * 1000.0,
                (t_total - t_hnsw).as_secs_f64() * 1000.0,
                t_total.as_secs_f64() * 1000.0,
                chunk_vecs.len(),
            );

            search
        };

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
    pub async fn import_conversations(&mut self, limit: Option<usize>) -> Result<usize> {
        let projects = import::discover_projects(&self.projects_dir)?;
        let mut total = 0usize;
        const BATCH_SIZE: usize = 10;

        for (dir, project_name) in &projects {
            let files = import::list_jsonl_files(dir)?;
            for file_path in &files {
                if self.storage.is_file_imported(file_path)? {
                    continue;
                }
                let chunks = import::parse_jsonl_file(file_path, project_name)?;

                // Batch embed for throughput
                for batch in chunks.chunks(BATCH_SIZE) {
                    let texts: Vec<String> = batch.iter().map(|c| c.content.clone()).collect();
                    let emb = self.embeddings.clone();
                    let embeddings = tokio::task::spawn_blocking(move || {
                        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
                        emb.embed(&refs)
                    })
                    .await??;

                    let mut idx = self.search.write().await;
                    for (chunk, embedding) in batch.iter().zip(embeddings.into_iter()) {
                        self.storage.insert_chunk(chunk, &embedding)?;
                        idx.insert_chunk(chunk.id.clone(), embedding);
                    }
                }

                self.storage.mark_file_imported(file_path, chunks.len())?;

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

                total += chunks.len();

                if let Some(lim) = limit {
                    if total >= lim {
                        return Ok(total);
                    }
                }
            }
        }

        // Flush index to persist any new vectors from import (M-4)
        self.flush_index().await;

        Ok(total)
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

    /// Start the MCP server on stdio.
    pub async fn serve_mcp(self) -> Result<()> {
        let server = CsrServer::new(self.storage, self.embeddings, self.search, self.projects_dir, self.index_dir);
        let service = server
            .serve(rmcp::transport::io::stdio())
            .await?;
        service.waiting().await?;
        Ok(())
    }
}
