# LAPI + TAD + Story Synthesis Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement CSR's three-part algorithmic moat: Lifecycle-Aware Predictive Injection (LAPI), Temporal Attention Decay (TAD), and zero-cost V3 Story Synthesis to beat claude-mem and mem0.

**Architecture:** Four phases building bottom-up: (1) V3 story synthesis for immediate 94%+ coverage, (2) LAPI weight profiles per hook phase, (3) unified decay fixing recency double-count, (4) TAD with retrieval event tracking. Each phase is independently shippable and testable.

**Tech Stack:** Rust, SQLite (rusqlite 0.38), HNSW (hnsw_rs), FastEmbed, chrono, serde, fs2

**Research docs:** `.plans/05-algorithm-research.md`, `docs/research/CSR_NOVEL_ALGORITHMS.md`, `docs/analysis/claude-mem-competitive-analysis.md`

---

## Phase 1: V3 Story Synthesis (94%+ coverage at $0)

### Task 1.1: Story synthesis module — failing tests

**Files:**
- Create: `csr-engine/src/extraction/story.rs`
- Modify: `csr-engine/src/extraction/mod.rs` (add `pub mod story;`)
- Test: inline `#[cfg(test)]` in `story.rs`

**Step 1: Write the failing test**

```rust
// In csr-engine/src/extraction/story.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_synthesize_from_v3_with_user_request() {
        let v3_content = "## User Request\n\"Fix the authentication timeout bug in login flow\"\n\n## Solution Pattern\ncreation: auth.rs\n  Fixed timeout handling\n\n## Code Context\nLANGUAGES: Rust\n";
        let story = synthesize_story_from_v3(v3_content, "my-project");
        assert!(story.is_some());
        let s = story.unwrap();
        assert!(s.contains("authentication") || s.contains("timeout") || s.contains("login"));
        assert!(s.len() <= 500);
        assert!(s.len() >= 20);
    }

    #[test]
    fn test_synthesize_from_v3_empty() {
        let story = synthesize_story_from_v3("", "proj");
        assert!(story.is_none());
    }

    #[test]
    fn test_synthesize_from_heuristic() {
        let heuristic = "[Heuristic] Project: anukriti Messages: 35 (17 user) Tools: Agent, Bash, Edit, Glob, Grep, Read";
        let story = synthesize_story_from_heuristic(heuristic, "anukriti");
        assert!(!story.is_empty());
        assert!(story.contains("anukriti"));
    }

    #[test]
    fn test_needs_haiku_escalation() {
        // Short V3 + few messages = no haiku needed
        assert!(!needs_haiku(Some("## User Request\n\"fix bug\"\n## Solution Pattern\ndone"), Some("heuristic"), 10));
        // No enrichment + many messages = needs haiku
        assert!(needs_haiku(None, None, 20));
        // Long session with short V3 = needs haiku
        assert!(needs_haiku(Some("## User Request\n\"x\""), None, 50));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cd csr-engine && cargo test extraction::story --lib -- --nocapture 2>&1 | tail -5`
Expected: FAIL with "cannot find module" or "function not defined"

**Step 3: Write minimal implementation**

```rust
//! V3-to-Story synthesis — generate 2-3 sentence stories from existing extraction data.
//!
//! Tier 1: V3 search_index → extract User Request + Solution Pattern → story (free, ~2ms)
//! Tier 2: Heuristic enrichment → template story (free, ~1ms)
//! Tier 3: Haiku escalation (only when Tier 1+2 insufficient, $0.004)

/// Synthesize a story from V3 extraction content.
/// Extracts `## User Request` and `## Solution Pattern` sections.
/// Returns None if content is empty or unparseable.
pub fn synthesize_story_from_v3(v3_content: &str, project: &str) -> Option<String> {
    if v3_content.trim().is_empty() {
        return None;
    }

    let user_request = extract_section(v3_content, "## User Request");
    let solution = extract_section(v3_content, "## Solution Pattern");
    let code_ctx = extract_section(v3_content, "## Code Context");

    // Build story from available sections
    let mut parts = Vec::new();

    if let Some(req) = user_request {
        let cleaned = clean_request_text(&req);
        if !cleaned.is_empty() {
            parts.push(cleaned);
        }
    }

    if let Some(sol) = solution {
        let files = extract_files_from_solution(&sol);
        if !files.is_empty() {
            let file_list: String = files.into_iter().take(3).collect::<Vec<_>>().join(", ");
            parts.push(format!("Modified {}", file_list));
        }
    }

    if let Some(ctx) = code_ctx {
        if let Some(lang) = ctx.lines().find(|l| l.starts_with("LANGUAGES:")) {
            parts.push(lang.trim().to_string());
        }
    }

    if parts.is_empty() {
        return None;
    }

    // Join and cap at 500 chars
    let story: String = parts.join(". ");
    let capped: String = story.chars().take(500).collect();
    Some(capped)
}

