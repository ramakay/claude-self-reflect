//! SessionEnd hook — imports current conversation and generates session narrative.
//!
//! For ALL sessions:
//! 1. Imports the current conversation file (real-time indexing)
//!
//! When a Ralph session is active, additionally:
//! 2. Determines outcome (COMPLETED, ABANDONED, INCOMPLETE)
//! 3. Generates narrative from Ralph state
//! 4. Stores to CSR with rich tags for future searchability
//! 5. If COMPLETED, stores winning strategy separately
//! 6. Cleans up temp files

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::ralph_state::{Outcome, RalphState};
use super::HookInput;
use crate::engine::Engine;
use crate::mcp::tools;
use crate::search::cross_project::resolve_project_from_cwd;

/// Handle the session-end hook.
pub async fn handle(
    input: &HookInput,
    ralph: Option<&RalphState>,
    engine: &Engine,
    cwd: &Path,
) -> Result<()> {
    // 1. Final transcript import (shared helper, incremental)
    super::import_current_transcript(input, engine, cwd).await;

    // 2. V3 extraction ��� produces a genuinely searchable index
    if let Some(ref tp) = input.transcript_path {
        let path = PathBuf::from(tp);
        if path.exists() {
            if let Err(e) = run_v3_extraction(engine, &path, cwd).await {
                eprintln!("CSR: V3 extraction failed (non-fatal): {}", e);
            }
        }
    }

    // 3. Spawn detached Haiku story generation (fire-and-forget, outlives this process)
    if let Some(ref tp) = input.transcript_path {
        let cwd_str = cwd.to_string_lossy().to_string();
        crate::summarizer::spawn_detached_story_generation(tp, &cwd_str);
    }

    // TAD: Update session outcome for all retrieval events in this session.
    // For Ralph sessions, use the determined outcome; otherwise default to "neutral".
    if let Some(ref session_id) = input.session_id {
        let tad_outcome = ralph
            .map(|r| {
                let reason = input.reason.as_deref().unwrap_or("unknown");
                match r.determine_outcome(reason) {
                    Outcome::Completed => "success",
                    Outcome::Abandoned | Outcome::Incomplete => "failed",
                }
            })
            .unwrap_or("neutral");
        let _ = engine
            .storage()
            .update_session_outcome(session_id, tad_outcome);
    }

    // Ralph-specific: generate and store session narrative
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

/// Run V3 extraction inline at session-end.
/// Produces a rich search index (user's words, edit patterns, error recovery, AST context)
/// and stores it as a reflection that supersedes the Layer 1 heuristic.
async fn run_v3_extraction(engine: &Engine, transcript_path: &Path, cwd: &Path) -> Result<()> {
    let cwd_str = cwd.to_string_lossy();
    let project = resolve_project_from_cwd(&cwd_str).unwrap_or_else(|| "unknown".to_string());
    let conv_id = transcript_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // Skip if already V3-extracted
    if engine
        .storage()
        .is_conversation_enriched(&conv_id, "extracted_v3")
        .unwrap_or(false)
    {
        return Ok(());
    }

    let messages = crate::import::parse_jsonl_messages(transcript_path)?;
    if messages.len() < 3 {
        return Ok(()); // skip trivial sessions
    }

    let result = crate::extraction::extract_v3(&messages);
    if result.search_index.trim().is_empty() {
        return Ok(());
    }

    // Store search index as reflection (supersedes heuristic)
    let reflection_id = format!("v3_{}", conv_id);
    let tags = vec![
        "narrative_v3".to_string(),
        format!("conv_{}", conv_id),
        format!("project_{}", project),
    ];

    let emb = engine.embeddings().clone();
    let search_idx = result.search_index.clone();
    let embedding = tokio::task::spawn_blocking(move || emb.embed(&[search_idx.as_str()]))
        .await??;

    if let Some(vec) = embedding.into_iter().next() {
        // Supersede heuristic: delete old Layer 1 reflection if it exists
        if let Ok(Some(old_id)) =
            engine.storage().get_enrichment_reflection_id(&conv_id, "heuristic")
        {
            let _ = engine.storage().delete_reflection(&old_id);
            // Remove from search index too
            // (insert_reflection with same slot handles dedup in HNSW)
        }

        engine.storage().insert_reflection(
            &reflection_id,
            &result.search_index,
            &tags,
            &vec,
        )?;
        let mut idx = engine.search().write().await;
        idx.insert_reflection(reflection_id.clone(), vec);
        engine.storage().mark_enrichment_completed(
            &conv_id,
            "extracted_v3",
            &reflection_id,
        )?;

        eprintln!(
            "CSR: V3 search index stored ({} tokens, {} patterns, {} errors)",
            result.stats.search_index_tokens,
            result.stats.patterns_found,
            result.stats.errors_found,
        );
    }

    Ok(())
}

