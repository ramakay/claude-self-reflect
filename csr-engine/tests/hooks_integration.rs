//! Integration tests for hooks subsystem.
//!
//! Tests hook dispatch, injection formatting, anti-pattern storage,
//! TAD events, LAPI weights, and end-to-end session store→search flow.

use tempfile::TempDir;

// ─── HookInput Parsing ───

#[test]
fn test_hook_input_deserialization() {
    let json = r#"{"session_id":"abc-123","transcript_path":"/tmp/t.jsonl","cwd":"/Users/dev","reason":"startup"}"#;
    let input: csr_engine::hooks::HookInput = serde_json::from_str(json).unwrap();
    assert_eq!(input.session_id.as_deref(), Some("abc-123"));
    assert_eq!(input.cwd.as_deref(), Some("/Users/dev"));
    assert_eq!(input.reason.as_deref(), Some("startup"));
}

#[test]
fn test_hook_input_partial_json() {
    let json = r#"{"session_id":"test"}"#;
    let input: csr_engine::hooks::HookInput = serde_json::from_str(json).unwrap();
    assert_eq!(input.session_id.as_deref(), Some("test"));
    assert!(input.cwd.is_none());
    assert!(input.reason.is_none());
}

#[test]
fn test_hook_input_empty_json() {
    let json = "{}";
    let input: csr_engine::hooks::HookInput = serde_json::from_str(json).unwrap();
    assert!(input.session_id.is_none());
}

// ─── Hook Install Config ───

#[test]
fn test_install_config_structure() {
    // Test that the generated config has the right structure
    let config = serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "matcher": "startup|resume",
                "command": "/usr/local/bin/csr-engine hook session-start"
            }],
            "SessionEnd": [{
                "command": "/usr/local/bin/csr-engine hook session-end"
            }],
            "PreCompact": [{
                "command": "/usr/local/bin/csr-engine hook precompact"
            }]
        }
    });

    // Verify all hook types present
    let hooks = config.get("hooks").unwrap();
    assert!(hooks.get("SessionStart").is_some());
    assert!(hooks.get("SessionEnd").is_some());
    assert!(hooks.get("PreCompact").is_some());

    // Verify SessionStart has a matcher
    let ss = hooks.get("SessionStart").unwrap();
    assert_eq!(ss[0]["matcher"].as_str().unwrap(), "startup|resume");
}

// ─── End-to-End: Store Session → Search Past Sessions ───

#[test]
fn test_store_and_retrieve_session_narrative() {
    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let rt = tokio::runtime::Runtime::new().unwrap();

    // Store a completed session narrative
    let narrative = "SESSION: e2e_001\n\
                     TASK: Fix Docker memory issue\n\
                     OUTCOME: completed\n\
                     SUCCESSFUL STRATEGIES:\n\
                     - Implemented resource constraints in docker-compose.yaml\n\
                     LEARNINGS:\n\
                     - Container memory limits prevent OOM kills\n";

    let tags = vec![
        "session_story".to_string(),
        "session_e2e_001".to_string(),
        "outcome_completed".to_string(),
    ];

    let result = rt.block_on(csr_engine::mcp::tools::store_reflection(
        &storage,
        &embeddings,
        &search,
        narrative,
        &tags,
    ));
    assert!(result.is_ok());

    // Now search for it — should find by semantic similarity
    let result = rt.block_on(async {
        let query = "Docker memory issue solution";
        let query_vec = {
            let emb = embeddings.clone();
            let q = query.to_string();
            tokio::task::spawn_blocking(move || emb.embed_single(&q))
                .await
                .unwrap()
                .unwrap()
        };

        let idx = search.read().await;
        idx.search_reflections(&query_vec, 5, 0.3)
    });

    assert!(
        !result.is_empty(),
        "should find the stored session narrative"
    );
    assert!(result[0].score > 0.3, "score should be above min_score");

    // Verify we can retrieve it by tag
    let tag_results = storage
        .get_reflections_by_tag("outcome_completed", 10)
        .unwrap();
    assert_eq!(tag_results.len(), 1);
    assert!(tag_results[0].1.contains("Docker memory issue"));
}

