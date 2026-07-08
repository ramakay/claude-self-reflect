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
    // Chunks use last_timestamp (most recent activity) for ordering
    assert_eq!(chunks[0].timestamp, "2026-01-15T10:03:00Z");
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
        summary: None,
        author: csr_engine::provenance::Speaker::ToolResult,
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
                summary: None,
                author: csr_engine::provenance::Speaker::ToolResult,
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
            summary: None,
            author: csr_engine::provenance::Speaker::ToolResult,
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
            summary: None,
            author: csr_engine::provenance::Speaker::ToolResult,
        },
        ConversationChunk {
            id: "fts-2".into(),
            conversation_id: "conv-2".into(),
            project_name: "test".into(),
            timestamp: "2026-01-16T10:00:00Z".into(),
            content: "Authentication was added using JWT tokens in auth.rs".into(),
            message_count: 2,
            summary: None,
            author: csr_engine::provenance::Speaker::ToolResult,
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
            summary: None,
            author: csr_engine::provenance::Speaker::ToolResult,
        },
    }];

    let xml = format::format_search_results(&results, "docker memory", "test-project", 5, 2);

    assert!(xml.contains("<search>"), "should have search tag");
    assert!(xml.contains("</search>"), "should close search tag");
    assert!(xml.contains("0.850"), "should contain score");
    assert!(xml.contains("test-project"), "should contain project name");
    assert!(
        xml.contains("Docker memory fix applied"),
        "should contain content"
    );
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
            summary: None,
            author: csr_engine::provenance::Speaker::ToolResult,
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
            summary: None,
            author: csr_engine::provenance::Speaker::ToolResult,
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
            summary: None,
            author: csr_engine::provenance::Speaker::ToolResult,
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
            summary: None,
            author: csr_engine::provenance::Speaker::ToolResult,
        },
        ConversationChunk {
            id: "tl-2".into(),
            conversation_id: "c2".into(),
            project_name: "test".into(),
            timestamp: "2026-01-15T14:00:00Z".into(),
            content: "afternoon".into(),
            message_count: 1,
            summary: None,
            author: csr_engine::provenance::Speaker::ToolResult,
        },
        ConversationChunk {
            id: "tl-3".into(),
            conversation_id: "c3".into(),
            project_name: "test".into(),
            timestamp: "2026-01-16T10:00:00Z".into(),
            content: "next day".into(),
            message_count: 1,
            summary: None,
            author: csr_engine::provenance::Speaker::ToolResult,
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
            summary: None,
            author: csr_engine::provenance::Speaker::ToolResult,
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
        summary: None,
        author: csr_engine::provenance::Speaker::ToolResult,
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

// ─── Test 19: Enrichment state tracking ───

#[test]
fn test_enrichment_state_crud() {
    let storage = Storage::open_memory().unwrap();

    // Initially not enriched
    assert!(!storage
        .is_conversation_enriched("conv-1", "heuristic")
        .unwrap());

    // Mark completed
    storage
        .mark_enrichment_completed("conv-1", "heuristic", "refl-heur-1")
        .unwrap();
    assert!(storage
        .is_conversation_enriched("conv-1", "heuristic")
        .unwrap());

    // Different type is not enriched
    assert!(!storage
        .is_conversation_enriched("conv-1", "extracted_v3")
        .unwrap());
}

#[test]
fn test_enrichment_state_idempotent() {
    let storage = Storage::open_memory().unwrap();

    // Mark completed twice — should not error
    storage
        .mark_enrichment_completed("conv-1", "heuristic", "refl-1")
        .unwrap();
    storage
        .mark_enrichment_completed("conv-1", "heuristic", "refl-2")
        .unwrap();
    assert!(storage
        .is_conversation_enriched("conv-1", "heuristic")
        .unwrap());
}

#[test]
fn test_enrichment_failure_tracking() {
    let storage = Storage::open_memory().unwrap();

    storage
        .mark_enrichment_failed("conv-1", "ai_narrative", "API timeout")
        .unwrap();
    // Failed enrichment is NOT considered complete
    assert!(!storage
        .is_conversation_enriched("conv-1", "ai_narrative")
        .unwrap());
}

#[test]
fn test_unavailable_enrichment_not_requeued() {
    // Regression: missing source JSONL must stop re-queuing every daemon tick.
    let storage = Storage::open_memory().unwrap();
    let fake_emb: Vec<f32> = vec![0.0; 384];

    // Two conversations, each with a chunk + import_state row so they qualify for the queue.
    for conv in ["conv-missing", "conv-failed"] {
        let chunk = ConversationChunk {
            id: format!("{conv}-chunk"),
            conversation_id: conv.into(),
            project_name: "test".into(),
            timestamp: "2026-01-15T10:00:00Z".into(),
            content: "content".into(),
            message_count: 1,
            summary: None,
            author: csr_engine::provenance::Speaker::ToolResult,
        };
        storage.insert_chunk(&chunk, &fake_emb).unwrap();
        // mark_file_imported derives conversation_id from the file stem.
        storage
            .mark_file_imported(std::path::Path::new(&format!("/tmp/{conv}.jsonl")), 1)
            .unwrap();
    }

    // Missing source → unavailable (permanent); transient → failed (retryable).
    storage
        .mark_enrichment_unavailable("conv-missing", "extracted_v3", "source file missing")
        .unwrap();
    storage
        .mark_enrichment_failed("conv-failed", "extracted_v3", "transient io error")
        .unwrap();

    let queued = storage
        .get_unenriched_conversations("extracted_v3", 10)
        .unwrap();
    let ids: Vec<&str> = queued.iter().map(|(id, _)| id.as_str()).collect();

    assert!(
        !ids.contains(&"conv-missing"),
        "unavailable conversation must NOT be re-queued"
    );
    assert!(
        ids.contains(&"conv-failed"),
        "failed conversation should still be retried"
    );
}

#[test]
fn test_batch_id_tracking() {
    let storage = Storage::open_memory().unwrap();

    storage
        .set_batch_id("conv-1", "batch_abc", "hash_123")
        .unwrap();
    storage
        .set_batch_id("conv-2", "batch_abc", "hash_123")
        .unwrap();

    let convs = storage.get_conversations_by_batch("batch_abc").unwrap();
    assert_eq!(convs.len(), 2);
    assert!(convs.contains(&"conv-1".to_string()));
    assert!(convs.contains(&"conv-2".to_string()));
}

#[test]
fn test_reflection_deletion() {
    let storage = Storage::open_memory().unwrap();
    let fake_emb: Vec<f32> = vec![0.1; 384];

    storage
        .insert_reflection("to-delete", "content", &["tag1".into()], &fake_emb)
        .unwrap();
    assert!(storage.get_reflection_by_id("to-delete").unwrap().is_some());

    storage.delete_reflection("to-delete").unwrap();
    assert!(storage.get_reflection_by_id("to-delete").unwrap().is_none());
}

#[test]
fn test_enrichment_reflection_id_lookup() {
    let storage = Storage::open_memory().unwrap();

    // No enrichment yet
    assert!(storage
        .get_enrichment_reflection_id("conv-1", "heuristic")
        .unwrap()
        .is_none());

    // Mark completed
    storage
        .mark_enrichment_completed("conv-1", "heuristic", "refl-h-1")
        .unwrap();
    let id = storage
        .get_enrichment_reflection_id("conv-1", "heuristic")
        .unwrap();
    assert_eq!(id, Some("refl-h-1".to_string()));
}

// ─── Test 20: V3 extraction with fixture ───

#[test]
fn test_v3_extraction_with_fixture() {
    use csr_engine::extraction;

    let file = fixtures_dir().join("sample_conversation.jsonl");
    let messages = import::parse_jsonl_messages(&file).unwrap();
    assert!(!messages.is_empty());

    let result = extraction::extract_v3(&messages);
    assert!(result.stats.original_messages > 0);
    assert!(!result.search_index.is_empty());
    assert!(!result.context_cache.is_empty());
}

// ─── Test 21: Heuristic enrichment with fixture ───

#[test]
fn test_heuristic_enrichment_with_fixture() {
    use csr_engine::extraction::heuristic;

    let file = fixtures_dir().join("sample_conversation.jsonl");
    let messages = import::parse_jsonl_messages(&file).unwrap();
    assert!(!messages.is_empty());

    let result = heuristic::extract_heuristic(&messages);
    assert!(result.message_count > 0);
    assert!(result.user_message_count > 0);

    let reflection = heuristic::format_as_reflection(&result, "test-project");
    assert!(reflection.contains("[Heuristic]"));
    assert!(reflection.contains("test-project"));
}

// ─── Test 22: Completions — project name prefix matching ───

#[test]
fn test_completions_project_name_prefix() {
    let storage = Storage::open_memory().unwrap();

    // Insert chunks with known project names
    let chunk1 = ConversationChunk {
        id: "chunk-comp-1".to_string(),
        conversation_id: "conv-comp-1".to_string(),
        project_name: "claude-self-reflect".to_string(),
        timestamp: "2026-05-06T10:00:00Z".to_string(),
        content: "test content".to_string(),
        message_count: 5,
        summary: None,
        author: csr_engine::provenance::Speaker::ToolResult,
    };
    let chunk2 = ConversationChunk {
        id: "chunk-comp-2".to_string(),
        conversation_id: "conv-comp-2".to_string(),
        project_name: "claude-code-hooks".to_string(),
        timestamp: "2026-05-06T11:00:00Z".to_string(),
        content: "other content".to_string(),
        message_count: 3,
        summary: None,
        author: csr_engine::provenance::Speaker::ToolResult,
    };
    let chunk3 = ConversationChunk {
        id: "chunk-comp-3".to_string(),
        conversation_id: "conv-comp-3".to_string(),
        project_name: "my-other-project".to_string(),
        timestamp: "2026-05-06T12:00:00Z".to_string(),
        content: "unrelated".to_string(),
        message_count: 2,
        summary: None,
        author: csr_engine::provenance::Speaker::ToolResult,
    };

    let embedding = vec![0.1f32; 384];
    storage.insert_chunk(&chunk1, &embedding).unwrap();
    storage.insert_chunk(&chunk2, &embedding).unwrap();
    storage.insert_chunk(&chunk3, &embedding).unwrap();

    // Acceptance criterion: partial "clau" returns both claude-* projects
    let results = storage.list_project_names("clau", 100).unwrap();
    assert!(
        results.contains(&"claude-self-reflect".to_string()),
        "should contain claude-self-reflect, got: {:?}",
        results
    );
    assert!(
        results.contains(&"claude-code-hooks".to_string()),
        "should contain claude-code-hooks"
    );
    assert!(
        !results.contains(&"my-other-project".to_string()),
        "should NOT contain my-other-project"
    );

    // Empty prefix returns all
    let all = storage.list_project_names("", 100).unwrap();
    assert_eq!(all.len(), 3);

    // Non-matching prefix returns empty
    let none = storage.list_project_names("xyz", 100).unwrap();
    assert!(none.is_empty());
}

// ─── Test 23: Completions — session ID lookup ───

#[test]
fn test_completions_session_id_prefix() {
    let storage = Storage::open_memory().unwrap();
    // Session IDs are stored in iteration_learnings — no data means empty results
    let results = storage.list_session_ids("abc", 100).unwrap();
    assert!(results.is_empty());
}

// ─── Test 24: Search engine remove_reflection ───

#[test]
fn test_search_engine_remove_reflection() {
    let mut engine = SearchEngine::new(100);

    let mut emb = vec![0.0f32; 384];
    emb[0] = 1.0;

    engine.insert_reflection("refl-keep".to_string(), emb.clone());
    engine.insert_reflection("refl-remove".to_string(), emb.clone());
    assert_eq!(engine.reflection_count(), 2);

    engine.remove_reflection("refl-remove");

    // After removal, searching should not return the removed reflection
    let results = engine.search_reflections(&emb, 10, 0.0);
    for r in &results {
        assert_ne!(r.id, "refl-remove", "removed reflection should not appear");
    }
}

// ─── Test 23: parse_jsonl_messages ───

#[test]
fn test_parse_jsonl_messages() {
    let file = fixtures_dir().join("sample_conversation.jsonl");
    let messages = import::parse_jsonl_messages(&file).unwrap();
    assert!(!messages.is_empty());
    // Each message should have a type field
    for msg in &messages {
        let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            msg_type == "human" || msg_type == "assistant",
            "unexpected type: {msg_type}"
        );
    }
}

