//! Integration tests for the CSR engine pipeline.
//!
//! These tests cover: JSONL parsing → storage → search → formatting.
//! Embedding tests are behind the `embedding` feature flag since they require
//! downloading the ONNX model (~30MB) on first run.

use std::collections::HashSet;
use std::path::PathBuf;

use csr_engine::format::{self, EnrichedResult};
use csr_engine::import::{self, ConversationChunk};
use csr_engine::search::cross_project;
use csr_engine::search::decay;
use csr_engine::search::SearchEngine;
use csr_engine::storage::Storage;
use csr_engine::temporal;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

// ─── Test 1: JSONL parsing ───

#[test]
fn test_parse_jsonl_file() {
    let file = fixtures_dir().join("sample_conversation.jsonl");
    let chunks = import::parse_jsonl_file(&file, "test-project").unwrap();

    assert!(!chunks.is_empty(), "should parse at least one chunk");
    assert_eq!(chunks[0].project_name, "test-project");
    assert!(chunks[0].content.contains("Docker memory"));
    assert!(chunks[0].content.contains("Qdrant"));
    assert_eq!(chunks[0].timestamp, "2026-01-15T10:00:00Z");
}

#[test]
fn test_parse_jsonl_deterministic_ids() {
    let file = fixtures_dir().join("sample_conversation.jsonl");
    let chunks1 = import::parse_jsonl_file(&file, "test-project").unwrap();
    let chunks2 = import::parse_jsonl_file(&file, "test-project").unwrap();

    assert_eq!(chunks1.len(), chunks2.len());
    for (c1, c2) in chunks1.iter().zip(chunks2.iter()) {
        assert_eq!(c1.id, c2.id, "chunk IDs should be deterministic");
    }
}

// ─── Test 2: Storage round-trip ───

#[test]
fn test_storage_insert_and_retrieve() {
    let storage = Storage::open_memory().unwrap();

    let chunk = ConversationChunk {
        id: "test-chunk-1".into(),
        conversation_id: "conv-1".into(),
        project_name: "my-project".into(),
        timestamp: "2026-01-15T10:00:00Z".into(),
        content: "Docker memory issue discussion".into(),
        message_count: 4,
    };

    // Fake 384-dim embedding
    let embedding: Vec<f32> = (0..384).map(|i| (i as f32) / 384.0).collect();
    storage.insert_chunk(&chunk, &embedding).unwrap();

    let retrieved = storage.get_chunks_by_ids(&["test-chunk-1".into()]).unwrap();
    assert_eq!(retrieved.len(), 1);
    assert_eq!(retrieved[0].id, "test-chunk-1");
    assert_eq!(retrieved[0].project_name, "my-project");
    assert_eq!(retrieved[0].content, "Docker memory issue discussion");
    assert_eq!(retrieved[0].message_count, 4);
}

// ─── Test 3: Project filtering ───

#[test]
fn test_project_filtering() {
    let storage = Storage::open_memory().unwrap();
    let fake_emb: Vec<f32> = vec![0.0; 384];

    // Insert chunks from two projects
    for (i, project) in ["project-a", "project-b"].iter().enumerate() {
        for j in 0..3 {
            let chunk = ConversationChunk {
                id: format!("{}-chunk-{}", project, j),
                conversation_id: format!("conv-{}-{}", project, j),
                project_name: project.to_string(),
                timestamp: format!("2026-01-{}T10:00:00Z", 15 + i),
                content: format!("Content from {} chunk {}", project, j),
                message_count: 2,
            };
            storage.insert_chunk(&chunk, &fake_emb).unwrap();
        }
    }

    let ids_a = storage.get_chunk_ids_for_project("project-a").unwrap();
    assert_eq!(ids_a.len(), 3);
    for id in &ids_a {
        assert!(id.starts_with("project-a"));
    }

    let ids_b = storage.get_chunk_ids_for_project("project-b").unwrap();
    assert_eq!(ids_b.len(), 3);
}

// ─── Test 4: Time-range filtering ───