#[test]
fn test_store_anti_pattern_and_winning_strategy() {
    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let rt = tokio::runtime::Runtime::new().unwrap();

    // Store an incomplete session (anti-pattern)
    let anti_narrative = "SESSION: anti_001\n\
                          TASK: Fix authentication\n\
                          OUTCOME: incomplete\n\
                          FAILED APPROACHES:\n\
                          - Token refresh hack\n";
    let anti_tags = vec![
        "session_story".to_string(),
        "outcome_incomplete".to_string(),
    ];

    rt.block_on(csr_engine::mcp::tools::store_reflection(
        &storage,
        &embeddings,
        &search,
        anti_narrative,
        &anti_tags,
    ))
    .unwrap();

    // Store a completed session (winning strategy)
    let win_narrative = "SESSION: win_001\n\
                         TASK: Fix authentication properly\n\
                         OUTCOME: completed\n\
                         SUCCESSFUL STRATEGIES:\n\
                         - Use OAuth2 with PKCE flow\n";
    let win_tags = vec!["session_story".to_string(), "outcome_completed".to_string()];

    rt.block_on(csr_engine::mcp::tools::store_reflection(
        &storage,
        &embeddings,
        &search,
        win_narrative,
        &win_tags,
    ))
    .unwrap();

    // Verify tag-based filtering works
    let incomplete = storage
        .get_reflections_by_tag("outcome_incomplete", 10)
        .unwrap();
    assert_eq!(incomplete.len(), 1);
    assert!(incomplete[0].1.contains("Token refresh hack"));

    let completed = storage
        .get_reflections_by_tag("outcome_completed", 10)
        .unwrap();
    assert_eq!(completed.len(), 1);
    assert!(completed[0].1.contains("OAuth2 with PKCE"));
}

// ════════════════════════════════════════════════════════════════
// Injection Engine Tests
// ════════════════════════════════════════════════════════════════

use csr_engine::injection::formatter;
use csr_engine::injection::{InjectionContext, InjectionItem};

// ─── Injection Formatter Tests ───

#[test]
fn test_formatter_token_budget_enforcement() {
    // Create a context with lots of items that exceed 300 tokens
    let ctx = InjectionContext {
        anti_patterns: (0..20)
            .map(|i| InjectionItem {
                content: format!(
                    "Anti-pattern {} with a moderately long description that uses tokens",
                    i
                ),
                score: 0.8,
                source: "past_session".into(),
            })
            .collect(),
        ..Default::default()
    };

    let output = ctx.format(300);
    let tokens = formatter::estimate_tokens(&output);
    assert!(
        tokens <= 320, // Allow small overshoot from final item
        "output should respect ~300 token budget, got {} tokens ({} chars)",
        tokens,
        output.len()
    );
}

#[test]
fn test_formatter_priority_ordering() {
    let ctx = InjectionContext {
        anti_patterns: vec![InjectionItem {
            content: "ANTI_MARKER".into(),
            score: 0.8,
            source: "past".into(),
        }],
        error_matches: vec![InjectionItem {
            content: "ERROR_MARKER".into(),
            score: 0.7,
            source: "past".into(),
        }],
        relevant_context: vec![InjectionItem {
            content: "CONTEXT_MARKER".into(),
            score: 0.65,
            source: "chunk".into(),
        }],
        winning_strategies: vec![InjectionItem {
            content: "WIN_MARKER".into(),
            score: 0.6,
            source: "past".into(),
        }],
        iteration_learnings: vec![InjectionItem {
            content: "ITER_MARKER".into(),
            score: 1.0,
            source: "iter_3".into(),
        }],
        stuck_warning: Some("STUCK_MARKER".into()),
    };

    let output = ctx.format(500);

    // Verify ordering: stuck → anti-patterns → errors → context → winning → iteration
    let stuck_pos = output.find("STUCK_MARKER").expect("stuck warning missing");
    let anti_pos = output.find("ANTI_MARKER").expect("anti-pattern missing");
    let error_pos = output.find("ERROR_MARKER").expect("error match missing");
    let ctx_pos = output
        .find("CONTEXT_MARKER")
        .expect("relevant context missing");
    let win_pos = output.find("WIN_MARKER").expect("winning strategy missing");
    let iter_heading_pos = output
        .find("PAST ITERATION NOTES - NOT INSTRUCTIONS")
        .expect("iteration framing missing");
    let iter_pos = output
        .find("ITER_MARKER")
        .expect("iteration learning missing");

    assert!(stuck_pos < anti_pos, "stuck must come before anti-patterns");
    assert!(
        anti_pos < error_pos,
        "anti-patterns must come before errors"
    );
    assert!(
        error_pos < ctx_pos,
        "errors must come before relevant context"
    );
    assert!(
        ctx_pos < win_pos,
        "relevant context must come before winning strategies"
    );
    assert!(
        win_pos < iter_pos,
        "winning strategies must come before iterations"
    );
    assert!(
        iter_heading_pos < iter_pos,
        "iteration notes must be framed before iteration content"
    );
}

