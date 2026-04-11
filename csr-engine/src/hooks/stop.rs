//! Stop hook — iteration-level memory for Ralph sessions.
//!
//! Fires after every Claude response. When a Ralph session is active:
//! 1. Stores current iteration learnings (content, tags)
//! 2. Retrieves previous iteration learnings from same session
//! 3. Runs stuck detector on current Ralph state
//! 4. Builds InjectionContext and formats with 300-token budget
//! 5. Writes iteration context to `.ralph_iteration_context.md` in CWD
//!
//! For non-Ralph sessions, exits immediately with no output.
//! Always returns Ok(()) — never blocks Claude Code (C-1 fix).

use std::path::Path;

use anyhow::Result;

use super::ralph_state::RalphState;
use super::HookInput;
use crate::engine::Engine;
use crate::injection::formatter;
use crate::injection::stuck_detector;
use crate::injection::{InjectionContext, InjectionItem};
use crate::mcp::tools;

/// Default token budget for iteration context (compact to avoid context bloat).
const ITERATION_TOKEN_BUDGET: usize = 300;

/// Handle the stop hook.
/// Wrapped in catch-all: ALWAYS returns Ok(()) to never block Claude Code (C-1 fix).
pub async fn handle(
    input: &HookInput,
    ralph: Option<&RalphState>,
    engine: &Engine,
    cwd: &Path,
) -> Result<()> {
    // Import growing transcript for ALL sessions (real-time searchability)
    super::import_current_transcript(input, engine, cwd).await;

    // Ralph iteration memory only applies to Ralph sessions
    if ralph.is_none() {
        return Ok(());
    }

    if let Err(e) = handle_inner(input, ralph, engine, cwd).await {
        eprintln!("CSR: stop hook error (non-fatal): {}", e);
    }
    Ok(()) // Always succeed
}

async fn handle_inner(
    input: &HookInput,
    ralph: Option<&RalphState>,
    engine: &Engine,
    cwd: &Path,
) -> Result<()> {
    let ralph = match ralph {
        Some(r) => r,
        None => return Ok(()),
    };

    // Don't act on hook re-entry (stop_hook_active = true means we're already continuing)
    if input.stop_hook_active.unwrap_or(false) {
        return Ok(());
    }

    // 1. Store current iteration learnings (H-2 fix: only store when iteration changes)
    // We check if this iteration was already stored by looking for the tag
    let iter_tag = format!("iteration_{}", ralph.iteration);
    let session_tag = format!("session_{}", ralph.session_id);
    let already_stored = is_iteration_stored(engine, &session_tag, &iter_tag);
    if !already_stored {
        store_iteration_learnings(ralph, engine).await;
    }

    // 2. Retrieve previous iteration learnings
    let iteration_items = retrieve_past_iterations(ralph, engine);

    // 3. Run stuck detector
    let stuck = stuck_detector::analyze(ralph);
    let stuck_warning = stuck_detector::format_warning(&stuck);

    // 4. Build InjectionContext
    let ctx = InjectionContext {
        iteration_learnings: iteration_items,
        stuck_warning,
        ..Default::default()
    };

    if ctx.is_empty() {
        return Ok(());
    }

    // 5. Format and write context file
    let formatted = ctx.format(ITERATION_TOKEN_BUDGET);
    if !formatted.is_empty() {
        let context_path = cwd.join(".ralph_iteration_context.md");
        // Atomic write: tmp then rename
        let tmp_path = context_path.with_extension("md.tmp");
        std::fs::write(&tmp_path, &formatted)?;
        std::fs::rename(&tmp_path, &context_path)?;
    }

    // 6. Brief summary to stdout
    let item_count = ctx.total_items();
    if item_count > 0 || stuck.is_stuck {
        println!(
            "CSR: Iteration {} — {} past notes{}",
            ralph.iteration,
            item_count,
            if stuck.is_stuck {
                format!(
                    " [{}]",
                    match stuck.severity {
                        stuck_detector::StuckSeverity::Warning => "STUCK-WARNING",
                        stuck_detector::StuckSeverity::Critical => "STUCK-CRITICAL",
                        stuck_detector::StuckSeverity::Normal => "",
                    }
                )
            } else {
                String::new()
            }
        );
    }

    Ok(())
}

/// Check if an iteration was already stored (H-2 fix: prevent duplicate storage).
fn is_iteration_stored(engine: &Engine, session_tag: &str, iter_tag: &str) -> bool {
    if let Ok(reflections) = engine.storage().get_reflections_by_tag(session_tag, 20) {
        reflections
            .iter()
            .any(|(_, _, tags, _)| tags.contains(&iter_tag.to_string()))
    } else {
        false
    }
}

/// Store current iteration learnings as a reflection.
async fn store_iteration_learnings(ralph: &RalphState, engine: &Engine) {
    let mut content = format!(
        "ITERATION {} of session {}\nTask: {}\n",
        ralph.iteration, ralph.session_id, ralph.task
    );

    if !ralph.learnings.is_empty() {
        content.push_str("Learnings:\n");
        for learning in &ralph.learnings {
            content.push_str(&format!("- {}\n", learning));
        }
    }

    if !ralph.files_modified.is_empty() {
        content.push_str("Files modified:\n");
        for file in ralph.files_modified.iter().rev().take(5) {
            content.push_str(&format!("- {}\n", file));
        }
    }

    content.push_str(&format!("Confidence: {}%\n", ralph.exit_confidence));

    let tags = vec![
        "ralph_iteration".to_string(),
        format!("session_{}", ralph.session_id),
        format!("iteration_{}", ralph.iteration),
    ];

    if let Err(e) = tools::store_reflection(
        engine.storage(),
        engine.embeddings(),
        engine.search(),
        &content,
        &tags,
    )
    .await
    {
        eprintln!("CSR: Failed to store iteration learnings: {}", e);
    }
}

/// Retrieve learnings from previous iterations of the same session.
/// Synchronous — uses only tag-based storage queries, no embeddings.
fn retrieve_past_iterations(ralph: &RalphState, engine: &Engine) -> Vec<InjectionItem> {
    let session_tag = format!("session_{}", ralph.session_id);
    let results = engine
        .storage()
        .get_reflections_by_tag(&session_tag, 10);

    let mut items = Vec::new();
    if let Ok(reflections) = results {
        for (_id, content, tags, _ts) in reflections {
            let is_iteration = tags.iter().any(|t| t.starts_with("iteration_"));
            if !is_iteration {
                continue;
            }

            let iter_num: Option<usize> = tags.iter().find_map(|t| {
                t.strip_prefix("iteration_")
                    .and_then(|n| n.parse().ok())
            });

            // Only include iterations before the current one
            if let Some(n) = iter_num {
                if n >= ralph.iteration {
                    continue;
                }
            }

            items.push(InjectionItem {
                content: formatter::truncate_content(&content, 200),
                score: 1.0,
                source: format!("iteration_{}", iter_num.unwrap_or(0)),
            });

            if items.len() >= 5 {
                break;
            }
        }
    }

    items
}