#[test]
fn test_time_range_filtering() {
    let storage = Storage::open_memory().unwrap();
    let fake_emb: Vec<f32> = vec![0.0; 384];

    let timestamps = [
        "2026-01-10T10:00:00+00:00",
        "2026-01-15T10:00:00+00:00",
        "2026-01-20T10:00:00+00:00",
        "2026-02-01T10:00:00+00:00",
    ];

    for (i, ts) in timestamps.iter().enumerate() {
        let chunk = ConversationChunk {
            id: format!("time-chunk-{}", i),
            conversation_id: format!("conv-{}", i),
            project_name: "test".into(),
            timestamp: ts.to_string(),
            content: format!("Content at {}", ts),
            message_count: 1,
        };
        storage.insert_chunk(&chunk, &fake_emb).unwrap();
    }

    // Query: Jan 12 to Jan 18 (should get only the Jan 15 chunk)
    let ids = storage
        .get_chunk_ids_in_timerange(
            "2026-01-12T00:00:00+00:00",
            "2026-01-18T00:00:00+00:00",
            None,
        )
        .unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0], "time-chunk-1");
}

// ─── Test 5: FTS5 search ───

#[test]
fn test_fts5_search() {
    let storage = Storage::open_memory().unwrap();
    let fake_emb: Vec<f32> = vec![0.0; 384];

    let chunks = vec![
        ConversationChunk {
            id: "fts-1".into(),
            conversation_id: "conv-1".into(),
            project_name: "test".into(),
            timestamp: "2026-01-15T10:00:00Z".into(),
            content: "We modified docker-compose.yaml to fix the memory limit".into(),
            message_count: 2,
        },
        ConversationChunk {
            id: "fts-2".into(),
            conversation_id: "conv-2".into(),
            project_name: "test".into(),
            timestamp: "2026-01-16T10:00:00Z".into(),
            content: "Authentication was added using JWT tokens in auth.rs".into(),
            message_count: 2,
        },
    ];

    for chunk in &chunks {
        storage.insert_chunk(chunk, &fake_emb).unwrap();
    }

    // Search for docker-compose
    let results = storage.fts5_search("docker-compose", 10, None).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "fts-1");

    // Search for JWT
    let results = storage.fts5_search("JWT", 10, None).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "fts-2");
}

// ─── Test 6: Reflection store + retrieve ───

#[test]
fn test_reflection_store_and_retrieve() {
    let storage = Storage::open_memory().unwrap();

    let id = "refl-1";
    let content = "Always use batch embedding for import — 3.4x speedup";
    let tags = vec!["performance".to_string(), "embedding".to_string()];
    let fake_emb: Vec<f32> = vec![0.1; 384];

    storage
        .insert_reflection(id, content, &tags, &fake_emb)
        .unwrap();

    let result = storage.get_reflection_by_id(id).unwrap();
    assert!(result.is_some());
    let (r_content, r_tags, _timestamp) = result.unwrap();
    assert_eq!(r_content, content);
    assert_eq!(r_tags, tags);
}

#[test]
fn test_reflection_tag_search() {
    let storage = Storage::open_memory().unwrap();
    let fake_emb: Vec<f32> = vec![0.1; 384];

    storage
        .insert_reflection(
            "refl-session-1",
            "Learning from iteration 1",
            &["session_ralph_001".into(), "iteration_1".into()],
            &fake_emb,
        )
        .unwrap();

    storage
        .insert_reflection(
            "refl-session-2",
            "Learning from iteration 2",
            &["session_ralph_001".into(), "iteration_2".into()],
            &fake_emb,
        )
        .unwrap();

    storage
        .insert_reflection(
            "refl-other",
            "Unrelated reflection",
            &["general".into()],
            &fake_emb,
        )
        .unwrap();

    let results = storage
        .get_reflections_by_tag("session_ralph_001", 50)
        .unwrap();
    assert_eq!(results.len(), 2);
}

// ─── Test 7: Decay verification ───

#[test]
fn test_decay_known_values() {
    use chrono::{Duration, Utc};

    let now = Utc::now();

    // Zero age → no decay
    let score = decay::apply_decay(1.0, &now, &now, None, None);
    assert!((score - 1.0).abs() < 0.001);

    // 90 days → expected 0.85
    let past_90 = now - Duration::days(90);
    let score_90 = decay::apply_decay(1.0, &past_90, &now, None, None);
    assert!(
        (score_90 - 0.85).abs() < 0.01,
        "90-day decay: expected ~0.85, got {}",
        score_90
    );

    // 180 days → expected ~0.775
    let past_180 = now - Duration::days(180);
    let score_180 = decay::apply_decay(1.0, &past_180, &now, None, None);
    assert!(
        score_180 > 0.7 && score_180 < 0.8,
        "180-day decay: expected ~0.775, got {}",
        score_180
    );
}

