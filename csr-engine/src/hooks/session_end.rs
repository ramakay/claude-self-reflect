//! SessionEnd hook — generates session narrative and stores to CSR.
//!
//! When a Ralph session is active:
//! 1. Determines outcome (COMPLETED, ABANDONED, INCOMPLETE)
//! 2. Generates narrative from Ralph state
//! 3. Stores to CSR with rich tags for future searchability
//! 4. If COMPLETED, stores winning strategy separately
//! 5. Cleans up temp files

use std::path::Path;

use anyhow::Result;

use super::ralph_state::{Outcome, RalphState};
use super::HookInput;
use crate::engine::Engine;
use crate::mcp::tools;

/// Handle the session-end hook.
pub async fn handle(
    input: &HookInput,
    ralph: Option<&RalphState>,
    engine: &Engine,
    cwd: &Path,
) -> Result<()> {
    // If no Ralph session, exit silently
    let ralph = match ralph {
        Some(r) => r,
        None => return Ok(()),
    };

    let reason = input.reason.as_deref().unwrap_or("unknown");
    let outcome = ralph.determine_outcome(reason);

    // Generate narrative
    let narrative = ralph.to_narrative(&outcome);

    // Build tags
    let mut tags = vec![
        "ralph_session".to_string(),
        format!("session_{}", ralph.session_id),
        format!("outcome_{}", outcome),
        format!("iterations_{}", ralph.iteration),
        format!("work_type_{}", ralph.work_type.to_string().to_lowercase()),
    ];

    // Add error signature tags for searchability
    for (sig, _count) in &ralph.error_signatures {
        let tag = format!(
            "error_{}",
            sig.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .take(50)
                .collect::<String>()
                .to_lowercase()
        );
        tags.push(tag);
    }

    // Store session narrative
    let store_result = tools::store_reflection(
        engine.storage(),
        engine.embeddings(),
        engine.search(),
        &narrative,
        &tags,
    )
    .await;

    match &store_result {
        Ok(msg) => {
            println!(
                "CSR: Stored Ralph session narrative (outcome: {})",
                outcome,
            );
            tracing::debug!("{}", msg);
        }
        Err(e) => {
            eprintln!("CSR: Failed to store session narrative: {}", e);
        }
    }

    // If completed, store winning strategy separately
    if outcome == Outcome::Completed && !ralph.successful_strategies.is_empty() {
        let mut strategy_content = format!(
            "WINNING STRATEGY for task: {}\n\n",
            ralph.task,
        );
        for strategy in &ralph.successful_strategies {
            strategy_content.push_str(&format!("- {}\n", strategy));
        }
        if !ralph.learnings.is_empty() {
            strategy_content.push_str("\nKey learnings:\n");
            for learning in &ralph.learnings {
                strategy_content.push_str(&format!("- {}\n", learning));
            }
        }

        let strategy_tags = vec![
            "winning_strategy".to_string(),
            format!("session_{}", ralph.session_id),
            "outcome_completed".to_string(),
        ];

        if let Err(e) = tools::store_reflection(
            engine.storage(),
            engine.embeddings(),
            engine.search(),
            &strategy_content,
            &strategy_tags,
        )
        .await
        {
            eprintln!("CSR: Failed to store winning strategy: {}", e);
        } else {
            println!("CSR: Stored winning strategy for session {}", ralph.session_id);
        }
    }

    // Clean up temp files
    let context_file = cwd.join(".ralph_past_sessions.md");
    if context_file.exists() {
        if let Err(e) = std::fs::remove_file(&context_file) {
            eprintln!("CSR: Failed to clean up {}: {}", context_file.display(), e);
        }
    }

    // Output summary
    println!(
        "CSR: Session {} ended. Outcome: {}, Iterations: {}, Work type: {}",
        ralph.session_id, outcome, ralph.iteration, ralph.work_type,
    );

    Ok(())
}