/// Synthesize a story from heuristic enrichment data.
pub fn synthesize_story_from_heuristic(heuristic: &str, project: &str) -> String {
    let tools = extract_heuristic_field(heuristic, "Tools:");
    let msgs = extract_heuristic_field(heuristic, "Messages:");
    let has_errors = heuristic.contains("Had errors: yes");

    let mut story = format!("Session in {} project", project);
    if let Some(tools_str) = tools {
        story.push_str(&format!(" using {}", tools_str));
    }
    if let Some(msgs_str) = msgs {
        story.push_str(&format!(" ({} messages)", msgs_str));
    }
    if has_errors {
        story.push_str(" with error investigation");
    }
    story.push('.');
    story
}

/// Determine if a conversation needs LLM narrative (Haiku) vs template.
pub fn needs_haiku(v3_content: Option<&str>, heuristic: Option<&str>, msg_count: usize) -> bool {
    match (v3_content, heuristic) {
        (None, None) => msg_count >= 5,
        (Some(v3), _) => {
            let req = extract_section(v3, "## User Request");
            req.map(|r| r.len() < 30).unwrap_or(true) && msg_count >= 30
        }
        (None, Some(_)) => msg_count >= 50,
    }
}

// --- Helpers ---

fn extract_section(content: &str, header: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.iter().position(|l| l.starts_with(header))?;
    let mut result = String::new();
    for line in &lines[start + 1..] {
        if line.starts_with("## ") {
            break;
        }
        if !line.trim().is_empty() {
            if !result.is_empty() {
                result.push(' ');
            }
            result.push_str(line.trim());
        }
    }
    if result.is_empty() { None } else { Some(result) }
}

fn clean_request_text(raw: &str) -> String {
    raw.trim()
        .trim_matches('"')
        .replace("\\n", " ")
        .replace("\\\"", "\"")
        .chars()
        .take(200)
        .collect::<String>()
        .trim()
        .to_string()
}

fn extract_files_from_solution(solution: &str) -> Vec<String> {
    solution.lines()
        .filter(|l| l.starts_with("creation:") || l.starts_with("modification:"))
        .filter_map(|l| l.split(':').nth(1).map(|f| f.trim().to_string()))
        .collect()
}

fn extract_heuristic_field<'a>(heuristic: &'a str, field: &str) -> Option<&'a str> {
    heuristic.find(field).map(|pos| {
        let start = pos + field.len();
        let rest = &heuristic[start..];
        rest.split('\n').next().unwrap_or("").trim()
    })
}
```

**Step 4: Run test to verify it passes**

Run: `cd csr-engine && cargo test extraction::story --lib -- --nocapture 2>&1 | tail -5`
Expected: 4 tests PASS

**Step 5: Commit**

```bash
git add csr-engine/src/extraction/story.rs csr-engine/src/extraction/mod.rs
git commit -m "feat: V3 story synthesis module — zero-cost story generation from extraction data"
```

---

### Task 1.2: Backfill stories CLI subcommand

**Files:**
- Modify: `csr-engine/src/main.rs` (add `BackfillStories` subcommand)
- Modify: `csr-engine/src/summarizer.rs` (add `backfill_stories_cli` fn)
- Modify: `csr-engine/src/storage/queries.rs` (add `get_conversations_missing_stories` query)
- Modify: `csr-engine/src/storage/mod.rs` (expose new query)

**Step 1: Add storage query for conversations missing stories**

In `csr-engine/src/storage/queries.rs`, add:

```rust
/// Get conversations that have V3 or heuristic enrichment but no session_story.
/// Returns (conversation_id, enrichment_type, reflection_id) for each candidate.
pub fn get_conversations_missing_stories(
    conn: &Connection,
) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT e.conversation_id, e.enrichment_type, e.reflection_id
         FROM enrichment_state e
         WHERE e.enrichment_type IN ('extracted_v3', 'heuristic')
           AND e.status = 'completed'
           AND e.conversation_id NOT IN (
               SELECT conversation_id FROM enrichment_state
               WHERE enrichment_type = 'session_story' AND status = 'completed'
           )
         ORDER BY e.updated_at DESC"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}