// ─── Test 8: HNSW search with synthetic vectors ───

#[test]
fn test_hnsw_search_basic() {
    let mut engine = SearchEngine::new(100);

    // Insert 10 chunks with distinct vectors
    for i in 0..10 {
        let mut vec = vec![0.0f32; 384];
        vec[i % 384] = 1.0; // One-hot-ish vectors
        engine.insert_chunk(format!("chunk-{}", i), vec);
    }

    assert_eq!(engine.chunk_count(), 10);

    // Search with a vector similar to chunk-0
    let mut query = vec![0.0f32; 384];
    query[0] = 1.0;

    let results = engine.search_chunks(&query, 3, 0.0);
    assert!(!results.is_empty());
    assert_eq!(results[0].id, "chunk-0");
    assert!(results[0].score > 0.9);
}

#[test]
fn test_hnsw_filtered_search() {
    let mut engine = SearchEngine::new(100);

    for i in 0..10 {
        let mut vec = vec![0.0f32; 384];
        vec[i % 384] = 1.0;
        engine.insert_chunk(format!("chunk-{}", i), vec);
    }

    // Only allow chunks 5-9
    let allowed: HashSet<String> = (5..10).map(|i| format!("chunk-{}", i)).collect();

    let mut query = vec![0.0f32; 384];
    query[0] = 1.0; // Most similar to chunk-0, but that's filtered out

    let results = engine.search_chunks_filtered(&query, 3, 0.0, &allowed);
    for r in &results {
        assert!(
            allowed.contains(&r.id),
            "filtered result {} not in allowed set",
            r.id
        );
    }
}

// ─── Test 9: Reflection search ───

#[test]
fn test_reflection_search() {
    let mut engine = SearchEngine::new(100);

    for i in 0..5 {
        let mut vec = vec![0.0f32; 384];
        vec[i % 384] = 1.0;
        engine.insert_reflection(format!("refl-{}", i), vec);
    }

    assert_eq!(engine.reflection_count(), 5);

    let mut query = vec![0.0f32; 384];
    query[0] = 1.0;

    let results = engine.search_reflections(&query, 2, 0.0);
    assert!(!results.is_empty());
    assert_eq!(results[0].id, "refl-0");
}

// ─── Test 10: XML format structure ───

#[test]
fn test_format_search_results_structure() {
    let results = vec![EnrichedResult {
        score: 0.85,
        chunk: ConversationChunk {
            id: "c1".into(),
            conversation_id: "conv-1".into(),
            project_name: "test-project".into(),
            timestamp: "2026-01-15T10:00:00Z".into(),
            content: "Docker memory fix applied".into(),
            message_count: 4,
        },
    }];

    let xml = format::format_search_results(&results, "docker memory", "test-project", 5, 2);

    assert!(xml.contains("<search>"), "should have search tag");
    assert!(xml.contains("</search>"), "should close search tag");
    assert!(xml.contains("0.850"), "should contain score");
    assert!(xml.contains("test-project"), "should contain project name");
    assert!(xml.contains("Docker memory fix applied"), "should contain content");
    assert!(xml.contains("conv-1"), "should contain conversation ID");
}

#[test]
fn test_format_quick_check_structure() {
    let results = vec![EnrichedResult {
        score: 0.72,
        chunk: ConversationChunk {
            id: "c1".into(),
            conversation_id: "conv-1".into(),
            project_name: "test".into(),
            timestamp: "2026-01-15T10:00:00Z".into(),
            content: "JWT authentication setup".into(),
            message_count: 2,
        },
    }];

    let xml = format::format_quick_check(&results, "JWT auth");
    assert!(xml.contains("<quick_search>"));
    assert!(xml.contains("<count>1</count>"));
    assert!(xml.contains("<score>0.720</score>"));
}

#[test]
fn test_format_empty_results() {
    let xml = format::format_search_results(&[], "nonexistent", "all", 0, 0);
    assert!(xml.contains("NO RESULTS"));

    let xml = format::format_quick_check(&[], "nothing");
    assert!(xml.contains("<count>0</count>"));
}

// ─── Test 11: XML escaping ───

