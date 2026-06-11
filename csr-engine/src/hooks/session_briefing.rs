//! Session briefing hook — shells out to `claude -p --model haiku` to generate
//! a proactive briefing from recent session episodes.
//!
//! Runs async at SessionStart (non-blocking). Output is stored as a tagged
//! reflection (`session_briefing` tag) that the next UserPromptSubmit hook
//! surfaces as predictive context — so the briefing arrives at the user's
//! first prompt of the session, not at SessionStart itself.
//!
//! This avoids the agent-hook restriction (agent hooks only work for
//! PreToolUse/PostToolUse/PermissionRequest events) by using a regular
//! command hook that internally invokes Haiku via the Claude CLI.
//!
//! Always returns Ok(()) — never blocks Claude Code (catch-all wrapper).

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Result;

use super::HookInput;
use crate::engine::Engine;
use crate::search::cross_project::resolve_project_from_cwd;

/// Max seconds to wait for `claude -p` to return a briefing.
/// Generous because: (a) claude -p base startup + Haiku reasoning take time,
/// (b) Haiku reasons over the injected episodes to synthesize the briefing,
/// (c) we run async so user never waits.
const BRIEFING_TIMEOUT_SECS: u64 = 120;

/// Skip generating a new briefing if one was produced for this project within
/// this many minutes. Prevents a fresh `claude -p` spawn on every resume/compact
/// when nothing has materially changed.
const BRIEFING_DEBOUNCE_MINUTES: i64 = 30;

/// Most recent episodes to feed Haiku. Newest-first; enough to spot cross-session
/// patterns without bloating the prompt.
const MAX_EPISODES_IN_BRIEFING: usize = 6;

/// Per-episode char cap when injecting into the prompt. Episodes are JSON; the
/// lead fields (request/investigated/completed/next_steps/outcome) dominate.
const MAX_EPISODE_CHARS: usize = 700;

/// Instruction prepended to the injected episodes. Haiku summarizes the episodes
/// it is GIVEN — it does NOT search for them. Searching the tag name semantically
/// returns past briefing runs (which contain the words "session_episode"), not
/// episodes, and self-reinforces a false "no episodes found" verdict. The Rust
/// hook loads real episodes by tag and embeds them below.
const BRIEFING_INSTRUCTION: &str = concat!(
    "You are CSR Episode Analyst. Below are the most recent structured session ",
    "episodes for THIS project, newest first, as JSON. Each has fields: request, ",
    "investigated, completed, next_steps, outcome, error_signatures, tools_used, ",
    "files_modified, todos, approved_plan, prev_episode_id, anchors.\n\n",
    "Write a concise briefing (under 150 words). Start with '## Session Intelligence (CSR v9.2)'.\n",
    "Reference specific file names, error messages, and outcomes. Look for:\n",
    "- Recent work and outcomes\n",
    "- Patterns (repeated errors, same files across sessions)\n",
    "- Unfinished work (next_steps, partial/interrupted outcomes)\n\n",
    "Only report what the episodes contain. Do not fabricate information.\n\n",
    "=== RECENT EPISODES ===\n"
);

/// Handle the session-briefing hook. Always returns Ok(()).
pub async fn handle(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    if let Err(e) = handle_inner(input, engine, cwd).await {
        eprintln!("CSR: session-briefing error (non-fatal): {}", e);
    }
    Ok(())
}

async fn handle_inner(_input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    let project = resolve_project_from_cwd(&cwd.to_string_lossy());
    let project_name = project.as_deref().unwrap_or("unknown");

    // Debounce: skip if a briefing for this project was generated recently. Avoids
    // spawning a fresh `claude -p` on every resume/compact within a working session.
    if recent_briefing_exists(engine, project_name, BRIEFING_DEBOUNCE_MINUTES) {
        tracing::debug!(
            project = project_name,
            "skipping briefing — generated recently"
        );
        return Ok(());
    }

    // Load recent structured episodes for THIS project directly by tag. Do NOT
    // ask Haiku to semantic-search the tag name — that returns past briefing runs
    // (their prompt text contains "session_episode"), not episodes, and loops on a
    // false "fresh start" verdict. Feed Haiku the real episodes instead.
    let episodes = recent_episodes_for_project(engine, project_name);
    if episodes.is_empty() {
        tracing::debug!(
            project = project_name,
            "no episodes for project — skipping briefing"
        );
        return Ok(());
    }

    let prompt = format!("{}{}", BRIEFING_INSTRUCTION, episodes);

    // Shell out to claude -p with Haiku model.
    // Block on tokio's spawn_blocking since std::process::Command is sync.
    let briefing = tokio::task::spawn_blocking(move || invoke_haiku_briefing(&prompt)).await??;

    if briefing.trim().is_empty() {
        eprintln!("CSR: session-briefing returned empty output");
        return Ok(());
    }

    // Store briefing as a tagged reflection — replace any prior briefing for this project
    store_briefing(engine, project_name, &briefing)?;

    Ok(())
}

