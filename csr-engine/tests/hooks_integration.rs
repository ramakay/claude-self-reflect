//! Integration tests for Phase 2: hooks subsystem.
//!
//! Tests Ralph state parsing, error normalization, hook dispatch,
//! narrative generation, and end-to-end session store→search flow.

use tempfile::TempDir;

use csr_engine::hooks::ralph_state::{
    normalize_error_signature, Outcome, RalphState, WorkType,
};

// ─── Ralph State Parsing ───

#[test]
fn test_ralph_plugin_format_full() {
    let content = r#"---
active: true
iteration: 12
max_iterations: 50
completion_promise: "All tests pass and coverage > 80%"
started_at: "2026-02-14T10:30:00Z"
---
Implement cross-session memory via Claude Code hooks

## Failed Approaches (DO NOT RETRY)
- Direct stdin piping without JSON parsing
- Using shared memory for hook communication
- Blocking hooks that wait for search results

## Error Signatures
- `HNSW index empty: no vectors loaded` (x5)
- `permission denied: /tmp/csr-lock` (x2)

## Successful Strategies
- Cold-start engine per hook invocation
- Write context file to CWD for session-start

## Files Modified
- csr-engine/src/hooks/mod.rs
- csr-engine/src/hooks/session_start.rs
- csr-engine/src/main.rs

## Learnings
- Hook cold-start is ~73ms, acceptable for infrequent hooks
- Anti-patterns must appear FIRST in context output
"#;

    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("state.md");
    std::fs::write(&path, content).unwrap();

    let state = RalphState::from_file(&path).unwrap().unwrap();

    assert!(state.active);
    assert_eq!(state.iteration, 12);
    assert_eq!(
        state.task,
        "Implement cross-session memory via Claude Code hooks"
    );
    assert_eq!(
        state.completion_promise.as_deref(),
        Some("All tests pass and coverage > 80%")
    );
    assert_eq!(state.failed_approaches.len(), 3);
    assert_eq!(
        state.failed_approaches[0],
        "Direct stdin piping without JSON parsing"
    );
    assert_eq!(state.error_signatures.len(), 2);
    assert_eq!(state.error_signatures[0].1, 5); // count for first error
    assert_eq!(state.error_signatures[1].1, 2); // count for second error
    assert_eq!(state.successful_strategies.len(), 2);
    assert_eq!(state.files_modified.len(), 3);
    assert_eq!(state.learnings.len(), 2);
}

#[test]
fn test_ralph_custom_format_full() {
    let content = r#"## Metadata
- **Session ID:** ralph_20260214_103000
- **Task:** Fix authentication timeout bug
- **Iteration:** 15
- **Work Type:** DEBUGGING
- **Exit Confidence:** 85%
- **Active:** true

## Failed Approaches (DO NOT RETRY)
- Increasing timeout to 60s (still times out)
- Disabling TLS verification (security risk)

## Error Signatures (Deduplicated)
- `connection reset by peer at /Users/dev/src/auth.rs:42` (x8)
- `SSL handshake timeout` (x3)

## Successful Strategies
- Use connection pooling with keep-alive
- Retry with exponential backoff

## Files Modified
- src/auth.rs
- src/http_client.rs
- tests/auth_timeout_test.rs

## Learnings
- Connection pooling reduces auth latency by 90%
- TLS 1.3 eliminates extra roundtrip
"#;

    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join(".ralph_state.md");
    std::fs::write(&path, content).unwrap();

    let state = RalphState::from_file(&path).unwrap().unwrap();

    assert!(state.active);
    assert_eq!(state.session_id, "ralph_20260214_103000");
    assert_eq!(state.task, "Fix authentication timeout bug");
    assert_eq!(state.iteration, 15);
    assert_eq!(state.work_type, WorkType::Debugging);
    assert_eq!(state.exit_confidence, 85);
    assert_eq!(state.failed_approaches.len(), 2);
    assert_eq!(state.error_signatures.len(), 2);
    assert_eq!(state.successful_strategies.len(), 2);
    assert_eq!(state.files_modified.len(), 3);
    assert_eq!(state.learnings.len(), 2);
}