// ─── Test 24: HNSW dedup on re-insert ───

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

// ─── Test 25: HNSW persistence round-trip ───

#[test]
fn test_hnsw_persistence_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let index_dir = dir.path().join("index");

    // Build index with 50 chunks + 5 reflections
    let mut engine = SearchEngine::new(100);
    for i in 0..50 {
        let mut emb = vec![0.0f32; 384];
        emb[i % 384] = 1.0;
        engine.insert_chunk(format!("chunk-{}", i), emb);
    }
    for i in 0..5 {
        let mut emb = vec![0.0f32; 384];
        emb[(i + 50) % 384] = 1.0;
        engine.insert_reflection(format!("refl-{}", i), emb);
    }

    assert!(engine.is_dirty());
    engine.dump_to_disk(&index_dir, 50, 5).unwrap();
    assert!(!engine.is_dirty());

    // Search before dump
    let mut query = vec![0.0f32; 384];
    query[0] = 1.0;
    let results_before = engine.search_chunks(&query, 3, 0.0);

    // Load from disk
    let loaded = SearchEngine::load_from_disk(&index_dir, 50, 5).unwrap();
    assert_eq!(loaded.chunk_count(), 50);
    assert_eq!(loaded.reflection_count(), 5);

    // Search after load — same results
    let results_after = loaded.search_chunks(&query, 3, 0.0);
    assert_eq!(results_before.len(), results_after.len());
    assert_eq!(results_before[0].id, results_after[0].id);
    assert!((results_before[0].score - results_after[0].score).abs() < 0.01);
}

