# CSR v9 — Dreamer + AST v2 + Outcome Scoring Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Turn CSR from a high-speed recall engine into a local learning system that tracks code evolution, consolidates facts from sessions, and scores injection effectiveness — all in the same single binary.

**Architecture:** Four features layered bottom-up: (1) foundation fixes (busy_timeout, memory_id on RawResult, current_files population), (2) outcome-scored injection using existing retrieval_events table, (3) AST v2 incremental diffing in PostToolUse, (4) Dreamer v1 consolidation producing typed facts. Session-aware review context emerges from wiring (2)+(3)+(4) into PromptSubmit.

**Tech Stack:** Rust, rusqlite 0.38, ast-grep-core 0.40, tree-sitter (via ast-grep), FastEmbed, HNSW, Anthropic Batch API

---

## Task 1: Foundation — busy_timeout + memory_id + current_files

These are prerequisites flagged by Codex review. Without them, outcome scoring and review context cannot work.

**Files:**
- Modify: `csr-engine/src/storage/mod.rs:22` (add busy_timeout)
- Modify: `csr-engine/src/injection/predictor.rs:34` (add memory_id to RawResult)
- Modify: `csr-engine/src/hooks/prompt_submit.rs:107-108` (populate current_files)
- Test: `csr-engine/tests/` (existing test suite must still pass)

**Step 1: Add busy_timeout pragma**

In `storage/mod.rs:22`, after the existing WAL pragma:

```rust
conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;")?;
```

**Step 2: Add memory_id to RawResult**

In `injection/predictor.rs:34`, add field to `RawResult`:

```rust
pub struct RawResult {
    pub content: String,
    pub score: f32,
    pub source: String,
    pub timestamp: Option<String>,
    pub files: Vec<String>,
    pub error_patterns: Vec<String>,
    pub tags: Vec<String>,
    pub conversation_id: Option<String>,
    /// Stable storage ID (chunk_id or reflection_id) for outcome tracking.
    pub memory_id: Option<String>,
}
```

Update all RawResult constructors in `prompt_submit.rs` (search_chunks_with_vec, search_reflections_with_vec) to pass actual chunk/reflection IDs.

**Step 3: Fix current_files population**

In `prompt_submit.rs:107-108`, replace the hardcoded empty vecs. Extract file paths from the user's prompt and recent tool_use context:

```rust
// Extract file paths mentioned in the prompt (simple regex: paths with extensions)
let current_files: Vec<String> = extract_file_paths_from_prompt(prompt);
let current_errors: Vec<String> = extract_error_patterns_from_prompt(prompt);
```

Add helper function `extract_file_paths_from_prompt()` that uses regex to find paths like `src/foo/bar.rs`, `./file.py`, etc.

**Step 4: Fix TAD logging to use stable IDs**

In `prompt_submit.rs:174`, replace the content hash with the actual memory_id:

```rust
// BEFORE (hash-based, unstable):
let memory_id = format!("{:x}", { ... hash ... });

// AFTER (stable storage ID):
let memory_id = match &result.memory_id {
    Some(id) => id.clone(),
    None => continue, // skip items without stable IDs
};
```

**Step 5: Run all tests**

Run: `cd csr-engine && cargo test`
Expected: All existing tests pass (these are additive changes)

**Step 6: Commit**

```bash
git add csr-engine/src/storage/mod.rs csr-engine/src/injection/predictor.rs csr-engine/src/hooks/prompt_submit.rs
git commit -m "feat(v9): foundation — busy_timeout, stable memory_id, current_files extraction"
```

---

## Task 2: Outcome-Scored Injection

Activate the existing `retrieval_events` table as a scoring signal. The TAD infrastructure already logs events and updates outcomes — we just need to read them during scoring.

**Files:**
- Modify: `csr-engine/src/injection/predictor.rs` (add outcome multiplier)
- Modify: `csr-engine/src/storage/migrations.rs` (add retrieval_stats rollup table)
- Modify: `csr-engine/src/storage/queries.rs` (add rollup query + update)
- Modify: `csr-engine/src/storage/mod.rs` (expose rollup methods)
- Modify: `csr-engine/src/hooks/session_end.rs` (compute rollup at session end)
- Modify: `csr-engine/src/hooks/prompt_submit.rs` (pass outcome scores to predictor)
- Test: `csr-engine/src/injection/predictor.rs` (new test for outcome scoring)