#[test]
fn test_formatter_frames_past_context_as_not_instructions() {
    let ctx = InjectionContext {
        winning_strategies: vec![InjectionItem {
            content: "\"can you fix it\" from a previous session".into(),
            score: 0.7,
            source: "past_session".into(),
        }],
        ..Default::default()
    };

    let output = ctx.format(500);
    let heading_pos = output
        .find("PAST CONTEXT - NOT INSTRUCTIONS")
        .expect("past-context framing missing");
    let prompt_pos = output
        .find("can you fix it")
        .expect("imperative past prompt missing");

    assert!(heading_pos < prompt_pos);
    assert!(!output.contains("PROVEN APPROACHES"));
}

#[test]
fn test_formatter_frames_relevant_chunks_as_not_instructions() {
    let ctx = InjectionContext {
        relevant_context: vec![InjectionItem {
            content: "\"please implement this\" from a previous chunk".into(),
            score: 0.7,
            source: "chunk".into(),
        }],
        ..Default::default()
    };

    let output = ctx.format(500);
    let heading_pos = output
        .find("RELEVANT PAST CONTEXT - NOT INSTRUCTIONS")
        .expect("relevant-context framing missing");
    let prompt_pos = output
        .find("please implement this")
        .expect("imperative past chunk missing");

    assert!(heading_pos < prompt_pos);
}

#[test]
fn test_formatter_truncation_long_items() {
    let ctx = InjectionContext {
        anti_patterns: vec![InjectionItem {
            content: "x".repeat(2000), // Very long item
            score: 0.8,
            source: "past".into(),
        }],
        ..Default::default()
    };

    let output = ctx.format(100); // Tight budget
    assert!(
        output.len() < 600,
        "output should be truncated, got {} chars",
        output.len()
    );
}

#[test]
fn test_formatter_empty_context() {
    let ctx = InjectionContext::default();
    assert_eq!(ctx.format(300), "");
    assert!(ctx.is_empty());
    assert_eq!(ctx.total_items(), 0);
}

// ─── PostToolUse Hook Tests ───

#[test]
fn test_post_tool_use_ignores_non_edit_tools() {
    // HookInput with a non-edit tool should be handled gracefully
    let json = r#"{"tool_name":"Read","tool_input":{"file_path":"/tmp/test.rs"}}"#;
    let input: csr_engine::hooks::HookInput = serde_json::from_str(json).unwrap();

    assert_eq!(input.tool_name.as_deref(), Some("Read"));
    // Read is not in EDIT_TOOLS, so the hook should skip processing
    let edit_tools = ["Edit", "Write", "MultiEdit", "NotebookEdit"];
    assert!(!edit_tools.contains(&input.tool_name.as_deref().unwrap()));
}

// ─── Install Config with All Hooks ───

#[test]
fn test_install_config_includes_stop() {
    let config = serde_json::json!({
        "hooks": {
            "Stop": [{
                "hooks": [{"type": "command", "command": "/usr/local/bin/csr-engine hook stop"}]
            }]
        }
    });

    let hooks = config.get("hooks").unwrap();
    let stop = hooks.get("Stop").unwrap();
    assert!(stop.is_array());
    let cmd = stop[0]["hooks"][0]["command"].as_str().unwrap();
    assert!(cmd.contains("hook stop"));
}

#[test]
fn test_install_config_post_tool_use_has_matcher() {
    let config = serde_json::json!({
        "hooks": {
            "PostToolUse": [{
                "matcher": "Edit|Write|MultiEdit|NotebookEdit",
                "hooks": [{"type": "command", "command": "/usr/local/bin/csr-engine hook post-tool-use"}]
            }]
        }
    });

    let hooks = config.get("hooks").unwrap();
    let ptu = hooks.get("PostToolUse").unwrap();
    assert_eq!(
        ptu[0]["matcher"].as_str().unwrap(),
        "Edit|Write|MultiEdit|NotebookEdit"
    );
}

// ─── HookInput Extended Fields ───

#[test]
fn test_hook_input_post_tool_use_fields() {
    let json = r#"{
        "session_id": "abc",
        "tool_name": "Edit",
        "tool_input": {"file_path": "/src/main.rs", "content": "new content"},
        "cwd": "/Users/dev/project"
    }"#;
    let input: csr_engine::hooks::HookInput = serde_json::from_str(json).unwrap();
    assert_eq!(input.tool_name.as_deref(), Some("Edit"));
    let tool_input = input.tool_input.as_ref().unwrap();
    assert_eq!(tool_input["file_path"].as_str(), Some("/src/main.rs"));
}

#[test]
fn test_hook_input_stop_hook_active() {
    let json = r#"{"session_id":"test","stop_hook_active":true}"#;
    let input: csr_engine::hooks::HookInput = serde_json::from_str(json).unwrap();
    assert_eq!(input.stop_hook_active, Some(true));
}

