//! Measures RSS/wall-clock for one uncapped `EmbeddingEngine::embed()` call
//! over N synthetic documents, each padded to fastembed's 512-token limit.
//!
//! Run under `/usr/bin/time -l` to capture "maximum resident set size":
//!   /usr/bin/time -l cargo run --release --example embed_batch_rss
//!
//! N is `CSR_EMBED_RSS_DOCS` (default 256). Each doc is ~480 whitespace-
//! separated words so the tokenizer pads it out to the 512-token max length,
//! reproducing the worst-case tensor shape for a single uncapped `embed()`
//! call (see EMBED_BATCH_SIZE in src/embeddings/mod.rs).

use std::time::Instant;

use csr_engine::embeddings::EmbeddingEngine;

fn synthetic_doc(seed: usize) -> String {
    // ~480 whitespace-separated tokens; distinct per doc so nothing collapses
    // to a cached/degenerate case, but content is otherwise irrelevant.
    (0..480)
        .map(|i| format!("token{}_{}", seed, i))
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() -> anyhow::Result<()> {
    let n: usize = std::env::var("CSR_EMBED_RSS_DOCS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(256);

    let docs: Vec<String> = (0..n).map(synthetic_doc).collect();
    let doc_refs: Vec<&str> = docs.iter().map(String::as_str).collect();

    let engine = EmbeddingEngine::new()?;
    // Warm the model outside the timed region so the measurement reflects
    // only the batch embed call, not the one-time ONNX load.
    engine.warm()?;

    let start = Instant::now();
    let embeddings = engine.embed(&doc_refs)?;
    let elapsed = start.elapsed();

    assert_eq!(
        embeddings.len(),
        n,
        "expected {} vectors, got {}",
        n,
        embeddings.len()
    );
    for v in &embeddings {
        assert_eq!(v.len(), EmbeddingEngine::dimension());
    }

    println!("N={} elapsed_ms={}", n, elapsed.as_millis());
    Ok(())
}
