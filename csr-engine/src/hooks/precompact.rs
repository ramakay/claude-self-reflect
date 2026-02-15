//! PreCompact hook — backs up Ralph state before context compaction.
//!
//! Context compaction erases in-context state; this preserves it to CSR
//! so the session can be recovered or referenced in future sessions.
//! Always exits 0 (never blocks compaction).

use anyhow::Result;

use super::ralph_state::RalphState;
use super::HookInput;
use crate::engine::Engine;
use crate::mcp::tools;

/// Handle the precompact hook.
/// Wrapped in catch-all: ALWAYS returns Ok(()) to never block compaction (E-1 fix).
pub async fn handle(
    input: &HookInput,
    ralph: Option<&RalphState>,
    engine: &Engine,
) -> Result<()> {
    if let Err(e) = handle_inner(input, ralph, engine).await {
        eprintln!("CSR: precompact error (non-fatal): {}", e);
    }
    Ok(()) // Always succeed
}

async fn handle_inner(
    _input: &HookInput,
    ralph: Option<&RalphState>,
    engine: &Engine,
) -> Result<()> {
    // If no Ralph session, exit silently
    let ralph = match ralph {
        Some(r) => r,
        None => return Ok(()),
    };

    // Serialize current state
    let state_text = format!(
        "PRE-COMPACTION BACKUP for Ralph session: {}\n\
         Task: {}\n\
         Iteration: {}\n\
         Work Type: {}\n\
         Exit Confidence: {}%\n\n\
         {}",
        ralph.session_id,
        ralph.task,
        ralph.iteration,
        ralph.work_type,
        ralph.exit_confidence,
        ralph.to_narrative(&super::ralph_state::Outcome::Incomplete),
    );

    let tags = vec![
        "ralph_state".to_string(),
        "pre_compact_backup".to_string(),
        format!("session_{}", ralph.session_id),
        format!("iteration_{}", ralph.iteration),
    ];

    tools::store_reflection(
        engine.storage(),
        engine.embeddings(),
        engine.search(),
        &state_text,
        &tags,
    )
    .await?;

    println!(
        "CSR: Ralph state backed up before compaction (session: {}, iteration: {})",
        ralph.session_id, ralph.iteration,
    );

    Ok(())
}