// ─── Test 26: Stale index detection ───

#[test]
fn test_hnsw_stale_index_detection() {
    let dir = tempfile::tempdir().unwrap();
    let index_dir = dir.path().join("index");

    // Build and dump with 10 chunks
    let mut engine = SearchEngine::new(100);
    for i in 0..10 {
        let emb = vec![0.1f32; 384];
        engine.insert_chunk(format!("chunk-{}", i), emb);
    }
    engine.dump_to_disk(&index_dir, 10, 0).unwrap();

    // DB grew since the dump (additive drift) — cache loads so Engine::new can
    // incrementally backfill the new rows instead of doing a full rebuild.
    let result = SearchEngine::load_from_disk(&index_dir, 15, 0);
    assert!(
        result.is_some(),
        "additive drift should load the cache (backfill), not rebuild"
    );

    // DB shrank (rows deleted) — must reject so the rebuild drops orphan vectors.
    let result = SearchEngine::load_from_disk(&index_dir, 5, 0);
    assert!(result.is_none(), "deletion should reject the stale cache");

    // Exact match should work
    let result = SearchEngine::load_from_disk(&index_dir, 10, 0);
    assert!(result.is_some(), "should accept matching cache");
}

// ─── Test 27: Missing files fallback ───

#[test]
fn test_hnsw_missing_files_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("nonexistent_index");

    let result = SearchEngine::load_from_disk(&nonexistent, 100, 10);
    assert!(result.is_none(), "should return None for missing directory");
}

