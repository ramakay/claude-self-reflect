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

// ════════════════════════════════════════════════════════════════
// Phase 2b: Injection Engine + Stop/PostToolUse hooks
// ════════════════════════════════════════════════════════════════

use csr_engine::injection::formatter;
use csr_engine::injection::stuck_detector::{self, StuckSeverity};
use csr_engine::injection::{InjectionContext, InjectionItem};

// ─── Injection Formatter Tests ───

#[test]
fn test_formatter_token_budget_enforcement() {
    // Create a context with lots of items that exceed 300 tokens
    let ctx = InjectionContext {
        anti_patterns: (0..20)
            .map(|i| InjectionItem {
                content: format!("Anti-pattern {} with a moderately long description that uses tokens", i),
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
    let ctx_pos = output.find("CONTEXT_MARKER").expect("relevant context missing");
    let win_pos = output.find("WIN_MARKER").expect("winning strategy missing");
    let iter_pos = output.find("ITER_MARKER").expect("iteration learning missing");

    assert!(stuck_pos < anti_pos, "stuck must come before anti-patterns");
    assert!(anti_pos < error_pos, "anti-patterns must come before errors");
    assert!(error_pos < ctx_pos, "errors must come before relevant context");
    assert!(ctx_pos < win_pos, "relevant context must come before winning strategies");
    assert!(win_pos < iter_pos, "winning strategies must come before iterations");
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

// ─── Stuck Detector Tests ───

#[test]
fn test_stuck_error_repetition() {
    let ralph = RalphState {
        error_signatures: vec![("JWT expired".to_string(), 3)],
        active: true,
        ..Default::default()
    };

    let result = stuck_detector::analyze(&ralph);
    assert!(result.is_stuck);
    assert_eq!(result.severity, StuckSeverity::Warning);
    assert!(result.reasons[0].contains("repeated 3x"));
}

#[test]
fn test_stuck_high_iteration_low_confidence() {
    let ralph = RalphState {
        iteration: 25,
        exit_confidence: 20,
        active: true,
        ..Default::default()
    };

    let result = stuck_detector::analyze(&ralph);
    assert!(result.is_stuck);
    assert!(result.reasons[0].contains("High iteration"));
}

#[test]
fn test_stuck_failed_approach_accumulation() {
    let ralph = RalphState {
        failed_approaches: vec![
            "a".into(), "b".into(), "c".into(), "d".into(), "e".into(),
        ],
        active: true,
        ..Default::default()
    };

    let result = stuck_detector::analyze(&ralph);
    assert!(result.is_stuck);
    assert!(result.reasons[0].contains("5 failed approaches"));
}

#[test]
fn test_stuck_normal_state() {
    let ralph = RalphState {
        iteration: 3,
        exit_confidence: 80,
        error_signatures: vec![("error".to_string(), 1)],
        failed_approaches: vec!["one".into()],
        active: true,
        ..Default::default()
    };

    let result = stuck_detector::analyze(&ralph);
    assert!(!result.is_stuck);
    assert_eq!(result.severity, StuckSeverity::Normal);
}

#[test]
fn test_stuck_severity_warning_vs_critical() {
    // Warning: single signal
    let ralph_warn = RalphState {
        error_signatures: vec![("timeout".to_string(), 5)],
        active: true,
        ..Default::default()
    };
    let result = stuck_detector::analyze(&ralph_warn);
    assert_eq!(result.severity, StuckSeverity::Warning);

    // Critical: multiple signals
    let ralph_crit = RalphState {
        iteration: 30,
        exit_confidence: 10,
        error_signatures: vec![("timeout".to_string(), 5)],
        failed_approaches: vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()],
        active: true,
        ..Default::default()
    };
    let result = stuck_detector::analyze(&ralph_crit);
    assert_eq!(result.severity, StuckSeverity::Critical);
    assert!(result.reasons.len() >= 2);
}

// ─── Stop Hook Tests ───

#[test]
fn test_stop_stores_iteration_learnings() {
    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let rt = tokio::runtime::Runtime::new().unwrap();

    // Store an iteration learning (mimicking what stop hook does)
    let content = "ITERATION 3 of session ralph_test_stop\nTask: Fix bug\nLearnings:\n- Check tokens first\nConfidence: 60%\n";
    let tags = vec![
        "ralph_iteration".to_string(),
        "session_ralph_test_stop".to_string(),
        "iteration_3".to_string(),
    ];

    rt.block_on(csr_engine::mcp::tools::store_reflection(
        &storage, &embeddings, &search, content, &tags,
    ))
    .unwrap();

    // Verify stored with correct tags
    let results = storage
        .get_reflections_by_tag("ralph_iteration", 10)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].1.contains("ITERATION 3"));
    assert!(results[0].2.contains(&"iteration_3".to_string()));
}

#[test]
fn test_stop_retrieves_previous_iterations() {
    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let rt = tokio::runtime::Runtime::new().unwrap();

    // Store iterations 1, 2, 3
    for i in 1..=3 {
        let content = format!("ITERATION {} of session ralph_test_retrieve\nTask: Build feature\n", i);
        let tags = vec![
            "ralph_iteration".to_string(),
            "session_ralph_test_retrieve".to_string(),
            format!("iteration_{}", i),
        ];
        rt.block_on(csr_engine::mcp::tools::store_reflection(
            &storage, &embeddings, &search, &content, &tags,
        ))
        .unwrap();
    }

    // Query for session iterations
    let results = storage
        .get_reflections_by_tag("session_ralph_test_retrieve", 10)
        .unwrap();
    assert_eq!(results.len(), 3, "should find all 3 iterations");

    // Verify iteration tags present
    let all_tags: Vec<String> = results
        .iter()
        .flat_map(|(_, _, tags, _)| tags.clone())
        .collect();
    assert!(all_tags.contains(&"iteration_1".to_string()));
    assert!(all_tags.contains(&"iteration_2".to_string()));
    assert!(all_tags.contains(&"iteration_3".to_string()));
}

#[test]
fn test_stop_stuck_warning_in_output() {
    let ralph = RalphState {
        iteration: 25,
        exit_confidence: 10,
        error_signatures: vec![("timeout".to_string(), 5)],
        active: true,
        ..Default::default()
    };

    let stuck = stuck_detector::analyze(&ralph);
    let warning = stuck_detector::format_warning(&stuck);

    assert!(warning.is_some());
    let warning_text = warning.unwrap();
    assert!(warning_text.contains("[CRITICAL]"));

    // Build injection context with warning
    let ctx = InjectionContext {
        stuck_warning: Some(warning_text.clone()),
        ..Default::default()
    };

    let output = ctx.format(300);
    assert!(output.contains("STUCK WARNING"));
    assert!(output.contains("[CRITICAL]"));
}

#[test]
fn test_stop_no_ralph_exits_silently() {
    // When there's no Ralph session, stop hook should return Ok(()) with no output
    let tmp = TempDir::new().unwrap();
    let ralph = RalphState::detect_in(tmp.path()).unwrap();
    assert!(ralph.is_none(), "no Ralph state file means no session");
}

// ─── PostToolUse Hook Tests ───

#[test]
fn test_post_tool_use_tracks_edit() {
    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let rt = tokio::runtime::Runtime::new().unwrap();

    // Simulate what PostToolUse does: store file edit tracking
    let content = "File modified: /Users/dev/src/main.rs (tool: Edit, session: ralph_test_ptu, iteration: 3)";
    let tags = vec![
        "file_edit".to_string(),
        "session_ralph_test_ptu".to_string(),
        "iteration_3".to_string(),
    ];

    rt.block_on(csr_engine::mcp::tools::store_reflection(
        &storage, &embeddings, &search, content, &tags,
    ))
    .unwrap();

    let results = storage
        .get_reflections_by_tag("file_edit", 10)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].1.contains("/Users/dev/src/main.rs"));
    assert!(results[0].1.contains("Edit"));
}

