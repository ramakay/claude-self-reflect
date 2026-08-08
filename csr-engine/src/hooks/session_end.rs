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

        // Micro sessions (single prompt/reply, e.g. a manual `claude -p` test
        // invocation) get no story: a story would resurface them as "past sessions"
        // at SessionStart, injecting the test harness back into real sessions.
        let message_count = engine
            .storage()
            .conversation_message_count(&conv_id)
            .unwrap_or(0);

        // Meta sessions (probe runs, command-only invocations) get no story
        // either — no user message survives provenance filtering, so there is
        // no real work to narrate, only CSR examining itself.
        let is_meta_session = crate::import::parse_jsonl_messages(Path::new(tp))
            .map(|msgs| !has_real_user_request(&msgs))
            .unwrap_or(false);

        if !has_story
            && !is_meta_session
            && message_count >= super::session_start::MIN_ENRICHED_MESSAGES
        {
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
    //    Then compute retrieval stats rollup for outcome-scored injection (v9).
    if let Some(ref session_id) = input.session_id {
        let _ = engine
            .storage()
            .update_session_outcome(session_id, "success");
        let _ = engine
            .storage()
            .update_retrieval_stats_for_session(session_id);
    }

    // 6. Ratification re-enqueue (non-fatal): delete enrichment_state so the
    //    daemon re-scores this conversation on its next poll. Enqueue-only —
    //    never call an LLM from the hook.
    if !crate::daemon::ratification::check_disabled() {
        if let Some(ref tp) = input.transcript_path {
            let reenqueue = (|| -> Result<()> {
                let conv_id = std::path::Path::new(tp)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let chunk_ids = engine.storage().get_chunk_ids_for_conversation(&conv_id)?;
                if chunk_ids.is_empty() {
                    return Ok(());
                }
                engine
                    .storage()
                    .reset_enrichment(&conv_id, "ratification")?;
                Ok(())
            })();
            if let Err(e) = reenqueue {
                tracing::debug!("CSR: ratification re-enqueue failed (non-fatal): {e}");
            }
        }
    }

    Ok(())
}

/// True if any user message carries a genuine request after provenance
/// filtering. Probe runs and command-only sessions have none — every user
/// message is command plumbing or CSR's own emitted format quoted back.
fn has_real_user_request(messages: &[serde_json::Value]) -> bool {
    messages.iter().any(|m| {
        // Match on the outer transcript `type`: Claude Code writes "user"
        // (get_message_data's role injection only maps the legacy "human").
        let msg_type = m.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if msg_type != "user" && msg_type != "human" {
            return false;
        }
        let data = crate::extraction::get_message_data(m);
        let text = crate::hooks::stop::extract_text_from_content(
            data.get("content").unwrap_or(&serde_json::Value::Null),
        );
        crate::extraction::provenance::extractable(&text).is_some()
    })
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

    // Store with explicit story_id so enrichment link is correct (Codex M-5)
    let story_id = format!("story_{}", conv_id);
    let tags = vec![
        "session_story".to_string(),
        format!("project_{}", project),
        format!("conv_{}", conv_id),
    ];

    let emb = engine.embeddings().clone();
    let story_for_embed = story.clone();
    let embedding =
        match tokio::task::spawn_blocking(move || emb.embed_single(&story_for_embed)).await {
            Ok(Ok(v)) => v,
            _ => {
                eprintln!("CSR: V3 story embed failed (non-fatal)");
                return false;
            }
        };

    if let Err(e) = engine
        .storage()
        .insert_reflection(&story_id, &story, &tags, &embedding)
    {
        eprintln!("CSR: V3 story store failed (non-fatal): {}", e);
        return false;
    }
    {
        let mut idx = engine.search().write().await;
        idx.insert_reflection(story_id.clone(), embedding);
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

    let messages = crate::import::parse_jsonl_messages_for_search(transcript_path)?;
    if messages.len() < 3 {
        return Ok(()); // skip trivial sessions
    }

    let result = crate::extraction::extract_v3(&messages);
    if result.search_index.trim().is_empty() {
        return Ok(());
    }

    // Store rich V3 content: search_index + signature + context_cache
    // (same format as daemon, so story synthesis can see Signature for outcome extraction)
    let reflection_id = format!("v3_{}", conv_id);
    let tags = vec![
        "narrative_v3".to_string(),
        format!("conv_{}", conv_id),
        format!("project_{}", project),
    ];

    let sig_json = serde_json::to_string(&result.signature).unwrap_or_default();
    let rich_content = format!(
        "{}\n\n---\nSignature: {}\nContext:\n{}",
        result.search_index, sig_json, result.context_cache
    );

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
            // Remove from HNSW search index too (Codex M-6)
            let mut idx = engine.search().write().await;
            idx.remove_reflection(&old_id);
        }

        engine
            .storage()
            .insert_reflection(&reflection_id, &rich_content, &tags, &vec)?;
        {
            let mut idx = engine.search().write().await;
            idx.insert_reflection(reflection_id.clone(), vec);
        } // Write lock released before context_cache embed
        engine
            .storage()
            .mark_enrichment_completed(&conv_id, "extracted_v3", &reflection_id)?;

        eprintln!(
            "CSR: V3 search index stored ({} tokens, {} patterns, {} errors)",
            result.stats.search_index_tokens,
            result.stats.patterns_found,
            result.stats.errors_found,
        );

        // Persist context_cache as linked reflection for error recovery retrieval.
        // This is CSR's competitive edge: debugging solutions become searchable.
        if !result.context_cache.trim().is_empty() {
            let cache_id = format!("v3_cache_{}", conv_id);
            let cache_tags = vec![
                "context_cache".to_string(),
                "error_recovery".to_string(),
                format!("conv_{}", conv_id),
                format!("project_{}", project),
            ];
            let cache_emb = engine.embeddings().clone();
            let cache_text = result.context_cache.clone();
            if let Ok(Ok(cache_embedding)) =
                tokio::task::spawn_blocking(move || cache_emb.embed(&[cache_text.as_str()])).await
            {
                if let Some(cache_vec) = cache_embedding.into_iter().next() {
                    let _ = engine.storage().insert_reflection(
                        &cache_id,
                        &result.context_cache,
                        &cache_tags,
                        &cache_vec,
                    );
                    {
                        let mut idx = engine.search().write().await;
                        idx.insert_reflection(cache_id, cache_vec);
                    }
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(json: &str) -> serde_json::Value {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn test_probe_session_has_no_real_request() {
        // A /memory-feedback run: command plumbing + the probe prompt text.
        let messages = vec![
            msg(
                r#"{"type":"user","message":{"content":"<command-message>memory-feedback</command-message>\n<command-name>/memory-feedback</command-name>\nCSR Memory Feedback Probe\nYou are reporting on the quality of the memory context CSR injected"}}"#,
            ),
            msg(
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Report written."}]}}"#,
            ),
        ];
        assert!(!has_real_user_request(&messages));
    }

    #[test]
    fn test_real_session_has_request() {
        let messages = vec![
            msg(r#"{"type":"user","message":{"content":"fix the briefing staleness bug"}}"#),
            msg(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"On it."}]}}"#),
        ];
        assert!(has_real_user_request(&messages));
    }
}
