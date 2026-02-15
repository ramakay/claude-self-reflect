use criterion::{criterion_group, criterion_main, Criterion, BenchmarkId};
use std::time::Duration;

use hnsw_rs::hnsw::Hnsw;
use hnsw_rs::prelude::DistCosine;

// ─── Embedding Benchmark ───

fn bench_embed_single(c: &mut Criterion) {
    use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

    let cache_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".cache")
        .join("csr-engine")
        .join("fastembed");

    let options = InitOptions::new(EmbeddingModel::AllMiniLML6V2)
        .with_cache_dir(cache_dir)
        .with_show_download_progress(false);

    let mut model = TextEmbedding::try_new(options).expect("failed to init embedding model");

    let mut group = c.benchmark_group("embedding");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("embed_single", |b| {
        b.iter(|| {
            let result = model
                .embed(vec!["How do I fix the Docker memory issue?".to_string()], None)
                .unwrap();
            assert_eq!(result[0].len(), 384);
        });
    });

    group.bench_function("embed_batch_10", |b| {
        let texts: Vec<String> = (0..10)
            .map(|i| format!("Test sentence number {} for batch embedding", i))
            .collect();
        b.iter(|| {
            let result = model.embed(texts.clone(), None).unwrap();
            assert_eq!(result.len(), 10);
        });
    });

    group.finish();
}

// ─── HNSW Search Benchmark ───

fn bench_search_hnsw(c: &mut Criterion) {
    let mut group = c.benchmark_group("hnsw_search");
    group.sample_size(100);

    for &size in &[1_000, 10_000] {
        // Build index with random 384-dim vectors
        let index: Hnsw<f32, DistCosine> = Hnsw::new(16, size, 16, 200, DistCosine {});

        let mut rng_state: u64 = 42;
        let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(size);
        for _ in 0..size {
            let vec: Vec<f32> = (0..384)
                .map(|_| {
                    // Simple LCG pseudo-random
                    rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    ((rng_state >> 33) as f32) / (u32::MAX as f32) - 0.5
                })
                .collect();
            vectors.push(vec);
        }

        for (i, vec) in vectors.iter().enumerate() {
            index.insert((vec, i));
        }

        // Query vector (first vector — should find itself as top match)
        let query = &vectors[0];

        group.bench_with_input(
            BenchmarkId::new("search_top5", size),
            &size,
            |b, _| {
                b.iter(|| {
                    let results = index.search(query, 5, 100);
                    assert!(!results.is_empty());
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("search_top20", size),
            &size,
            |b, _| {
                b.iter(|| {
                    let results = index.search(query, 20, 100);
                    assert!(!results.is_empty());
                });
            },
        );
    }

    group.finish();
}

// ─── JSONL Parse Benchmark ───

fn bench_jsonl_parse(c: &mut Criterion) {
    // Create a temp file with realistic JSONL content
    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let jsonl_path = tmp_dir.path().join("test-conversation.jsonl");

    // Generate 200 messages (~4 chunks of 50)
    let mut content = String::new();
    for i in 0..200 {
        let msg_type = if i % 2 == 0 { "human" } else { "assistant" };
        let line = serde_json::json!({
            "type": msg_type,
            "timestamp": "2026-02-14T10:00:00Z",
            "message": {
                "content": [{
                    "type": "text",
                    "text": format!("This is test message {} about Docker containers, performance optimization, and debugging. It has enough content to be realistic for embedding benchmarks.", i)
                }]
            }
        });
        content.push_str(&serde_json::to_string(&line).unwrap());
        content.push('\n');
    }
    std::fs::write(&jsonl_path, &content).expect("failed to write test JSONL");

    let mut group = c.benchmark_group("jsonl_parse");
    group.sample_size(200);

    group.bench_function("parse_200_messages", |b| {
        b.iter(|| {
            let chunks = csr_engine::import::parse_jsonl_file(&jsonl_path, "test-project")
                .expect("parse failed");
            assert_eq!(chunks.len(), 4); // 200 messages / 50 per chunk
        });
    });

    group.finish();
}

// ─── SQLite Storage Benchmark ───

fn bench_sqlite_operations(c: &mut Criterion) {
    use csr_engine::import::ConversationChunk;

    let tmp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let db_path = tmp_dir.path().join("bench.db");
    let storage = csr_engine::storage::Storage::open(&db_path).expect("failed to open storage");

    // Pre-generate test data
    let mut rng_state: u64 = 123;
    let chunks_and_vecs: Vec<(ConversationChunk, Vec<f32>)> = (0..100)
        .map(|i| {
            let vec: Vec<f32> = (0..384)
                .map(|_| {
                    rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    ((rng_state >> 33) as f32) / (u32::MAX as f32) - 0.5
                })
                .collect();
            let chunk = ConversationChunk {
                id: format!("bench-chunk-{}", i),
                conversation_id: format!("conv-{}", i / 10),
                project_name: "bench-project".to_string(),
                timestamp: "2026-02-14T10:00:00Z".to_string(),
                content: format!("Benchmark chunk {} content about Docker and Rust.", i),
                message_count: 10,
                summary: None,
            };
            (chunk, vec)
        })
        .collect();

    // Insert all chunks first
    for (chunk, vec) in &chunks_and_vecs {
        storage.insert_chunk(chunk, vec).expect("insert failed");
    }

    let mut group = c.benchmark_group("sqlite");
    group.sample_size(50);

    group.bench_function("load_all_chunk_vectors_100", |b| {
        b.iter(|| {
            let vecs = storage.load_all_chunk_vectors().expect("load failed");
            assert_eq!(vecs.len(), 100);
        });
    });

    let ids: Vec<String> = (0..10).map(|i| format!("bench-chunk-{}", i)).collect();
    group.bench_function("get_chunks_by_ids_10", |b| {
        b.iter(|| {
            let chunks = storage.get_chunks_by_ids(&ids).expect("get failed");
            assert_eq!(chunks.len(), 10);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_embed_single,
    bench_search_hnsw,
    bench_jsonl_parse,
    bench_sqlite_operations,
);
criterion_main!(benches);