```

Expose in `storage/mod.rs`:

```rust
pub fn get_conversations_missing_stories(&self) -> Result<Vec<(String, String, String)>> {
    let conn = self.conn.lock().unwrap();
    queries::get_conversations_missing_stories(&conn)
}
```

**Step 2: Add backfill function to summarizer.rs**

```rust
/// Backfill stories for all conversations that have V3/heuristic enrichment but no story.
/// Tier 1: V3 → synthesize locally (free). Tier 2: Heuristic → template (free).
/// Tier 3: Haiku (optional, with --haiku flag).
pub async fn backfill_stories_cli(
    engine: &Engine,
    use_haiku: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let candidates = engine.storage().get_conversations_missing_stories()?;
    eprintln!("CSR: {} conversations missing stories", candidates.len());

    let mut tier1 = 0usize;
    let mut tier2 = 0usize;
    let mut tier3_candidates = Vec::new();

    for (conv_id, enrichment_type, reflection_id) in &candidates {
        // Get the enrichment content
        let content = match engine.storage().get_reflection_by_id(reflection_id)? {
            Some((content, _, _)) => content,
            None => continue, // phantom reference
        };

        let project = extract_project_from_reflection(engine, conv_id);

        let story = if enrichment_type == "extracted_v3" {
            tier1 += 1;
            crate::extraction::story::synthesize_story_from_v3(&content, &project)
        } else {
            tier2 += 1;
            Some(crate::extraction::story::synthesize_story_from_heuristic(&content, &project))
        };

        if let Some(story_text) = story {
            if !dry_run {
                let story_id = format!("story_{}", conv_id);
                let tags = vec![
                    "session_story".to_string(),
                    format!("project_{}", project),
                    format!("conv_{}", conv_id),
                ];
                crate::mcp::tools::store_reflection(
                    engine.storage(), engine.embeddings(), engine.search(),
                    &story_text, &tags,
                ).await?;
                engine.storage().mark_enrichment_completed(conv_id, "session_story", &story_id)?;
            }
            eprintln!("  [{}] {} ({}chars)", if enrichment_type == "extracted_v3" { "V3" } else { "heuristic" }, &conv_id[..8], story_text.len());
        } else if use_haiku {
            tier3_candidates.push(conv_id.clone());
        }
    }

    eprintln!("CSR backfill: {} V3-synthesized, {} heuristic-templated, {} haiku-candidates",
              tier1, tier2, tier3_candidates.len());
    Ok(())
}

