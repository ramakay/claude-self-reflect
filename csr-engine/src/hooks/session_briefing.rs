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
/// (b) Haiku must call csr_reflect_on_past and reason over results,
/// (c) we run async so user never waits.
const BRIEFING_TIMEOUT_SECS: u64 = 120;

/// Skip generating a new briefing if one was produced for this project within
/// this many minutes. Prevents a fresh `claude -p` spawn on every resume/compact
/// when nothing has materially changed.
const BRIEFING_DEBOUNCE_MINUTES: i64 = 30;

/// Prompt sent to Haiku for episode analysis.
const BRIEFING_PROMPT: &str = concat!(
    "You are CSR Episode Analyst. Generate a brief, actionable session briefing.\n\n",
    "STEP 1: Use the csr_reflect_on_past tool to search for recent episodes.\n",
    "Query: \"session_episode\"\n",
    "Limit: 5\n\n",
    "STEP 2: Each result is a JSON episode with fields: request, investigated, completed, ",
    "next_steps, outcome, error_signatures, tools_used, files_modified, ",
    "todos, approved_plan, prev_episode_id, anchors.\n\n",
    "STEP 3: Write a concise briefing (under 150 words). Start with '## Session Intelligence (CSR v9.2)'.\n",
    "Reference specific file names, error messages, and outcomes. Look for:\n",
    "- Recent work and outcomes\n",
    "- Patterns (repeated errors, same files across sessions)\n",
    "- Unfinished work (next_steps, partial/interrupted outcomes)\n\n",
    "If no episodes found, output exactly: 'No recent session episodes found — fresh start.'\n",
    "Do not fabricate information. Only report what the episodes contain."
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

    // Shell out to claude -p with Haiku model.
    // Block on tokio's spawn_blocking since std::process::Command is sync.
    let briefing = tokio::task::spawn_blocking(invoke_haiku_briefing).await??;

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
fn invoke_haiku_briefing() -> Result<String> {
    // Restrict the subprocess to ONLY the csr-engine MCP server. Without this the
    // nested `claude -p` loads every configured MCP server (~30s) just to call one
    // tool. --strict-mcp-config + a one-server config keeps startup to csr-engine's
    // own ~fast cache load.
    let mcp_config_path = write_minimal_mcp_config()?;

    let mut child = Command::new("claude")
        .arg("-p")
        .arg("--model")
        .arg("claude-haiku-4-5-20251001")
        .arg("--output-format")
        .arg("text")
        .arg("--strict-mcp-config")
        .arg("--mcp-config")
        .arg(&mcp_config_path)
        .arg("--dangerously-skip-permissions")
        .arg(BRIEFING_PROMPT)
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

/// Write a minimal MCP config containing only the csr-engine server, used with
/// `--strict-mcp-config` so the briefing subprocess doesn't load every other server.
/// Points at the currently-running binary so the subprocess uses the same version.
fn write_minimal_mcp_config() -> Result<std::path::PathBuf> {
    let csr_bin = std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "csr-engine".to_string());
    let config = serde_json::json!({
        "mcpServers": {
            "claude-self-reflect": { "command": csr_bin }
        }
    });
    let dir = dirs::home_dir()
        .map(|h| h.join(".claude-self-reflect"))
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("briefing-mcp.json");
    std::fs::write(&path, serde_json::to_string(&config)?)?;
    Ok(path)
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
    fn test_briefing_prompt_contains_required_steps() {
        assert!(BRIEFING_PROMPT.contains("csr_reflect_on_past"));
        assert!(BRIEFING_PROMPT.contains("session_episode"));
        assert!(BRIEFING_PROMPT.contains("Session Intelligence"));
    }

    #[test]
    #[allow(clippy::assertions_on_constants)] // intentional regression guard on the const
    fn test_briefing_timeout_reasonable() {
        // claude -p has ~30s startup; allow generous async budget up to 3 min
        assert!((60..=180).contains(&BRIEFING_TIMEOUT_SECS));
    }

    #[test]
    fn test_minimal_mcp_config_has_only_csr() {
        let path = write_minimal_mcp_config().unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let servers = v["mcpServers"].as_object().unwrap();
        assert_eq!(
            servers.len(),
            1,
            "strict config must list exactly one server"
        );
        assert!(servers.contains_key("claude-self-reflect"));
    }

    #[test]
    #[allow(clippy::assertions_on_constants)] // intentional regression guard on the const
    fn test_debounce_window_reasonable() {
        assert!((5..=120).contains(&BRIEFING_DEBOUNCE_MINUTES));
    }
}