**Step 1: Write the failing test**

In predictor.rs tests:

```rust
#[test]
fn test_outcome_multiplier_boosts_successful_memories() {
    let result = ScoredResult {
        content: "test".into(),
        raw_score: 0.8,
        final_score: 0.5,
        source: "chunk".into(),
        signals: vec![],
    };
    // Memory with 5 successes, 1 failure → positive delta
    let boosted = apply_outcome_multiplier(result.final_score, 5, 1);
    assert!(boosted > 0.5);
    assert!(boosted < 0.7); // Bounded, not runaway

    // Memory with 1 success, 5 failures → negative delta
    let penalized = apply_outcome_multiplier(0.5, 1, 5);
    assert!(penalized < 0.5);
    assert!(penalized > 0.3); // Bounded, not zeroed
}

#[test]
fn test_outcome_multiplier_requires_minimum_events() {
    // Memory with <3 events → no change (Codex recommendation)
    let unchanged = apply_outcome_multiplier(0.5, 1, 0);
    assert_eq!(unchanged, 0.5);
}
```

**Step 2: Run test to verify it fails**

Run: `cd csr-engine && cargo test test_outcome_multiplier`
Expected: FAIL — `apply_outcome_multiplier` not defined

**Step 3: Add retrieval_stats rollup table**

In `storage/migrations.rs`, add after the retrieval_events block:

```rust
conn.execute_batch(
    "
    CREATE TABLE IF NOT EXISTS retrieval_stats (
        memory_id TEXT PRIMARY KEY,
        success_count INTEGER DEFAULT 0,
        failure_count INTEGER DEFAULT 0,
        neutral_count INTEGER DEFAULT 0,
        last_updated TEXT DEFAULT (datetime('now'))
    );

    CREATE INDEX IF NOT EXISTS idx_retrieval_stats_updated
        ON retrieval_stats(last_updated DESC);
    "
)?;
```

**Step 4: Add rollup query to storage/queries.rs**

```rust
/// Compute retrieval stats from events and upsert into rollup table.
pub fn update_retrieval_stats(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute_batch(&format!(
        "INSERT OR REPLACE INTO retrieval_stats (memory_id, success_count, failure_count, neutral_count, last_updated)
         SELECT memory_id,
                SUM(CASE WHEN session_outcome = 'success' THEN 1 ELSE 0 END),
                SUM(CASE WHEN session_outcome IN ('stuck', 'abandoned') THEN 1 ELSE 0 END),
                SUM(CASE WHEN session_outcome = 'neutral' THEN 1 ELSE 0 END),
                datetime('now')
         FROM retrieval_events
         WHERE session_id = '{session_id}'
         GROUP BY memory_id"
    ))?;
    Ok(())
}

/// Batch-fetch outcome stats for scoring.
pub fn get_outcome_stats_batch(
    conn: &Connection,
    memory_ids: &[&str],
) -> Result<HashMap<String, (i64, i64)>> {
    // Returns HashMap<memory_id, (success_count, failure_count)>
    // Only returns entries with success_count + failure_count >= 3 (minimum events gate)
    ...
}
```

**Step 5: Implement outcome multiplier in predictor.rs**

```rust
/// Post-scoring outcome multiplier (Codex recommendation: gated, bounded).
/// Only applies when memory has >= 3 non-neutral retrieval events.
pub fn apply_outcome_multiplier(base_score: f32, successes: i64, failures: i64) -> f32 {
    let total = successes + failures;
    if total < 3 {
        return base_score; // Not enough signal
    }
    let ratio = successes as f32 / total as f32; // 0.0 to 1.0
    let delta = (ratio - 0.5) * 0.2; // -0.1 to +0.1
    (base_score * (1.0 + delta)).clamp(0.05, 1.0)
}
```

