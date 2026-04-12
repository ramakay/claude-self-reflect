//! Stop hook — imports transcript for real-time searchability.
//!
//! For all sessions, imports the current transcript so content is searchable.
//! Always returns Ok(()) — never blocks Claude Code.

use std::path::Path;

use anyhow::Result;

use super::HookInput;
use crate::engine::Engine;

/// Handle the stop hook.
/// Always returns Ok(()) to never block Claude Code (C-1 fix).
pub async fn handle(
    input: &HookInput,
    engine: &Engine,
    cwd: &Path,
) -> Result<()> {
    // Import growing transcript for ALL sessions (real-time searchability)
    super::import_current_transcript(input, engine, cwd).await;
    Ok(())
}