fn extract_project_from_reflection(engine: &Engine, conv_id: &str) -> String {
    // Try to find project from chunk data
    engine.storage().get_chunks_by_conversation(conv_id)
        .ok()
        .and_then(|chunks| chunks.first().map(|c| c.project_name.clone()))
        .unwrap_or_else(|| "unknown".to_string())
}
```

**Step 3: Add CLI subcommand to main.rs**

In the `Commands` enum:

```rust
/// Backfill session stories from V3/heuristic data (zero cost)
BackfillStories {
    /// Also generate Haiku stories for complex sessions
    #[arg(long)]
    haiku: bool,
    /// Preview without writing
    #[arg(long)]
    dry_run: bool,
},
```

In the match block:

```rust
if let Some(Commands::BackfillStories { haiku, dry_run }) = args.command {
    if let Some(parent) = args.db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let eng = engine::Engine::new(&args.db_path, &args.projects_dir)?;
    return csr_engine::summarizer::backfill_stories_cli(&eng, haiku, dry_run).await;
}
```

**Step 4: Build and test**

Run: `cd csr-engine && cargo build --release 2>&1 | tail -3`
Expected: Compiles clean

Run: `./target/release/csr-engine backfill-stories --dry-run 2>&1 | tail -10`
Expected: Shows candidates without writing

**Step 5: Run real backfill**

Run: `./target/release/csr-engine backfill-stories 2>&1`
Expected: Stories generated for 100+ conversations

**Step 6: Commit**

```bash
git add csr-engine/src/main.rs csr-engine/src/summarizer.rs csr-engine/src/storage/queries.rs csr-engine/src/storage/mod.rs
git commit -m "feat: backfill-stories CLI — V3 synthesis achieves 94%+ story coverage at zero cost"
```

---

### Task 1.3: Integrate V3 synthesis into session_end (skip Haiku when possible)

**Files:**
- Modify: `csr-engine/src/hooks/session_end.rs` (try local synthesis before spawning Haiku)

**Step 1: Add synthesis before Haiku spawn**

After the V3 extraction block (line ~41) and before the Haiku spawn (line ~43), insert:

```rust
// Try local V3 story synthesis BEFORE spawning Haiku (free, instant)
if let Some(ref tp) = input.transcript_path {
    let conv_id = std::path::Path::new(tp)
        .file_stem().unwrap_or_default()
        .to_string_lossy().to_string();
    let project = resolve_project_from_cwd(&cwd.to_string_lossy())
        .unwrap_or_else(|| "unknown".to_string());

    if !engine.storage().is_conversation_enriched(&conv_id, "session_story").unwrap_or(false) {
        // Try V3 synthesis first
        if let Ok(Some(ref_id)) = engine.storage().get_enrichment_reflection_id(&conv_id, "extracted_v3") {
            if let Ok(Some((v3_content, _, _))) = engine.storage().get_reflection_by_id(&ref_id) {
                if let Some(story) = crate::extraction::story::synthesize_story_from_v3(&v3_content, &project) {
                    let story_id = format!("story_{}", conv_id);
                    let tags = vec![
                        "session_story".to_string(),
                        format!("project_{}", project),
                        format!("conv_{}", conv_id),
                    ];
                    if let Ok(_) = crate::mcp::tools::store_reflection(
                        engine.storage(), engine.embeddings(), engine.search(),
                        &story, &tags,
                    ).await {
                        let _ = engine.storage().mark_enrichment_completed(&conv_id, "session_story", &story_id);
                        eprintln!("CSR: V3 story synthesized locally ({}chars), skipping Haiku", story.len());
                        // Skip Haiku spawn — story already generated
                        // Continue to Ralph-specific handling below
                    }
                }
            }
        }
    }
}

// Fallback: spawn detached Haiku story generation (only if no local story was generated)
if let Some(ref tp) = input.transcript_path {
    let conv_id = std::path::Path::new(tp)
        .file_stem().unwrap_or_default()
        .to_string_lossy().to_string();
    if !engine.storage().is_conversation_enriched(&conv_id, "session_story").unwrap_or(false) {
        let cwd_str = cwd.to_string_lossy().to_string();
        crate::summarizer::spawn_detached_story_generation(tp, &cwd_str);
    }
}
```

**Step 2: Build and test**

Run: `cd csr-engine && cargo build --release 2>&1 | tail -3`
Expected: Compiles clean

Run: `cargo test --test hooks_integration 2>&1 | grep "test result"`
Expected: All hooks tests pass

**Step 3: Commit**

```bash
git add csr-engine/src/hooks/session_end.rs
git commit -m "feat: session_end tries V3 story synthesis before Haiku — instant, free default"
```

---

## Phase 2: LAPI — Lifecycle-Aware Weight Profiles

### Task 2.1: HookPhase enum and WeightProfile struct

**Files:**
- Create: `csr-engine/src/injection/weights.rs`
- Modify: `csr-engine/src/injection/mod.rs` (add `pub mod weights;`)

**Step 1: Write the failing test**

```rust
// In csr-engine/src/injection/weights.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_weight_profiles_sum_to_one() {
        for phase in [HookPhase::SessionStart, HookPhase::PromptSubmit, HookPhase::Stop, HookPhase::PreCompact] {
            let w = WeightProfile::for_phase(phase);
            let sum = w.semantic + w.recency + w.file_overlap + w.error_match + w.phase_boost;
            assert!((sum - 1.0).abs() < 0.001, "Phase {:?} weights sum to {}, not 1.0", phase, sum);
        }
    }

    #[test]
    fn test_session_start_prefers_phase_boost() {
        let w = WeightProfile::for_phase(HookPhase::SessionStart);
        assert!(w.phase_boost > w.semantic, "SessionStart should boost phase-appropriate results most");
    }

    #[test]
    fn test_prompt_submit_prefers_semantic() {
        let w = WeightProfile::for_phase(HookPhase::PromptSubmit);
        assert!(w.semantic >= w.phase_boost, "PromptSubmit should weight semantic match highest");
    }

    #[test]
    fn test_stop_prefers_error_match() {
        let w = WeightProfile::for_phase(HookPhase::Stop);
        assert!(w.error_match + w.phase_boost > w.semantic, "Stop should weight stuck-detection signals highly");
    }

    #[test]
    fn test_phase_boost_computation() {
        // Anti-pattern should score high at SessionStart
        let score = compute_phase_boost("anti_pattern", &[], HookPhase::SessionStart);
        assert!(score > 0.8);
        // Chunk should score high at PromptSubmit
        let score = compute_phase_boost("chunk", &[], HookPhase::PromptSubmit);
        assert!(score > 0.7);
        // Anti-pattern should score high at Stop
        let score = compute_phase_boost("anti_pattern", &[], HookPhase::Stop);
        assert!(score > 0.9);
    }
}
```

**Step 2: Implement**

```rust
//! Lifecycle-aware weight profiles for LAPI (Lifecycle-Aware Predictive Injection).
//!
//! Different hook phases need different retrieval priorities:
//! - SessionStart: strategies + anti-patterns (big picture)
//! - PromptSubmit: code context + error solutions (specific help)
//! - Stop: stuck patterns + iteration learnings (escape hatch)
//! - PreCompact: session state (preserve work)

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HookPhase {
    SessionStart,
    PromptSubmit,
    Stop,
    PreCompact,
}

