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

    pub fn new(db_path: &Path, projects_dir: &Path) -> Result<Self> {
        tracing::info!(?db_path, "opening storage");
        let storage = Arc::new(Storage::open(db_path)?);

        tracing::info!("initializing embedding engine");
        let embeddings = Arc::new(EmbeddingEngine::new()?);

        tracing::info!("building search index from stored vectors");
        let mut search = SearchEngine::new(10_000);

        // Load existing vectors from SQLite into HNSW
        let chunk_vecs = storage.load_all_chunk_vectors()?;
        for (id, vec) in &chunk_vecs {
            search.insert_chunk(id.clone(), vec.clone());
        }
        let reflection_vecs = storage.load_all_reflection_vectors()?;
        for (id, vec) in &reflection_vecs {
            search.insert_reflection(id.clone(), vec.clone());
        }
        tracing::info!(
            chunks = chunk_vecs.len(),
            reflections = reflection_vecs.len(),
            "search index ready"
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
