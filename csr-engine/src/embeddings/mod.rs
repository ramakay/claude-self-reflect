pub mod cache;

use std::sync::Mutex;

use anyhow::Result;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

/// Wraps fastembed for 384-dim all-MiniLM-L6-v2 embeddings.
///
/// The ONNX model (fp32 AllMiniLML6V2) is NOT loaded by `new()` — it is
/// loaded lazily, on first `embed`/`embed_single` call, behind a
/// `Mutex<Option<TextEmbedding>>` acting as a fallible once-cell (a plain
/// `OnceLock<TextEmbedding>` cannot hold the `Result` from a failed init).
/// `new()` is cheap on purpose: it runs unconditionally in every
/// `Engine::new` (MCP server startup, every hook invocation), and most
/// hook invocations never call `embed` at all. Callers that DO know they
/// are about to embed and want the load off the hot path (import/daemon)
/// should call `warm()` right after construction.
pub struct EmbeddingEngine {
    model: Mutex<Option<TextEmbedding>>,
}

/// Serializes first-run model downloads within this process. Concurrent
/// `TextEmbedding::try_new` calls (parallel test threads, racing hooks on a
/// fresh install) contend on hf-hub's cross-process blob lock and fail with
/// "Lock acquisition failed" instead of waiting.
static MODEL_INIT_LOCK: Mutex<()> = Mutex::new(());

/// Resolve the ONNX intra-op thread cap: `CSR_EMBED_THREADS` if it parses
/// as `1..=runtime::MAX_THREAD_OVERRIDE`, else `min(4, available_parallelism)`.
/// Junk, zero, or oversized values fall back to the default rather than
/// erroring — this runs on the lazy-init path of every hook process and must
/// never fail, and ORT narrows the count to a C int, so an unbounded value
/// could wrap to zero.
fn resolve_intra_threads() -> usize {
    if let Ok(raw) = std::env::var("CSR_EMBED_THREADS") {
        if let Ok(n) = raw.trim().parse::<usize>() {
            if (1..=crate::runtime::MAX_THREAD_OVERRIDE).contains(&n) {
                return n;
            }
        }
    }
    std::thread::available_parallelism()
        .map(|n| n.get().min(4))
        .unwrap_or(4)
}

impl EmbeddingEngine {
    /// Construct the engine without loading the ONNX model. Cheap and
    /// infallible in practice (no I/O beyond what `Mutex::new` does).
    pub fn new() -> Result<Self> {
        Ok(Self {
            model: Mutex::new(None),
        })
    }

    /// True once the ONNX model has been loaded into memory. Never
    /// triggers a load itself.
    pub fn is_loaded(&self) -> bool {
        matches!(self.model.lock(), Ok(guard) if guard.is_some())
    }

    /// Force the model to load now instead of on first `embed`. Use where
    /// first-embed latency or an early failure matters more than startup
    /// memory (daemon, --import/--enrich, setup, eval) — not the MCP server
    /// or hooks, which stay lazy so a process that never embeds never pays
    /// the model load (~130MB resident, ~45ms).
    pub fn warm(&self) -> Result<()> {
        self.ensure_loaded()
    }