#[derive(Debug, Clone, Copy)]
pub struct WeightProfile {
    pub semantic: f32,
    pub recency: f32,
    pub file_overlap: f32,
    pub error_match: f32,
    pub phase_boost: f32,
}

impl WeightProfile {
    pub fn for_phase(phase: HookPhase) -> Self {
        match phase {
            HookPhase::SessionStart => Self {
                semantic: 0.25, recency: 0.10, file_overlap: 0.15,
                error_match: 0.10, phase_boost: 0.40,
            },
            HookPhase::PromptSubmit => Self {
                semantic: 0.40, recency: 0.15, file_overlap: 0.20,
                error_match: 0.10, phase_boost: 0.15,
            },
            HookPhase::Stop => Self {
                semantic: 0.20, recency: 0.10, file_overlap: 0.10,
                error_match: 0.25, phase_boost: 0.35,
            },
            HookPhase::PreCompact => Self {
                semantic: 0.30, recency: 0.30, file_overlap: 0.15,
                error_match: 0.05, phase_boost: 0.20,
            },
        }
    }
}

/// Phase-specific boost: how well does this result's TYPE match what this phase needs?
pub fn compute_phase_boost(source: &str, tags: &[String], phase: HookPhase) -> f32 {
    match phase {
        HookPhase::SessionStart => {
            if tags.iter().any(|t| t.starts_with("outcome_")) { return 1.0; }
            if source == "anti_pattern" { return 0.9; }
            if source == "reflection" { return 0.7; }
            0.2
        }
        HookPhase::PromptSubmit => {
            if source == "chunk" { return 0.8; }
            if tags.iter().any(|t| t == "error_recovery") { return 1.0; }
            if source == "reflection" { return 0.5; }
            0.3
        }
        HookPhase::Stop => {
            if source == "anti_pattern" { return 1.0; }
            if tags.iter().any(|t| t.starts_with("iteration_")) { return 0.9; }
            0.2
        }
        HookPhase::PreCompact => {
            if tags.iter().any(|t| t == "ralph_session") { return 1.0; }
            if source == "reflection" { return 0.7; }
            0.3
        }
    }
}
```

**Step 3: Run tests**

Run: `cd csr-engine && cargo test injection::weights --lib -- --nocapture 2>&1 | tail -5`
Expected: 5 tests PASS

**Step 4: Commit**

```bash
git add csr-engine/src/injection/weights.rs csr-engine/src/injection/mod.rs
git commit -m "feat: LAPI weight profiles — lifecycle-aware retrieval scoring per hook phase"
```

---

### Task 2.2: Wire LAPI into predictor.rs

**Files:**
- Modify: `csr-engine/src/injection/predictor.rs` (add phase parameter to `rank_results`)
- Modify: `csr-engine/src/hooks/prompt_submit.rs` (pass `HookPhase::PromptSubmit`)
- Modify: `csr-engine/src/hooks/stop.rs` (pass `HookPhase::Stop` if it uses predictor)

**Step 1: Update `rank_results` signature**

Add `phase: Option<HookPhase>` parameter. When `None`, use current fixed weights (backwards compatible). When `Some`, use `WeightProfile::for_phase()`.

```rust
pub fn rank_results(
    results: Vec<RawResult>,
    current_files: &[String],
    current_errors: &[String],
    phase: Option<super::weights::HookPhase>,
) -> Vec<ScoredResult> {
    let weights = phase
        .map(super::weights::WeightProfile::for_phase)
        .unwrap_or(super::weights::WeightProfile::for_phase(
            super::weights::HookPhase::PromptSubmit, // default = current behavior
        ));

    let mut scored: Vec<ScoredResult> = results
        .into_iter()
        .map(|r| score_result(r, current_files, current_errors, &weights))
        .collect();
    scored.sort_by(|a, b| b.final_score.partial_cmp(&a.final_score).unwrap_or(std::cmp::Ordering::Equal));
    scored
}
```

Update `score_result` to use `WeightProfile` instead of hardcoded weights.

**Step 2: Update all callers**

- `prompt_submit.rs`: `rank_results(raw, &files, &errors, Some(HookPhase::PromptSubmit))`
- Any other callers: `rank_results(raw, &files, &errors, None)` for backward compat

**Step 3: Run ALL tests**

Run: `cd csr-engine && cargo test 2>&1 | grep "test result"`
Expected: All 340+ tests pass (existing tests use `None` phase = same behavior)

**Step 4: Commit**

```bash
git add csr-engine/src/injection/predictor.rs csr-engine/src/hooks/prompt_submit.rs
git commit -m "feat: LAPI wired into predictor — phase-aware scoring for prompt_submit hook"
```

---

## Phase 3: Unified Decay (fix recency double-count)

### Task 3.1: DecayConfig and unified decay function

**Files:**
- Modify: `csr-engine/src/search/decay.rs` (add `DecayConfig`, unified function)

**Step 1: Write failing tests**

```rust
#[test]
fn test_decay_config_for_injection() {
    let config = DecayConfig::for_injection();
    assert_eq!(config.base_half_life_days, 30.0);
    assert_eq!(config.decay_weight, 0.5);
}

