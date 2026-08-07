//! Haiku-curated session stories via Claude CLI headless mode.
//!
//! Flow: SQLite retrieves context → Haiku formats it into a 2-3 sentence story → stored as reflection.
//! Graceful degradation: returns None if claude CLI unavailable or times out.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::engine::Engine;
use crate::search::cross_project::resolve_project_from_cwd;
use crate::search::SearchEngine;
use crate::storage::Storage;

/// Context assembled from DB for Haiku's prompt.
pub struct StoryContext {
    pub current_session_excerpt: String,
    pub current_v3_enrichment: Option<String>,
    pub related_sessions: Vec<RelatedSession>,
    pub project_name: String,
}

pub struct RelatedSession {
    pub summary: String,
    pub age_label: String,
    pub score: f32,
}

const HAIKU_TIMEOUT: Duration = Duration::from_secs(30);
const TRANSCRIPT_SAMPLE_MSGS: usize = 20;
const TRANSCRIPT_CHAR_CAP: usize = 30_000;

/// Call `claude -p` (model chain: env override → haiku alias → CLI default)
/// to generate a curated session story.
/// Returns None if narratives are disabled, claude CLI is unavailable, times out, or fails.
pub async fn generate_session_story(
    context: &StoryContext,
) -> Option<(String, crate::narrative::ParsedNarrative)> {
    if crate::narrative::narratives_disabled() {
        return None;
    }
    let prompt = build_prompt(context);
    let parsed = call_claude_headless(&prompt).await?;
    Some((parsed.text.clone(), parsed))
}

/// Build the Haiku prompt from assembled DB context.
fn build_prompt(ctx: &StoryContext) -> String {
    let mut prompt = String::with_capacity(4096);

    prompt.push_str(
        "You are summarizing a coding session for future context restoration after memory compaction.\n\n",
    );

    // Current session excerpt
    if !ctx.current_session_excerpt.is_empty() {
        prompt.push_str("CURRENT SESSION TRANSCRIPT (excerpt):\n");
        prompt.push_str(&ctx.current_session_excerpt);
        prompt.push_str("\n\n");
    }

    // V3 enrichment (structured analysis)
    if let Some(ref v3) = ctx.current_v3_enrichment {
        prompt.push_str("STRUCTURED ANALYSIS OF CURRENT SESSION:\n");
        prompt.push_str(v3);
        prompt.push_str("\n\n");
    }

    // Related past sessions
    if !ctx.related_sessions.is_empty() {
        prompt.push_str("RELATED PAST SESSIONS:\n");
        for rel in &ctx.related_sessions {
            prompt.push_str(&format!(
                "- [{}] (relevance: {:.0}%) {}\n",
                rel.age_label,
                rel.score * 100.0,
                rel.summary
            ));
        }
        prompt.push('\n');
    }

    prompt.push_str(
        "Write 2-3 sentences: what was being worked on, key files/tools used, current state \
         (done/in-progress/blocked), and how it relates to recent past work if applicable. \
         Be specific about technical details. No preamble, no markdown headers.",
    );

    prompt
}

/// Invoke `claude -p` (model chain: env override → haiku alias → CLI default)
/// with prompt piped via stdin (avoids OS arg length limits).
async fn call_claude_headless(prompt: &str) -> Option<crate::narrative::ParsedNarrative> {
    use tokio::io::AsyncWriteExt;

    for candidate in crate::narrative::model_candidates() {
        let attempt = tokio::time::timeout(HAIKU_TIMEOUT, async {
            let mut cmd = tokio::process::Command::new("claude");
            // Nested sessions inherit the user's hook config — without this guard
            // every narrative call fires all 6 CSR hooks and its Stop hook stores
            // the analyst transcript as a session_episode (meta-episode pollution).
            cmd.env("CSR_DISABLE_RECURSIVE_HOOKS", "1");
            if let Some(model) = &candidate {
                cmd.args(["--model", model]);
            }
            let mut child = match cmd
                .args(["-p", "-", "--output-format", "json"])
                .stdout(Stdio::piped())
                // Piped intentionally — required for model-not-found detection on the failure path; do not revert to null().
                .stderr(Stdio::piped())
                .stdin(Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => return None, // claude CLI not found — no point walking the chain
            };

            // Write prompt to stdin then close it
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(prompt.as_bytes()).await;
                drop(stdin);
            }

            child.wait_with_output().await.ok()
        })
        .await;

        match attempt {
            Ok(Some(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                match crate::narrative::classify_attempt(output.status.success(), &stdout, &stderr)
                {
                    crate::narrative::AttemptOutcome::Parsed(mut parsed) => {
                        // Cap at 1000 chars — stories should be concise
                        parsed.text = parsed.text.chars().take(1000).collect();
                        return Some(parsed);
                    }
                    crate::narrative::AttemptOutcome::ModelNotFound => continue,
                    crate::narrative::AttemptOutcome::Failed(_) => return None,
                }
            }
            _ => return None, // timeout or spawn failure
        }
    }
    None
}

