//! SessionStart hook — searches CSR for relevant past sessions and injects context.
//!
//! When a Ralph session is active, performs 4 searches:
//! 1. Similar tasks (min_score=0.5, limit=2)
//! 2. Similar errors (min_score=0.6, limit=1 each)
//! 3. Anti-patterns from incomplete/abandoned sessions (min_score=0.5, limit=2)
//! 4. Winning strategies from completed sessions (min_score=0.6, limit=1)
//!
//! Anti-patterns are placed FIRST in the output (critical for fast loops).

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::ralph_state::RalphState;
use super::HookInput;
use crate::engine::Engine;
use crate::injection::anti_pattern;
use crate::search::cross_project::resolve_project_from_cwd;
use crate::storage::queries::SessionInfo;
use crate::temporal;

/// Handle the session-start hook.
/// Wrapped in catch-all: ALWAYS returns Ok(()) to never block Claude Code (C-1 fix).
pub async fn handle(
    input: &HookInput,
    ralph: Option<&RalphState>,
    engine: &Engine,
    cwd: &Path,
) -> Result<()> {
    if let Err(e) = handle_inner(input, ralph, engine, cwd).await {
        eprintln!("CSR: session-start hook error (non-fatal): {}", e);
    }
    Ok(()) // Always succeed
}