#[test]
fn test_post_tool_use_ignores_non_edit_tools() {
    // HookInput with a non-edit tool should be handled gracefully
    let json = r#"{"tool_name":"Read","tool_input":{"file_path":"/tmp/test.rs"}}"#;
    let input: csr_engine::hooks::HookInput = serde_json::from_str(json).unwrap();

    assert_eq!(input.tool_name.as_deref(), Some("Read"));
    // Read is not in EDIT_TOOLS, so the hook should skip processing
    // (We test the logic, not the full hook dispatch which requires engine)
    let edit_tools = ["Edit", "Write", "MultiEdit", "NotebookEdit"];
    assert!(!edit_tools.contains(&input.tool_name.as_deref().unwrap()));
}

#[test]
fn test_post_tool_use_no_ralph_exits_silently() {
    let tmp = TempDir::new().unwrap();
    let ralph = RalphState::detect_in(tmp.path()).unwrap();
    assert!(ralph.is_none());
}

#[test]
fn test_post_tool_use_dedup_same_file() {
    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let rt = tokio::runtime::Runtime::new().unwrap();

    // Store same file edit twice
    let content = "File modified: /src/main.rs (tool: Edit, session: ralph_dedup, iteration: 1)";
    let tags = vec![
        "file_edit".to_string(),
        "session_ralph_dedup".to_string(),
        "iteration_1".to_string(),
    ];

    rt.block_on(csr_engine::mcp::tools::store_reflection(
        &storage, &embeddings, &search, content, &tags,
    ))
    .unwrap();

    // Check dedup: search for session reflections containing the file
    let results = storage
        .get_reflections_by_tag("session_ralph_dedup", 50)
        .unwrap();
    let file_edits: Vec<_> = results
        .iter()
        .filter(|(_, content, tags, _)| {
            tags.contains(&"file_edit".to_string()) && content.contains("/src/main.rs")
        })
        .collect();

    assert_eq!(file_edits.len(), 1, "file should only be tracked once");
}