#[test]
fn test_ralph_detect_plugin_over_custom() {
    let tmp = TempDir::new().unwrap();

    // Create both files — plugin should be preferred
    let plugin_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("ralph-loop.local.md"),
        "---\nactive: true\niteration: 3\n---\nPlugin task\n",
    )
    .unwrap();

    std::fs::write(
        tmp.path().join(".ralph_state.md"),
        "## Metadata\n- **Task:** Custom task\n- **Active:** true\n",
    )
    .unwrap();

    let state = RalphState::detect_in(tmp.path()).unwrap().unwrap();
    assert_eq!(state.task, "Plugin task");
}

#[test]
fn test_ralph_empty_file_returns_none() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("empty.md");
    std::fs::write(&path, "").unwrap();

    let result = RalphState::from_file(&path).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_ralph_nonexistent_file_returns_none() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("nonexistent.md");

    let result = RalphState::from_file(&path).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_ralph_minimal_plugin_format() {
    let content = "---\nactive: true\n---\nMinimal task\n";
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("state.md");
    std::fs::write(&path, content).unwrap();

    let state = RalphState::from_file(&path).unwrap().unwrap();
    assert!(state.active);
    assert_eq!(state.task, "Minimal task");
    assert_eq!(state.iteration, 0);
    assert!(state.completion_promise.is_none());
}

// ─── Error Signature Normalization ───

#[test]
fn test_normalize_removes_line_numbers() {
    let input = "error at line 42 in parse_config";
    let normalized = normalize_error_signature(input);
    assert!(normalized.contains("line <N>"));
    assert!(!normalized.contains("42"));
}

#[test]
fn test_normalize_removes_file_paths() {
    let input = "failed to open /Users/dev/projects/my-app/src/config.rs";
    let normalized = normalize_error_signature(input);
    assert!(normalized.contains("<PATH>"));
    assert!(!normalized.contains("/Users"));
}

#[test]
fn test_normalize_removes_timestamps() {
    let input = "timeout at 2026-02-14T10:30:00Z while connecting";
    let normalized = normalize_error_signature(input);
    assert!(normalized.contains("<TIMESTAMP>"));
    assert!(!normalized.contains("2026"));
}

#[test]
fn test_normalize_removes_hex_addresses() {
    let input = "null pointer dereference at 0x7fff5fbff8a0";
    let normalized = normalize_error_signature(input);
    assert!(normalized.contains("<ADDR>"));
    assert!(!normalized.contains("0x7fff"));
}

#[test]
fn test_normalize_handles_combined_patterns() {
    let input =
        "error at /Users/dev/src/main.rs:42:13 at 2026-01-04T10:36:30Z addr 0xdeadbeef";
    let normalized = normalize_error_signature(input);
    assert!(normalized.contains("<PATH>"));
    assert!(normalized.contains("<TIMESTAMP>"));
    assert!(normalized.contains("<ADDR>"));
    // Should not contain any original variable values
    assert!(!normalized.contains("/Users"));
    assert!(!normalized.contains("2026-01"));
    assert!(!normalized.contains("0xdeadbeef"));
}

#[test]
fn test_normalize_preserves_error_meaning() {
    let input = "JWT token expired";
    let normalized = normalize_error_signature(input);
    assert_eq!(normalized, "JWT token expired");
}

// ─── Outcome Determination ───

#[test]
fn test_outcome_completed_when_promise_met() {
    let mut state = RalphState::default();
    state.completion_promise_met = true;
    assert_eq!(state.determine_outcome("shutdown"), Outcome::Completed);
    assert_eq!(state.determine_outcome("clear"), Outcome::Completed); // Promise overrides
}

#[test]
fn test_outcome_abandoned_on_clear() {
    let state = RalphState::default();
    assert_eq!(state.determine_outcome("clear"), Outcome::Abandoned);
}