async fn handle_inner(
    input: &HookInput,
    ralph: Option<&RalphState>,
    engine: &Engine,
    cwd: &Path,
) -> Result<()> {
    let session_id = input
        .session_id
        .as_deref()
        .unwrap_or("unknown");

    // If no Ralph session, output status + compact timeline
    let ralph = match ralph {
        Some(r) => r,
        None => {
            // Single lock acquisition for both counts (M-3 fix)
            let (chunk_count, reflection_count) = {
                let search = engine.search().read().await;
                (search.chunk_count(), search.reflection_count())
            };

            // Resolve project from cwd for scoped lookup
            let project = resolve_project_from_cwd(&cwd.to_string_lossy());

            // Over-fetch 8, filter displayable, take 5 (Bug 3 fix)
            let sessions = engine
                .storage()
                .get_recent_sessions(8, project.as_deref())
                .unwrap_or_default();

            let displayable: Vec<&SessionInfo> = sessions
                .iter()
                .filter(|s| is_displayable(s))
                .take(5)
                .collect();

            let now = Utc::now();

            // Build full output string, single println (L-3 fix)
            let mut output = format!("CSR engine ready. Session: {session_id}. {chunk_count} chunks, {reflection_count} reflections indexed.");

            if !displayable.is_empty() {
                output.push_str("\n\nRecent sessions:");
                for session in &displayable {
                    output.push_str(&format!("\n  {}", format_session_line(session, &now)));
                }

                // Cross-project pulse: search for related concepts in other projects
                if let Some(cross_project_note) = cross_project_pulse(
                    engine,
                    &displayable,
                    project.as_deref().unwrap_or(""),
                )
                .await
                {
                    output.push_str(&format!("\n\n{cross_project_note}"));
                }

                // Enrichment-aware suggestion from most recent displayable session
                let suggestion = infer_next_action_from_session(displayable[0]);
                output.push_str(&format!("\n\nSuggested next: {suggestion}"));
                output.push_str("\nDrill down: use reflect_on_past, search_by_file, or search_by_concept (AST-powered)");
                output.push_str("\nNote: current session not yet imported — timeline shows prior sessions");
                output.push_str("\n\n[INSTRUCTION: When the user sends their first message, proactively greet them with the suggested next action above and any cross-project insights. Keep it natural — 1-2 sentences. If the user's message already has a clear task, skip the greeting and focus on their request.]");
            }

            println!("{output}");
            return Ok(());
        }
    };

    let storage = engine.storage();
    let embeddings = engine.embeddings();
    let search = engine.search();

    let mut context_parts: Vec<String> = Vec::new();
    let mut anti_pattern_count = 0usize;
    let mut winning_count = 0usize;
    let mut error_count = 0usize;
    let mut similar_count = 0usize;

    // 1. Search for anti-patterns (incomplete/abandoned sessions) — output FIRST
    // Uses shared anti_pattern module (also used by prompt_submit hook)
    let anti_items = anti_pattern::find_anti_patterns(
        storage, embeddings, search, &ralph.task, 0.5, 2,
    )
    .await;

    if !anti_items.is_empty() {
        anti_pattern_count = anti_items.len();
        let mut section = String::from("## DON'T RETRY THESE (Anti-Patterns from Past Sessions)\n\n");
        for item in &anti_items {
            section.push_str(&format!("**[Score: {:.2}]**\n{}\n\n---\n\n", item.score, item.content));
        }
        context_parts.push(section);
    }

    // 2. Search for similar errors
    for (sig, _count) in &ralph.error_signatures {
        let error_query = format!("error blocked solved: {}", sig);
        let results = search_reflections_unfiltered(
            storage, embeddings, search, &error_query, 0.6, 1,
        )
        .await;

        if !results.is_empty() {
            error_count += results.len();
            let mut section = String::from("## Past Error Solutions\n\n");
            for (content, score) in &results {
                section.push_str(&format!(
                    "**Error pattern:** `{}`\n**[Score: {:.2}]**\n{}\n\n---\n\n",
                    sig, score, content
                ));
            }
            context_parts.push(section);
        }
    }

    // 3. Search for winning strategies (completed sessions)
    let win_query = format!("successful solution: {}", ralph.task);
    let win_results = anti_pattern::search_reflections_by_tag(
        storage,
        embeddings,
        search,
        &win_query,
        0.6,
        1,
        &["outcome_completed"],
    )
    .await;

    if !win_results.is_empty() {
        winning_count = win_results.len();
        let mut section = String::from("## Winning Strategies from Past Sessions\n\n");
        for (content, score) in &win_results {
            section.push_str(&format!("**[Score: {:.2}]**\n{}\n\n---\n\n", score, content));
        }
        context_parts.push(section);
    }

    // 4. Search for similar tasks
    let task_query = format!("ralph session: {}", ralph.task);
    let task_results = search_reflections_unfiltered(
        storage, embeddings, search, &task_query, 0.5, 2,
    )
    .await;

    if !task_results.is_empty() {
        similar_count = task_results.len();
        let mut section = String::from("## Similar Past Sessions\n\n");
        for (content, score) in &task_results {
            section.push_str(&format!("**[Score: {:.2}]**\n{}\n\n---\n\n", score, content));
        }
        context_parts.push(section);
    }

    // Write context file if there are any results
    let total_results = anti_pattern_count + winning_count + error_count + similar_count;
    if !context_parts.is_empty() {
        let mut file_content = String::from("# CSR Past Session Context\n\n");
        file_content.push_str(&format!(
            "> Auto-generated by CSR engine for Ralph session `{}`\n\n",
            ralph.session_id,
        ));
        for part in &context_parts {
            file_content.push_str(part);
        }

        let context_path = cwd.join(".ralph_past_sessions.md");
        std::fs::write(&context_path, &file_content)?;
    }

    // Output summary to stdout
    println!(
        "CSR: Found {} relevant results for Ralph session '{}':",
        total_results, ralph.session_id,
    );
    println!("  - Anti-patterns: {}", anti_pattern_count);
    println!("  - Winning strategies: {}", winning_count);
    println!("  - Error matches: {}", error_count);
    println!("  - Similar tasks: {}", similar_count);

    if total_results > 0 {
        println!(
            "  Context written to: {}",
            cwd.join(".ralph_past_sessions.md").display()
        );
    }

    Ok(())
}

/// Search reflections without tag filtering.
async fn search_reflections_unfiltered(
    storage: &std::sync::Arc<crate::storage::Storage>,
    embeddings: &std::sync::Arc<crate::embeddings::EmbeddingEngine>,
    search: &std::sync::Arc<tokio::sync::RwLock<crate::search::SearchEngine>>,
    query: &str,
    min_score: f32,
    limit: usize,
) -> Vec<(String, f32)> {
    let query_vec = match embed_query(embeddings, query).await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let results = {
        let idx = search.read().await;
        idx.search_reflections(&query_vec, limit, min_score)
    };

    let mut enriched = Vec::new();
    for result in &results {
        if let Ok(Some((content, _tags, _ts))) = storage.get_reflection_by_id(&result.id) {
            enriched.push((content, result.score));
        }
    }

    enriched
}

/// Embed a query string via spawn_blocking.
async fn embed_query(
    embeddings: &std::sync::Arc<crate::embeddings::EmbeddingEngine>,
    query: &str,
) -> Result<Vec<f32>> {
    let q = query.to_string();
    let emb = embeddings.clone();
    Ok(tokio::task::spawn_blocking(move || emb.embed_single(&q)).await??)
}

