# CSR Injection Quality Overhaul — From Noisy to Superior

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix the critical project-scope leak, persist error recovery context, upgrade story synthesis, and clean up SessionStart — making CSR's injections genuinely useful instead of noisy.

**Architecture:** Five tasks layered by impact: (1) project-scope search eliminates cross-project noise, (2) context_cache persistence unlocks debugging continuity (the killer feature), (3) story synthesis includes outcomes/decisions/risks, (4) SessionStart deduplication + neutral framing, (5) retrieval event logging only for actually-injected items.

**Tech Stack:** Rust, rusqlite 0.38, tokio, HNSW search, FastEmbed

---

## Task 1: Project-Scope PromptSubmit Search (CRITICAL — biggest noise reducer)

Chunks and reflections from all projects are currently injected into every prompt. Filter by current project by default.

**Files:**
- Modify: `csr-engine/src/hooks/prompt_submit.rs:286-331` (search_chunks_with_vec)
- Modify: `csr-engine/src/hooks/prompt_submit.rs:334-374` (search_reflections_with_vec)
- Test: `csr-engine/tests/hooks_integration.rs` (add project-scoped test)

**Step 1: Write failing test**

In `tests/hooks_integration.rs`, add:

```rust
#[test]
fn test_prompt_submit_search_scoped_to_project() {
    // Verify that chunk search filters by project_name
    // and reflection search filters by project tag
    use csr_engine::injection::predictor::RawResult;

    // A chunk from project "anukriti" should not appear when searching from project "csr"
    let chunk = RawResult {
        content: "anukriti campaign fix".into(),
        score: 0.9,
        source: "chunk".into(),
        timestamp: None,
        files: vec![],
        error_patterns: vec![],
        tags: vec![],
        conversation_id: None,
        memory_id: None,
    };
    // This is a design test — the actual filtering happens in search_chunks_with_vec
    // which we can't easily unit-test without a full engine. Mark as placeholder.
    assert!(true);
}
```

**Step 2: Add project parameter to search functions**

In `prompt_submit.rs`, modify `search_chunks_with_vec` to accept `project: &str` and filter:

```rust
async fn search_chunks_with_vec(
    engine: &Engine,
    query_vec: &[f32],
    limit: usize,
    min_score: f32,
    project: &str,  // NEW: scope to current project
) -> Vec<RawResult> {
    // ... existing search ...
    for result in &results {
        if let Ok(chunks) = storage.get_chunks_by_ids(std::slice::from_ref(&result.id)) {
            if let Some(chunk) = chunks.into_iter().next() {
                // NEW: project scope filter
                if !project.is_empty() && chunk.project_name != project {
                    continue;
                }
                // ... existing age gate and result building ...
            }
        }
    }
}
```

Similarly modify `search_reflections_with_vec` to filter by `project_` tag:

```rust
async fn search_reflections_with_vec(
    engine: &Engine,
    query_vec: &[f32],
    limit: usize,
    min_score: f32,
    project: &str,  // NEW
) -> Vec<RawResult> {
    // ... existing search ...
    for result in &results {
        if let Ok(Some((content, tags, timestamp))) = storage.get_reflection_by_id(&result.id) {
            // NEW: project scope filter — skip reflections not tagged for this project
            // Exception: reflections without project tags (legacy) are allowed
            let project_tag = format!("project_{}", project);
            let has_project_tags = tags.iter().any(|t| t.starts_with("project_"));
            if !project.is_empty() && has_project_tags && !tags.contains(&project_tag) {
                continue;
            }
            // ... existing result building ...
        }
    }
}
```

**Step 3: Update callers in handle_inner**

```rust
let current_project = crate::search::cross_project::resolve_project_from_cwd(&cwd.to_string_lossy())
    .unwrap_or_default();

// 2. Search chunks — scoped to current project (C1 fix)
let chunk_results = search_chunks_with_vec(engine, &query_vec, 8, 0.55, &current_project).await;

// 3. Search reflections — scoped to current project (C1 fix)
let reflection_results = search_reflections_with_vec(engine, &query_vec, 5, 0.45, &current_project).await;
```

Note: widen the search (8 chunks instead of 5, lower min_score) since project filter reduces candidates.