/// Sample a transcript: first N + last N messages, capped at char_cap.
pub fn sample_transcript(messages: &[serde_json::Value], n: usize, char_cap: usize) -> String {
    let total = messages.len();
    let gap_marker = serde_json::json!({"gap": format!("... {} messages omitted ...", total.saturating_sub(n * 2))});
    let selected: Vec<&serde_json::Value> = if total <= n * 2 {
        messages.iter().collect()
    } else {
        let mut s: Vec<&serde_json::Value> = messages[..n].iter().collect();
        s.push(&gap_marker);
        s.extend(messages[total - n..].iter());
        s
    };

    let mut result = String::new();
    for msg in &selected {
        let data = crate::extraction::get_message_data(msg);
        let role = data
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("system");
        let content = crate::extraction::content_to_lower(&data);
        // Take first 500 chars per message to keep prompt manageable
        let preview: String = content.chars().take(500).collect();
        result.push_str(&format!("[{}]: {}\n", role, preview));

        if result.len() >= char_cap {
            result.truncate(result.floor_char_boundary(char_cap));
            result.push_str("\n... (truncated)");
            break;
        }
    }
    result
}

/// Build story context by retrieving data from the engine's DB + search index.
pub async fn build_story_context(
    engine: &Engine,
    cwd: &str,
    messages: &[serde_json::Value],
) -> StoryContext {
    let project_name = resolve_project_from_cwd(cwd).unwrap_or_else(|| "unknown".to_string());

    // 1. Sample transcript
    let excerpt = sample_transcript(messages, TRANSCRIPT_SAMPLE_MSGS, TRANSCRIPT_CHAR_CAP);

    // 2. Get V3 enrichment for this project's most recent conversation
    let v3 = get_latest_v3_enrichment(engine.storage(), &project_name);

    // 3. HNSW search for related conversations
    let related = find_related_sessions(
        engine.embeddings(),
        engine.search(),
        engine.storage(),
        &excerpt,
    )
    .await;

    StoryContext {
        current_session_excerpt: excerpt,
        current_v3_enrichment: v3,
        related_sessions: related,
        project_name,
    }
}

/// Get the latest V3 enrichment reflection for this project.
fn get_latest_v3_enrichment(storage: &Arc<Storage>, project: &str) -> Option<String> {
    let tag = format!("project_{}", project);
    let reflections = storage.get_reflections_by_tag(&tag, 5).ok()?;
    reflections
        .into_iter()
        .find(|(_, _, tags, _)| tags.iter().any(|t| t.starts_with("narrative_")))
        .map(|(_, content, _, _)| {
            // Cap at 1500 chars for prompt budget
            content.chars().take(1500).collect()
        })
}

/// HNSW search for related sessions using the transcript excerpt as query.
async fn find_related_sessions(
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    storage: &Arc<Storage>,
    excerpt: &str,
) -> Vec<RelatedSession> {
    // Use first 500 chars as query text
    let query_text: String = excerpt.chars().take(500).collect();
    if query_text.len() < 20 {
        return vec![];
    }

    let emb = embeddings.clone();
    let q = query_text.clone();
    let embed_result = tokio::task::spawn_blocking(move || emb.embed(&[q.as_str()])).await;

    let vec = match embed_result {
        Ok(Ok(vecs)) => match vecs.into_iter().next() {
            Some(v) => v,
            None => return vec![],
        },
        _ => return vec![],
    };

    let idx = search.read().await;
    let results = idx.search_reflections(&vec, 3, 0.4);
    drop(idx);

    let now = chrono::Utc::now();
    let mut related = Vec::new();
    for r in results {
        if let Ok(Some((content, _tags, timestamp))) = storage.get_reflection_by_id(&r.id) {
            let age = crate::temporal::format_relative_time(
                &crate::temporal::parse_timestamp(&timestamp).unwrap_or(now),
            );
            related.push(RelatedSession {
                summary: content.chars().take(300).collect(),
                age_label: age,
                score: r.score,
            });
        }
    }
    related
}