#[test]
fn test_decay_config_for_search() {
    let config = DecayConfig::for_search();
    assert_eq!(config.base_half_life_days, 90.0);
    assert_eq!(config.decay_weight, 0.3);
}

#[test]
fn test_unified_decay_matches_original() {
    let now = Utc::now();
    let past = now - Duration::days(90);
    let config = DecayConfig::for_search();
    let unified = apply_decay_unified(1.0, &past, &now, &config);
    let original = apply_decay(1.0, &past, &now, None, None);
    assert!((unified - original).abs() < 0.001, "unified={} original={}", unified, original);
}
```

**Step 2: Implement DecayConfig**

```rust
#[derive(Debug, Clone)]
pub struct DecayConfig {
    pub decay_weight: f64,
    pub base_half_life_days: f64,
}

impl Default for DecayConfig {
    fn default() -> Self { Self { decay_weight: 0.3, base_half_life_days: 90.0 } }
}

impl DecayConfig {
    pub fn for_injection() -> Self { Self { decay_weight: 0.5, base_half_life_days: 30.0 } }
    pub fn for_search() -> Self { Self { decay_weight: 0.3, base_half_life_days: 90.0 } }
}

pub fn apply_decay_unified(
    score: f32, timestamp: &DateTime<Utc>, now: &DateTime<Utc>, config: &DecayConfig,
) -> f32 {
    let age_days = (*now - *timestamp).num_seconds() as f64 / 86400.0;
    if age_days <= 0.0 { return score; }
    let time_factor = 2.0_f64.powf(-age_days / config.base_half_life_days);
    let adjusted = (score as f64) * ((1.0 - config.decay_weight) + config.decay_weight * time_factor);
    adjusted as f32
}
```

**Step 3: Update predictor.rs to use unified decay instead of its own recency**

Replace `compute_recency_boost` to call `apply_decay_unified` with `DecayConfig::for_injection()`.

**Step 4: Run ALL tests, commit**

```bash
git commit -m "fix: unified decay config — eliminates recency double-counting between decay.rs and predictor.rs"
```

---

## Phase 4: TAD — Temporal Attention Decay (The Moat)

### Task 4.1: Schema migration — retrieval_events table

**Files:**
- Modify: `csr-engine/src/storage/queries.rs` (add CREATE TABLE, insert/update/query functions)
- Modify: `csr-engine/src/storage/mod.rs` (expose new functions, add migration)

**Step 1: Add table creation to schema init**

```sql
CREATE TABLE IF NOT EXISTS retrieval_events (
    id TEXT PRIMARY KEY,
    memory_id TEXT NOT NULL,
    memory_type TEXT NOT NULL,
    retrieved_at TEXT NOT NULL,
    hook_phase TEXT NOT NULL,
    session_outcome TEXT DEFAULT 'neutral',
    session_id TEXT
);
CREATE INDEX IF NOT EXISTS idx_retrieval_memory ON retrieval_events(memory_id);
CREATE INDEX IF NOT EXISTS idx_retrieval_session ON retrieval_events(session_id);
```

**Step 2: Add query functions**

```rust
pub fn log_retrieval_event(conn: &Connection, memory_id: &str, memory_type: &str, hook_phase: &str, session_id: &str) -> Result<()>
pub fn update_session_outcome(conn: &Connection, session_id: &str, outcome: &str) -> Result<()>
pub fn get_retrieval_events_for_memory(conn: &Connection, memory_id: &str) -> Result<Vec<RetrievalEvent>>
```

**Step 3: Commit**

```bash
git commit -m "feat: retrieval_events schema — tracks which memories were surfaced and session outcomes"
```

---

### Task 4.2: TAD decay function

**Files:**
- Modify: `csr-engine/src/search/decay.rs` (add TAD computation)

**Step 1: Write failing tests**

```rust
#[test]
fn test_tad_reinforced_memory_decays_slower() {
    let now = Utc::now();
    let past = now - Duration::days(90);
    let config = DecayConfig::for_search();

    // Memory with successful retrieval events
    let events = vec![RetrievalEvent {
        retrieved_at: now - Duration::days(10),
        session_outcome: SessionOutcome::Success,
    }];

    let standard = apply_decay_unified(1.0, &past, &now, &config);
    let reinforced = apply_tad(1.0, &past, &now, &events, &config);
    assert!(reinforced > standard, "reinforced={} should be > standard={}", reinforced, standard);
}