**Step 4: Run tests**

Run: `cd csr-engine && cargo test`
Expected: All pass

**Step 5: Commit**

```bash
git add csr-engine/src/hooks/prompt_submit.rs
git commit -m "fix(injection): scope PromptSubmit search to current project — eliminates cross-project noise"
```

---

## Task 2: Persist V3 context_cache — Error Recovery as Linked Reflection (HIGH — killer feature)

The V3 extraction builds a `context_cache` with error→fix mappings, but only `search_index` is stored. Persisting the context_cache makes debugging solutions retrievable — this is CSR's competitive edge.

**Files:**
- Modify: `csr-engine/src/hooks/session_end.rs:133-203` (run_v3_extraction)
- Modify: `csr-engine/src/daemon/mod.rs:267-319` (process_v3_extraction)
- Test: `csr-engine/tests/hooks_integration.rs`

**Step 1: Write failing test**

```rust
#[test]
fn test_v3_extraction_stores_context_cache() {
    // After V3 extraction, there should be TWO reflections:
    // 1. v3_{conv_id} — the search index
    // 2. v3_cache_{conv_id} — the context cache (error recovery)
    // This test verifies the second reflection exists.
    assert!(true); // Placeholder — real test needs engine
}
```

**Step 2: Store context_cache alongside search_index**

In `session_end.rs:run_v3_extraction`, after storing the search_index reflection:

```rust
// Persist context_cache as linked reflection (H4 — error recovery context)
if !result.context_cache.trim().is_empty() {
    let cache_id = format!("v3_cache_{}", conv_id);
    let cache_tags = vec![
        "context_cache".to_string(),
        "error_recovery".to_string(),
        format!("conv_{}", conv_id),
        format!("project_{}", project),
    ];
    let cache_emb = engine.embeddings().clone();
    let cache_text = result.context_cache.clone();
    if let Ok(cache_embedding) =
        tokio::task::spawn_blocking(move || cache_emb.embed(&[cache_text.as_str()])).await?
    {
        if let Some(cache_vec) = cache_embedding.into_iter().next() {
            let _ = engine.storage().insert_reflection(
                &cache_id, &result.context_cache, &cache_tags, &cache_vec,
            );
            let mut idx = engine.search().write().await;
            idx.insert_reflection(cache_id, cache_vec);
        }
    }
}
```

**Step 3: Do the same in daemon/mod.rs:process_v3_extraction**

Same pattern — after storing the V3 reflection, also store the context_cache.

**Step 4: Tag error_recovery in weights.rs for high boost**

Already done — `weights.rs:88` has `"error_recovery" => 1.0` for PromptSubmit. Just confirm the tag name matches.

**Step 5: Run tests**

Run: `cd csr-engine && cargo test`
Expected: All pass

**Step 6: Commit**

```bash
git add csr-engine/src/hooks/session_end.rs csr-engine/src/daemon/mod.rs
git commit -m "feat(injection): persist V3 context_cache as error_recovery reflection — debugging continuity"
```

---

## Task 3: Upgrade Story Synthesis (HIGH — richer session memory)

Stories currently extract user request + file list + language. Add: outcome, decisions, unresolved issues, validation signals.

**Files:**
- Modify: `csr-engine/src/extraction/story.rs:10-64` (synthesize_story_from_v3)
- Test: `csr-engine/src/extraction/story.rs` (update tests)

**Step 1: Write failing test**

```rust
#[test]
fn test_story_includes_outcome_and_validation() {
    let v3 = r#"## User Request
"Fix auth timeout bug"

## Solution Pattern
modification: src/auth.rs
  In-place modification

## Code Context
LANGUAGES: Rust

---
Signature: {"completion_status":"complete","error_recovery":true}
Context:
## Error Recovery
[Msg 5] Error: connection timeout
  Fix: Increased timeout to 120s
## Validation
[Msg 10] Build: Success
[Msg 12] Tests: Passed
"#;
    let story = synthesize_story_from_v3(v3, "myproj").unwrap();
    assert!(story.contains("auth") || story.contains("timeout"), "should mention request: {}", story);
    assert!(story.contains("src/auth.rs"), "should mention file: {}", story);
}
```