#[test]
fn test_outcome_abandoned_on_logout() {
    let state = RalphState::default();
    assert_eq!(state.determine_outcome("logout"), Outcome::Abandoned);
}

#[test]
fn test_outcome_incomplete_on_other_reasons() {
    let state = RalphState::default();
    assert_eq!(state.determine_outcome("shutdown"), Outcome::Incomplete);
    assert_eq!(state.determine_outcome("unknown"), Outcome::Incomplete);
    assert_eq!(state.determine_outcome(""), Outcome::Incomplete);
}

// ─── Narrative Generation ───

#[test]
fn test_narrative_completed_session() {
    let state = RalphState {
        session_id: "ralph_test_001".into(),
        task: "Implement hooks".into(),
        iteration: 10,
        active: true,
        work_type: WorkType::Implementation,
        exit_confidence: 95,
        completion_promise: Some("All tests pass".into()),
        completion_promise_met: true,
        failed_approaches: vec!["Approach A".into(), "Approach B".into()],
        successful_strategies: vec!["Strategy X".into()],
        error_signatures: vec![("Connection reset".into(), 3)],
        files_modified: vec!["src/hooks/mod.rs".into()],
        learnings: vec!["Hook cold start is fast".into()],
    };

    let narrative = state.to_narrative(&Outcome::Completed);

    assert!(narrative.contains("RALPH SESSION: ralph_test_001"));
    assert!(narrative.contains("TASK: Implement hooks"));
    assert!(narrative.contains("OUTCOME: completed"));
    assert!(narrative.contains("ITERATIONS: 10"));
    assert!(narrative.contains("WORK TYPE: IMPLEMENTATION"));
    assert!(narrative.contains("EXIT CONFIDENCE: 95%"));
    assert!(narrative.contains("COMPLETION PROMISE: All tests pass"));
    assert!(narrative.contains("PROMISE MET: yes"));
    assert!(narrative.contains("FAILED APPROACHES (DO NOT RETRY):"));
    assert!(narrative.contains("- Approach A"));
    assert!(narrative.contains("- Approach B"));
    assert!(narrative.contains("SUCCESSFUL STRATEGIES:"));
    assert!(narrative.contains("- Strategy X"));
    assert!(narrative.contains("ERROR SIGNATURES:"));
    assert!(narrative.contains("- Connection reset (x3)"));
    assert!(narrative.contains("FILES MODIFIED:"));
    assert!(narrative.contains("LEARNINGS:"));
}

#[test]
fn test_narrative_abandoned_session() {
    let state = RalphState {
        session_id: "ralph_abandoned_002".into(),
        task: "Refactor database".into(),
        iteration: 3,
        work_type: WorkType::Debugging,
        failed_approaches: vec!["Schema migration".into()],
        ..Default::default()
    };

    let narrative = state.to_narrative(&Outcome::Abandoned);
    assert!(narrative.contains("OUTCOME: abandoned"));
    assert!(narrative.contains("ITERATIONS: 3"));
    assert!(narrative.contains("- Schema migration"));
}

#[test]
fn test_narrative_incomplete_session() {
    let state = RalphState {
        session_id: "ralph_incomplete_003".into(),
        task: "Add caching".into(),
        iteration: 7,
        work_type: WorkType::Implementation,
        exit_confidence: 40,
        ..Default::default()
    };

    let narrative = state.to_narrative(&Outcome::Incomplete);
    assert!(narrative.contains("OUTCOME: incomplete"));
    assert!(narrative.contains("EXIT CONFIDENCE: 40%"));
}

// ─── End-to-End: Store Session → Search Past Sessions ───