**Step 6: Wire into prompt_submit.rs**

After `rank_results_with_continuity()`, batch-fetch outcome stats and apply multiplier:

```rust
// Fetch outcome stats for all candidate memory IDs
let memory_ids: Vec<&str> = scored.iter()
    .filter_map(|r| r.memory_id.as_deref())
    .collect();
let outcome_stats = storage.get_outcome_stats_batch(&memory_ids).unwrap_or_default();

// Apply outcome multiplier
for result in &mut scored {
    if let Some(ref mid) = result.memory_id {
        if let Some(&(successes, failures)) = outcome_stats.get(mid) {
            result.final_score = predictor::apply_outcome_multiplier(
                result.final_score, successes, failures
            );
        }
    }
}
```

**Step 7: Wire rollup into session_end.rs**

After the existing TAD outcome update:

```rust
// Compute retrieval stats rollup for this session
if let Some(ref session_id) = input.session_id {
    let _ = storage.update_retrieval_stats(session_id);
}
```

**Step 8: Run tests**

Run: `cd csr-engine && cargo test`
Expected: All pass including new outcome tests

**Step 9: Commit**

```bash
git add csr-engine/src/injection/predictor.rs csr-engine/src/storage/ csr-engine/src/hooks/prompt_submit.rs csr-engine/src/hooks/session_end.rs
git commit -m "feat(v9): outcome-scored injection — retrieval_stats rollup + gated multiplier"
```

---

## Task 3: AST v2 — Incremental Code Evolution Tracking

Track structural code changes (functions added/removed, types changed) per edit in PostToolUse. Store diffs, expose via new MCP tool.

**Files:**
- Modify: `csr-engine/src/storage/migrations.rs` (add code_evolution table)
- Modify: `csr-engine/src/storage/queries.rs` (add code_evolution CRUD)
- Modify: `csr-engine/src/storage/mod.rs` (expose code_evolution methods)
- Modify: `csr-engine/src/extraction/ast_analysis.rs` (add incremental diff function)
- Modify: `csr-engine/src/hooks/post_tool_use.rs` (capture AST diffs on Edit/Write)
- Create: `csr-engine/src/mcp/code_evolution.rs` (MCP tool handler — if separate file needed)
- Modify: `csr-engine/src/mcp/tools.rs` (register csr_code_evolution tool)
- Test: new tests for AST diffing and code_evolution storage

**Step 1: Write failing test for AST diff**

In `extraction/ast_analysis.rs` tests:

```rust
#[test]
fn test_ast_diff_detects_added_function() {
    let before = "fn foo() {}";
    let after = "fn foo() {}\nfn bar() {}";
    let diff = compute_ast_diff(before, after, "rust");
    assert!(diff.functions_added.contains(&"bar".to_string()));
    assert!(diff.functions_removed.is_empty());
}

#[test]
fn test_ast_diff_detects_removed_function() {
    let before = "fn foo() {}\nfn bar() {}";
    let after = "fn foo() {}";
    let diff = compute_ast_diff(before, after, "rust");
    assert!(diff.functions_removed.contains(&"bar".to_string()));
}
```

**Step 2: Run test to verify it fails**

Run: `cd csr-engine && cargo test test_ast_diff`
Expected: FAIL — `compute_ast_diff` not defined

**Step 3: Add code_evolution table**

In `storage/migrations.rs`:

```rust
conn.execute_batch(
    "
    CREATE TABLE IF NOT EXISTS code_evolution (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        file_path TEXT NOT NULL,
        language TEXT,
        timestamp TEXT NOT NULL DEFAULT (datetime('now')),
        tool_name TEXT,
        functions_added TEXT DEFAULT '[]',
        functions_removed TEXT DEFAULT '[]',
        types_added TEXT DEFAULT '[]',
        types_removed TEXT DEFAULT '[]',
        imports_added TEXT DEFAULT '[]',
        imports_removed TEXT DEFAULT '[]'
    );

    CREATE INDEX IF NOT EXISTS idx_code_evolution_file ON code_evolution(file_path);
    CREATE INDEX IF NOT EXISTS idx_code_evolution_session ON code_evolution(session_id, timestamp);
    "
)?;
```

