//! PreCompact hook — imports the transcript before compaction.
//!
//! For ALL sessions:
//! 1. Imports current transcript (so it's searchable post-compact)
//! 2. Triggers one dream cycle
//!
//! The session's story is written after the compaction instead, by SessionStart, from the
//! summary Claude Code writes as it compacts: that needs no model call.
//!
//! Always exits 0 (never blocks compaction).

use std::path::Path;

use anyhow::Result;

use super::HookInput;
use crate::engine::Engine;

/// Only compact recaps consume this snapshot. The Stop episode remains the
/// source of the first-task anchor for startup, resume and other sessions.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct CompactSnapshot {
    pub anchor: Option<super::stop::CompactAnchor>,
    pub completions: Vec<super::stop::CompletionReceipt>,
    pub completions_untied: bool,
}

/// Handle the precompact hook.
/// Always returns Ok(()) to never block compaction (E-1 fix).
pub async fn handle(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    // Import transcript for ALL sessions before compaction destroys context
    let mut rows = Vec::new();
    let mut observe = |row: &serde_json::Value| {
        // Progress payloads dominate large transcripts and contain no recap
        // evidence. Keep only the row types used by the episode extractor.
        if matches!(
            row.get("type").and_then(|v| v.as_str()),
            Some("user" | "human" | "assistant" | "system")
        ) {
            rows.push(row.clone());
        }
    };
    super::import_current_transcript_observing(input, engine, cwd, Some(&mut observe)).await;
    if let Some(session_id) = input.session_id.as_deref() {
        if !rows.is_empty() {
            let (sanitized, _) = crate::import::sanitize_messages_for_search(&rows);
            let episode =
                super::stop::extract_episode_from_messages(&sanitized, &rows, session_id, "");
            let snapshot = CompactSnapshot {
                anchor: episode.compact_anchor,
                completions: episode.completions,
                completions_untied: episode.completions_untied,
            };
            let saved = serde_json::to_string(&snapshot)
                .map_err(anyhow::Error::from)
                .and_then(|json| {
                    engine
                        .storage()
                        .set_meta(&format!("compact_anchor:{session_id}"), &json)
                });
            if let Err(error) = saved {
                eprintln!("CSR: compact anchor refresh failed (non-fatal): {error}");
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{embeddings::EmbeddingEngine, search::SearchEngine, storage::Storage};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    const FIRST: &str = "Design the discussion side pane for users.";
    const LATEST: &str = "Fix the database durability regression before release.";

    async fn exercise(padding_rows: usize, unchanged: bool) -> (String, f64, u64) {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = Engine::from_parts(
            storage.clone(),
            Arc::new(EmbeddingEngine::disabled()),
            Arc::new(RwLock::new(SearchEngine::new(16))),
            temp.path().to_path_buf(),
        );
        let path = temp.path().join("r8-compact.jsonl");
        let first = serde_json::json!({"type":"user","message":{"content":FIRST}}).to_string();
        let done = serde_json::json!({"type":"assistant","message":{"content":"Done.","stop_reason":"end_turn"}}).to_string();
        let ep = crate::hooks::stop::extract_episode(&[&first, &done], "r8-compact", "fixture");
        storage
            .insert_reflection(
                "r8-episode",
                &serde_json::to_string(&ep).unwrap(),
                &crate::hooks::stop::episode_tags(&ep),
                &[0.0; 384],
            )
            .unwrap();
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "{first}\n{done}").unwrap();
        let padding = serde_json::json!({"type":"progress","data":"x".repeat(65_000)}).to_string();
        for _ in 0..padding_rows {
            writeln!(file, "{padding}").unwrap();
        }
        writeln!(
            file,
            "{}",
            serde_json::json!({"type":"user","message":{"content":LATEST}})
        )
        .unwrap();
        drop(file);
        let canonical = path.canonicalize().unwrap();
        // Existing imported chunks keep this benchmark independent of embeddings.
        storage
            .upsert_import_state_explicit(canonical.to_str().unwrap(), "r8-compact", 100, "fixture")
            .unwrap();
        if unchanged {
            storage.mark_file_imported(&canonical, 100).unwrap();
        }
        let input = HookInput {
            session_id: Some("r8-compact".into()),
            transcript_path: Some(canonical.to_string_lossy().into()),
            ..Default::default()
        };
        let start = std::time::Instant::now();
        handle(&input, &engine, temp.path()).await.unwrap();
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        let block = crate::hooks::session_start::format_session_start_tier0_for_session(
            &storage,
            "fixture",
            &ep,
            &[],
            "now",
            Some("r8-compact"),
            Some("compact"),
        );
        // Other startup paths must keep their first-task anchor.
        for source in ["startup", "resume", "clear"] {
            let block = crate::hooks::session_start::format_session_start_tier0_for_session(
                &storage,
                "fixture",
                &ep,
                &[],
                "now",
                Some("r8-compact"),
                Some(source),
            );
            assert!(block.contains(FIRST), "{block}");
        }
        (block, elapsed, std::fs::metadata(path).unwrap().len())
    }

    #[test]
    fn r8_precompact_refreshes_task_before_stop() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        std::env::set_var("CSR_NO_DREAM_ON_COMPACT", "1");
        std::env::remove_var("CSR_NO_RECAP");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for unchanged in [false, true] {
            let (block, _, _) = runtime.block_on(exercise(0, unchanged));
            assert!(block.contains(LATEST), "{block}");
        }
        std::env::remove_var("CSR_NO_DREAM_ON_COMPACT");
    }

    #[test]
    #[ignore = "65 MB PreCompact latency measurement; run explicitly in release mode"]
    fn r8_precompact_timing_65mb() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        std::env::set_var("CSR_NO_DREAM_ON_COMPACT", "1");
        std::env::remove_var("CSR_NO_RECAP");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for _ in 0..5 {
            let (_, elapsed, bytes) = runtime.block_on(exercise(1000, false));
            eprintln!("precompact_bytes={bytes} precompact_ms={elapsed:.3}");
        }
        std::env::remove_var("CSR_NO_DREAM_ON_COMPACT");
    }
}