/// Maximum time to wait for the story lock before giving up.
const STORY_LOCK_TIMEOUT: Duration = Duration::from_secs(90);

/// CLI entrypoint for `csr-engine generate-story`. Called as a detached process from SessionEnd.
/// Reads transcript, builds context, calls Haiku, stores reflection, exits.
///
/// Uses an advisory file lock to serialize concurrent Haiku calls — when multiple sessions end
/// simultaneously, processes queue up instead of competing for the `claude` CLI.
pub async fn generate_story_cli(
    engine: &Engine,
    transcript: &std::path::Path,
    cwd: &str,
) -> anyhow::Result<()> {
    let conv_id = transcript
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let project = resolve_project_from_cwd(cwd).unwrap_or_else(|| "unknown".to_string());

    if !transcript.exists() {
        log_story_event(&project, &conv_id, "skip:file_missing");
        return Ok(());
    }

    let messages = crate::import::parse_jsonl_messages_for_search(transcript)?;
    if messages.len() < 5 {
        log_story_event(
            &project,
            &conv_id,
            &format!("skip:too_few_messages({})", messages.len()),
        );
        return Ok(());
    }

    // Skip if this conversation already has a story (idempotent — handles retries and races)
    if engine
        .storage()
        .is_conversation_enriched(&conv_id, "session_story")
        .unwrap_or(false)
    {
        log_story_event(&project, &conv_id, "skip:story_exists");
        return Ok(());
    }

    // Acquire advisory lock — serializes concurrent Haiku calls.
    // Blocks until the lock is available or timeout expires.
    let lock_path = dirs::home_dir()
        .unwrap_or_default()
        .join(".claude-self-reflect")
        .join("story-generation.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)?;

    let acquired = acquire_lock_with_timeout(&lock_file, STORY_LOCK_TIMEOUT);
    if !acquired {
        log_story_event(&project, &conv_id, "skip:lock_timeout(90s)");
        return Ok(());
    }
    log_story_event(&project, &conv_id, "lock_acquired");

    // Lock held — generate story (Haiku call happens here)
    let result = generate_and_store(engine, transcript, cwd, &conv_id, &project, &messages).await;

    // Release lock (drop closes the file, releasing the advisory lock)
    drop(lock_file);

    result
}

/// Generate story and store it. Separated so the lock scope is clear.
async fn generate_and_store(
    engine: &Engine,
    _transcript: &std::path::Path,
    cwd: &str,
    conv_id: &str,
    project: &str,
    messages: &[serde_json::Value],
) -> anyhow::Result<()> {
    let ctx = build_story_context(engine, cwd, messages).await;

    let story = match generate_session_story(&ctx).await {
        Some((text, parsed)) => {
            let _ = engine
                .storage()
                .record_narrative_usage(&crate::storage::NarrativeUsageRow {
                    call_site: "story".into(),
                    model: parsed.model.clone(),
                    input_tokens: parsed.input_tokens,
                    output_tokens: parsed.output_tokens,
                    cache_read_tokens: parsed.cache_read_tokens,
                    cache_creation_tokens: parsed.cache_creation_tokens,
                    duration_ms: 0, // HAIKU_TIMEOUT bounds it; wall-clock not tracked on this path
                    success: true,
                });
            text
        }
        None => {
            if crate::narrative::narratives_disabled() {
                log_story_event(project, conv_id, "skip:narratives_disabled");
            } else {
                let _ =
                    engine
                        .storage()
                        .record_narrative_usage(&crate::storage::NarrativeUsageRow {
                            call_site: "story".into(),
                            model: "unknown".into(),
                            input_tokens: 0,
                            output_tokens: 0,
                            cache_read_tokens: 0,
                            cache_creation_tokens: 0,
                            duration_ms: 0,
                            success: false,
                        });
                log_story_event(project, conv_id, "skip:haiku_unavailable_or_timeout");
            }
            return Ok(());
        }
    };

    let reflection_id = format!("story_{}", conv_id);
    let tags = vec![
        "session_story".to_string(),
        format!("project_{}", ctx.project_name),
        format!("conv_{}", conv_id),
    ];

    crate::mcp::tools::store_reflection(
        engine.storage(),
        engine.embeddings(),
        engine.search(),
        &story,
        &tags,
    )
    .await?;

    // Mark as completed so retries and races skip this conversation
    let _ = engine
        .storage()
        .mark_enrichment_completed(conv_id, "session_story", &reflection_id);

    log_story_event(project, conv_id, &format!("stored:{}chars", story.len()));
    eprintln!("CSR: stored session story ({} chars)", story.len());
    Ok(())
}

/// Try to acquire an exclusive file lock, polling with backoff up to `timeout`.
/// Returns true if acquired, false if timed out.
fn acquire_lock_with_timeout(file: &std::fs::File, timeout: Duration) -> bool {
    use fs2::FileExt;
    let start = std::time::Instant::now();
    let mut sleep_ms = 100u64; // start at 100ms, backoff to 1s

    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return true,
            Err(_) => {
                if start.elapsed() >= timeout {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(sleep_ms));
                sleep_ms = (sleep_ms * 2).min(1000); // exponential backoff, cap at 1s
            }
        }
    }
}