**Step 4: Implement compute_ast_diff**

In `extraction/ast_analysis.rs`:

```rust
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AstDiff {
    pub functions_added: Vec<String>,
    pub functions_removed: Vec<String>,
    pub types_added: Vec<String>,
    pub types_removed: Vec<String>,
    pub imports_added: Vec<String>,
    pub imports_removed: Vec<String>,
}

/// Compute structural diff between before and after code.
/// Uses ast-grep to extract symbols from both, then set-diff.
pub fn compute_ast_diff(before: &str, after: &str, language: &str) -> AstDiff {
    let before_ctx = extract_symbols_from_source(before, language);
    let after_ctx = extract_symbols_from_source(after, language);

    AstDiff {
        functions_added: set_diff(&after_ctx.functions, &before_ctx.functions),
        functions_removed: set_diff(&before_ctx.functions, &after_ctx.functions),
        types_added: set_diff(&after_ctx.types, &before_ctx.types),
        types_removed: set_diff(&before_ctx.types, &after_ctx.types),
        imports_added: set_diff(&after_ctx.imports, &before_ctx.imports),
        imports_removed: set_diff(&before_ctx.imports, &after_ctx.imports),
    }
}

/// Extract symbols from a single source string (helper reusing existing AST machinery).
fn extract_symbols_from_source(source: &str, language: &str) -> CodeContext {
    // Reuse existing SupportLang parsing + KindMatcher extraction
    // Wrap in catch_unwind for robustness
    ...
}

fn set_diff(a: &BTreeSet<String>, b: &BTreeSet<String>) -> Vec<String> {
    a.difference(b).cloned().collect()
}
```

**Step 5: Wire into PostToolUse hook**

In `hooks/post_tool_use.rs`:

```rust
pub async fn handle(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    // Existing: import transcript
    super::import_current_transcript(input, engine, cwd).await;

    // NEW: Track code evolution for Edit/Write operations
    if let Some(ref tool_name) = input.tool_name {
        if tool_name == "Edit" || tool_name == "Write" {
            if let Err(e) = track_code_evolution(input, engine).await {
                eprintln!("CSR: code evolution tracking error (non-fatal): {}", e);
            }
        }
    }

    Ok(())
}

async fn track_code_evolution(input: &HookInput, engine: &Engine) -> Result<()> {
    let tool_input = input.tool_input.as_ref().ok_or_else(|| anyhow::anyhow!("no tool_input"))?;
    let file_path = tool_input.get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("no file_path"))?;

    // Detect language from file extension
    let language = detect_language_from_path(file_path);

    // For Edit: compute diff between old_string and new_string
    // For Write: treat as all-new (before = "")
    let (before, after) = if input.tool_name.as_deref() == Some("Edit") {
        let old = tool_input.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
        let new = tool_input.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
        (old.to_string(), new.to_string())
    } else {
        let content = tool_input.get("content").and_then(|v| v.as_str()).unwrap_or("");
        (String::new(), content.to_string())
    };

    let diff = crate::extraction::ast_analysis::compute_ast_diff(&before, &after, &language);

    // Only store if something structural actually changed
    if diff.functions_added.is_empty() && diff.functions_removed.is_empty()
        && diff.types_added.is_empty() && diff.types_removed.is_empty()
        && diff.imports_added.is_empty() && diff.imports_removed.is_empty()
    {
        return Ok(());
    }

    let session_id = input.session_id.as_deref().unwrap_or("unknown");
    engine.storage().insert_code_evolution(
        session_id, file_path, &language,
        input.tool_name.as_deref().unwrap_or("unknown"),
        &diff,
    )?;

    Ok(())
}
```

**Step 6: Add MCP tool csr_code_evolution**

In `mcp/tools.rs`, add new tool:

```rust
#[tool(
    name = "csr_code_evolution",
    description = "Query structural code changes (functions, types, imports added/removed) across sessions for a file or project",
    annotations(read_only_hint = true, destructive_hint = false, idempotent_hint = true)
)]
async fn code_evolution(
    &self,
    #[tool(param)]
    params: Parameters<CodeEvolutionParams>,
) -> Result<CallToolResult, McpError> {
    // Query code_evolution table filtered by file_path and/or session_id
    // Return formatted structural change history
    ...
}
```

**Step 7: Run tests**

Run: `cd csr-engine && cargo test`
Expected: All pass including new AST diff tests

**Step 8: Commit**

```bash
git add csr-engine/src/extraction/ast_analysis.rs csr-engine/src/hooks/post_tool_use.rs csr-engine/src/storage/ csr-engine/src/mcp/
git commit -m "feat(v9): AST v2 — incremental code evolution tracking in PostToolUse + MCP tool"
```

---

## Task 4: Dreamer v1 — Typed Fact Consolidation

Upgrade daemon's V3 extraction to produce typed facts (decisions, conventions, preferences, bug patterns). These are stored as additional tagged reflections — NOT superseding V3 (Codex recommendation).

**Files:**
- Modify: `csr-engine/src/daemon/mod.rs` (add consolidation loop)
- Create: `csr-engine/src/daemon/consolidation.rs` (fact extraction logic)
- Modify: `csr-engine/src/storage/queries.rs` (add fact storage + queries)
- Modify: `csr-engine/src/storage/mod.rs` (expose fact methods)
- Test: new tests for fact extraction and consolidation

**Step 1: Write failing test**

```rust
#[test]
fn test_extract_facts_from_v3_narrative() {
    let narrative = "User decided to use axum instead of warp for the web server. \
        Convention established: all handlers must validate input before DB access. \
        Recurring bug: off-by-one in pagination when offset equals total.";

    let facts = extract_facts(narrative);
    assert!(facts.iter().any(|f| f.fact_type == "architectural_decision"));
    assert!(facts.iter().any(|f| f.fact_type == "convention"));
    assert!(facts.iter().any(|f| f.fact_type == "bug_pattern"));
}
```

**Step 2: Run test to verify it fails**

Run: `cd csr-engine && cargo test test_extract_facts`
Expected: FAIL — `extract_facts` not defined

**Step 3: Implement consolidation module**

Create `csr-engine/src/daemon/consolidation.rs`:

```rust
//! Dreamer v1 — consolidates raw session narratives into typed facts.
//!
//! Fact types:
//! - architectural_decision: "Chose X because Y"
//! - convention: "Always do X when Y"
//! - preference: "User prefers X over Y"
//! - bug_pattern: "X keeps happening because Y"

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidatedFact {
    pub fact_type: String,
    pub content: String,
    pub confidence: f32,
    pub source_conversation_ids: Vec<String>,
}

/// Extract typed facts from a V3 narrative or AI narrative.
/// Uses keyword heuristics + sentence structure analysis.
/// This is Layer 0 of the Dreamer — no LLM calls.
pub fn extract_facts(narrative: &str) -> Vec<ConsolidatedFact> {
    let mut facts = Vec::new();

    for sentence in narrative.split(|c| c == '.' || c == '\n') {
        let s = sentence.trim();
        if s.is_empty() { continue; }
        let lower = s.to_lowercase();

        // Decision detection
        if lower.contains("decided") || lower.contains("chose") || lower.contains("switched to")
            || lower.contains("instead of") || lower.contains("migrated")
        {
            facts.push(ConsolidatedFact {
                fact_type: "architectural_decision".into(),
                content: s.to_string(),
                confidence: 0.7,
                source_conversation_ids: vec![],
            });
            continue;
        }

        // Convention detection
        if lower.contains("convention") || lower.contains("always") || lower.contains("must")
            || lower.contains("should not") || lower.contains("never")
            || lower.contains("rule:")
        {
            facts.push(ConsolidatedFact {
                fact_type: "convention".into(),
                content: s.to_string(),
                confidence: 0.7,
                source_conversation_ids: vec![],
            });
            continue;
        }

        // Bug pattern detection
        if lower.contains("bug") || lower.contains("recurring") || lower.contains("keeps happening")
            || lower.contains("off-by-one") || lower.contains("regression")
        {
            facts.push(ConsolidatedFact {
                fact_type: "bug_pattern".into(),
                content: s.to_string(),
                confidence: 0.6,
                source_conversation_ids: vec![],
            });
            continue;
        }

        // Preference detection
        if lower.contains("prefer") || lower.contains("likes") || lower.contains("rather than")
            || lower.contains("favorite") || lower.contains("style")
        {
            facts.push(ConsolidatedFact {
                fact_type: "preference".into(),
                content: s.to_string(),
                confidence: 0.5,
                source_conversation_ids: vec![],
            });
        }
    }

    facts
}
```