/// Search for cross-project concept matches from the most recent session's enrichment.
/// Returns a one-line note if a match is found in another project.
async fn cross_project_pulse(
    engine: &Engine,
    sessions: &[&SessionInfo],
    current_project: &str,
) -> Option<String> {
    // Use the most recent session's enrichment or summary as the query
    let query_text = sessions.first().and_then(|s| {
        s.enrichment
            .as_deref()
            .or(s.summary.as_deref())
    })?;

    if query_text.trim().is_empty() {
        return None;
    }

    let query_vec = embed_query(engine.embeddings(), query_text).await.ok()?;

    // Search all reflections (unfiltered by project)
    let results = {
        let idx = engine.search().read().await;
        idx.search_reflections(&query_vec, 5, 0.5)
    };

    // Find first result from a DIFFERENT project
    for result in &results {
        if let Ok(Some((content, tags, _ts))) = engine.storage().get_reflection_by_id(&result.id) {
            // Check if this reflection belongs to a different project
            // Heuristic reflections contain "Project: <name>" in first line
            let other_project = content
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("[Heuristic] Project: "))
                .unwrap_or("");

            // Also check tags for project info
            let tag_project = tags.iter().find(|t| t.starts_with("project:")).map(|t| &t[8..]);
            let proj = if !other_project.is_empty() {
                other_project
            } else if let Some(tp) = tag_project {
                tp
            } else {
                continue;
            };

            if !proj.is_empty() && proj != current_project {
                return Some(format!(
                    "Cross-project: similar concepts found in {} ({:.2}) — use reflect_on_past(project:\"all\") to explore",
                    proj, result.score
                ));
            }
        }
    }

    None
}

/// Format a relative time label with hour-level granularity for same-day sessions.
/// Uses `temporal::parse_timestamp` for parsing, pure fn with injected `now`.
fn relative_time_label(timestamp: &str, now: &DateTime<Utc>) -> String {
    let ts = match temporal::parse_timestamp(timestamp) {
        Some(t) => t,
        None => return "???".to_string(),
    };
    let diff = *now - ts;
    let total_minutes = diff.num_minutes();
    let days = diff.num_days();

    if days == 0 {
        if total_minutes < 1 {
            "just now".to_string()
        } else if total_minutes < 60 {
            format!("{}m ago", total_minutes)
        } else {
            format!("{}h ago", diff.num_hours())
        }
    } else if days == 1 {
        "yesterday".to_string()
    } else if days < 7 {
        format!("{}d ago", days)
    } else if days < 30 {
        format!("{}w ago", days / 7)
    } else {
        format!("{}mo ago", days / 30)
    }
}

/// Truncate content to max_chars and sanitize to a single line for timeline display.
fn compact_preview(content: &str, max_chars: usize) -> String {
    // Sanitize first: collapse newlines to spaces, strip control chars
    let clean = sanitize_preview(content);
    if clean.len() <= max_chars {
        return clean;
    }
    let boundary = clean.floor_char_boundary(max_chars);
    format!("{}...", &clean[..boundary])
}

/// Sanitize a preview string for safe stdout injection.
/// Collapses newlines to spaces, strips control characters (M-2 fix).
fn sanitize_preview(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .filter(|c| !c.is_control() || *c == ' ')
        .collect()
}

/// Strip common preamble prefixes that add noise to timeline summaries.
fn strip_preamble(s: &str) -> &str {
    const PREFIXES: &[&str] = &[
        "Implement the following plan:",
        "Execute the following plan:",
        "Follow the following plan:",
        "Here is the plan:",
        "Please implement:",
        "Please execute:",
    ];

    let trimmed = s.trim();
    for prefix in PREFIXES {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let rest = rest.trim();
            // Also strip leading markdown headers
            return rest.trim_start_matches('#').trim();
        }
    }
    // Strip leading markdown headers even without a prefix
    if trimmed.starts_with('#') {
        return trimmed.trim_start_matches('#').trim();
    }
    trimmed
}

/// Check if a session has enough content to be worth displaying.
fn is_displayable(session: &SessionInfo) -> bool {
    if session.total_messages < 2 {
        return false;
    }
    // Must have either a non-empty summary or enrichment
    let has_summary = session
        .summary
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let has_enrichment = session.enrichment.is_some();
    has_summary || has_enrichment
}