    /// Load the model if it isn't already loaded (downloads ~30MB on first
    /// run). Idempotent: a second call after a successful load is a cheap
    /// lock-and-check. A failed load leaves the cell empty so the next
    /// call retries from scratch.
    fn ensure_loaded(&self) -> Result<()> {
        let mut guard = self
            .model
            .lock()
            .map_err(|e| anyhow::anyhow!("embedding lock: {e}"))?;
        if guard.is_some() {
            return Ok(());
        }

        let cache_dir = cache::cache_dir();
        std::fs::create_dir_all(&cache_dir)?;

        let _init_guard = MODEL_INIT_LOCK
            .lock()
            .map_err(|e| anyhow::anyhow!("model init lock: {e}"))?;

        let threads = resolve_intra_threads();

        // Retry across processes: another csr-engine may hold the hf-hub blob
        // lock mid-download; once it finishes, the cached model loads instantly.
        let mut last_err = None;
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_secs(2 * attempt as u64));
            }
            let options = InitOptions::new(EmbeddingModel::AllMiniLML6V2)
                .with_cache_dir(cache_dir.clone())
                .with_show_download_progress(true)
                .with_intra_threads(threads);
            match TextEmbedding::try_new(options) {
                Ok(model) => {
                    *guard = Some(model);
                    return Ok(());
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
    /// Loads the model on first call if it hasn't been `warm()`ed already.
    pub fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.ensure_loaded()?;
        let docs: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
        let mut guard = self
            .model
            .lock()
            .map_err(|e| anyhow::anyhow!("embedding lock: {e}"))?;
        let model = guard
            .as_mut()
            .expect("ensure_loaded returned Ok, so the model is populated");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `new()` must not touch the ONNX model — this is the whole point of
    /// lazy init. No env/network access needed to verify this, so it's
    /// always safe to run.
    #[test]
    fn new_does_not_load_the_model() {
        let engine = EmbeddingEngine::new().expect("new() is infallible in practice");
        assert!(
            !engine.is_loaded(),
            "EmbeddingEngine::new() must not eagerly load the ONNX model"
        );
    }

    /// First `embed` loads the model; a second `embed` reuses the already
    /// loaded model instead of reloading it. Downloads the model on first
    /// run, so this is gated like the rest of the model-dependent suite.
    #[test]
    #[ignore = "downloads the ~30MB ONNX model on first run; run with --ignored"]
    fn first_embed_loads_then_reuses_the_model() {
        let engine = EmbeddingEngine::new().unwrap();
        assert!(!engine.is_loaded());

        let first = engine.embed_single("lazy load probe").unwrap();
        assert!(engine.is_loaded(), "first embed must load the model");
        assert_eq!(first.len(), EmbeddingEngine::dimension());

        // Second call must not reload: same loaded model, no panic/reinit.
        let second = engine.embed_single("second call reuses the model").unwrap();
        assert!(engine.is_loaded());
        assert_eq!(second.len(), EmbeddingEngine::dimension());
    }

    #[test]
    #[ignore = "downloads the ~30MB ONNX model on first run; run with --ignored"]
    fn warm_loads_the_model_eagerly() {
        let engine = EmbeddingEngine::new().unwrap();
        assert!(!engine.is_loaded());
        engine.warm().unwrap();
        assert!(engine.is_loaded(), "warm() must load the model immediately");
    }

    // Serialize env-var mutation across these tests: `cargo test` runs
    // tests in the same process on multiple threads, and std::env::var is
    // process-global.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env<F: FnOnce()>(value: Option<&str>, f: F) {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = std::env::var("CSR_EMBED_THREADS").ok();
        match value {
            Some(v) => std::env::set_var("CSR_EMBED_THREADS", v),
            None => std::env::remove_var("CSR_EMBED_THREADS"),
        }
        f();
        match previous {
            Some(v) => std::env::set_var("CSR_EMBED_THREADS", v),
            None => std::env::remove_var("CSR_EMBED_THREADS"),
        }
    }

    #[test]
    fn thread_count_env_unset_falls_back_to_default() {
        with_env(None, || {
            let expected = std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(4);
            assert_eq!(resolve_intra_threads(), expected);
        });
    }

    #[test]
    fn thread_count_env_valid_override_is_used() {
        with_env(Some("2"), || {
            assert_eq!(resolve_intra_threads(), 2);
        });
    }

    #[test]
    fn thread_count_env_junk_falls_back_to_default() {
        with_env(Some("not-a-number"), || {
            let expected = std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(4);
            assert_eq!(resolve_intra_threads(), expected);
        });
    }

    #[test]
    fn thread_count_env_excessive_falls_back_to_default() {
        with_env(Some("4294967296"), || {
            let expected = std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(4);
            assert_eq!(resolve_intra_threads(), expected);
        });
        with_env(
            Some(&(crate::runtime::MAX_THREAD_OVERRIDE + 1).to_string()),
            || {
                let expected = std::thread::available_parallelism()
                    .map(|n| n.get().min(4))
                    .unwrap_or(4);
                assert_eq!(resolve_intra_threads(), expected);
            },
        );
    }

    #[test]
    fn thread_count_env_zero_falls_back_to_default() {
        with_env(Some("0"), || {
            let expected = std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(4);
            assert_eq!(resolve_intra_threads(), expected);
        });
    }
}