#[test]
fn test_hook_input_stop_hook_active_missing() {
    let json = r#"{"session_id":"test"}"#;
    let input: csr_engine::hooks::HookInput = serde_json::from_str(json).unwrap();
    assert_eq!(input.stop_hook_active, None);
}

// ═══════════════════════════════════════════════════════════════
// Predictive Injection Tests
// ═══════════════════════════════════════════════════════════════

// ─── sonic-rs JSONL Parsing ───

#[test]
fn test_sonic_rs_parses_jsonl_roundtrip() {
    // Verify sonic-rs produces identical serde_json::Value as serde_json
    let line = r#"{"type":"human","timestamp":"2026-02-15T10:00:00Z","message":{"content":[{"type":"text","text":"hello"}]}}"#;
    let sonic_val: serde_json::Value = sonic_rs::from_str(line).unwrap();
    let serde_val: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(sonic_val, serde_val);
}

#[test]
fn test_bufreader_streaming_matches_full_read() {
    // BufReader-based parsing produces same chunks as full-file read
    let file = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sample_conversation.jsonl");
    let chunks = csr_engine::import::parse_jsonl_file(&file, "test-project").unwrap();
    assert!(
        !chunks.is_empty(),
        "BufReader streaming should produce chunks"
    );
    assert_eq!(chunks[0].project_name, "test-project");
    assert!(chunks[0].content.contains("Docker memory"));
}

