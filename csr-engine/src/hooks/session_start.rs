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

            // Get 5 most recent chunks for compact timeline (fast DB query, no embedding)
            let chunks = engine
                .storage()
                .get_recent_chunks(5, project.as_deref())
                .unwrap_or_default();

            let now = Utc::now();

            // Build full output string, single println (L-3 fix)
            let mut output = format!("CSR engine ready. Session: {session_id}. {chunk_count} chunks, {reflection_count} reflections indexed.");

            if !chunks.is_empty() {
                output.push_str("\n\n\u{1f4cb} Recent sessions:");
                for chunk in &chunks {
                    output.push_str(&format!("\n  {}", format_timeline_line(&chunk.timestamp, chunk.message_count, &chunk.content, &now)));
                }

                // Infer next action from the most recent session
                let suggestion = infer_next_action(&chunks[0].content);
                output.push_str(&format!("\n\n\u{1f4a1} Suggested next: {suggestion}"));
                output.push_str("\n\u{1f50d} Drill down: use get_recent_work, reflect_on_past, or get_full_conversation");
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

/// Format a relative time label for the timeline: "today", "yesterday", "3d ago", "2w ago".
/// Uses `temporal::parse_timestamp` for parsing, pure fn with injected `now`.
fn relative_time_label(timestamp: &str, now: &DateTime<Utc>) -> String {
    let ts = match temporal::parse_timestamp(timestamp) {
        Some(t) => t,
        None => return "???".to_string(),
    };
    let diff = *now - ts;
    let days = diff.num_days();

    if days == 0 {
        "today".to_string()
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

/// Compose one aligned timeline row: "today     |  42 msgs | Implemented HNSW persist..."
fn format_timeline_line(timestamp: &str, message_count: usize, content: &str, now: &DateTime<Utc>) -> String {
    let label = relative_time_label(timestamp, now);
    let preview = compact_preview(content, 55);
    format!("{:<9} | {:>3} msgs | {}", label, message_count, preview)
}

/// Sanitize a preview string for safe stdout injection.
/// Collapses newlines to spaces, strips control characters (M-2 fix).
fn sanitize_preview(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .filter(|c| !c.is_control() || *c == ' ')
        .collect()
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

    #[test]
    fn test_infer_phase_generic() {
        // Matches first "phase N" found in content
        let result = infer_next_action("Phase 4 implementation is next");
        assert!(result.contains("Phase 4"));
        // Also works for any phase number
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
        // Phase number should win over TODO since it's checked first
        let result = infer_next_action("TODO: implement Phase 5 features");
        assert!(result.contains("Phase 5"));
    }

    #[test]
    fn test_infer_unicode_no_panic() {
        // Should not panic on non-ASCII content (M-1 fix)
        let result = infer_next_action("İstanbul projesinde çalışmaya devam");
        assert!(!result.is_empty());
    }

    #[test]
    fn test_sanitize_preview() {
        assert_eq!(sanitize_preview("hello\nworld"), "hello world");
        assert_eq!(sanitize_preview("no\r\nnewlines"), "no  newlines");
        assert_eq!(sanitize_preview("clean text"), "clean text");
        // Control chars stripped
        assert_eq!(sanitize_preview("a\x00b\x01c"), "abc");
    }

    // --- Timeline helper tests ---

    #[test]
    fn test_relative_time_today() {
        let now = Utc::now();
        let ts = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        assert_eq!(relative_time_label(&ts, &now), "today");
    }

    #[test]
    fn test_relative_time_yesterday() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::hours(30)).format("%Y-%m-%dT%H:%M:%SZ").to_string();
        assert_eq!(relative_time_label(&ts, &now), "yesterday");
    }

    #[test]
    fn test_relative_time_days_ago() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::days(3)).format("%Y-%m-%dT%H:%M:%SZ").to_string();
        assert_eq!(relative_time_label(&ts, &now), "3d ago");
    }

    #[test]
    fn test_relative_time_weeks_ago() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::days(14)).format("%Y-%m-%dT%H:%M:%SZ").to_string();
        assert_eq!(relative_time_label(&ts, &now), "2w ago");
    }

    #[test]
    fn test_relative_time_months_ago() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::days(45)).format("%Y-%m-%dT%H:%M:%SZ").to_string();
        assert_eq!(relative_time_label(&ts, &now), "1mo ago");
    }

    #[test]
    fn test_relative_time_invalid() {
        let now = Utc::now();
        assert_eq!(relative_time_label("not-a-timestamp", &now), "???");
    }

    #[test]
    fn test_compact_preview_short() {
        assert_eq!(compact_preview("short text", 55), "short text");
    }

    #[test]
    fn test_compact_preview_truncates() {
        let long = "a".repeat(100);
        let result = compact_preview(&long, 55);
        assert!(result.ends_with("..."));
        // 55 chars + "..." = 58
        assert!(result.len() <= 58);
    }

    #[test]
    fn test_compact_preview_strips_newlines() {
        assert_eq!(compact_preview("hello\nworld\nfoo", 55), "hello world foo");
    }

    #[test]
    fn test_compact_preview_unicode_safe() {
        // Unicode chars should not cause panics on truncation
        let content = "\u{1f600}".repeat(30); // 30 smiley faces (4 bytes each)
        let result = compact_preview(&content, 10);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_format_timeline_line_alignment() {
        let now = Utc::now();
        let ts = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let line = format_timeline_line(&ts, 42, "Implemented HNSW persistence", &now);
        assert!(line.contains("today"));
        assert!(line.contains("42 msgs"));
        assert!(line.contains("Implemented HNSW persistence"));
    }

    #[test]
    fn test_format_timeline_line_pads_label() {
        let now = Utc::now();
        let ts = (now - chrono::Duration::days(3)).format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let line = format_timeline_line(&ts, 8, "Short session", &now);
        // "3d ago" should be padded to 9 chars
        assert!(line.starts_with("3d ago   "));
    }
}