/// Spawn a detached `csr-engine generate-story` process. Returns immediately.
/// The child process outlives the caller — safe for SessionEnd.
pub fn spawn_detached_story_generation(transcript: &str, cwd: &str) {
    if crate::narrative::narratives_disabled() {
        return;
    }
    let project = resolve_project_from_cwd(cwd).unwrap_or_else(|| "unknown".to_string());
    let conv_id = std::path::Path::new(transcript)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("CSR: cannot find own exe for story generation: {}", e);
            log_story_event(&project, &conv_id, &format!("error:no_exe({})", e));
            return;
        }
    };

    match std::process::Command::new(exe)
        .args(["generate-story", "--transcript", transcript, "--cwd", cwd])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit()) // keep stderr for debug logs
        .spawn()
    {
        Ok(_) => {
            log_story_event(&project, &conv_id, "spawned");
            eprintln!("CSR: spawned background story generation");
        }
        Err(e) => {
            log_story_event(&project, &conv_id, &format!("error:spawn_failed({})", e));
            eprintln!("CSR: failed to spawn story generation: {}", e);
        }
    }
}

/// Backfill stories for all conversations that have V3/heuristic enrichment but no story.
/// Tier 1: V3 → synthesize locally (free). Tier 2: Heuristic → template (free).
pub async fn backfill_stories_cli(engine: &Engine, dry_run: bool) -> anyhow::Result<()> {
    let candidates = engine
        .storage()
        .get_conversations_missing_stories(crate::hooks::session_start::MIN_ENRICHED_MESSAGES)?;
    eprintln!("CSR: {} conversations missing stories", candidates.len());

    let mut tier1 = 0usize;
    let mut tier2 = 0usize;

    for (conv_id, enrichment_type, reflection_id) in &candidates {
        // Get the enrichment content
        let content = match engine.storage().get_reflection_by_id(reflection_id)? {
            Some((content, _, _)) => content,
            None => continue, // phantom reference
        };

        let project = engine
            .storage()
            .get_project_for_conversation(conv_id)?
            .unwrap_or_else(|| "unknown".to_string());

        let story = if enrichment_type == "extracted_v3" {
            tier1 += 1;
            crate::extraction::story::synthesize_story_from_v3(&content, &project)
        } else {
            tier2 += 1;
            Some(crate::extraction::story::synthesize_story_from_heuristic(
                &content, &project,
            ))
        };

        if let Some(story_text) = story {
            if !dry_run {
                let story_id = format!("story_{}", conv_id);
                let tags = vec![
                    "session_story".to_string(),
                    format!("project_{}", project),
                    format!("conv_{}", conv_id),
                ];
                crate::mcp::tools::store_reflection(
                    engine.storage(),
                    engine.embeddings(),
                    engine.search(),
                    &story_text,
                    &tags,
                )
                .await?;
                engine
                    .storage()
                    .mark_enrichment_completed(conv_id, "session_story", &story_id)?;
            }
            let cid_short = if conv_id.len() > 8 {
                &conv_id[..8]
            } else {
                conv_id
            };
            eprintln!(
                "  [{}] {} ({}chars)",
                if enrichment_type == "extracted_v3" {
                    "V3"
                } else {
                    "heuristic"
                },
                cid_short,
                story_text.len()
            );
        }
    }

    eprintln!(
        "CSR backfill: {} V3-synthesized, {} heuristic-templated{}",
        tier1,
        tier2,
        if dry_run { " (dry run)" } else { "" }
    );
    Ok(())
}

