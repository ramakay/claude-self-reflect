use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use rmcp::ServiceExt;
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::import;
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
    pub async fn import_conversations(&mut self, limit: Option<usize>) -> Result<usize> {
        let projects = import::discover_projects(&self.projects_dir)?;
        let mut total = 0usize;

        for (dir, project_name) in &projects {
            let files = import::list_jsonl_files(dir)?;
            for file_path in &files {
                if self.storage.is_file_imported(file_path)? {
                    continue;
                }
                let chunks = import::parse_jsonl_file(file_path, project_name)?;
                for chunk in &chunks {
                    let embedding = {
                        let text = chunk.content.clone();
                        let emb = self.embeddings.clone();
                        tokio::task::spawn_blocking(move || emb.embed_single(&text))
                            .await??
                    };
                    self.storage
                        .insert_chunk(chunk, &embedding)?;
                    {
                        let mut idx = self.search.write().await;
                        idx.insert_chunk(chunk.id.clone(), embedding);
                    }
                }
                self.storage.mark_file_imported(
                    file_path,
                    chunks.len(),
                )?;
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

    /// Start the MCP server on stdio.
    pub async fn serve_mcp(self) -> Result<()> {
        let server = CsrServer::new(self.storage, self.embeddings, self.search);
        let service = server
            .serve(rmcp::transport::io::stdio())
            .await?;
        service.waiting().await?;
        Ok(())
    }
}