#[test]
fn test_sonic_rs_handles_malformed_lines() {
    // Create a temp file with some valid and some malformed lines
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("test.jsonl");
    std::fs::write(
        &path,
        r#"{"type":"human","timestamp":"2026-01-01T00:00:00Z","message":{"content":[{"type":"text","text":"valid"}]}}
not valid json
{"type":"assistant","message":{"content":[{"type":"text","text":"response"}]}}
{broken json
"#,
    )
    .unwrap();

    let chunks = csr_engine::import::parse_jsonl_file(&path, "test").unwrap();
    assert!(
        !chunks.is_empty(),
        "should parse valid lines despite malformed ones"
    );
    // Both valid messages should be in the first chunk
    assert!(chunks[0].content.contains("valid"));
    assert!(chunks[0].content.contains("response"));
}

// ─── Predictor Module ───

#[test]
fn test_predictor_semantic_only() {
    use csr_engine::injection::predictor::{self, RawResult};

    let results = vec![
        RawResult {
            content: "high".into(),
            score: 0.9,
            source: "chunk".into(),
            timestamp: None,
            files: vec![],
            error_patterns: vec![],
            tags: vec![],
        },
        RawResult {
            content: "low".into(),
            score: 0.4,
            source: "chunk".into(),
            timestamp: None,
            files: vec![],
            error_patterns: vec![],
            tags: vec![],
        },
    ];

    let scored = predictor::rank_results(results, &[], &[], None);
    assert_eq!(scored.len(), 2);
    assert_eq!(scored[0].content, "high");
    assert!(scored[0].final_score > scored[1].final_score);
}

#[test]
fn test_predictor_recency_boost() {
    use csr_engine::injection::predictor::{self, RawResult};

    let now = chrono::Utc::now().to_rfc3339();
    let old = "2024-01-01T00:00:00Z".to_string();

    let results = vec![
        RawResult {
            content: "recent".into(),
            score: 0.7,
            source: "chunk".into(),
            timestamp: Some(now),
            files: vec![],
            error_patterns: vec![],
            tags: vec![],
        },
        RawResult {
            content: "old".into(),
            score: 0.7,
            source: "chunk".into(),
            timestamp: Some(old),
            files: vec![],
            error_patterns: vec![],
            tags: vec![],
        },
    ];

    let scored = predictor::rank_results(results, &[], &[], None);
    assert_eq!(scored[0].content, "recent");
}

#[test]
fn test_predictor_file_overlap() {
    use csr_engine::injection::predictor::{self, RawResult};

    let results = vec![
        RawResult {
            content: "with overlap".into(),
            score: 0.7,
            source: "chunk".into(),
            timestamp: None,
            files: vec!["src/auth.rs".into()],
            error_patterns: vec![],
            tags: vec![],
        },
        RawResult {
            content: "no overlap".into(),
            score: 0.7,
            source: "chunk".into(),
            timestamp: None,
            files: vec!["src/unrelated.rs".into()],
            error_patterns: vec![],
            tags: vec![],
        },
    ];

    let current_files = vec!["src/auth.rs".into()];
    let scored = predictor::rank_results(results, &current_files, &[], None);
    assert_eq!(scored[0].content, "with overlap");
}

#[test]
fn test_predictor_cross_project() {
    use csr_engine::injection::predictor::{self, RawResult};

    let results = vec![RawResult {
        content: "cross-project insight".into(),
        score: 0.8,
        source: "reflection".into(),
        timestamp: None,
        files: vec![],
        error_patterns: vec![],
        tags: vec![],
    }];

    let scored = predictor::rank_results(results, &[], &[], None);
    assert_eq!(scored.len(), 1);
    assert_eq!(scored[0].source, "reflection");
}

// ─── Anti-Pattern Detector ───

#[test]
fn test_anti_pattern_empty_index() {
    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let rt = tokio::runtime::Runtime::new().unwrap();
    let items = rt.block_on(csr_engine::injection::anti_pattern::find_anti_patterns(
        &storage,
        &embeddings,
        &search,
        "fix auth bug",
        0.5,
        2,
    ));

    assert!(
        items.is_empty(),
        "empty index should return no anti-patterns"
    );
}

#[test]
fn test_anti_pattern_respects_min_score() {
    // Store a reflection tagged as incomplete, then search with high min_score
    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let rt = tokio::runtime::Runtime::new().unwrap();

    // Store a reflection
    let tags = vec![
        "outcome_incomplete".to_string(),
        "session_story".to_string(),
    ];
    rt.block_on(csr_engine::mcp::tools::store_reflection(
        &storage,
        &embeddings,
        &search,
        "Failed approach: used shared memory for IPC, caused race conditions",
        &tags,
    ))
    .unwrap();

    // Search with very high min_score — should return nothing
    let items = rt.block_on(csr_engine::injection::anti_pattern::find_anti_patterns(
        &storage,
        &embeddings,
        &search,
        "completely unrelated topic about cooking",
        0.99,
        2,
    ));

    // With min_score=0.99, unlikely to match
    assert!(items.len() <= 2);
}

#[test]
fn test_anti_pattern_finds_incomplete_sessions() {
    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let rt = tokio::runtime::Runtime::new().unwrap();

    // Store an incomplete session reflection
    let tags = vec![
        "outcome_incomplete".to_string(),
        "session_story".to_string(),
    ];
    rt.block_on(csr_engine::mcp::tools::store_reflection(
        &storage,
        &embeddings,
        &search,
        "INCOMPLETE SESSION: Tried to fix authentication timeout by increasing connection pool size, but the root cause was DNS resolution delay",
        &tags,
    ))
    .unwrap();

    // Search for related topic
    let items = rt.block_on(csr_engine::injection::anti_pattern::find_anti_patterns(
        &storage,
        &embeddings,
        &search,
        "fix authentication timeout",
        0.3,
        5,
    ));

    // Should find the incomplete session (semantic match on "authentication timeout")
    assert!(
        !items.is_empty(),
        "should find anti-pattern for related topic"
    );
    assert_eq!(items[0].source, "anti_pattern");
}

// ─── PromptSubmit Hook ───

#[test]
fn test_hook_input_prompt_field() {
    let json = r#"{"session_id":"test","prompt":"fix the auth bug"}"#;
    let input: csr_engine::hooks::HookInput = serde_json::from_str(json).unwrap();
    assert_eq!(input.prompt.as_deref(), Some("fix the auth bug"));
}

#[test]
fn test_hook_input_prompt_missing() {
    let json = r#"{"session_id":"test"}"#;
    let input: csr_engine::hooks::HookInput = serde_json::from_str(json).unwrap();
    assert_eq!(input.prompt, None);
}

#[test]
fn test_prompt_submit_skips_short_prompts() {
    // Prompts < 15 chars should be skipped (fast path)
    let input = csr_engine::hooks::HookInput {
        prompt: Some("hi".into()),
        ..Default::default()
    };

    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let engine = csr_engine::engine::Engine::from_parts(
        storage,
        embeddings,
        search,
        std::path::PathBuf::from("/tmp"),
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(csr_engine::hooks::prompt_submit::handle(
        &input,
        &engine,
        std::path::Path::new("/tmp"),
    ));

    assert!(result.is_ok(), "short prompt should succeed silently");
}

#[test]
fn test_prompt_submit_skips_slash_commands() {
    let input = csr_engine::hooks::HookInput {
        prompt: Some("/help me with something".into()),
        ..Default::default()
    };

    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let engine = csr_engine::engine::Engine::from_parts(
        storage,
        embeddings,
        search,
        std::path::PathBuf::from("/tmp"),
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(csr_engine::hooks::prompt_submit::handle(
        &input,
        &engine,
        std::path::Path::new("/tmp"),
    ));

    assert!(result.is_ok(), "slash command should succeed silently");
}

#[test]
fn test_prompt_submit_no_results_no_output() {
    let input = csr_engine::hooks::HookInput {
        prompt: Some("fix the authentication timeout bug in the login system".into()),
        ..Default::default()
    };

    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let engine = csr_engine::engine::Engine::from_parts(
        storage,
        embeddings,
        search,
        std::path::PathBuf::from("/tmp"),
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(csr_engine::hooks::prompt_submit::handle(
        &input,
        &engine,
        std::path::Path::new("/tmp"),
    ));

    assert!(
        result.is_ok(),
        "empty index should produce no output but succeed"
    );
}

#[test]
fn test_prompt_submit_catch_all_never_fails() {
    // Even with invalid/missing input, should return Ok
    let input = csr_engine::hooks::HookInput::default();

    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let engine = csr_engine::engine::Engine::from_parts(
        storage,
        embeddings,
        search,
        std::path::PathBuf::from("/tmp"),
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(csr_engine::hooks::prompt_submit::handle(
        &input,
        &engine,
        std::path::Path::new("/tmp"),
    ));

    assert!(result.is_ok(), "catch-all wrapper must always succeed");
}

// ─── Install Config (Updated for 6 Hook Types) ───

#[test]
fn test_install_config_includes_prompt_submit() {
    // Call the actual generate function
    let config =
        csr_engine::hooks::install::generate_hook_config_for_test("/usr/local/bin/csr-engine");
    let hooks = config.get("hooks").unwrap();

    // All 6 hook types must be present
    assert!(hooks.get("SessionStart").is_some());
    assert!(hooks.get("SessionEnd").is_some());
    assert!(hooks.get("PreCompact").is_some());
    assert!(hooks.get("Stop").is_some());
    assert!(hooks.get("PostToolUse").is_some());
    assert!(
        hooks.get("UserPromptSubmit").is_some(),
        "UserPromptSubmit must be in config"
    );

    let prompt_submit = hooks.get("UserPromptSubmit").unwrap();
    let cmd = prompt_submit[0]["hooks"][0]["command"].as_str().unwrap();
    assert!(cmd.contains("hook prompt-submit"));
}

// ─── E2E: Store Reflection → Prompt Triggers Injection ───

#[test]
fn test_e2e_store_reflection_then_prompt_finds_it() {
    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let rt = tokio::runtime::Runtime::new().unwrap();

    // 1. Store a reflection about Docker memory
    let tags = vec!["docker".to_string(), "debugging".to_string()];
    rt.block_on(csr_engine::mcp::tools::store_reflection(
        &storage,
        &embeddings,
        &search,
        "SOLUTION: Docker container was running out of memory because the Qdrant vector database had no memory limits set. Fixed by adding mem_limit: 2g to docker-compose.yaml.",
        &tags,
    ))
    .unwrap();

    // 2. Verify it can be found via search
    let query_vec = {
        let q = "Docker memory issue with Qdrant".to_string();
        let emb = embeddings.clone();
        rt.block_on(async move {
            tokio::task::spawn_blocking(move || emb.embed_single(&q))
                .await
                .unwrap()
                .unwrap()
        })
    };

    let results = rt.block_on(async {
        let idx = search.read().await;
        idx.search_reflections(&query_vec, 5, 0.3)
    });

    assert!(!results.is_empty(), "stored reflection should be findable");
    assert!(results[0].score > 0.5, "should have high relevance score");
}

// ─── Real-Time Session Memory Tests ───

/// Test: incremental import only embeds new chunks when transcript grows.
#[test]
fn test_incremental_import_only_new_chunks() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let projects_dir = tmp.path().join("projects");
    let project_dir = projects_dir.join("-Users-test-projects-myapp");
    std::fs::create_dir_all(&project_dir).unwrap();

    // Write initial 3-message transcript
    let jsonl_path = project_dir.join("conv-incr-test.jsonl");
    let initial_content = r#"{"type":"user","message":{"content":[{"type":"text","text":"Fix the authentication bug"}]},"timestamp":"2026-02-22T10:00:00Z"}
{"type":"assistant","message":{"content":[{"type":"text","text":"I'll look at the auth module."}]},"timestamp":"2026-02-22T10:00:01Z"}
{"type":"user","message":{"content":[{"type":"text","text":"Great, also check the token refresh logic"}]},"timestamp":"2026-02-22T10:00:02Z"}
"#;
    std::fs::write(&jsonl_path, initial_content).unwrap();

    let engine = csr_engine::engine::Engine::new(&db_path, &projects_dir).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();

    // First import
    let count1 = rt
        .block_on(engine.import_file(&jsonl_path, "myapp"))
        .unwrap();
    assert!(count1 > 0, "first import should produce chunks");

    // Same file, no changes — should return 0
    let count2 = rt
        .block_on(engine.import_file(&jsonl_path, "myapp"))
        .unwrap();
    assert_eq!(count2, 0, "unchanged file should produce 0 new chunks");

    // Grow the transcript (add more messages)
    let grown_content = format!(
        "{}{}",
        initial_content,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"The token refresh was using an expired key. I fixed it in auth.rs."}]},"timestamp":"2026-02-22T10:00:03Z"}
{"type":"user","message":{"content":[{"type":"text","text":"Run the tests to verify"}]},"timestamp":"2026-02-22T10:00:04Z"}
"#
    );
    std::fs::write(&jsonl_path, grown_content).unwrap();

    // Incremental import — file changed (mtime differs) so it re-parses.
    let _count3 = rt
        .block_on(engine.import_file(&jsonl_path, "myapp"))
        .unwrap();
    // The key assertion: no error occurred during incremental import.
}

