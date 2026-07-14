//! Session briefing hook — shells out to `claude -p`, walking a model chain
//! (env override → haiku alias → CLI default) to generate a proactive
//! briefing from recent session episodes.
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
    "Only report what the episodes contain. Do not fabricate information.\n",
    "Omit tangents and asides unrelated to this project's technical work ",
    "(e.g. travel, unrelated tooling). Do not mention them at all — not even ",
    "to flag them as unrelated.\n",
    "Each episode is prefixed with its age (e.g. '2h ago'). Refer to timing ",
    "using those ages. Do NOT cite absolute calendar dates: the JSON timestamps ",
    "are UTC and will read as the wrong day in the user's timezone.\n\n",
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

    if crate::narrative::narratives_disabled() {
        tracing::debug!("AI narratives disabled via CSR_NO_AI_NARRATIVES — skipping briefing");
        return Ok(());
    }

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
    let window = recent_episodes_for_project(engine, project_name);
    if window.rendered.is_empty() {
        tracing::debug!(
            project = project_name,
            "no episodes for project — skipping briefing"
        );
        return Ok(());
    }

    // Content-hash cache: if the episode window is byte-identical to the one
    // that produced the last successful briefing, a new Haiku call would emit
    // the same briefing — skip it entirely. Debounce (above) catches rapid
    // resumes; this catches "resumed hours later but nothing new happened".
    // Digest is over stable (ts, content) pairs — NOT the rendered prompt, which
    // embeds relative ages ("2h ago") that change with wall-clock time.
    let hash_key = format!("briefing_input_hash:{}", project_name);
    let episode_digest = stable_episode_digest(&window.stable_entries);
    if engine
        .storage()
        .get_meta(&hash_key)
        .ok()
        .flatten()
        .as_deref()
        == Some(episode_digest.as_str())
        && briefing_exists(engine, project_name)
    {
        tracing::debug!(
            project = project_name,
            "episodes unchanged since last briefing — skipping claude -p"
        );
        return Ok(());
    }

    let prompt = format!("{}{}", BRIEFING_INSTRUCTION, window.rendered);

    // Shell out to claude -p, walking the narrative model chain on model-not-found.
    // Block on tokio's spawn_blocking since std::process::Command is sync.
    let started = std::time::Instant::now();
    let result = tokio::task::spawn_blocking(move || invoke_narrative_briefing(&prompt)).await?;
    let duration_ms = started.elapsed().as_millis() as i64;
    let parsed = match result {
        Ok(p) => p,
        Err(e) => {
            // Best-effort: a failed insert must never mask the original error.
            let _ = engine
                .storage()
                .record_narrative_usage(&crate::storage::NarrativeUsageRow {
                    call_site: "briefing".into(),
                    model: "unknown".into(),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    duration_ms,
                    success: false,
                });
            return Err(e);
        }
    };

    // Accounting is best-effort: a failed insert must never fail the hook.
    let _ = engine
        .storage()
        .record_narrative_usage(&crate::storage::NarrativeUsageRow {
            call_site: "briefing".into(),
            model: parsed.model.clone(),
            input_tokens: parsed.input_tokens,
            output_tokens: parsed.output_tokens,
            cache_read_tokens: parsed.cache_read_tokens,
            cache_creation_tokens: parsed.cache_creation_tokens,
            duration_ms,
            success: true,
        });

    if parsed.text.trim().is_empty() {
        eprintln!("CSR: session-briefing returned empty output");
        return Ok(());
    }

    // Store briefing as a tagged reflection — replace any prior briefing for this project
    store_briefing(engine, project_name, &parsed.text)?;
    let _ = engine.storage().set_meta(&hash_key, &episode_digest);

    Ok(())
}