// ─── Test 28: Soft-deleted reflections survive persistence ───

#[test]
fn test_hnsw_persistence_soft_delete() {
    let dir = tempfile::tempdir().unwrap();
    let index_dir = dir.path().join("index");

    // Insert 5 reflections, remove 1
    let mut engine = SearchEngine::new(100);
    for i in 0..5 {
        let mut emb = vec![0.0f32; 384];
        emb[i % 384] = 1.0;
        engine.insert_reflection(format!("refl-{}", i), emb);
    }
    assert_eq!(engine.reflection_count(), 5);

    engine.remove_reflection("refl-2");
    assert_eq!(engine.reflection_count(), 4);

    // Dump and reload — pass DB counts (0 chunks, 5 reflections in DB)
    engine.dump_to_disk(&index_dir, 0, 5).unwrap();

    // Load expects DB counts to match manifest
    let loaded = SearchEngine::load_from_disk(&index_dir, 0, 5).unwrap();
    assert_eq!(loaded.reflection_count(), 4);

    // Searching should not return the removed reflection
    let mut query = vec![0.0f32; 384];
    query[2] = 1.0; // Most similar to refl-2
    let results = loaded.search_reflections(&query, 10, 0.0);
    for r in &results {
        assert_ne!(
            r.id, "refl-2",
            "removed reflection should not appear after reload"
        );
    }
}