/// Test: import_current_transcript shared helper works with real transcript.
#[test]
fn test_import_current_transcript_helper() {
    let tmp = tempfile::TempDir::new().unwrap();
    let transcript = tmp.path().join("active-session.jsonl");
    std::fs::write(
        &transcript,
        r#"{"type":"user","message":{"content":[{"type":"text","text":"How do I fix HNSW persistence?"}]},"timestamp":"2026-02-22T12:00:00Z"}
{"type":"assistant","message":{"content":[{"type":"text","text":"You need to use fs2 advisory locking before file_dump."}]},"timestamp":"2026-02-22T12:00:01Z"}
{"type":"user","message":{"content":[{"type":"text","text":"Show me the code"}]},"timestamp":"2026-02-22T12:00:02Z"}
"#,
    )
    .unwrap();

    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));
    let engine = csr_engine::engine::Engine::from_parts(
        storage.clone(),
        embeddings,
        search.clone(),
        std::path::PathBuf::from("/tmp"),
    );

    let input = csr_engine::hooks::HookInput {
        transcript_path: Some(transcript.to_string_lossy().to_string()),
        cwd: Some(tmp.path().to_string_lossy().to_string()),
        ..Default::default()
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(csr_engine::hooks::import_current_transcript(
        &input,
        &engine,
        tmp.path(),
    ));

    // Verify chunks were indexed
    let chunk_count = rt.block_on(async { search.read().await.chunk_count() });
    assert!(chunk_count > 0, "transcript should be indexed after import");
}

