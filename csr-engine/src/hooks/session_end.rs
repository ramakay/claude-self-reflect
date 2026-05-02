//! SessionEnd hook — imports current conversation and generates session narrative.
//!
//! For ALL sessions:
//! 1. Imports the current conversation file (real-time indexing)
//! 2. Runs V3 extraction for searchable index
//! 3. Synthesizes a V3 story (zero-cost) or falls back to Haiku
//! 4. Updates TAD outcome to "success" (normal session end)

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::HookInput;
use crate::engine::Engine;
use crate::mcp::tools;
use crate::search::cross_project::resolve_project_from_cwd;

/// Handle the session-end hook.
pub async fn handle(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    // 1. Final transcript import (shared helper, incremental)
    super::import_current_transcript(input, engine, cwd).await;

    // 2. V3 extraction — produces a genuinely searchable index
    if let Some(ref tp) = input.transcript_path {
        let path = PathBuf::from(tp);
        if path.exists() {
            if let Err(e) = run_v3_extraction(engine, &path, cwd).await {
                eprintln!("CSR: V3 extraction failed (non-fatal): {}", e);
            }
        }
    }

    // 3. Try local V3 story synthesis BEFORE spawning Haiku (free, instant)
    //    Falls back to detached Haiku generation if local synthesis fails.
    if let Some(ref tp) = input.transcript_path {
        let conv_id = std::path::Path::new(tp)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let project = resolve_project_from_cwd(&cwd.to_string_lossy())
            .unwrap_or_else(|| "unknown".to_string());

        let has_story = engine
            .storage()
            .is_conversation_enriched(&conv_id, "session_story")
            .unwrap_or(false);

        if !has_story {
            // Try V3 synthesis first (zero-cost, instant)
            let synthesized = try_v3_story_synthesis(engine, &conv_id, &project).await;

            if !synthesized {
                // Fallback: spawn detached Haiku story generation
                let cwd_str = cwd.to_string_lossy().to_string();
                crate::summarizer::spawn_detached_story_generation(tp, &cwd_str);
            }
        }
    }

    // 4. Write last-session summary for SwiftBar status plugin
    write_session_summary(engine, input, cwd);

    // 5. TAD: Update session outcome — normal session end = "success"
    if let Some(ref session_id) = input.session_id {
        let _ = engine
            .storage()
            .update_session_outcome(session_id, "success");
    }

    Ok(())
}

/// Try to synthesize a story locally from existing V3 extraction data.
/// Returns true if a story was successfully synthesized and stored.
async fn try_v3_story_synthesis(engine: &Engine, conv_id: &str, project: &str) -> bool {
    // Check if V3 extraction exists for this conversation
    let ref_id = match engine
        .storage()
        .get_enrichment_reflection_id(conv_id, "extracted_v3")
    {
        Ok(Some(id)) => id,
        _ => return false,
    };

    let v3_content = match engine.storage().get_reflection_by_id(&ref_id) {
        Ok(Some((content, _, _))) => content,
        _ => return false,
    };

    let story = match crate::extraction::story::synthesize_story_from_v3(&v3_content, project) {
        Some(s) => s,
        None => return false,
    };

    let story_id = format!("story_{}", conv_id);
    let tags = vec![
        "session_story".to_string(),
        format!("project_{}", project),
        format!("conv_{}", conv_id),
    ];

    if let Err(e) = tools::store_reflection(
        engine.storage(),
        engine.embeddings(),
        engine.search(),
        &story,
        &tags,
    )
    .await
    {
        eprintln!("CSR: V3 story store failed (non-fatal): {}", e);
        return false;
    }

    let _ = engine
        .storage()
        .mark_enrichment_completed(conv_id, "session_story", &story_id);
    eprintln!(
        "CSR: V3 story synthesized locally ({}chars), skipping Haiku",
        story.len()
    );
    true
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
    let embedding =
        tokio::task::spawn_blocking(move || emb.embed(&[search_idx.as_str()])).await??;

    if let Some(vec) = embedding.into_iter().next() {
        // Supersede heuristic: delete old Layer 1 reflection if it exists
        if let Ok(Some(old_id)) = engine
            .storage()
            .get_enrichment_reflection_id(&conv_id, "heuristic")
        {
            let _ = engine.storage().delete_reflection(&old_id);
            // Remove from search index too
            // (insert_reflection with same slot handles dedup in HNSW)
        }

        engine
            .storage()
            .insert_reflection(&reflection_id, &result.search_index, &tags, &vec)?;
        let mut idx = engine.search().write().await;
        idx.insert_reflection(reflection_id.clone(), vec);
        engine
            .storage()
            .mark_enrichment_completed(&conv_id, "extracted_v3", &reflection_id)?;

        eprintln!(
            "CSR: V3 search index stored ({} tokens, {} patterns, {} errors)",
            result.stats.search_index_tokens,
            result.stats.patterns_found,
            result.stats.errors_found,
        );
    }

    Ok(())
}

/// Write a brief session summary for the SwiftBar status plugin.
/// Extracts the first user message + enrichment info as a 2-line summary.
fn write_session_summary(engine: &Engine, input: &HookInput, cwd: &Path) {
    let project =
        resolve_project_from_cwd(&cwd.to_string_lossy()).unwrap_or_else(|| "unknown".to_string());

    // Try to get a summary from the transcript's first user message
    let mut summary = String::new();
    if let Some(ref tp) = input.transcript_path {
        let path = PathBuf::from(tp);
        if path.exists() {
            if let Ok(messages) = crate::import::parse_jsonl_messages(&path) {
                // Find the first substantial user message
                let first_user = messages.iter().find_map(|m| {
                    let data = crate::extraction::get_message_data(m);
                    let role = data.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    if role == "user" {
                        let content = crate::extraction::content_to_lower(&data);
                        if content.len() > 15 {
                            Some(content.chars().take(120).collect::<String>())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                });
                if let Some(msg) = first_user {
                    summary = format!("[{}] {}", project, msg);
                }
            }
        }
    }

    // Try enrichment for richer context
    if let Some(ref tp) = input.transcript_path {
        let conv_id = std::path::Path::new(tp)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if let Ok(Some(ref_id)) = engine
            .storage()
            .get_enrichment_reflection_id(&conv_id, "extracted_v3")
        {
            if let Ok(Some((content, _, _))) = engine.storage().get_reflection_by_id(&ref_id) {
                // Extract Search Summary or User Request from V3
                for header in &["## Search Summary", "## User Request"] {
                    if let Some(pos) = content.find(header) {
                        let after = &content[pos + header.len()..];
                        let para: String = after
                            .lines()
                            .skip(1)
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or("")
                            .trim()
                            .trim_matches('"')
                            .chars()
                            .take(200)
                            .collect();
                        if para.len() > 20 {
                            summary = format!("[{}] {}", project, para);
                            break;
                        }
                    }
                }
            }
        }
    }

    if summary.is_empty() {
        summary = format!("[{}] Session completed", project);
    }

    if let Some(home) = dirs::home_dir() {
        let path = home
            .join(".claude-self-reflect")
            .join("last-session-summary.txt");
        let _ = std::fs::write(&path, &summary);
    }
}