// ─── Test 29: Cached load timing ───

#[test]
fn test_hnsw_cached_load_timing() {
    let dir = tempfile::tempdir().unwrap();
    let index_dir = dir.path().join("index");

    // Build a 1000-vector index
    let mut engine = SearchEngine::new(2000);
    for i in 0..1000 {
        let mut emb = vec![0.0f32; 384];
        emb[i % 384] = (i as f32 + 1.0) / 1000.0;
        engine.insert_chunk(format!("perf-chunk-{}", i), emb);
    }
    engine.dump_to_disk(&index_dir, 1000, 0).unwrap();

    // Time the cached load
    let start = std::time::Instant::now();
    let loaded = SearchEngine::load_from_disk(&index_dir, 1000, 0);
    let elapsed = start.elapsed();

    assert!(loaded.is_some(), "cached load should succeed");
    assert!(
        elapsed.as_millis() < 500,
        "cached load took {}ms, expected <500ms",
        elapsed.as_millis()
    );
}

// ─── LAPI: Phase-Aware Scoring ───

#[test]
fn test_lapi_phase_aware_scoring() {
    use csr_engine::injection::predictor::{self, RawResult};
    use csr_engine::injection::weights::HookPhase;

    // Create a chunk and a reflection with same semantic score
    let results = vec![
        RawResult {
            content: "docker container fix".into(),
            score: 0.8,
            source: "chunk".into(),
            timestamp: None,
            files: vec![],
            error_patterns: vec![],
            tags: vec![],
            conversation_id: None,
            memory_id: None,
        },
        RawResult {
            content: "session strategy for docker".into(),
            score: 0.8,
            source: "reflection".into(),
            timestamp: None,
            files: vec![],
            error_patterns: vec![],
            tags: vec!["outcome_completed".to_string()],
            conversation_id: None,
            memory_id: None,
        },
    ];

    // PromptSubmit: chunks should rank higher (semantic weight dominates)
    let prompt_scored =
        predictor::rank_results(results.clone(), &[], &[], Some(HookPhase::PromptSubmit));
    // Chunk gets phase_boost=0.8, reflection gets phase_boost=0.5
    // With PromptSubmit weights: semantic=0.40 dominates, but phase_boost=0.15 still helps chunk
    assert_eq!(
        prompt_scored[0].source, "chunk",
        "PromptSubmit should prefer chunks"
    );

    // SessionStart: reflection with outcome tag should rank higher
    let start_scored =
        predictor::rank_results(results.clone(), &[], &[], Some(HookPhase::SessionStart));
    // Reflection with outcome_completed gets phase_boost=1.0, chunk gets 0.2
    // SessionStart phase_boost weight=0.40 → reflection wins
    assert_eq!(
        start_scored[0].source, "reflection",
        "SessionStart should prefer outcome reflections"
    );
}

#[test]
fn test_lapi_stop_phase_prefers_anti_patterns() {
    use csr_engine::injection::predictor::{self, RawResult};
    use csr_engine::injection::weights::HookPhase;

    let results = vec![
        RawResult {
            content: "regular chunk content".into(),
            score: 0.8,
            source: "chunk".into(),
            timestamp: None,
            files: vec![],
            error_patterns: vec![],
            tags: vec![],
            conversation_id: None,
            memory_id: None,
        },
        RawResult {
            content: "failed approach for this problem".into(),
            score: 0.8,
            source: "anti_pattern".into(),
            timestamp: None,
            files: vec![],
            error_patterns: vec![],
            tags: vec![],
            conversation_id: None,
            memory_id: None,
        },
    ];

    let scored = predictor::rank_results(results, &[], &[], Some(HookPhase::Stop));
    assert_eq!(
        scored[0].source, "anti_pattern",
        "Stop phase should prefer anti-patterns"
    );
}

// ─── TAD: Temporal Attention Decay ───