#[test]
fn test_xml_escaping_in_output() {
    let results = vec![EnrichedResult {
        score: 0.9,
        chunk: ConversationChunk {
            id: "esc-1".into(),
            conversation_id: "conv-1".into(),
            project_name: "test & <project>".into(),
            timestamp: "2026-01-15T10:00:00Z".into(),
            content: "Content with <script>alert('xss')</script> & \"quotes\"".into(),
            message_count: 1,
        },
    }];

    let xml = format::format_search_results(&results, "test <query>", "test & <project>", 0, 0);

    // Project name in XML attributes should be escaped
    assert!(xml.contains("test &amp; &lt;project&gt;"));
    // Query should be escaped
    assert!(xml.contains("test &lt;query&gt;"));
    // Content in CDATA is OK as-is (CDATA handles it)
}

// ─── Test 12: Temporal parsing ───

#[test]
fn test_temporal_roundtrip() {
    let (start, end) = temporal::parse_time_expression("last 7 days").unwrap();
    assert!(start < end);
    let diff = (end - start).num_days();
    assert!((diff - 7).abs() <= 1);
}

#[test]
fn test_temporal_iso_date() {
    let (start, end) = temporal::parse_time_expression("2026-01-15").unwrap();
    assert_eq!(start.format("%Y-%m-%d").to_string(), "2026-01-15");
    assert_eq!((end - start).num_days(), 1);
}

// ─── Test 13: Cross-project resolver ───

#[test]
fn test_cross_project_scope_all() {
    let (project, label) = cross_project::normalize_project_scope(Some("all"));
    assert!(project.is_none());
    assert_eq!(label, "all");
}

#[test]
fn test_cross_project_scope_all_case_insensitive() {
    let (project, _) = cross_project::normalize_project_scope(Some("ALL"));
    assert!(project.is_none());
    let (project, _) = cross_project::normalize_project_scope(Some("All"));
    assert!(project.is_none());
}

#[test]
fn test_cross_project_scope_specific() {
    let (project, label) = cross_project::normalize_project_scope(Some("my-project"));
    assert_eq!(project, Some("my-project".to_string()));
    assert_eq!(label, "my-project");
}

#[test]
fn test_cross_project_resolve_subdirectory() {
    let result =
        cross_project::resolve_project_from_cwd("/Users/name/projects/claude-self-reflect/src");
    assert_eq!(result, Some("claude-self-reflect".to_string()));
}

#[test]
fn test_cross_project_resolve_claude_dir_format() {
    let result = cross_project::resolve_project_from_cwd(
        "/Users/name/.claude/projects/-Users-name-projects-my-app",
    );
    assert_eq!(result, Some("my-app".to_string()));
}

// ─── Test 14: Import state tracking ───

#[test]
fn test_import_state() {
    let storage = Storage::open_memory().unwrap();
    let path = std::path::Path::new("/tmp/test-conv.jsonl");

    assert!(!storage.is_file_imported(path).unwrap());

    storage.mark_file_imported(path, 5).unwrap();
    assert!(storage.is_file_imported(path).unwrap());
}

// ─── Test 15: Recent chunks query ───

#[test]
fn test_recent_chunks() {
    let storage = Storage::open_memory().unwrap();
    let fake_emb: Vec<f32> = vec![0.0; 384];

    for i in 0..5 {
        let chunk = ConversationChunk {
            id: format!("recent-{}", i),
            conversation_id: format!("conv-{}", i),
            project_name: "test".into(),
            timestamp: format!("2026-01-{}T10:00:00Z", 15 + i),
            content: format!("Content {}", i),
            message_count: 1,
        };
        storage.insert_chunk(&chunk, &fake_emb).unwrap();
    }

    let recent = storage.get_recent_chunks(3, None).unwrap();
    assert_eq!(recent.len(), 3);
    // Should be in reverse chronological order
    assert!(recent[0].timestamp >= recent[1].timestamp);
    assert!(recent[1].timestamp >= recent[2].timestamp);
}

// ─── Test 16: Timeline grouping ───

#[test]
fn test_timeline_grouping() {
    let chunks = vec![
        ConversationChunk {
            id: "tl-1".into(),
            conversation_id: "c1".into(),
            project_name: "test".into(),
            timestamp: "2026-01-15T10:00:00Z".into(),
            content: "morning".into(),
            message_count: 1,
        },
        ConversationChunk {
            id: "tl-2".into(),
            conversation_id: "c2".into(),
            project_name: "test".into(),
            timestamp: "2026-01-15T14:00:00Z".into(),
            content: "afternoon".into(),
            message_count: 1,
        },
        ConversationChunk {
            id: "tl-3".into(),
            conversation_id: "c3".into(),
            project_name: "test".into(),
            timestamp: "2026-01-16T10:00:00Z".into(),
            content: "next day".into(),
            message_count: 1,
        },
    ];

    let groups = temporal::group_chunks_by_period(&chunks, "day");
    assert_eq!(groups.len(), 2);
    assert_eq!(groups["2026-01-15"].len(), 2);
    assert_eq!(groups["2026-01-16"].len(), 1);

    let hour_groups = temporal::group_chunks_by_period(&chunks, "hour");
    assert_eq!(hour_groups.len(), 3); // 10:00, 14:00, 10:00 next day
}