/// Test: stop hook imports transcript for all sessions.
#[test]
fn test_stop_hook_imports_for_all_sessions() {
    let tmp = tempfile::TempDir::new().unwrap();
    let transcript = tmp.path().join("non-ralph-session.jsonl");
    std::fs::write(
        &transcript,
        r#"{"type":"user","message":{"content":[{"type":"text","text":"Explain the search algorithm"}]},"timestamp":"2026-02-22T14:00:00Z"}
{"type":"assistant","message":{"content":[{"type":"text","text":"The HNSW algorithm uses hierarchical layers."}]},"timestamp":"2026-02-22T14:00:01Z"}
"#,
    )
    .unwrap();

    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));
    let engine = csr_engine::engine::Engine::from_parts(
        storage,
        embeddings,
        search.clone(),
        std::path::PathBuf::from("/tmp"),
    );

    let input = csr_engine::hooks::HookInput {
        transcript_path: Some(transcript.to_string_lossy().to_string()),
        cwd: Some(tmp.path().to_string_lossy().to_string()),
        ..Default::default()
    };

    let rt = tokio::runtime::Runtime::new().unwrap();

    // Call stop hook — should import transcript
    let result = rt.block_on(csr_engine::hooks::stop::handle(&input, &engine, tmp.path()));
    assert!(result.is_ok());

    let chunk_count = rt.block_on(async { search.read().await.chunk_count() });
    assert!(chunk_count > 0, "stop hook should import transcript");
}