**Step 4: Add consolidation loop to daemon**

In `daemon/mod.rs`, add a fourth loop alongside extraction_loop and narrator_loop:

```rust
/// Consolidation loop — runs after V3 extraction, produces typed facts.
async fn consolidation_loop(engine: Arc<Engine>, shutdown: Arc<AtomicBool>) {
    let interval = Duration::from_secs(120); // Run every 2 minutes
    loop {
        if shutdown.load(Ordering::Relaxed) { break; }
        if let Err(e) = consolidation_loop_inner(&engine).await {
            eprintln!("CSR daemon: consolidation error: {}", e);
        }
        tokio::time::sleep(interval).await;
    }
}

async fn consolidation_loop_inner(engine: &Engine) -> Result<()> {
    // Find V3-enriched conversations that haven't been consolidated yet
    let unconsolidated = engine.storage()
        .get_unconsolidated_conversations(10)?; // batch of 10

    for (conv_id, reflection_content) in unconsolidated {
        let facts = consolidation::extract_facts(&reflection_content);
        if facts.is_empty() { continue; }

        for fact in &facts {
            // Store each fact as a tagged reflection
            let tags = vec![
                format!("consolidated_fact"),
                format!("fact_type_{}", fact.fact_type),
                format!("conv_{}", conv_id),
            ];
            let content = format!("[{}] {}", fact.fact_type, fact.content);
            engine.store_reflection_with_tags(&content, tags).await?;
        }

        // Mark conversation as consolidated
        engine.storage().mark_consolidated(&conv_id)?;
    }

    Ok(())
}
```

**Step 5: Add storage queries for consolidation**

In `storage/queries.rs`:

```rust
/// Get conversations with V3 extraction but no consolidation.
pub fn get_unconsolidated_conversations(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<(String, String)>> {
    // JOIN enrichment_state (has extracted_v3 completed)
    // LEFT JOIN enrichment_state (no consolidated_fact)
    // INNER JOIN reflections (get the V3 content)
    ...
}

/// Mark a conversation as consolidated.
pub fn mark_consolidated(conn: &Connection, conversation_id: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO enrichment_state (conversation_id, enrichment_type, status)
         VALUES (?1, 'consolidated_fact', 'completed')",
        rusqlite::params![conversation_id],
    )?;
    Ok(())
}
```

**Step 6: Run tests**

Run: `cd csr-engine && cargo test`
Expected: All pass

**Step 7: Commit**

```bash
git add csr-engine/src/daemon/ csr-engine/src/storage/
git commit -m "feat(v9): Dreamer v1 — typed fact consolidation (decisions, conventions, preferences, bug patterns)"
```

---

## Task 5: Session-Aware Review Context

Wire outcomes + AST evolution + consolidated facts into PromptSubmit injection. This is the user-visible payoff.

**Files:**
- Modify: `csr-engine/src/hooks/prompt_submit.rs` (add review context assembly)
- Modify: `csr-engine/src/injection/mod.rs` (add review_context field to InjectionContext if needed)
- Test: integration test for review context injection

**Step 1: Write failing test**

```rust
#[test]
fn test_review_context_includes_code_evolution() {
    // Setup: store a code_evolution record for "src/auth.rs"
    // User prompt mentions "auth"
    // Verify: injection includes code evolution context
    ...
}
```

**Step 2: Implement review context assembly**

In `prompt_submit.rs`, after the existing search + scoring block, add:

```rust
// 6. Session-aware review context
// Query consolidated facts matching current files
let review_items = assemble_review_context(engine, &current_files, prompt).await;
for item in review_items {
    ctx.relevant_context.push(item);
}
```

```rust
async fn assemble_review_context(
    engine: &Engine,
    current_files: &[String],
    prompt: &str,
) -> Vec<InjectionItem> {
    let mut items = Vec::new();

    // 1. Code evolution for mentioned files
    for file in current_files.iter().take(3) {
        if let Ok(evolutions) = engine.storage().get_recent_code_evolution(file, 5) {
            if !evolutions.is_empty() {
                let summary = format_evolution_summary(file, &evolutions);
                items.push(InjectionItem {
                    content: summary,
                    score: 0.9, // High priority
                    source: "code_evolution".into(),
                });
            }
        }
    }

    // 2. Consolidated facts (conventions, decisions) relevant to current context
    if let Ok(facts) = engine.storage().search_consolidated_facts(prompt, 3) {
        for (content, fact_type) in facts {
            items.push(InjectionItem {
                content: format!("[{}] {}", fact_type, content),
                score: 0.85,
                source: "consolidated_fact".into(),
            });
        }
    }

    items
}

fn format_evolution_summary(file: &str, evolutions: &[(String, String, String)]) -> String {
    // Format: "src/auth.rs: +2 functions (validate, authorize), -1 (old_check) across 3 sessions"
    ...
}
```

**Step 3: Add phase_boost for new source types**

In `injection/weights.rs`, update `compute_phase_boost` for PromptSubmit:

```rust
HookPhase::PromptSubmit => {
    if source == "code_evolution" {
        return 0.9; // High priority — structural context
    }
    if tags.iter().any(|t| t.starts_with("consolidated_fact")) {
        return 0.85; // Conventions and decisions
    }
    // ... existing rules ...
}
```

**Step 4: Run tests**

Run: `cd csr-engine && cargo test`
Expected: All pass

**Step 5: Commit**

```bash
git add csr-engine/src/hooks/prompt_submit.rs csr-engine/src/injection/
git commit -m "feat(v9): session-aware review context — code evolution + consolidated facts in injection"
```

---

## Task 6: Integration Test + Full Build Verification

**Step 1: Run full test suite**

```bash
cd csr-engine && cargo test
cd csr-engine && cargo test --test hooks_integration
cd csr-engine && cargo test --test integration
```

**Step 2: Run clippy + fmt**

```bash
cd csr-engine && cargo fmt && cargo clippy
```

**Step 3: Build release**

```bash
cd csr-engine && cargo build --release
```
Expected: Clean build, ~44-45MB binary

**Step 4: Run eval**

```bash
./target/release/csr-engine eval
./target/release/csr-engine eval --full
```

**Step 5: Commit any fixes**

```bash
git add -A
git commit -m "chore(v9): integration fixes and cleanup"
```

---

## Dependency Graph

```
Task 1 (Foundation)
  ├── Task 2 (Outcome Scoring) — needs memory_id + current_files
  ├── Task 3 (AST v2) — needs code_evolution table
  └── Task 4 (Dreamer v1) — independent, but facts feed Task 5
        │
        └── Task 5 (Review Context) — needs outcomes + AST + facts
              │
              └── Task 6 (Integration) — validates everything
```

Tasks 2, 3, and 4 can run in parallel after Task 1.
Task 5 requires 2, 3, and 4 complete.
Task 6 is final validation.

---

## Risk Mitigations (from Codex Review)

| Risk | Mitigation |
|------|------------|
| Weight sum != 1.0 | Outcome scoring is a POST-scoring multiplier, not a new weight |
| Per-candidate DB queries | Batch query via `get_outcome_stats_batch()`, one call per prompt |
| PostToolUse latency | AST diff is lightweight (~1ms for small edits), non-blocking on failure |
| V3 supersession breakage | Consolidated facts are ADDITIONAL reflections, not replacements |
| SQLite lock contention | `busy_timeout=5000` added in Task 1 |
| Empty current_files | Regex extraction from prompt; graceful degradation if none found |
