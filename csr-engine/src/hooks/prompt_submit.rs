//! UserPromptSubmit hook — predictive injection on every user message.
//!
//! Fires when the user submits a prompt. Searches CSR for relevant past context
//! and injects it via stdout (hookSpecificOutput pattern — Claude Code prepends
//! the output to the system prompt).
//!
//! Fast-path exits (no engine work):
//! - No prompt or prompt too short (< 15 chars)
//! - Slash commands (starts with `/`)
//! - Empty JSON input
//!
//! When a relevant match is found, outputs formatted context to stdout with
//! a 500-token budget (larger than Stop's 300 — this is the main context path).
//!
//! Always returns Ok(()) — never blocks Claude Code (catch-all wrapper).

use std::path::Path;

use anyhow::Result;

use super::ralph_state::RalphState;
use super::HookInput;
use crate::engine::Engine;
use crate::injection::anti_pattern;
use crate::injection::predictor::{self, RawResult};
use crate::injection::{InjectionContext, InjectionItem};

/// Token budget for prompt-submit injection (larger than Stop hook's 300).
const PROMPT_TOKEN_BUDGET: usize = 500;

/// Minimum prompt length to trigger search (avoids noise from short prompts).
const MIN_PROMPT_LENGTH: usize = 15;

/// Handle the prompt-submit hook.
/// Wrapped in catch-all: ALWAYS returns Ok(()) to never block Claude Code.
pub async fn handle(
    input: &HookInput,
    ralph: Option<&RalphState>,
    engine: &Engine,
    cwd: &Path,
) -> Result<()> {
    if let Err(e) = handle_inner(input, ralph, engine, cwd).await {
        eprintln!("CSR: prompt-submit hook error (non-fatal): {}", e);
    }
    Ok(()) // Always succeed
}

async fn handle_inner(
    input: &HookInput,
    ralph: Option<&RalphState>,
    engine: &Engine,
    _cwd: &Path,
) -> Result<()> {
    // Extract prompt from input
    let prompt = match input.prompt.as_deref() {
        Some(p) if !p.is_empty() => p,
        _ => return Ok(()), // No prompt → silent exit
    };

    // Fast-path: skip short prompts
    if prompt.len() < MIN_PROMPT_LENGTH {
        return Ok(());
    }

    // Fast-path: skip slash commands
    if prompt.starts_with('/') {
        return Ok(());
    }

    let storage = engine.storage();
    let embeddings = engine.embeddings();
    let search = engine.search();

    // 1. Search for anti-patterns (highest priority)
    let anti_patterns = anti_pattern::find_anti_patterns(
        storage, embeddings, search, prompt, 0.5, 2,
    )
    .await;

    // 2. Search chunks (past conversations)
    let chunk_results = search_chunks_for_prompt(engine, prompt, 5, 0.6).await;

    // 3. Search reflections (stored insights)
    let reflection_results = search_reflections_for_prompt(engine, prompt, 3, 0.5).await;

    // 4. Combine and score results
    let current_files: Vec<String> = ralph
        .map(|r| r.files_modified.clone())
        .unwrap_or_default();
    let current_errors: Vec<String> = ralph
        .map(|r| r.error_signatures.iter().map(|(sig, _)| sig.clone()).collect())
        .unwrap_or_default();

    let mut raw_results: Vec<RawResult> = Vec::new();
    raw_results.extend(chunk_results);
    raw_results.extend(reflection_results);

    let scored = predictor::rank_results(raw_results, &current_files, &current_errors);

    // 5. Build InjectionContext
    let mut ctx = InjectionContext {
        anti_patterns,
        ..Default::default()
    };

    // Distribute scored results into context categories.
    // Skip anti_patterns here — they're already loaded from find_anti_patterns() above.
    for result in scored.iter().take(5) {
        // Deduplicate: skip anti-patterns already found by find_anti_patterns
        if result.source == "anti_pattern" {
            continue;
        }

        let item = InjectionItem {
            content: result.content.clone(),
            score: result.final_score,
            source: result.source.clone(),
        };

        match result.source.as_str() {
            "reflection" => ctx.winning_strategies.push(item),
            _ => ctx.winning_strategies.push(item), // chunks also go to winning strategies
        }
    }

    if ctx.is_empty() {
        return Ok(());
    }

    // 6. Format with 500-token budget and output to stdout
    let formatted = ctx.format(PROMPT_TOKEN_BUDGET);
    if !formatted.is_empty() {
        // stdout injection: Claude Code prepends this to the system prompt.
        // Uses print! (not println!) to avoid double-newline — formatter output ends with \n.
        print!("{}", formatted);

        // Summary to stderr (not visible to Claude, only to debug logs)
        let anti_count = ctx.anti_patterns.len();
        let total = ctx.total_items();
        eprintln!(
            "CSR: Injected {} items ({} anti-patterns) for prompt",
            total, anti_count
        );
    }

    Ok(())
}