// ─── Test 17: Full pipeline (storage + search + format) ───

#[test]
fn test_full_pipeline_storage_search_format() {
    let storage = Storage::open_memory().unwrap();
    let mut search_engine = SearchEngine::new(100);

    // Insert chunks with known vectors
    let chunks = vec![
        ("pipeline-1", "Docker container memory limit fix", 0usize),
        ("pipeline-2", "JWT authentication implementation", 1),
        ("pipeline-3", "PostgreSQL query optimization", 2),
    ];

    for (id, content, dim) in &chunks {
        let chunk = ConversationChunk {
            id: id.to_string(),
            conversation_id: format!("conv-{}", id),
            project_name: "test".into(),
            timestamp: "2026-01-15T10:00:00Z".into(),
            content: content.to_string(),
            message_count: 2,
        };

        let mut embedding = vec![0.0f32; 384];
        embedding[*dim] = 1.0;

        storage.insert_chunk(&chunk, &embedding).unwrap();
        search_engine.insert_chunk(id.to_string(), embedding);
    }

    // Search for "Docker" (vector similar to pipeline-1)
    let mut query_vec = vec![0.0f32; 384];
    query_vec[0] = 1.0;

    let results = search_engine.search_chunks(&query_vec, 3, 0.0);
    assert!(!results.is_empty());
    assert_eq!(results[0].id, "pipeline-1");

    // Enrich and format
    let ids: Vec<String> = results.iter().map(|r| r.id.clone()).collect();
    let stored = storage.get_chunks_by_ids(&ids).unwrap();

    let enriched: Vec<EnrichedResult> = results
        .iter()
        .filter_map(|r| {
            stored
                .iter()
                .find(|c| c.id == r.id)
                .map(|c| EnrichedResult {
                    score: r.score,
                    chunk: c.clone(),
                })
        })
        .collect();

    let xml = format::format_search_results(&enriched, "docker", "test", 1, 1);
    assert!(xml.contains("<search>"));
    assert!(xml.contains("Docker container memory limit fix"));
}

// ─── Test 18: Embedding vectors round-trip through storage ───

#[test]
fn test_vector_storage_roundtrip() {
    let storage = Storage::open_memory().unwrap();

    let chunk = ConversationChunk {
        id: "vec-rt-1".into(),
        conversation_id: "conv-1".into(),
        project_name: "test".into(),
        timestamp: "2026-01-15T10:00:00Z".into(),
        content: "test content".into(),
        message_count: 1,
    };

    // Create a specific vector
    let embedding: Vec<f32> = (0..384).map(|i| (i as f32) * 0.001).collect();
    storage.insert_chunk(&chunk, &embedding).unwrap();

    // Load it back
    let loaded = storage.load_all_chunk_vectors().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].0, "vec-rt-1");

    // Verify the vector values are preserved
    for (orig, loaded) in embedding.iter().zip(loaded[0].1.iter()) {
        assert!(
            (orig - loaded).abs() < 1e-6,
            "vector mismatch: {} vs {}",
            orig,
            loaded
        );
    }
}

// ─── Test 19: HNSW dedup on re-insert ───

#[test]
fn test_hnsw_dedup_insert() {
    let mut engine = SearchEngine::new(100);

    let mut emb = vec![0.0f32; 384];
    emb[0] = 1.0;

    engine.insert_chunk("dup-1".to_string(), emb.clone());
    engine.insert_chunk("dup-1".to_string(), emb.clone()); // duplicate
    engine.insert_chunk("dup-2".to_string(), emb.clone());

    // Should have 2 unique chunks, not 3
    assert_eq!(engine.chunk_count(), 2);

    // Same for reflections
    engine.insert_reflection("ref-1".to_string(), emb.clone());
    engine.insert_reflection("ref-1".to_string(), emb.clone());
    assert_eq!(engine.reflection_count(), 1);
}
