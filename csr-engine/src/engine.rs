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
}

impl Engine {
    /// Create an engine from pre-built components (for testing).
    pub fn from_parts(
        storage: Arc<Storage>,
        embeddings: Arc<EmbeddingEngine>,
        search: Arc<RwLock<SearchEngine>>,
        projects_dir: PathBuf,
    ) -> Self {
        Self {
            storage,
            embeddings,
            search,
            projects_dir,
        }
    }

    // TODO(phase-3): HNSW rebuild is 13.6s at 14K chunks (13.5s of that is insertion).
    // Every hook invocation pays this cost. Fix options:
    // (1) Serialize HNSW to disk (hnsw_rs supports file I/O) — load in ~100ms
    // (2) Lazy init for hooks that don't search (Stop, PostToolUse file tracking)
    // (3) Persistent daemon mode for hooks instead of per-invocation processes
    // Measured 2026-02-15: storage=1ms, embed=89ms, vectors=13ms, hnsw=13496ms
    pub fn new(db_path: &Path, projects_dir: &Path) -> Result<Self> {
        let t0 = std::time::Instant::now();

        tracing::info!(?db_path, "opening storage");
        let storage = Arc::new(Storage::open(db_path)?);
        let t_storage = t0.elapsed();

        tracing::info!("initializing embedding engine");
        let embeddings = Arc::new(EmbeddingEngine::new()?);
        let t_embed = t0.elapsed();

        tracing::info!("building search index from stored vectors");

        // Load existing vectors from SQLite into HNSW
        let chunk_vecs = storage.load_all_chunk_vectors()?;
        let t_load = t0.elapsed();

        // Size HNSW to actual data + 20% headroom for growth
        let estimated_size = (chunk_vecs.len() + 1000).max(10_000);
        let mut search = SearchEngine::new(estimated_size);
        for (id, vec) in &chunk_vecs {
            search.insert_chunk(id.clone(), vec.clone());
        }
        let reflection_vecs = storage.load_all_reflection_vectors()?;
        for (id, vec) in &reflection_vecs {
            search.insert_reflection(id.clone(), vec.clone());
        }
        let t_total = t0.elapsed();

        tracing::info!(
            chunks = chunk_vecs.len(),
            reflections = reflection_vecs.len(),
            "search index ready"
        );
        // Always emit startup timing to stderr so hooks can surface it
        eprintln!(
            "CSR startup: storage={:.0}ms embed={:.0}ms vectors={:.0}ms hnsw={:.0}ms total={:.0}ms ({} chunks)",
            t_storage.as_secs_f64() * 1000.0,
            (t_embed - t_storage).as_secs_f64() * 1000.0,
            (t_load - t_embed).as_secs_f64() * 1000.0,
            (t_total - t_load).as_secs_f64() * 1000.0,
            t_total.as_secs_f64() * 1000.0,
            chunk_vecs.len(),
        );

        Ok(Self {
            storage,
            embeddings,
            search: Arc::new(RwLock::new(search)),
            projects_dir: projects_dir.to_path_buf(),
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

    /// Start the file system watcher as a background task.
    /// Returns a JoinHandle that can be awaited or dropped.
    pub fn start_watcher(&self) -> tokio::task::JoinHandle<()> {
        let watcher = FileWatcher::new(
            self.projects_dir.clone(),
            self.storage.clone(),
            self.embeddings.clone(),
            self.search.clone(),
        );
        watcher.spawn()
    }

    /// Start the MCP server on stdio.
    pub async fn serve_mcp(self) -> Result<()> {
        let server = CsrServer::new(self.storage, self.embeddings, self.search, self.projects_dir);
        let service = server
            .serve(rmcp::transport::io::stdio())
            .await?;
        service.waiting().await?;
        Ok(())
    }
}