/// Log story generation events to hook-timing.log for diagnostics.
fn log_story_event(project: &str, conv_id: &str, event: &str) {
    let cid_short = if conv_id.len() > 8 {
        &conv_id[..8]
    } else {
        conv_id
    };
    crate::telemetry::append_timing_line(&format!(
        "CSR story [{}] conv={}: {}",
        project, cid_short, event
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_prompt_with_full_context() {
        let ctx = StoryContext {
            current_session_excerpt: "user: fix the bug\nassistant: done".to_string(),
            current_v3_enrichment: Some("Tools: Edit | Files: main.rs".to_string()),
            related_sessions: vec![RelatedSession {
                summary: "Fixed similar bug in auth module".to_string(),
                age_label: "2d ago".to_string(),
                score: 0.72,
            }],
            project_name: "my-project".to_string(),
        };
        let prompt = build_prompt(&ctx);
        assert!(prompt.contains("CURRENT SESSION TRANSCRIPT"));
        assert!(prompt.contains("fix the bug"));
        assert!(prompt.contains("STRUCTURED ANALYSIS"));
        assert!(prompt.contains("Tools: Edit"));
        assert!(prompt.contains("RELATED PAST SESSIONS"));
        assert!(prompt.contains("72%"));
        assert!(prompt.contains("2-3 sentences"));
    }

    #[test]
    fn test_build_prompt_minimal() {
        let ctx = StoryContext {
            current_session_excerpt: "user: hello".to_string(),
            current_v3_enrichment: None,
            related_sessions: vec![],
            project_name: "test".to_string(),
        };
        let prompt = build_prompt(&ctx);
        assert!(prompt.contains("hello"));
        assert!(!prompt.contains("STRUCTURED ANALYSIS"));
        assert!(!prompt.contains("RELATED PAST SESSIONS"));
    }

    #[test]
    fn test_sample_transcript_small() {
        let messages: Vec<serde_json::Value> = (0..5)
            .map(|i| {
                serde_json::json!({
                    "type": "user",
                    "message": {"role": "user", "content": format!("message {}", i)}
                })
            })
            .collect();
        let result = sample_transcript(&messages, 20, 50_000);
        // All 5 messages should be included (< 2*20)
        assert!(result.contains("message 0"));
        assert!(result.contains("message 4"));
    }

    #[test]
    fn test_sample_transcript_large_samples_ends() {
        let messages: Vec<serde_json::Value> = (0..100)
            .map(|i| {
                serde_json::json!({
                    "type": "user",
                    "message": {"role": "user", "content": format!("msg-{}", i)}
                })
            })
            .collect();
        let result = sample_transcript(&messages, 5, 50_000);
        // First 5 and last 5 should be present
        assert!(result.contains("msg-0"));
        assert!(result.contains("msg-4"));
        assert!(result.contains("msg-95"));
        assert!(result.contains("msg-99"));
        // Middle should be omitted — gap marker appears as [system] since it lacks role/content
        assert!(!result.contains("msg-50"));
        // 11 entries: 5 first + 1 gap + 5 last
        let line_count = result.lines().count();
        assert_eq!(
            line_count, 11,
            "expected 11 lines (5+1+5), got {}",
            line_count
        );
    }

    #[test]
    fn test_sample_transcript_respects_char_cap() {
        let messages: Vec<serde_json::Value> = (0..100)
            .map(|i| {
                serde_json::json!({
                    "type": "user",
                    "message": {"role": "user", "content": format!("message-{}-{}", i, "x".repeat(200))}
                })
            })
            .collect();
        let result = sample_transcript(&messages, 20, 500);
        assert!(result.len() <= 520); // cap + "... (truncated)"
    }
}
