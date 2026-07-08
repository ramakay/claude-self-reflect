pub mod cache;

use std::sync::Mutex;

use anyhow::Result;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

/// Wraps fastembed for 384-dim all-MiniLM-L6-v2 embeddings.
/// Thread-safe via Mutex (TextEmbedding::embed requires &mut self).
pub struct EmbeddingEngine {
    model: Mutex<TextEmbedding>,
}

/// Serializes first-run model downloads within this process. Concurrent
/// `TextEmbedding::try_new` calls (parallel test threads, racing hooks on a
/// fresh install) contend on hf-hub's cross-process blob lock and fail with
/// "Lock acquisition failed" instead of waiting.
static MODEL_INIT_LOCK: Mutex<()> = Mutex::new(());

impl EmbeddingEngine {
    /// Initialize the embedding model (downloads ~30MB on first run).
    pub fn new() -> Result<Self> {
        let cache_dir = cache::cache_dir();
        std::fs::create_dir_all(&cache_dir)?;

        let _init_guard = MODEL_INIT_LOCK
            .lock()
            .map_err(|e| anyhow::anyhow!("model init lock: {e}"))?;

        // Retry across processes: another csr-engine may hold the hf-hub blob
        // lock mid-download; once it finishes, the cached model loads instantly.
        let mut last_err = None;
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_secs(2 * attempt as u64));
            }
            let options = InitOptions::new(EmbeddingModel::AllMiniLML6V2)
                .with_cache_dir(cache_dir.clone())
                .with_show_download_progress(true);
            match TextEmbedding::try_new(options) {
                Ok(model) => {
                    return Ok(Self {
                        model: Mutex::new(model),
                    })
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(anyhow::anyhow!(
            "embedding model init failed after retries: {}",
            last_err.expect("at least one attempt ran")
        ))
    }

    /// Embed a batch of texts. Returns one 384-dim vector per input.
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

    /// Returns the embedding dimension (384 for all-MiniLM-L6-v2).
    pub fn dimension() -> usize {
        384
    }
}