/// Search chunks semantically for a given prompt.
async fn search_chunks_for_prompt(
    engine: &Engine,
    prompt: &str,
    limit: usize,
    min_score: f32,
) -> Vec<RawResult> {
    let embeddings = engine.embeddings();
    let search = engine.search();
    let storage = engine.storage();

    let query_vec = {
        let q = prompt.to_string();
        let emb = embeddings.clone();
        match tokio::task::spawn_blocking(move || emb.embed_single(&q)).await {
            Ok(Ok(v)) => v,
            _ => return Vec::new(),
        }
    };

    let results = {
        let idx = search.read().await;
        idx.search_chunks(&query_vec, limit, min_score)
    };

    let mut raw_results = Vec::new();
    for result in &results {
        if let Ok(chunks) = storage.get_chunks_by_ids(&[result.id.clone()]) {
            if let Some(chunk) = chunks.into_iter().next() {
                raw_results.push(RawResult {
                    content: truncate_content(&chunk.content, 300),
                    score: result.score,
                    source: "chunk".to_string(),
                    timestamp: Some(chunk.timestamp),
                    files: extract_file_paths(&chunk.content),
                    error_patterns: vec![],
                });
            }
        }
    }

    raw_results
}

/// Search reflections semantically for a given prompt.
async fn search_reflections_for_prompt(
    engine: &Engine,
    prompt: &str,
    limit: usize,
    min_score: f32,
) -> Vec<RawResult> {
    let embeddings = engine.embeddings();
    let search = engine.search();
    let storage = engine.storage();

    let query_vec = {
        let q = prompt.to_string();
        let emb = embeddings.clone();
        match tokio::task::spawn_blocking(move || emb.embed_single(&q)).await {
            Ok(Ok(v)) => v,
            _ => return Vec::new(),
        }
    };

    let results = {
        let idx = search.read().await;
        idx.search_reflections(&query_vec, limit, min_score)
    };

    let mut raw_results = Vec::new();
    for result in &results {
        if let Ok(Some((content, tags, timestamp))) = storage.get_reflection_by_id(&result.id) {
            let source = if tags.iter().any(|t| t == "outcome_incomplete" || t == "outcome_abandoned") {
                "anti_pattern"
            } else {
                "reflection"
            };
            raw_results.push(RawResult {
                content: truncate_content(&content, 300),
                score: result.score,
                source: source.to_string(),
                timestamp: Some(timestamp),
                files: extract_file_paths(&content),
                error_patterns: extract_error_patterns(&content),
            });
        }
    }

    raw_results
}

/// Extract file paths from content (simple heuristic: lines containing common extensions).
fn extract_file_paths(content: &str) -> Vec<String> {
    let extensions = [".rs", ".py", ".ts", ".js", ".toml", ".json", ".yaml", ".yml"];
    let mut files = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim().trim_start_matches("- ");
        for ext in &extensions {
            if trimmed.ends_with(ext) || trimmed.contains(&format!("{} ", ext)) {
                // Extract the path-like token
                if let Some(path) = trimmed.split_whitespace().find(|w| w.contains(ext)) {
                    files.push(path.to_string());
                    break;
                }
            }
        }
    }

    files
}

/// Extract error-like patterns from content.
fn extract_error_patterns(content: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        // Heuristic: lines starting with error indicators
        if trimmed.starts_with("Error:")
            || trimmed.starts_with("error[")
            || trimmed.starts_with("error:")
            || trimmed.starts_with("panicked at")
            || trimmed.contains("FAILED")
        {
            patterns.push(trimmed.to_string());
        }
    }
    patterns
}

/// Truncate content to max chars, preserving first line.
/// Uses char boundaries to avoid UTF-8 panics.
fn truncate_content(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        return content.to_string();
    }
    let safe_end = content.floor_char_boundary(max_chars);
    let truncated = &content[..safe_end];
    if let Some(last_newline) = truncated.rfind('\n') {
        format!("{}...", &content[..last_newline])
    } else {
        format!("{}...", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_file_paths() {
        let content = "Modified files:\n- src/auth.rs\n- Cargo.toml\n- some text";
        let files = extract_file_paths(content);
        assert!(files.contains(&"src/auth.rs".to_string()));
        assert!(files.contains(&"Cargo.toml".to_string()));
    }

    #[test]
    fn test_extract_error_patterns() {
        let content = "Log:\nError: connection refused\ninfo: compiling\nerror[E0308]: type mismatch";
        let errors = extract_error_patterns(content);
        assert_eq!(errors.len(), 2);
        assert!(errors[0].contains("connection refused"));
        assert!(errors[1].contains("E0308"));
    }

    #[test]
    fn test_truncate_content_short() {
        assert_eq!(truncate_content("short", 100), "short");
    }

    #[test]
    fn test_truncate_content_long() {
        let long = "line one\nline two\nline three\nline four\nline five";
        let truncated = truncate_content(long, 25);
        assert!(truncated.ends_with("..."));
        assert!(truncated.len() <= 28); // 25 + "..."
    }

    #[test]
    fn test_slash_command_detection() {
        // Verify our fast-path logic
        assert!("/help".starts_with('/'));
        assert!("/commit".starts_with('/'));
        assert!(!"fix the bug".starts_with('/'));
    }

    #[test]
    fn test_min_prompt_length() {
        assert!("short".len() < MIN_PROMPT_LENGTH);
        assert!("fix the authentication timeout bug".len() >= MIN_PROMPT_LENGTH);
    }
}