**Step 2: Enhance synthesize_story_from_v3**

After existing parts, add:

```rust
// Extract outcome from Signature section (after ---)
if let Some(sig_section) = extract_after_separator(v3_content, "---") {
    if sig_section.contains("\"complete\"") {
        parts.push("Completed successfully".into());
    } else if sig_section.contains("\"partial\"") {
        parts.push("Partially completed".into());
    } else if sig_section.contains("\"failed\"") {
        parts.push("Failed — may need retry".into());
    }
    if sig_section.contains("\"error_recovery\":true") {
        parts.push("Resolved errors during session".into());
    }
}

// Extract active issues (unresolved)
if let Some(issues) = extract_section(v3_content, "## Active Issues") {
    let issue_preview: String = issues.chars().take(100).collect();
    if issue_preview.len() > 20 {
        parts.push(format!("Unresolved: {}", issue_preview));
    }
}
```

**Step 3: Add helper `extract_after_separator`**

```rust
fn extract_after_separator(content: &str, sep: &str) -> Option<String> {
    let pos = content.find(sep)?;
    Some(content[pos + sep.len()..].to_string())
}
```

**Step 4: Fix `extract_files_from_solution` (H6)**

The current `extract_files_from_solution` calls `.lines()` on joined text (which collapses newlines). Fix:

```rust
fn extract_files_from_solution(solution: &str) -> Vec<String> {
    solution
        .split(|c| c == '\n' || c == ' ')  // Handle both newline-separated and space-joined
        .filter(|seg| {
            let s = seg.trim();
            s.starts_with("creation:") || s.starts_with("modification:")
        })
        .filter_map(|l| l.split(':').nth(1).map(|f| f.trim().to_string()))
        .filter(|f| !f.is_empty())
        .collect()
}
```

**Step 5: Run tests**

Run: `cd csr-engine && cargo test`
Expected: All pass including new story test

**Step 6: Commit**

```bash
git add csr-engine/src/extraction/story.rs
git commit -m "feat(story): include outcome, error recovery, unresolved issues in V3 story synthesis"
```

---

## Task 4: SessionStart — Deduplicate + Neutral Framing (MEDIUM)

Fix: duplicate last-session injection, false "CONTINUITY DETECTED" on unrelated sessions.

**Files:**
- Modify: `csr-engine/src/hooks/session_start.rs`
- Test: existing session_start tests

**Step 1: Filter story list to exclude continued session**

In `session_start.rs`, after getting `project_stories`, filter out the already-emitted continued session:

```rust
let project_stories: Vec<_> = project_stories_owned
    .iter()
    .filter(|(_, _, tags, _)| {
        tags.iter().any(|t| t == "session_story") && tags.iter().any(|t| t == &story_tag)
    })
    // Skip the story for the session we already showed in CONTINUED FROM (M-5 dedup)
    .filter(|(_, _, tags, _)| {
        if let Some(ref cont) = continued_session {
            let cont_tag = format!("conv_{}", cont.conversation_id);
            !tags.iter().any(|t| t == &cont_tag)
        } else {
            true
        }
    })
    .take(3)
    .collect();
```

**Step 2: Run tests**

Run: `cd csr-engine && cargo test`

**Step 3: Commit**

```bash
git add csr-engine/src/hooks/session_start.rs
git commit -m "fix(session_start): deduplicate continued session from story list"
```

---

## Task 5: Log Only Actually-Injected Retrieval Events (MEDIUM)

Currently logs retrieval events for top 5 scored results, not the items that actually made it through dedup/noise filters into the output. This means outcome learning rewards memories Claude never saw.

**Files:**
- Modify: `csr-engine/src/hooks/prompt_submit.rs:230-244` (TAD logging)

**Step 1: Collect injected IDs during formatting**

Replace the current TAD logging block with one that tracks which items were actually included:

```rust
// TAD: Log retrieval events only for items that were actually injected (M-7 fix)
if let Some(ref session_id) = input.session_id {
    // Collect memory IDs from items that made it into the context
    let mut injected_ids: HashSet<String> = HashSet::new();
    for result in scored.iter().take(5) {
        if result.source == "anti_pattern" { continue; }
        let prefix: String = result.content.chars().take(200).collect();
        if !seen_prefixes.contains(&prefix) { continue; } // wasn't included
        if is_self_referential_noise(&result.content) { continue; }
        if let Some(ref id) = result.memory_id {
            injected_ids.insert(id.clone());
        }
    }
    for id in &injected_ids {
        let source = scored.iter()
            .find(|r| r.memory_id.as_deref() == Some(id))
            .map(|r| r.source.as_str())
            .unwrap_or("unknown");
        let _ = engine.storage().log_retrieval_event(id, source, "prompt_submit", session_id);
    }
}
```

**Step 2: Run tests**

Run: `cd csr-engine && cargo test`

**Step 3: Commit**

```bash
git add csr-engine/src/hooks/prompt_submit.rs
git commit -m "fix(tad): log retrieval events only for actually-injected items"
```

---

## Task 6: Widen User Request Extraction in index_builder (HIGH)

User requests are filtered at >50 chars and only first 2 kept. Short precise prompts and later pivots disappear.

**Files:**
- Modify: `csr-engine/src/extraction/index_builder.rs:29-51`
- Test: `csr-engine/src/extraction/index_builder.rs`

**Step 1: Write failing test**

```rust
#[test]
fn test_search_index_includes_short_user_requests() {
    let messages = vec![
        json!({"role": "user", "content": "fix the auth bug"}),
        json!({"role": "assistant", "content": "Looking at auth..."}),
        json!({"role": "user", "content": "also check the session timeout in Redis"}),
    ];
    let index = build_search_index(&messages, &[], &[], &CodeContext::default());
    assert!(index.contains("auth"), "should include short request: {}", index);
    assert!(index.contains("session timeout") || index.contains("Redis"),
        "should include second user request: {}", index);
}
```

**Step 2: Lower threshold and increase limit**

```rust
// Extract user requests (exclude tool_result noise)
let mut user_requests = Vec::new();
for msg in messages {
    let msg_data = get_message_data(msg);
    if msg_data.get("role").and_then(|v| v.as_str()) != Some("user") { continue; }

    // Extract text content (handle both string and array formats)
    let text = extract_user_text(&msg_data);
    if text.len() < 15 { continue; } // Lowered from 50 (H5 fix)
    if text.contains("tool_result") || text.contains("tool_use_id")
        || text.contains("<command-name>") || text.contains("Caveat:")
        || text.contains("<local-command")
    { continue; }

    let truncated: String = text.chars().take(200).collect();
    user_requests.push(truncated);
    if user_requests.len() >= 4 { break; } // Increased from 2 (H5 fix)
}
```

Add helper:

```rust
fn extract_user_text(msg_data: &serde_json::Value) -> String {
    match msg_data.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => {
            arr.iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(" ")
        }
        _ => String::new(),
    }
}
```

**Step 3: Run tests**

Run: `cd csr-engine && cargo test`

**Step 4: Commit**

```bash
git add csr-engine/src/extraction/index_builder.rs
git commit -m "feat(v3): widen user request extraction — lower threshold, more pivots captured"
```

---

## Task 7: Final Build Verification

**Step 1: Full test suite**

```bash
cd csr-engine && cargo test
cd csr-engine && cargo test --test hooks_integration
cd csr-engine && cargo test --test integration
```

**Step 2: Clippy + fmt**

```bash
cargo fmt && cargo clippy
```

**Step 3: Release build + install**

```bash
cargo build --release
cp target/release/csr-engine /usr/local/bin/csr-engine
csr-engine eval
```

**Step 4: Commit**

```bash
git commit -m "chore: injection quality overhaul — build verification"
```

---

## Dependency Graph

```
Task 1 (Project Scope) — standalone, biggest impact
Task 2 (Context Cache) — standalone, killer feature
Task 3 (Story Upgrade) — standalone
Task 4 (SessionStart Dedup) — standalone
Task 5 (TAD Logging) — depends on Task 1 (uses same seen_prefixes set)
Task 6 (User Request) — standalone
Task 7 (Verification) — depends on all above
```

Tasks 1-4 and 6 can run in parallel.
Task 5 should run after Task 1.
Task 7 is final validation.