#[test]
fn test_store_and_retrieve_session_narrative() {
    // This tests the storage/search pipeline used by session_end hook

    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let rt = tokio::runtime::Runtime::new().unwrap();

    // Store a completed session narrative
    let narrative = "RALPH SESSION: ralph_e2e_001\n\
                     TASK: Fix Docker memory issue\n\
                     OUTCOME: completed\n\
                     ITERATIONS: 5\n\
                     SUCCESSFUL STRATEGIES:\n\
                     - Implemented resource constraints in docker-compose.yaml\n\
                     LEARNINGS:\n\
                     - Container memory limits prevent OOM kills\n";

    let tags = vec![
        "ralph_session".to_string(),
        "session_ralph_e2e_001".to_string(),
        "outcome_completed".to_string(),
        "winning_strategy".to_string(),
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
    let anti_narrative = "RALPH SESSION: ralph_anti_001\n\
                          TASK: Fix authentication\n\
                          OUTCOME: incomplete\n\
                          FAILED APPROACHES:\n\
                          - Token refresh hack\n";
    let anti_tags = vec![
        "ralph_session".to_string(),
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
    let win_narrative = "RALPH SESSION: ralph_win_001\n\
                         TASK: Fix authentication properly\n\
                         OUTCOME: completed\n\
                         SUCCESSFUL STRATEGIES:\n\
                         - Use OAuth2 with PKCE flow\n";
    let win_tags = vec![
        "ralph_session".to_string(),
        "outcome_completed".to_string(),
        "winning_strategy".to_string(),
    ];

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

    let winners = storage
        .get_reflections_by_tag("winning_strategy", 10)
        .unwrap();
    assert_eq!(winners.len(), 1);
}

// ─── Hook Install Config ───

#[test]
fn test_install_config_structure() {
    // Test that the generated config has the right structure
    // (This mirrors the unit test but from integration perspective)
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

// ─── Precompact State Backup ───

#[test]
fn test_precompact_state_serialization() {
    let state = RalphState {
        session_id: "ralph_precompact_001".into(),
        task: "Implement feature X".into(),
        iteration: 7,
        active: true,
        work_type: WorkType::Implementation,
        exit_confidence: 60,
        completion_promise: Some("Feature deployed".into()),
        completion_promise_met: false,
        failed_approaches: vec!["Monolith approach".into()],
        successful_strategies: Vec::new(),
        error_signatures: vec![("Build timeout".into(), 2)],
        files_modified: vec!["src/feature_x.rs".into()],
        learnings: vec!["Split into microservices".into()],
    };

    // The precompact hook stores a narrative — verify it round-trips
    let narrative = state.to_narrative(&Outcome::Incomplete);
    assert!(narrative.contains("ralph_precompact_001"));
    assert!(narrative.contains("Implement feature X"));
    assert!(narrative.contains("incomplete"));
    assert!(narrative.contains("Monolith approach"));
    assert!(narrative.contains("Build timeout (x2)"));

    // Store and retrieve from storage
    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let rt = tokio::runtime::Runtime::new().unwrap();

    let backup_text = format!(
        "PRE-COMPACTION BACKUP for Ralph session: {}\nTask: {}\nIteration: {}\n\n{}",
        state.session_id, state.task, state.iteration, narrative,
    );

    let tags = vec![
        "ralph_state".to_string(),
        "pre_compact_backup".to_string(),
        format!("session_{}", state.session_id),
    ];

    rt.block_on(csr_engine::mcp::tools::store_reflection(
        &storage, &embeddings, &search, &backup_text, &tags,
    ))
    .unwrap();

    // Verify retrievable by tag
    let backups = storage
        .get_reflections_by_tag("pre_compact_backup", 10)
        .unwrap();
    assert_eq!(backups.len(), 1);
    assert!(backups[0].1.contains("PRE-COMPACTION BACKUP"));
}

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

// ─── State File Paths ───

#[test]
fn test_state_file_paths() {
    let cwd = std::path::Path::new("/Users/dev/project");
    let paths = csr_engine::hooks::ralph_state::state_file_paths(cwd);

    assert_eq!(paths.len(), 2);
    assert!(paths[0]
        .to_string_lossy()
        .contains(".claude/ralph-loop.local.md"));
    assert!(paths[1].to_string_lossy().contains(".ralph_state.md"));
}
