//! PostToolUse hook — imports transcript for real-time searchability.
//!
//! Fires after each successful tool call. Imports the current transcript
//! so new content becomes searchable.
//!
//! Always returns Ok(()) — never blocks Claude Code (C-2 fix).

use std::path::Path;

use anyhow::Result;

use super::HookInput;
use crate::engine::Engine;

/// Handle the post-tool-use hook.
/// Always returns Ok(()) to never block Claude Code (C-2 fix).
pub async fn handle(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    // Import growing transcript (real-time searchability)
    super::import_current_transcript(input, engine, cwd).await;
    Ok(())
}