/// Parse heuristic enrichment text to extract structured fields.
/// Format: "[Heuristic] Project: X\nMessages: N (M user)\nTools: ...\nFiles: ...\nHad errors: yes"
struct EnrichmentFields {
    tools: Vec<String>,
    files: Vec<String>,
    has_errors: bool,
    has_edit_tool: bool,
}

fn parse_enrichment(enrichment: &str) -> EnrichmentFields {
    let mut tools = Vec::new();
    let mut files = Vec::new();
    let mut has_errors = false;

    for line in enrichment.lines() {
        if let Some(rest) = line.strip_prefix("Tools: ") {
            tools = rest.split(", ").map(|s| s.trim().to_string()).collect();
        } else if let Some(rest) = line.strip_prefix("Files: ") {
            files = rest.split(", ").map(|s| s.trim().to_string()).collect();
        } else if line.contains("Had errors: yes") {
            has_errors = true;
        }
    }

    let has_edit_tool = tools.iter().any(|t| t == "Edit" || t == "MultiEdit");
    EnrichmentFields {
        tools,
        files,
        has_errors,
        has_edit_tool,
    }
}

/// Build a compact display line from enrichment data (tools + files).
/// Returns None if no enrichment, falls back to summary.
fn enrichment_display(enrichment: &str) -> Option<String> {
    let fields = parse_enrichment(enrichment);
    let mut parts = Vec::new();

    if !fields.tools.is_empty() {
        let tool_list: Vec<&str> = fields.tools.iter().take(6).map(|s| s.as_str()).collect();
        parts.push(format!("Tools: {}", tool_list.join(", ")));
    }
    if !fields.files.is_empty() {
        let file_list: Vec<&str> = fields.files.iter().take(4).map(|s| s.as_str()).collect();
        parts.push(format!("Files: {}", file_list.join(", ")));
    }
    if fields.has_errors {
        parts.push("Had errors".to_string());
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" | "))
    }
}

/// Format a session timeline line using enrichment data when available.
fn format_session_line(session: &SessionInfo, now: &DateTime<Utc>) -> String {
    let label = relative_time_label(&session.timestamp, now);

    // Prefer enrichment display for rich context, fall back to summary
    let display = session
        .enrichment
        .as_deref()
        .and_then(enrichment_display)
        .or_else(|| {
            session.summary.as_deref().map(|s| {
                let cleaned = strip_preamble(s);
                compact_preview(cleaned, 65)
            })
        })
        .unwrap_or_else(|| "---".to_string());

    format!(
        "{:<9} | {:>3} msgs | {}",
        label, session.total_messages, display
    )
}

/// Infer a suggested next action from enrichment data.
/// Uses structured enrichment fields for better accuracy than keyword matching.
fn infer_next_action_from_session(session: &SessionInfo) -> String {
    // Try enrichment-based inference first
    if let Some(enrichment) = session.enrichment.as_deref() {
        let fields = parse_enrichment(enrichment);

        if fields.has_errors && fields.has_edit_tool {
            return "Fix errors from last session — edits were in progress".to_string();
        }
        if fields.has_errors {
            return "Investigate and fix errors from last session".to_string();
        }
        if fields.has_edit_tool && !fields.files.is_empty() {
            let file_list: Vec<&str> = fields.files.iter().take(3).map(|s| s.as_str()).collect();
            return format!("Continue work on {}", file_list.join(", "));
        }
        if session.total_messages > 200 {
            return "Large session — review progress and continue".to_string();
        }
    }

    // Fall back to keyword-based inference on summary
    let text = session.summary.as_deref().unwrap_or("");
    infer_next_action(text)
}

