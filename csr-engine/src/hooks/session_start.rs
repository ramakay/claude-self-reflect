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

use super::ralph_state::RalphState;
use super::HookInput;
use crate::engine::Engine;
use crate::injection::anti_pattern;

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

    // If no Ralph session, output brief status and exit
    let ralph = match ralph {
        Some(r) => r,
        None => {
            println!(
                "CSR engine ready. Session: {}. {} chunks, {} reflections indexed.",
                session_id,
                engine.search().read().await.chunk_count(),
                engine.search().read().await.reflection_count(),
            );
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