#[test]
fn test_tad_failed_memory_decays_faster() {
    let now = Utc::now();
    let past = now - Duration::days(90);
    let config = DecayConfig::for_search();

    let events = vec![RetrievalEvent {
        retrieved_at: now - Duration::days(5),
        session_outcome: SessionOutcome::Failed,
    }];

    let standard = apply_decay_unified(1.0, &past, &now, &config);
    let suppressed = apply_tad(1.0, &past, &now, &events, &config);
    assert!(suppressed < standard, "suppressed={} should be < standard={}", suppressed, standard);
}

#[test]
fn test_tad_no_events_equals_standard() {
    let now = Utc::now();
    let past = now - Duration::days(90);
    let config = DecayConfig::for_search();
    let standard = apply_decay_unified(1.0, &past, &now, &config);
    let tad = apply_tad(1.0, &past, &now, &[], &config);
    assert!((tad - standard).abs() < 0.001);
}
```

**Step 2: Implement**

```rust
#[derive(Debug, Clone)]
pub struct RetrievalEvent {
    pub retrieved_at: DateTime<Utc>,
    pub session_outcome: SessionOutcome,
}

#[derive(Debug, Clone, Copy)]
pub enum SessionOutcome { Success, Failed, Neutral }

pub fn apply_tad(
    score: f32, timestamp: &DateTime<Utc>, now: &DateTime<Utc>,
    retrieval_events: &[RetrievalEvent], config: &DecayConfig,
) -> f32 {
    let reinforcement = compute_reinforcement(retrieval_events, now);
    let effective_half_life = config.base_half_life_days * 2.0_f64.powf(reinforcement);
    let age_days = (*now - *timestamp).num_seconds() as f64 / 86400.0;
    if age_days <= 0.0 { return score; }
    let time_factor = 2.0_f64.powf(-age_days / effective_half_life);
    let adjusted = (score as f64) * ((1.0 - config.decay_weight) + config.decay_weight * time_factor);
    adjusted as f32
}