#[test]
fn test_tad_reinforcement_changes_ranking() {
    use chrono::{Duration, Utc};
    use csr_engine::search::decay::{apply_tad, DecayConfig, RetrievalEvent, SessionOutcome};

    let now = Utc::now();
    let memory_age = now - Duration::days(60);
    let config = DecayConfig::for_search();

    // Baseline: no retrieval events
    let baseline = apply_tad(1.0, &memory_age, &now, &[], &config);

    // Successful retrieval: memory should be preserved longer
    let success_events = vec![RetrievalEvent {
        retrieved_at: now - Duration::days(5),
        session_outcome: SessionOutcome::Success,
    }];
    let reinforced = apply_tad(1.0, &memory_age, &now, &success_events, &config);
    assert!(
        reinforced > baseline,
        "reinforced={} > baseline={}",
        reinforced,
        baseline
    );

    // Failed retrieval: memory should decay faster
    let fail_events = vec![RetrievalEvent {
        retrieved_at: now - Duration::days(5),
        session_outcome: SessionOutcome::Failed,
    }];
    let suppressed = apply_tad(1.0, &memory_age, &now, &fail_events, &config);
    assert!(
        suppressed < baseline,
        "suppressed={} < baseline={}",
        suppressed,
        baseline
    );

    // Order: reinforced > baseline > suppressed
    assert!(
        reinforced > baseline && baseline > suppressed,
        "Expected reinforced({}) > baseline({}) > suppressed({})",
        reinforced,
        baseline,
        suppressed
    );
}

// ─── TAD: Storage Integration ───

#[test]
fn test_tad_retrieval_events_storage() {
    let storage = Storage::open_memory().unwrap();

    // Log a retrieval event
    storage
        .log_retrieval_event("mem_123", "chunk", "prompt_submit", "session_abc")
        .unwrap();

    // Log another for the same session
    storage
        .log_retrieval_event("mem_456", "reflection", "prompt_submit", "session_abc")
        .unwrap();

    // Query events for a memory
    let events = storage.get_retrieval_events_for_memory("mem_123").unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1, "neutral"); // default outcome

    // Update session outcome
    let updated = storage
        .update_session_outcome("session_abc", "success")
        .unwrap();
    assert_eq!(updated, 2); // both events updated

    // Verify outcome was updated
    let events = storage.get_retrieval_events_for_memory("mem_123").unwrap();
    assert_eq!(events[0].1, "success");

    let events = storage.get_retrieval_events_for_memory("mem_456").unwrap();
    assert_eq!(events[0].1, "success");
}

// ─── Unified Decay Config ───

#[test]
fn test_decay_config_injection_vs_search() {
    use chrono::{Duration, Utc};
    use csr_engine::search::decay::{apply_tad, DecayConfig};

    let now = Utc::now();
    let age_30_days = now - Duration::days(30);

    let injection_config = DecayConfig::for_injection();
    let search_config = DecayConfig::for_search();

    let injection_score = apply_tad(1.0, &age_30_days, &now, &[], &injection_config);
    let search_score = apply_tad(1.0, &age_30_days, &now, &[], &search_config);

    // Injection decays faster (30-day half-life, 50% weight)
    // Search decays slower (90-day half-life, 30% weight)
    assert!(
        injection_score < search_score,
        "injection({}) should decay faster than search({})",
        injection_score,
        search_score
    );
}

// ─── Test: get_imported_chunk_count ───

#[test]
fn test_get_imported_chunk_count() {
    let storage = Storage::open_memory().unwrap();
    let path = std::path::Path::new("/tmp/test-incr.jsonl");

    // Never imported = 0
    assert_eq!(storage.get_imported_chunk_count(path).unwrap(), 0);

    // After marking with 5 chunks
    storage.mark_file_imported(path, 5).unwrap();
    assert_eq!(storage.get_imported_chunk_count(path).unwrap(), 5);

    // Update to 10 chunks (transcript grew)
    storage.mark_file_imported(path, 10).unwrap();
    assert_eq!(storage.get_imported_chunk_count(path).unwrap(), 10);
}

// ─── Test: get_chunk_content ───