/// Invoke `claude -p` with JSON output, walking the narrative model candidate
/// chain on model-not-found. Returns the parsed narrative, or an error if the
/// invocation fails or times out.
///
/// The episodes are embedded in `prompt`, so Haiku needs NO tools. We pass an
/// empty MCP config with `--strict-mcp-config` so the subprocess loads ZERO MCP
/// servers (not even csr-engine) — fastest possible `claude -p` startup and no
/// recursive csr-engine spawn.
fn invoke_narrative_briefing(prompt: &str) -> Result<crate::narrative::ParsedNarrative> {
    let mcp_config_path = write_minimal_mcp_config()?;
    let mut last_err: Option<anyhow::Error> = None;

    for candidate in crate::narrative::model_candidates() {
        let mut cmd = Command::new("claude");
        cmd.arg("-p")
            // The prompt MUST precede --mcp-config: that flag is variadic in the
            // claude CLI and consumes any trailing positional arg as another config
            // file path, failing with ENAMETOOLONG on the episode text.
            .arg(prompt);
        if let Some(model) = &candidate {
            cmd.arg("--model").arg(model);
        }
        cmd.arg("--output-format")
            .arg("json")
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
            .env("CSR_DISABLE_RECURSIVE_HOOKS", "1"); // signal to nested csr-engine to skip hooks

        let mut child = cmd.spawn()?;

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
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        match crate::narrative::classify_attempt(output.status.success(), &stdout, &stderr) {
            crate::narrative::AttemptOutcome::Parsed(parsed) => return Ok(parsed),
            crate::narrative::AttemptOutcome::ModelNotFound => {
                tracing::warn!(model = ?candidate, "narrative model unavailable — trying next candidate");
                last_err = Some(anyhow::anyhow!(
                    "model unavailable: {}",
                    if stderr.is_empty() { &stdout } else { &stderr }
                ));
                continue; // walk the chain ONLY on model-not-found
            }
            crate::narrative::AttemptOutcome::Failed(msg) => anyhow::bail!("{}", msg),
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no narrative model candidates succeeded")))
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

/// Prompt-ready episode window plus stable (time-independent) precursors for
/// the content-hash skip-gate.
struct EpisodeWindow {
    /// Prompt-ready text with relative ages — fed to claude -p.
    rendered: String,
    /// (timestamp, content) pairs for included episodes, BEFORE age formatting —
    /// used only for the stable digest.
    stable_entries: Vec<(String, String)>,
}

/// Digest of the stable (time-independent) precursor of an episode window:
/// (timestamp, content) pairs, BEFORE relative-age rendering. Two calls to
/// `recent_episodes_for_project` at different wall-clock times over the SAME
/// underlying episodes must yield the SAME digest here, even though the
/// *rendered* prompt text differs (different "Xh ago" strings). This is what
/// makes the briefing skip-gate actually skip across a debounce-window-sized
/// gap instead of re-triggering `claude -p` on every call.
fn stable_episode_digest(entries: &[(String, String)]) -> String {
    let mut stable = String::new();
    for (ts, content) in entries {
        stable.push_str(ts);
        stable.push('\n');
        stable.push_str(content);
        stable.push('\n');
    }
    format!("{:016x}", crate::narrative::fnv1a_64(stable.as_bytes()))
}

/// Load the most recent `session_episode` reflections for `project` and format
/// them (newest first, capped) for injection into the briefing prompt. Returns an
/// empty window if none exist — the caller skips the briefing entirely.
fn recent_episodes_for_project(engine: &Engine, project: &str) -> EpisodeWindow {
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
            return EpisodeWindow {
                rendered: String::new(),
                stable_entries: Vec::new(),
            };
        }
    };

    let mut out = String::new();
    let mut stable_entries = Vec::new();
    let mut n = 0;
    for (_, content, tags, ts) in &rows {
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
        // Stable precursor BEFORE age formatting — used for the skip-gate digest.
        // Full untruncated content: truncation is time-independent, but full content
        // is simpler and strictly safer for uniqueness.
        stable_entries.push((ts.clone(), content.clone()));
        // Relative age (e.g. "2h ago") so Haiku reports timing without citing the
        // raw UTC timestamp, which reads as a future date in the user's timezone.
        let age = crate::temporal::parse_timestamp(ts)
            .map(|t| crate::temporal::format_relative_time(&t))
            .unwrap_or_else(|| "recently".to_string());
        if content.len() > MAX_EPISODE_CHARS {
            let end = content.floor_char_boundary(MAX_EPISODE_CHARS);
            // Mark the cut so Haiku treats it as an excerpt, not malformed JSON.
            out.push_str(&format!(
                "\n[Episode {} — {}]\n{}…[truncated]\n",
                n,
                age,
                &content[..end]
            ));
        } else {
            out.push_str(&format!("\n[Episode {} — {}]\n{}\n", n, age, content));
        }
    }
    EpisodeWindow {
        rendered: out,
        stable_entries,
    }
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

/// True if ANY `session_briefing` reflection exists for this project, regardless
/// of age. Used by the content-hash skip-gate: if the user deleted reflections
/// (so no briefing exists at all), we must regenerate even when the episode
/// digest matches a stale stored hash.
fn briefing_exists(engine: &Engine, project: &str) -> bool {
    let project_tag = format!("project_{}", project);
    let Ok(existing) = engine
        .storage()
        .get_reflections_by_tag("session_briefing", 50)
    else {
        return false;
    };
    existing
        .iter()
        .any(|(_, _, tags, _)| tags.iter().any(|t| t == &project_tag))
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

    #[test]
    fn test_narratives_disabled_gate_is_callable() {
        // Callability smoke test: proves crate::narrative::narratives_disabled() is
        // reachable and returns a bool. Does NOT exercise handle_inner's actual gate
        // logic — we don't mutate CSR_NO_AI_NARRATIVES here (that env var is also
        // touched by narrative.rs's own tests in the same --lib test binary, and cargo
        // test runs tests in parallel by default — mutating shared process env from
        // two test modules is a real race). Just prove the gate function is reachable
        // and returns a bool without disabling narratives process-wide for every other
        // test in this binary.
        let _ = crate::narrative::narratives_disabled();
    }

    #[test]
    fn test_briefing_hash_key_roundtrip() {
        // Digest must be stable and hex-encoded — it is persisted in the meta table.
        let digest = format!("{:016x}", crate::narrative::fnv1a_64(b"episode window"));
        assert_eq!(digest.len(), 16);
        assert_eq!(
            digest,
            format!("{:016x}", crate::narrative::fnv1a_64(b"episode window"))
        );
    }

    #[test]
    fn test_stable_episode_digest_ignores_relative_age() {
        let entries = vec![(
            "2026-07-10T10:00:00Z".to_string(),
            r#"{"request":"fix bug"}"#.to_string(),
        )];
        let digest_a = stable_episode_digest(&entries);
        let digest_b = stable_episode_digest(&entries);
        assert_eq!(digest_a, digest_b, "stable digest must be deterministic");

        // Simulate the OLD (buggy) approach: hashing the rendered text, which
        // embeds a relative age that changes with wall-clock time even when
        // the underlying episode window is identical.
        let rendered_2h_ago = format!("\n[Episode 1 — 2h ago]\n{}\n", entries[0].1);
        let rendered_3h_ago = format!("\n[Episode 1 — 3h ago]\n{}\n", entries[0].1);
        let old_digest_1 = format!(
            "{:016x}",
            crate::narrative::fnv1a_64(rendered_2h_ago.as_bytes())
        );
        let old_digest_2 = format!(
            "{:016x}",
            crate::narrative::fnv1a_64(rendered_3h_ago.as_bytes())
        );
        assert_ne!(
            old_digest_1, old_digest_2,
            "sanity: old rendered-text hashing DOES change with age — this is the bug"
        );
        // The new stable digest does not depend on age at all.
        assert_eq!(digest_a, digest_b);
    }
}
