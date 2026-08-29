//! PreCompact hook — imports transcript + generates session story before compaction.
//!
//! For ALL sessions:
//! 1. Imports current transcript (so it's searchable post-compact)
//! 2. Generates Haiku-curated session story (stored for SessionStart re-injection)
//!
//! Always exits 0 (never blocks compaction).

use std::path::Path;

use anyhow::Result;

use super::HookInput;
use crate::engine::Engine;

/// Handle the precompact hook.
/// Always returns Ok(()) to never block compaction (E-1 fix).
pub async fn handle(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    // Import transcript for ALL sessions before compaction destroys context
    super::import_current_transcript(input, engine, cwd).await;

    // Spawn detached Haiku story generation before compaction (fire-and-forget)
    if let Some(ref tp) = input.transcript_path {
        let cwd_str = cwd.to_string_lossy().to_string();
        crate::summarizer::spawn_detached_story_generation(tp, &cwd_str);
    }

    // v10: a compaction is a natural consolidation boundary. Trigger exactly
    // one dream cycle over the just-imported transcript, detached and
    // fire-and-forget, so it never blocks compaction. It advertises the live
    // "dreaming" statusline marker for its duration and self-skips if a dream
    // is already running (marker guard in `dream::run_dream_marked`).
    // Independent of the idle-gated daemon cadence — a compaction is an
    // explicit trigger. Opt out with CSR_NO_DREAM_ON_COMPACT / CSR_NO_DREAMING.
    crate::dream::spawn_detached_compact_dream();

    Ok(())
}