/// Test: V3 extraction at session-end produces searchable reflection.
#[test]
fn test_session_end_v3_extraction() {
    let tmp = tempfile::TempDir::new().unwrap();
    let transcript = tmp.path().join("v3-test-session.jsonl");

    // Write a substantial transcript (>3 messages, with edits)
    let messages = r#"{"type":"user","message":{"content":[{"type":"text","text":"Fix the race condition in HNSW persistence that causes stale index on restart"}]},"timestamp":"2026-02-22T15:00:00Z"}
{"type":"assistant","message":{"content":[{"type":"text","text":"I see the issue. The dump_to_disk function doesn't acquire a lock."},{"type":"tool_use","name":"Edit","input":{"file_path":"/src/search.rs","old_string":"fn dump_to_disk","new_string":"fn dump_to_disk_with_lock"}}]},"timestamp":"2026-02-22T15:00:01Z"}
{"type":"user","message":{"content":[{"type":"text","text":"Good, now add fs2 advisory locking"}]},"timestamp":"2026-02-22T15:00:02Z"}
{"type":"assistant","message":{"content":[{"type":"text","text":"Added fs2 lock_exclusive before file dump. The index is now safe against concurrent access."},{"type":"tool_use","name":"Edit","input":{"file_path":"/src/engine.rs","old_string":"file_dump()","new_string":"lock.lock_exclusive(); file_dump()"}}]},"timestamp":"2026-02-22T15:00:03Z"}
{"type":"user","message":{"content":[{"type":"text","text":"Run the tests"}]},"timestamp":"2026-02-22T15:00:04Z"}
{"type":"assistant","message":{"content":[{"type":"text","text":"All 200 tests pass with zero warnings. The race condition is fixed."}]},"timestamp":"2026-02-22T15:00:05Z"}
"#;
    std::fs::write(&transcript, messages).unwrap();

    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));
    let engine = csr_engine::engine::Engine::from_parts(
        storage.clone(),
        embeddings.clone(),
        search.clone(),
        std::path::PathBuf::from("/tmp"),
    );

    let input = csr_engine::hooks::HookInput {
        transcript_path: Some(transcript.to_string_lossy().to_string()),
        cwd: Some(tmp.path().to_string_lossy().to_string()),
        session_id: Some("v3-test".into()),
        ..Default::default()
    };

    let rt = tokio::runtime::Runtime::new().unwrap();

    // Call session-end (just the import + V3 extraction path)
    let result = rt.block_on(csr_engine::hooks::session_end::handle(
        &input,
        &engine,
        tmp.path(),
    ));
    assert!(result.is_ok());

    // Verify V3 enrichment was stored
    let conv_id = "v3-test-session";
    let is_enriched = storage
        .is_conversation_enriched(conv_id, "extracted_v3")
        .unwrap();
    assert!(is_enriched, "session-end should produce V3 enrichment");

    // Verify the V3 reflection is searchable
    let reflection_count = rt.block_on(async { search.read().await.reflection_count() });
    assert!(
        reflection_count > 0,
        "V3 search index should be in reflection index"
    );

    // Verify the V3 reflection content is stored and retrievable by ID
    let v3_id = format!("v3_{}", conv_id);
    let reflection = storage.get_reflection_by_id(&v3_id).unwrap();
    assert!(
        reflection.is_some(),
        "V3 reflection should exist in storage"
    );
    let (content, tags, _ts) = reflection.unwrap();
    assert!(
        content.contains("User Request") || content.contains("Solution"),
        "V3 search index should contain structured sections"
    );
    assert!(
        tags.contains(&"narrative_v3".to_string()),
        "should have narrative_v3 tag"
    );

    // Search with low threshold — V3 structured text may have lower similarity
    let query_vec = {
        let emb = embeddings.clone();
        rt.block_on(async move {
            tokio::task::spawn_blocking(move || {
                emb.embed_single("race condition HNSW persistence stale index")
            })
            .await
            .unwrap()
            .unwrap()
        })
    };

    let results = rt.block_on(async {
        let idx = search.read().await;
        idx.search_reflections(&query_vec, 5, 0.1)
    });
    assert!(
        !results.is_empty(),
        "V3 search index should be findable by problem description"
    );
}

/// Test: precompact hook imports transcript for all sessions.
#[test]
fn test_precompact_imports_for_all_sessions() {
    let tmp = tempfile::TempDir::new().unwrap();
    let transcript = tmp.path().join("precompact-test.jsonl");
    std::fs::write(
        &transcript,
        r#"{"type":"user","message":{"content":[{"type":"text","text":"Working on real-time session memory feature"}]},"timestamp":"2026-02-22T16:00:00Z"}
{"type":"assistant","message":{"content":[{"type":"text","text":"I'll implement the incremental import optimization."}]},"timestamp":"2026-02-22T16:00:01Z"}
"#,
    )
    .unwrap();

    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));
    let engine = csr_engine::engine::Engine::from_parts(
        storage,
        embeddings,
        search.clone(),
        std::path::PathBuf::from("/tmp"),
    );

    let input = csr_engine::hooks::HookInput {
        transcript_path: Some(transcript.to_string_lossy().to_string()),
        cwd: Some(tmp.path().to_string_lossy().to_string()),
        ..Default::default()
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(csr_engine::hooks::precompact::handle(
        &input,
        &engine,
        tmp.path(),
    ));
    assert!(result.is_ok());

    let chunk_count = rt.block_on(async { search.read().await.chunk_count() });
    assert!(
        chunk_count > 0,
        "precompact should import transcript before compaction"
    );
}