/// Infer a suggested next action from the last session's content.
/// Uses simple keyword heuristics — no embedding needed, ~0ms.
fn infer_next_action(content: &str) -> String {
    let lower = content.to_lowercase();

    // Check for explicit phase references (generic, not hardcoded — L-1 fix)
    if let Some(pos) = lower.find("phase ") {
        let after = &lower[pos + 6..];
        if let Some(num_end) = after.find(|c: char| !c.is_ascii_digit()) {
            let phase_num = &after[..num_end];
            if !phase_num.is_empty() {
                return format!("Continue with Phase {phase_num} implementation");
            }
        } else if !after.is_empty() && after.chars().all(|c| c.is_ascii_digit()) {
            // "phase N" at end of string
            return format!("Continue with Phase {after} implementation");
        }
    }
    // Also handle "next phase" (use lowered string for extraction — M-1 fix)
    if lower.contains("next phase") {
        return "Pick up the next phase discussed in last session".to_string();
    }

    // Check for incomplete work signals
    if lower.contains("todo") || lower.contains("fixme") || lower.contains("wip") {
        return "Continue incomplete work from last session".to_string();
    }
    if lower.contains("failing") || lower.contains("broken") || lower.contains("error") {
        return "Investigate issues from last session".to_string();
    }

    // Check for planning/review patterns
    if lower.contains("plan") && lower.contains("review") {
        return "Review and execute the plan from last session".to_string();
    }
    if lower.contains("plan") {
        return "Execute the plan discussed in last session".to_string();
    }

    // Check for test patterns
    if lower.contains("test") && (lower.contains("add") || lower.contains("write")) {
        return "Continue adding tests from last session".to_string();
    }

    // Check for refactor/cleanup
    if lower.contains("refactor") || lower.contains("cleanup") {
        return "Continue refactoring from last session".to_string();
    }

    // Default: generic continuation
    "Continue where you left off — ask what's next".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- infer_next_action (keyword fallback) tests ---

    #[test]
    fn test_infer_phase_generic() {
        let result = infer_next_action("Phase 4 implementation is next");
        assert!(result.contains("Phase 4"));
        let result = infer_next_action("Starting Phase 12 soon");
        assert!(result.contains("Phase 12"));
    }

    #[test]
    fn test_infer_error() {
        let result = infer_next_action("The build is failing with a linker error");
        assert!(result.contains("Investigate"));
    }

    #[test]
    fn test_infer_plan() {
        let result = infer_next_action("Here is the plan for the new feature");
        assert!(result.contains("plan"));
    }

    #[test]
    fn test_infer_todo() {
        let result = infer_next_action("TODO: finish the import logic");
        assert!(result.contains("incomplete"));
    }

    #[test]
    fn test_infer_default() {
        let result = infer_next_action("Just a normal conversation about Rust");
        assert!(result.contains("Continue where you left off"));
    }

    #[test]
    fn test_infer_refactor() {
        let result = infer_next_action("We started a refactor of the storage layer");
        assert!(result.contains("refactoring"));
    }

    #[test]
    fn test_infer_empty() {
        let result = infer_next_action("");
        assert!(result.contains("Continue where you left off"));
    }

    #[test]
    fn test_infer_priority_phase_over_todo() {
        let result = infer_next_action("TODO: implement Phase 5 features");
        assert!(result.contains("Phase 5"));
    }

    #[test]
    fn test_infer_unicode_no_panic() {
        let result = infer_next_action("İstanbul projesinde çalışmaya devam");
        assert!(!result.is_empty());
    }

    // --- sanitize_preview tests ---

    #[test]
    fn test_sanitize_preview() {
        assert_eq!(sanitize_preview("hello\nworld"), "hello world");
        assert_eq!(sanitize_preview("no\r\nnewlines"), "no  newlines");
        assert_eq!(sanitize_preview("clean text"), "clean text");
        assert_eq!(sanitize_preview("a\x00b\x01c"), "abc");
    }

    // --- relative_time_label tests (Bug 2: hour-level granularity) ---

    #[test]
    fn test_relative_time_just_now() {
        let now = Utc::now();
        let ts = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        assert_eq!(relative_time_label(&ts, &now), "just now");
    }

    #[test]
    fn test_relative_time_minutes_ago() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::minutes(15))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert_eq!(relative_time_label(&ts, &now), "15m ago");
    }

    #[test]
    fn test_relative_time_hours_ago() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::hours(3))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert_eq!(relative_time_label(&ts, &now), "3h ago");
    }

    #[test]
    fn test_same_day_differentiation() {
        let now = Utc::now();
        let ts_2h = (now - chrono::Duration::hours(2))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let ts_6h = (now - chrono::Duration::hours(6))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let label_2h = relative_time_label(&ts_2h, &now);
        let label_6h = relative_time_label(&ts_6h, &now);
        // Same-day sessions must be distinguishable
        assert_ne!(label_2h, label_6h);
        assert_eq!(label_2h, "2h ago");
        assert_eq!(label_6h, "6h ago");
    }

    #[test]
    fn test_relative_time_yesterday() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::hours(30))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert_eq!(relative_time_label(&ts, &now), "yesterday");
    }

    #[test]
    fn test_relative_time_days_ago() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::days(3))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert_eq!(relative_time_label(&ts, &now), "3d ago");
    }

    #[test]
    fn test_relative_time_weeks_ago() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::days(14))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert_eq!(relative_time_label(&ts, &now), "2w ago");
    }

    #[test]
    fn test_relative_time_months_ago() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::days(45))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert_eq!(relative_time_label(&ts, &now), "1mo ago");
    }

    #[test]
    fn test_relative_time_invalid() {
        let now = Utc::now();
        assert_eq!(relative_time_label("not-a-timestamp", &now), "???");
    }

    // --- compact_preview tests ---

    #[test]
    fn test_compact_preview_short() {
        assert_eq!(compact_preview("short text", 55), "short text");
    }

    #[test]
    fn test_compact_preview_truncates() {
        let long = "a".repeat(100);
        let result = compact_preview(&long, 55);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 58);
    }

    #[test]
    fn test_compact_preview_strips_newlines() {
        assert_eq!(compact_preview("hello\nworld\nfoo", 55), "hello world foo");
    }

    #[test]
    fn test_compact_preview_unicode_safe() {
        let content = "\u{1f600}".repeat(30);
        let result = compact_preview(&content, 10);
        assert!(result.ends_with("..."));
    }

    // --- strip_preamble tests (Bug 4) ---

    #[test]
    fn test_strip_preamble_implement() {
        assert_eq!(
            strip_preamble("Implement the following plan: Fix the timeline"),
            "Fix the timeline"
        );
    }

    #[test]
    fn test_strip_preamble_execute() {
        assert_eq!(
            strip_preamble("Execute the following plan: Phase 5 work"),
            "Phase 5 work"
        );
    }

    #[test]
    fn test_strip_preamble_markdown_header() {
        assert_eq!(strip_preamble("## Phase 3 Implementation"), "Phase 3 Implementation");
    }

    #[test]
    fn test_strip_preamble_no_match() {
        assert_eq!(strip_preamble("Normal text here"), "Normal text here");
    }

    #[test]
    fn test_strip_preamble_prefix_then_header() {
        assert_eq!(
            strip_preamble("Implement the following plan: # Big Plan"),
            "Big Plan"
        );
    }

    // --- is_displayable tests (Bug 3) ---

    #[test]
    fn test_is_displayable_good_session() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 50,
            chunk_count: 3,
            summary: Some("Did some work".to_string()),
            enrichment: None,
        };
        assert!(is_displayable(&session));
    }

    #[test]
    fn test_is_displayable_too_few_messages() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 1,
            chunk_count: 1,
            summary: Some("Short".to_string()),
            enrichment: None,
        };
        assert!(!is_displayable(&session));
    }

    #[test]
    fn test_is_displayable_empty_summary_no_enrichment() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 50,
            chunk_count: 3,
            summary: Some("   ".to_string()),
            enrichment: None,
        };
        assert!(!is_displayable(&session));
    }

    #[test]
    fn test_is_displayable_enrichment_only() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 50,
            chunk_count: 3,
            summary: None,
            enrichment: Some("[Heuristic] Project: test\nTools: Edit".to_string()),
        };
        assert!(is_displayable(&session));
    }

    // --- enrichment parsing + inference tests (Bug 6) ---

    #[test]
    fn test_parse_enrichment_full() {
        let enrichment = "[Heuristic] Project: csr\nMessages: 603 (227 user)\nTools: TaskCreate, Edit, Bash, Read\nFiles: mod.rs, engine.rs\nHad errors: yes";
        let fields = parse_enrichment(enrichment);
        assert_eq!(fields.tools, vec!["TaskCreate", "Edit", "Bash", "Read"]);
        assert_eq!(fields.files, vec!["mod.rs", "engine.rs"]);
        assert!(fields.has_errors);
        assert!(fields.has_edit_tool);
    }

    #[test]
    fn test_parse_enrichment_no_errors() {
        let enrichment = "[Heuristic] Project: csr\nMessages: 100 (50 user)\nTools: Read, Grep\nFiles: main.rs";
        let fields = parse_enrichment(enrichment);
        assert!(!fields.has_errors);
        assert!(!fields.has_edit_tool);
    }

    #[test]
    fn test_infer_from_enrichment_errors_with_edit() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 100,
            chunk_count: 5,
            summary: None,
            enrichment: Some("[Heuristic] Project: test\nTools: Edit, Bash\nFiles: main.rs\nHad errors: yes".to_string()),
        };
        let result = infer_next_action_from_session(&session);
        assert!(result.contains("Fix errors"));
        assert!(result.contains("edits were in progress"));
    }

    #[test]
    fn test_infer_from_enrichment_errors_no_edit() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 100,
            chunk_count: 5,
            summary: None,
            enrichment: Some("[Heuristic] Project: test\nTools: Read, Grep\nHad errors: yes".to_string()),
        };
        let result = infer_next_action_from_session(&session);
        assert!(result.contains("Investigate and fix errors"));
    }

    #[test]
    fn test_infer_from_enrichment_files() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 50,
            chunk_count: 3,
            summary: None,
            enrichment: Some("[Heuristic] Project: test\nTools: Edit, Read\nFiles: session_start.rs, queries.rs".to_string()),
        };
        let result = infer_next_action_from_session(&session);
        assert!(result.contains("Continue work on"));
        assert!(result.contains("session_start.rs"));
    }

    #[test]
    fn test_infer_from_enrichment_large_session() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 500,
            chunk_count: 10,
            summary: None,
            enrichment: Some("[Heuristic] Project: test\nMessages: 500 (200 user)\nTools: Read, Grep".to_string()),
        };
        let result = infer_next_action_from_session(&session);
        assert!(result.contains("Large session"));
    }

    #[test]
    fn test_infer_from_enrichment_fallback_to_keyword() {
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: "2026-02-15T10:00:00Z".to_string(),
            total_messages: 50,
            chunk_count: 3,
            summary: Some("Phase 4 implementation".to_string()),
            enrichment: None,
        };
        let result = infer_next_action_from_session(&session);
        assert!(result.contains("Phase 4"));
    }

    // --- enrichment_display tests ---

    #[test]
    fn test_enrichment_display_full() {
        let enrichment = "[Heuristic] Project: csr\nTools: Edit, Bash, Read\nFiles: mod.rs, engine.rs\nHad errors: yes";
        let display = enrichment_display(enrichment).unwrap();
        assert!(display.contains("Tools: Edit, Bash, Read"));
        assert!(display.contains("Files: mod.rs, engine.rs"));
        assert!(display.contains("Had errors"));
    }

    #[test]
    fn test_enrichment_display_no_structured_data() {
        let enrichment = "[Heuristic] Project: csr\nMessages: 10 (5 user)";
        assert!(enrichment_display(enrichment).is_none());
    }

    // --- format_session_line tests ---

    #[test]
    fn test_format_session_line_with_enrichment() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::hours(2))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: ts,
            total_messages: 293,
            chunk_count: 6,
            summary: Some("Some work".to_string()),
            enrichment: Some("[Heuristic] Project: test\nTools: Edit, Read\nFiles: main.rs".to_string()),
        };
        let line = format_session_line(&session, &now);
        assert!(line.contains("2h ago"));
        assert!(line.contains("293 msgs"));
        assert!(line.contains("Tools: Edit, Read"));
        assert!(line.contains("Files: main.rs"));
    }

    #[test]
    fn test_format_session_line_summary_fallback() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::days(1))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: ts,
            total_messages: 90,
            chunk_count: 2,
            summary: Some("Phase 3 HNSW persistence work".to_string()),
            enrichment: None,
        };
        let line = format_session_line(&session, &now);
        assert!(line.contains("yesterday"));
        assert!(line.contains("90 msgs"));
        assert!(line.contains("Phase 3 HNSW persistence work"));
    }

    #[test]
    fn test_format_session_line_strips_preamble() {
        let now = Utc::now();
        let ts = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let session = SessionInfo {
            conversation_id: "abc".to_string(),
            project_name: "test".to_string(),
            timestamp: ts,
            total_messages: 50,
            chunk_count: 2,
            summary: Some("Implement the following plan: Fix the bugs".to_string()),
            enrichment: None,
        };
        let line = format_session_line(&session, &now);
        assert!(line.contains("Fix the bugs"));
        assert!(!line.contains("Implement the following plan"));
    }
}