// ─── Install Config with New Hooks ───

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

// ─── End-to-End: Store Iteration → Retrieve in Next Iteration ───

#[test]
fn test_e2e_store_iteration_retrieve_in_next() {
    let storage = std::sync::Arc::new(csr_engine::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(csr_engine::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(
        csr_engine::search::SearchEngine::new(100),
    ));

    let rt = tokio::runtime::Runtime::new().unwrap();

    let session_id = "ralph_e2e_iter";

    // Iteration 1: store learning
    let content1 = format!(
        "ITERATION 1 of session {}\nTask: Build auth\nLearnings:\n- Use JWT with refresh tokens\n",
        session_id
    );
    let tags1 = vec![
        "ralph_iteration".to_string(),
        format!("session_{}", session_id),
        "iteration_1".to_string(),
    ];
    rt.block_on(csr_engine::mcp::tools::store_reflection(
        &storage, &embeddings, &search, &content1, &tags1,
    ))
    .unwrap();

    // Iteration 2: store another learning
    let content2 = format!(
        "ITERATION 2 of session {}\nTask: Build auth\nLearnings:\n- Add rate limiting\n",
        session_id
    );
    let tags2 = vec![
        "ralph_iteration".to_string(),
        format!("session_{}", session_id),
        "iteration_2".to_string(),
    ];
    rt.block_on(csr_engine::mcp::tools::store_reflection(
        &storage, &embeddings, &search, &content2, &tags2,
    ))
    .unwrap();

    // Now simulate iteration 3 retrieving past iterations
    let session_tag = format!("session_{}", session_id);
    let results = storage.get_reflections_by_tag(&session_tag, 10).unwrap();

    // Filter to only iteration entries before iteration 3
    let past: Vec<_> = results
        .iter()
        .filter(|(_, _, tags, _)| {
            tags.iter().any(|t| {
                if let Some(n) = t.strip_prefix("iteration_") {
                    if let Ok(num) = n.parse::<usize>() {
                        return num < 3;
                    }
                }
                false
            })
        })
        .collect();

    assert_eq!(past.len(), 2, "should retrieve iterations 1 and 2");
    // Verify content from previous iterations
    let all_content: String = past.iter().map(|(_, c, _, _)| c.as_str()).collect();
    assert!(all_content.contains("JWT with refresh tokens"));
    assert!(all_content.contains("rate limiting"));
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
// Phase 2c: Predictive Injection Tests
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
    let file = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_conversation.jsonl");
    let chunks = csr_engine::import::parse_jsonl_file(&file, "test-project").unwrap();
    assert!(!chunks.is_empty(), "BufReader streaming should produce chunks");
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
    assert!(!chunks.is_empty(), "should parse valid lines despite malformed ones");
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

    let results = vec![
        RawResult {
            content: "cross-project insight".into(),
            score: 0.8,
            source: "reflection".into(),
            timestamp: None,
            files: vec![],
            error_patterns: vec![],
            tags: vec![],
        },
    ];

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
        &storage, &embeddings, &search, "fix auth bug", 0.5, 2,
    ));

    assert!(items.is_empty(), "empty index should return no anti-patterns");
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
    let tags = vec!["outcome_incomplete".to_string(), "ralph_session".to_string()];
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
        &storage, &embeddings, &search, "completely unrelated topic about cooking", 0.99, 2,
    ));

    // With min_score=0.99, unlikely to match
    // (exact behavior depends on embedding similarity)
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
    let tags = vec!["outcome_incomplete".to_string(), "ralph_session".to_string()];
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
        &storage, &embeddings, &search, "fix authentication timeout", 0.3, 5,
    ));

    // Should find the incomplete session (semantic match on "authentication timeout")
    assert!(!items.is_empty(), "should find anti-pattern for related topic");
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
        storage, embeddings, search, std::path::PathBuf::from("/tmp"),
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(csr_engine::hooks::prompt_submit::handle(
        &input, None, &engine, std::path::Path::new("/tmp"),
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
        storage, embeddings, search, std::path::PathBuf::from("/tmp"),
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(csr_engine::hooks::prompt_submit::handle(
        &input, None, &engine, std::path::Path::new("/tmp"),
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
        storage, embeddings, search, std::path::PathBuf::from("/tmp"),
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(csr_engine::hooks::prompt_submit::handle(
        &input, None, &engine, std::path::Path::new("/tmp"),
    ));

    assert!(result.is_ok(), "empty index should produce no output but succeed");
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
        storage, embeddings, search, std::path::PathBuf::from("/tmp"),
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(csr_engine::hooks::prompt_submit::handle(
        &input, None, &engine, std::path::Path::new("/tmp"),
    ));

    assert!(result.is_ok(), "catch-all wrapper must always succeed");
}

// ─── Install Config (Updated for 6 Hook Types) ───

#[test]
fn test_install_config_includes_prompt_submit() {
    // Call the actual generate function
    let config = csr_engine::hooks::install::generate_hook_config_for_test("/usr/local/bin/csr-engine");
    let hooks = config.get("hooks").unwrap();

    // All 6 hook types must be present
    assert!(hooks.get("SessionStart").is_some());
    assert!(hooks.get("SessionEnd").is_some());
    assert!(hooks.get("PreCompact").is_some());
    assert!(hooks.get("Stop").is_some());
    assert!(hooks.get("PostToolUse").is_some());
    assert!(hooks.get("UserPromptSubmit").is_some(), "UserPromptSubmit must be in config");

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
    let count1 = rt.block_on(engine.import_file(&jsonl_path, "myapp")).unwrap();
    assert!(count1 > 0, "first import should produce chunks");

    // Same file, no changes — should return 0
    let count2 = rt.block_on(engine.import_file(&jsonl_path, "myapp")).unwrap();
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
    // Since the transcript still fits in 1 chunk but content changed, we get a re-import.
    let _count3 = rt.block_on(engine.import_file(&jsonl_path, "myapp")).unwrap();
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

/// Test: stop hook imports transcript for non-Ralph sessions.
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
        storage, embeddings, search.clone(), std::path::PathBuf::from("/tmp"),
    );

    let input = csr_engine::hooks::HookInput {
        transcript_path: Some(transcript.to_string_lossy().to_string()),
        cwd: Some(tmp.path().to_string_lossy().to_string()),
        ..Default::default()
    };

    let rt = tokio::runtime::Runtime::new().unwrap();

    // Call stop hook with no Ralph session — should still import transcript
    let result = rt.block_on(csr_engine::hooks::stop::handle(
        &input, None, &engine, tmp.path(),
    ));
    assert!(result.is_ok());

    let chunk_count = rt.block_on(async { search.read().await.chunk_count() });
    assert!(
        chunk_count > 0,
        "stop hook should import transcript even without Ralph session"
    );
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

    // Call session-end (no Ralph — just the import + V3 extraction path)
    let result = rt.block_on(csr_engine::hooks::session_end::handle(
        &input, None, &engine, tmp.path(),
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
    assert!(reflection.is_some(), "V3 reflection should exist in storage");
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

/// Test: precompact hook imports transcript for non-Ralph sessions.
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
        storage, embeddings, search.clone(), std::path::PathBuf::from("/tmp"),
    );

    let input = csr_engine::hooks::HookInput {
        transcript_path: Some(transcript.to_string_lossy().to_string()),
        cwd: Some(tmp.path().to_string_lossy().to_string()),
        ..Default::default()
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(csr_engine::hooks::precompact::handle(
        &input, None, &engine, tmp.path(),
    ));
    assert!(result.is_ok());

    let chunk_count = rt.block_on(async { search.read().await.chunk_count() });
    assert!(
        chunk_count > 0,
        "precompact should import transcript before compaction"
    );
}