#[test]
fn test_get_chunk_content() {
    let storage = Storage::open_memory().unwrap();
    let fake_emb: Vec<f32> = vec![0.0; 384];

    // Missing chunk
    assert!(storage.get_chunk_content("nonexistent").unwrap().is_none());

    // Insert and retrieve
    let chunk = ConversationChunk {
        id: "content-test-1".into(),
        conversation_id: "conv-ct".into(),
        project_name: "test".into(),
        timestamp: "2026-02-22T10:00:00Z".into(),
        content: "This is the chunk content for testing retrieval".into(),
        message_count: 5,
        summary: Some("Test summary".into()),
        author: csr_engine::provenance::Speaker::ToolResult,
    };
    storage.insert_chunk(&chunk, &fake_emb).unwrap();

    let content = storage.get_chunk_content("content-test-1").unwrap();
    assert_eq!(
        content.as_deref(),
        Some("This is the chunk content for testing retrieval")
    );
}

// ─── Test: Batch TAD retrieval events ───

#[test]
fn test_tad_batch_retrieval_events() {
    let storage = Storage::open_memory().unwrap();

    let chunk = ConversationChunk {
        id: "chunk-tad-1".into(),
        conversation_id: "conv-tad-1".into(),
        project_name: "test".into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        content: "test content".into(),
        message_count: 1,
        summary: None,
        author: csr_engine::provenance::Speaker::ToolResult,
    };
    storage.insert_chunk(&chunk, &[0.1; 384]).unwrap();
    storage
        .log_retrieval_event("chunk-tad-1", "chunk", "prompt_submit", "session-1")
        .unwrap();
    storage
        .update_session_outcome("session-1", "success")
        .unwrap();

    let events = storage
        .get_retrieval_events_batch(&["chunk-tad-1", "nonexistent"])
        .unwrap();
    assert!(events.contains_key("chunk-tad-1"));
    assert!(!events.contains_key("nonexistent"));
    let chunk_events = &events["chunk-tad-1"];
    assert_eq!(chunk_events.len(), 1);
    assert_eq!(
        chunk_events[0].session_outcome,
        csr_engine::search::decay::SessionOutcome::Success
    );
}

#[test]
fn episode_v2_full_cycle_with_anchors() {
    // Arrange: temp project with one Rust source file
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("auth.rs");
    std::fs::write(&src, "fn validate_token(t: &str) -> bool { t.len() > 8 }\n").unwrap();

    // Transcript that modified that file via Edit + left a todo
    let transcript = [
        r#"{"type":"user","message":{"content":"fix token validation"}}"#.to_string(),
        format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Edit","input":{{"file_path":"{}"}}}}]}}}}"#,
            src.display()
        ),
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"add test","status":"pending"}]}}]}}"#.to_string(),
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done, fixed."}]}}"#.to_string(),
    ];
    let lines: Vec<&str> = transcript.iter().map(|s| s.as_str()).collect();

    // Act: extract
    let ep = csr_engine::hooks::stop::extract_episode(&lines, "it-sess", "it-proj");

    // Assert v2 fields
    assert_eq!(ep.schema, "v2");
    assert_eq!(ep.todos.len(), 1);
    assert_eq!(ep.files_modified.len(), 1);

    // Anchor capture + graded verification
    let anchors = csr_engine::extraction::anchors::capture_file_anchors(&src);
    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0].name, "validate_token");
    assert_eq!(
        csr_engine::extraction::anchors::verify_anchor(&anchors[0], dir.path()),
        csr_engine::extraction::anchors::AnchorVerdict::Intact
    );
    std::fs::write(
        &src,
        "fn validate_token(t: &str) -> bool { t.len() > 99 }\n",
    )
    .unwrap();
    assert_eq!(
        csr_engine::extraction::anchors::verify_anchor(&anchors[0], dir.path()),
        csr_engine::extraction::anchors::AnchorVerdict::Modified
    );
}

// ─── v9.4 code property graph: LIVE round-trip (gate) ───

mod codegraph_roundtrip {
    use csr_engine::extraction::codegraph as cg;
    use csr_engine::storage::Storage;

    const PROJECT: &str = "proj";
    const REPO: &str = "proj";
    const FILE: &str = "src/demo.rs";