/// Invoke `claude -p --model haiku-4-5 "<prompt>"` and capture stdout.
/// Returns the briefing text, or an error if the invocation fails or times out.
///
/// The episodes are embedded in `prompt`, so Haiku needs NO tools. We pass an
/// empty MCP config with `--strict-mcp-config` so the subprocess loads ZERO MCP
/// servers (not even csr-engine) — fastest possible `claude -p` startup and no
/// recursive csr-engine spawn.
fn invoke_haiku_briefing(prompt: &str) -> Result<String> {
    let mcp_config_path = write_minimal_mcp_config()?;

    let mut child = Command::new("claude")
        .arg("-p")
        // The prompt MUST precede --mcp-config: that flag is variadic in the
        // claude CLI and consumes any trailing positional arg as another config
        // file path, failing with ENAMETOOLONG on the episode text.
        .arg(prompt)
        .arg("--model")
        .arg("claude-haiku-4-5-20251001")
        .arg("--output-format")
        .arg("text")
        .arg("--strict-mcp-config")
        .arg("--mcp-config")
        .arg(&mcp_config_path)
        // No --dangerously-skip-permissions: episodes are session-derived text and
        // the empty MCP config means zero tools, so this is a pure text summary.
        // Skipping permissions would only widen the blast radius if an episode
        // contained adversarial content. Print mode won't prompt interactively.
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .env("CSR_DISABLE_RECURSIVE_HOOKS", "1") // signal to nested csr-engine to skip hooks
        .spawn()?;

    // Manual timeout: poll for completion up to BRIEFING_TIMEOUT_SECS.
    let timeout = Duration::from_secs(BRIEFING_TIMEOUT_SECS);
    let start = std::time::Instant::now();
    loop {
        match child.try_wait()? {
            Some(_status) => break,
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    anyhow::bail!("claude -p timed out after {}s", BRIEFING_TIMEOUT_SECS);
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("claude -p failed: {}", stderr);
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Store briefing as a tagged reflection. Replaces any previous briefing
/// for the same project (delete + insert).
fn store_briefing(engine: &Engine, project: &str, briefing: &str) -> Result<()> {
    let project_tag = format!("project_{}", project);
    let tags = vec![
        "session_briefing".to_string(),
        "schema_v1".to_string(),
        project_tag.clone(),
    ];

    // GC: delete existing session_briefing reflections for this project
    if let Ok(existing) = engine
        .storage()
        .get_reflections_by_tag("session_briefing", 50)
    {
        for (id, _, ref existing_tags, _) in &existing {
            if existing_tags.iter().any(|t| t == &project_tag) {
                let _ = engine.storage().delete_reflection(id);
            }
        }
    }

    // Embed and store
    let embedding = engine.embeddings().embed_single(briefing)?;
    let id = uuid::Uuid::new_v4().to_string();
    engine
        .storage()
        .insert_reflection(&id, briefing, &tags, &embedding)?;

    Ok(())
}

/// Write an EMPTY MCP config, used with `--strict-mcp-config` so the briefing
/// subprocess loads ZERO MCP servers. Episodes are injected into the prompt, so
/// Haiku needs no tools — this avoids loading any MCP server (including a recursive
/// csr-engine) and gives the fastest possible `claude -p` startup.
fn write_minimal_mcp_config() -> Result<std::path::PathBuf> {
    let config = serde_json::json!({ "mcpServers": {} });
    let dir = dirs::home_dir()
        .map(|h| h.join(".claude-self-reflect"))
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("briefing-mcp.json");
    std::fs::write(&path, serde_json::to_string(&config)?)?;
    Ok(path)
}

/// Load the most recent `session_episode` reflections for `project` and format
/// them (newest first, capped) for injection into the briefing prompt. Returns an
/// empty string if none exist — the caller skips the briefing entirely.
fn recent_episodes_for_project(engine: &Engine, project: &str) -> String {
    let project_tag = format!("project_{}", project);
    // Fetch by BOTH tags so the LIMIT applies after filtering — a busy project
    // can't be starved by its own non-episode reflections crowding a pre-filter
    // window. Over-fetch (2x) to absorb prefix-collision rows the exact match below
    // drops (LIKE can match project_foo inside project_foo-bar).
    // Over-fetch candidates so leftover meta-episodes (pre-cleanup) or rare
    // project substring-collisions in the newest slice can't crowd out the 6 real
    // episodes we want. The two-tag query already matches exact JSON elements; this
    // window is filtered down to MAX_EPISODES_IN_BRIEFING below.
    const CANDIDATE_WINDOW: usize = 48;
    let rows = match engine.storage().get_reflections_by_two_tags(
        &project_tag,
        "session_episode",
        CANDIDATE_WINDOW,
    ) {
        Ok(rows) => rows,
        Err(e) => {
            // Distinguish a real DB error from "no episodes" — both return empty,
            // but only this path is an operational problem worth surfacing.
            tracing::warn!(project, error = %e, "failed to load episodes for briefing");
            return String::new();
        }
    };

    let mut out = String::new();
    let mut n = 0;
    for (_, content, tags, _) in &rows {
        if n >= MAX_EPISODES_IN_BRIEFING {
            break;
        }
        // Exact project match (LIKE query can substring-match project_foo-bar).
        if !tags.iter().any(|t| t == &project_tag) {
            continue;
        }
        // Skip meta-episodes: transcripts of CSR's own agent subprocesses (the
        // briefing analyst, the compaction summarizer). Historically these leaked
        // into the episode store; the recursive-hook guard now prevents new ones,
        // but old rows remain — filter them so the briefing never summarizes itself.
        if is_meta_episode(content) {
            continue;
        }
        n += 1;
        if content.len() > MAX_EPISODE_CHARS {
            let end = content.floor_char_boundary(MAX_EPISODE_CHARS);
            // Mark the cut so Haiku treats it as an excerpt, not malformed JSON.
            out.push_str(&format!(
                "\n[Episode {}]\n{}…[truncated]\n",
                n,
                &content[..end]
            ));
        } else {
            out.push_str(&format!("\n[Episode {}]\n{}\n", n, content));
        }
    }
    out
}

/// True if an episode's content is CSR talking to itself (agent transcript,
/// pasted probe report, command-only session) rather than real user work.
/// Checks ONLY the `request` field (the first user message) so a real episode
/// that merely quotes CSR output elsewhere (e.g. in `completed`) is not
/// misclassified. Detection lives in `extraction::provenance` — the single
/// registry of CSR's emission formats and transcript plumbing.
fn is_meta_episode(content: &str) -> bool {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(req) = v.get("request").and_then(|r| r.as_str()) {
            // No genuine user request survives provenance filtering → meta.
            return crate::extraction::provenance::extractable(req).is_none();
        }
    }
    // Fallback for non-JSON content: scan only the leading window.
    let head_end = content.floor_char_boundary(400);
    crate::extraction::provenance::is_csr_emission(&content[..head_end])
}

/// True if a `session_briefing` reflection for this project was stored within the
/// last `within_minutes`. Reflection timestamps are stored as RFC3339.
fn recent_briefing_exists(engine: &Engine, project: &str, within_minutes: i64) -> bool {
    let project_tag = format!("project_{}", project);
    let Ok(existing) = engine
        .storage()
        .get_reflections_by_tag("session_briefing", 50)
    else {
        return false;
    };
    let cutoff = chrono::Utc::now() - chrono::Duration::minutes(within_minutes);
    existing.iter().any(|(_, _, tags, ts)| {
        tags.iter().any(|t| t == &project_tag)
            && chrono::DateTime::parse_from_rfc3339(ts)
                .map(|t| t.with_timezone(&chrono::Utc) > cutoff)
                .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_briefing_instruction_summarizes_not_searches() {
        // The instruction must NOT tell Haiku to search — episodes are injected.
        // Searching the tag name semantically returns past briefing runs, not
        // episodes, and self-reinforces a false "no episodes found" verdict.
        assert!(!BRIEFING_INSTRUCTION.contains("csr_reflect_on_past"));
        assert!(BRIEFING_INSTRUCTION.contains("Session Intelligence"));
        assert!(BRIEFING_INSTRUCTION.contains("Below are the most recent"));
    }

    #[test]
    #[allow(clippy::assertions_on_constants)] // intentional regression guard on the const
    fn test_briefing_timeout_reasonable() {
        // claude -p has ~30s startup; allow generous async budget up to 3 min
        assert!((60..=180).contains(&BRIEFING_TIMEOUT_SECS));
    }

    #[test]
    fn test_minimal_mcp_config_is_empty() {
        let path = write_minimal_mcp_config().unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let servers = v["mcpServers"].as_object().unwrap();
        assert_eq!(
            servers.len(),
            0,
            "briefing needs no tools — config must load zero MCP servers"
        );
    }

    #[test]
    #[allow(clippy::assertions_on_constants)] // intentional regression guard on the const
    fn test_debounce_window_reasonable() {
        assert!((5..=120).contains(&BRIEFING_DEBOUNCE_MINUTES));
    }

    #[test]
    fn test_is_meta_episode_filters_self_transcripts() {
        let analyst =
            r#"{"schema":"v2","request":"You are CSR Episode Analyst. Generate a brief"}"#;
        let summarizer =
            r#"{"schema":"v2","request":"You are summarizing a coding session for future"}"#;
        let real = r#"{"schema":"v2","request":"Fix the V3 retry storm in the daemon"}"#;
        assert!(is_meta_episode(analyst));
        assert!(is_meta_episode(summarizer));
        assert!(!is_meta_episode(real));
    }
}
