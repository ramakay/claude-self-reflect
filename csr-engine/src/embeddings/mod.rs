pub mod cache;

use std::sync::Mutex;

use anyhow::Result;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

/// Wraps fastembed for local ONNX embeddings (default: 384-dim all-MiniLM-L6-v2).
/// Thread-safe via Mutex (TextEmbedding::embed requires &mut self).
pub struct EmbeddingEngine {
    model: Mutex<TextEmbedding>,
    dim: usize,
}

impl EmbeddingEngine {
    /// Initialize the default embedding model (downloads ~30MB on first run).
    pub fn new() -> Result<Self> {
        Self::new_with_model(EmbeddingModel::AllMiniLML6V2)
    }

    /// Initialize a specific fastembed model. Used by the retrieval benchmark
    /// to compare candidate models against the default.
    pub fn new_with_model(embedding_model: EmbeddingModel) -> Result<Self> {
        let cache_dir = cache::cache_dir();
        std::fs::create_dir_all(&cache_dir)?;

        let dim = TextEmbedding::get_model_info(&embedding_model)?.dim;
        let options = InitOptions::new(embedding_model)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(true);

        let model = TextEmbedding::try_new(options)?;
        Ok(Self {
            model: Mutex::new(model),
            dim,
        })
    }

    /// Embed a batch of texts. Returns one vector per input.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let docs: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
        let mut model = self
            .model
            .lock()
            .map_err(|e| anyhow::anyhow!("embedding lock: {e}"))?;
        let embeddings = model.embed(docs, None)?;
        Ok(embeddings)
    }

    /// Embed a single text string.
    pub fn embed_single(&self, text: &str) -> Result<Vec<f32>> {
        let results = self.embed(&[text])?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("embedding returned empty result"))
    }

    /// Embedding dimension of the loaded model instance.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Returns the embedding dimension of the default model (384 for all-MiniLM-L6-v2).
    pub fn dimension() -> usize {
        384
    }
}