    /// Mirror the post_tool_use liveness path: extract → upsert → replace edges →
    /// resolve → rank. Also record a code_evolution row so the ledger timeline
    /// reflects the edit (as the real hook does).
    fn simulate_edit(storage: &Storage, source: &str, conv_id: &str, session_id: &str) {
        let frag =
            cg::extract_graph_fragment_for_file(source, FILE, REPO, PROJECT, conv_id, session_id);
        for node in &frag.nodes {
            storage.upsert_code_node(node).unwrap();
        }
        storage
            .replace_code_file_edges(PROJECT, FILE, &frag.edges)
            .unwrap();
        storage
            .insert_code_evolution(
                session_id,
                PROJECT,
                FILE,
                "rust",
                "Edit",
                "[\"foo\"]",
                "[]",
                "[]",
                "[]",
                "[]",
                "[]",
            )
            .unwrap();
        storage.resolve_code_edges(PROJECT).unwrap();
        storage.compute_code_rank(PROJECT).unwrap();
    }

    #[test]
    fn live_roundtrip_holds_history_across_edits() {
        let storage = Storage::open_memory().unwrap();

        // (b) First edit: foo calls bar; both defined in the file.
        let v1 = "fn foo() {\n    bar();\n}\nfn bar() {}\n";
        simulate_edit(&storage, v1, "conv_1", "sess_1");

        // (d) foo and bar appear as nodes.
        let foo_id = cg::node_id(REPO, FILE, "function", "foo");
        let bar_id = cg::node_id(REPO, FILE, "function", "bar");
        let ledger = storage.code_file_ledger(PROJECT, FILE).unwrap();
        let names: Vec<&str> = ledger.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"foo"), "foo node present: {names:?}");
        assert!(names.contains(&"bar"), "bar node present: {names:?}");

        // calls edge foo -> bar exists (resolved).
        let callees = storage.code_query_callees(&foo_id, 10).unwrap();
        assert!(
            callees.iter().any(|c| c.id == bar_id),
            "calls edge foo -> bar must exist: {:?}",
            callees.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
        let callers = storage.code_query_callers("bar", PROJECT, 10).unwrap();
        assert!(
            callers.iter().any(|c| c.name == "foo"),
            "bar's callers must include foo"
        );

        // file_ledger holds the conv_1 provenance.
        let foo_sym = ledger.symbols.iter().find(|s| s.name == "foo").unwrap();
        assert_eq!(foo_sym.first_conv_id, "conv_1", "history held: first conv");
        assert_eq!(foo_sym.last_conv_id, "conv_1");
        let h1 = foo_sym.body_hash.clone();

        // (e) Second edit changes foo's BODY (still calls bar) under conv_2.
        let v2 = "fn foo() {\n    let x = 42;\n    bar();\n}\nfn bar() {}\n";
        simulate_edit(&storage, v2, "conv_2", "sess_2");

        let ledger2 = storage.code_file_ledger(PROJECT, FILE).unwrap();
        let foo2 = ledger2.symbols.iter().find(|s| s.name == "foo").unwrap();

        // New state: body hash changed, last conv advanced to conv_2.
        assert_ne!(foo2.body_hash, h1, "new body recorded");
        assert_eq!(foo2.last_conv_id, "conv_2", "new state: last conv");
        // Prior conv still in history: first_conv_id immutable across edits.
        assert_eq!(
            foo2.first_conv_id, "conv_1",
            "immutable history holds across edits"
        );
        // Timeline shows BOTH edits.
        assert_eq!(ledger2.timeline.len(), 2, "both edits in timeline");

        // The graph still resolves foo -> bar after the second edit (liveness).
        let callees2 = storage.code_query_callees(&foo_id, 10).unwrap();
        assert!(
            callees2.iter().any(|c| c.id == bar_id),
            "foo -> bar still live"
        );

        // Print the load-bearing assertions proving history is held.
        println!(
            "ROUNDTRIP PROOF: foo first_conv={} last_conv={} body_changed={} timeline_entries={} foo->bar_resolved={}",
            foo2.first_conv_id,
            foo2.last_conv_id,
            foo2.body_hash != h1,
            ledger2.timeline.len(),
            callees2.iter().any(|c| c.id == bar_id),
        );
    }
}