fn compute_reinforcement(events: &[RetrievalEvent], now: &DateTime<Utc>) -> f64 {
    if events.is_empty() { return 0.0; }
    let mut score = 0.0;
    for event in events {
        let days_ago = (*now - event.retrieved_at).num_seconds() as f64 / 86400.0;
        let recency_weight = 2.0_f64.powf(-days_ago / 30.0);
        let outcome_weight = match event.session_outcome {
            SessionOutcome::Success => 1.0,
            SessionOutcome::Neutral => 0.0,
            SessionOutcome::Failed => -1.0,
        };
        score += outcome_weight * recency_weight;
    }
    score.clamp(-2.0, 2.0)
}
```

**Step 3: Run ALL tests, commit**

```bash
git commit -m "feat: Temporal Attention Decay — memories that help succeed persist longer"
```

---

### Task 4.3: Wire TAD into injection pipeline

**Files:**
- Modify: `csr-engine/src/hooks/prompt_submit.rs` (log retrieval events when injecting)
- Modify: `csr-engine/src/hooks/session_end.rs` (update session outcome for all retrieval events)

**Step 1: Log retrieval events during injection**

When `prompt_submit` injects context (after `print!("{}", formatted)`), log each result:

```rust
// Log retrieval events for TAD tracking
if let Some(ref session_id) = input.session_id {
    for result in scored.iter().take(5) {
        let _ = engine.storage().log_retrieval_event(
            &result.memory_id, // need to add this to ScoredResult
            &result.source,
            "prompt_submit",
            session_id,
        );
    }
}
```

**Step 2: Update session outcome in session_end**

```rust
// Update all retrieval events for this session with the outcome
if let Some(ref session_id) = input.session_id {
    let outcome = if ralph.map(|r| r.determine_outcome("end")) == Some(Outcome::Completed) {
        "success"
    } else {
        "neutral"
    };
    let _ = engine.storage().update_session_outcome(session_id, outcome);
}
```

**Step 3: Run ALL tests, commit**

```bash
git commit -m "feat: TAD wired into injection pipeline — tracks retrieval outcomes for adaptive decay"
```

---

## Phase 5: Final integration testing

### Task 5.1: End-to-end integration test

**Files:**
- Modify: `csr-engine/tests/integration.rs` (add LAPI + TAD integration test)

**Step 1: Write integration test**

```rust
#[tokio::test]
async fn test_lapi_phase_aware_scoring() {
    let engine = setup_test_engine().await;
    // Insert a chunk and a reflection
    // Search with PromptSubmit phase → chunk should rank higher
    // Search with SessionStart phase → reflection should rank higher
}

#[tokio::test]
async fn test_tad_reinforcement_improves_ranking() {
    let engine = setup_test_engine().await;
    // Insert two equally-scored memories
    // Log retrieval events: one succeeded, one failed
    // Search again → succeeded memory should rank higher
}
```

**Step 2: Run full test suite**

Run: `cd csr-engine && cargo test 2>&1 | grep "test result"`
Expected: All tests pass (340+ existing + new)

**Step 3: Build release, install, verify with real data**

```bash
cargo build --release
cp target/release/csr-engine ~/.local/bin/csr-engine
# Run backfill
csr-engine backfill-stories
# Check coverage
python3 -c "import sqlite3; db=sqlite3.connect('$HOME/.claude-self-reflect/csr-engine.db'); print(db.execute('SELECT COUNT(*) FROM enrichment_state WHERE enrichment_type=\"session_story\"').fetchone())"
```

**Step 4: Final commit**

```bash
git commit -m "test: LAPI + TAD end-to-end integration tests"
```

---

## Summary

| Phase | Tasks | New Tests | Key Deliverable |
|-------|-------|-----------|----------------|
| 1 | 1.1-1.3 | ~8 | 94%+ story coverage at $0 |
| 2 | 2.1-2.2 | ~7 | Lifecycle-aware retrieval (LAPI) |
| 3 | 3.1 | ~3 | Fix recency double-count bug |
| 4 | 4.1-4.3 | ~6 | Temporal Attention Decay (TAD) |
| 5 | 5.1 | ~2 | Integration verification |
| **Total** | **10 tasks** | **~26 tests** | **Three-part algorithmic moat** |
