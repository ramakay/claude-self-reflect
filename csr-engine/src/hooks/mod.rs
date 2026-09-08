//! Hook dispatcher for Claude Code lifecycle events.
//!
//! Claude Code invokes hooks as shell commands at lifecycle events. Each hook:
//! 1. Receives JSON on stdin (session_id, transcript_path, cwd, reason)
//! 2. Performs work (search, store, file I/O)
//! 3. Writes text to stdout (injected into Claude's context)
//! 4. Exits with code 0 (never blocks the session)

pub mod dream_match;
pub mod exposure;
pub mod install;
pub mod intent;
pub mod post_tool_use;
pub mod precompact;
pub mod prompt_submit;
pub mod reaction;
pub mod recap;
pub mod session_briefing;
pub mod session_end;
pub mod session_start;
pub mod stop;
pub mod subagent_stop;

use std::io::Read;
use std::path::Path;

use anyhow::Result;
use serde::Deserialize;

use crate::engine::Engine;
use crate::search::cross_project::resolve_project_from_cwd;

/// Input received from Claude Code via stdin JSON.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct HookInput {
    #[serde(alias = "sessionId")]
    pub session_id: Option<String>,
    #[serde(alias = "transcriptPath")]
    pub transcript_path: Option<String>,
    #[serde(alias = "agentTranscriptPath")]
    pub agent_transcript_path: Option<String>,
    pub cwd: Option<String>,
    pub reason: Option<String>,
    /// Tool name for PostToolUse hook
    #[serde(alias = "toolName")]
    pub tool_name: Option<String>,
    /// Tool input for PostToolUse hook (contains file_path, content, etc.)
    #[serde(alias = "toolInput")]
    pub tool_input: Option<serde_json::Value>,
    /// Whether the Stop hook is re-entering (to prevent infinite loops)
    #[serde(alias = "stopHookActive")]
    pub stop_hook_active: Option<bool>,
    /// User's prompt text for UserPromptSubmit hook
    pub prompt: Option<String>,
    /// Hook event source (e.g. "startup", "resume", "compact", "clear") for SessionStart
    pub source: Option<String>,
    /// SubagentStop child identifier and metadata (Claude Code payload).
    #[serde(alias = "agentId", alias = "agentName")]
    pub agent_id: Option<String>,
    #[serde(alias = "agentType", alias = "agentDisplayName")]
    pub agent_type: Option<String>,
    #[serde(alias = "lastAssistantMessage")]
    pub last_assistant_message: Option<String>,
}

/// Read and parse JSON from stdin. Returns a default HookInput if stdin is empty or invalid.
/// Guards against hanging when invoked from a terminal (S-4 fix).
pub fn read_stdin_json() -> HookInput {
    use std::io::IsTerminal;

    // If stdin is a TTY (not piped), don't block waiting for input
    if std::io::stdin().is_terminal() {
        return HookInput::default();
    }

    let mut buf = String::new();
    match std::io::stdin().read_to_string(&mut buf) {
        Ok(_) if !buf.trim().is_empty() => serde_json::from_str(&buf).unwrap_or_default(),
        _ => HookInput::default(),
    }
}

/// Shared helper: import the current transcript incrementally.
/// Used by stop, precompact, prompt_submit, and session_end hooks.
pub async fn import_current_transcript(input: &HookInput, engine: &Engine, cwd: &Path) {
    let Some(ref transcript) = input.transcript_path else {
        return;
    };
    let tp = std::path::PathBuf::from(transcript);
    if !tp.exists() {
        return;
    }

    // S-1 fix: validate transcript path is a .jsonl file.
    // The .jsonl extension check prevents importing arbitrary files (e.g. /etc/passwd)
    // via crafted hook JSON. Claude Code only produces .jsonl transcripts.
    let Ok(canonical) = tp.canonicalize() else {
        return;
    };
    if canonical.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        eprintln!(
            "CSR: refusing non-JSONL transcript: {}",
            canonical.display()
        );
        return;
    }

    let cwd_str = cwd.to_string_lossy();
    let project = resolve_project_from_cwd(&cwd_str).unwrap_or_else(|| "unknown".to_string());

    match engine.import_file(&canonical, &project).await {
        Ok(0) => {} // no new content, silent
        Ok(n) => eprintln!("CSR: indexed {} new chunks from active session", n),
        Err(e) => eprintln!("CSR: import failed (non-fatal): {}", e),
    }
}

/// Returns true when `hook_name`/`input` can skip `Engine::new` entirely —
/// checkable purely from the stdin JSON already parsed into `input`, i.e.
/// BEFORE paying the ~1.3GB/1s HNSW-load startup cost (see src/main.rs's
/// `Hook` branch). Conservative by construction: only hooks explicitly
/// reasoned about here are eligible, and the post-tool-use arm defers to
/// `post_tool_use::is_acted_on_tool` — the same list the handler itself
/// gates its code-evolution tracking on — instead of restating it, so this
/// can never drift out of sync with what the handler acts on.
/// stop/session-start/session-end/session-briefing/subagent-stop/precompact
/// are never skipped — they either always act or their no-op paths still
/// need the engine (e.g. re-entrancy bookkeeping).
pub fn hook_is_noop(hook_name: &str, input: &HookInput) -> bool {
    match hook_name {
        // post-tool-use::handle's only tool_name-scoped work is code-evolution
        // tracking (is_acted_on_tool); import_current_transcript otherwise
        // fires unconditionally there. Skipping Engine::new for a non-acted
        // tool therefore also skips that call's real-time transcript sync —
        // a deliberate trade, since the same content is still picked up by
        // the next stop/prompt-submit hook (or the daemon watcher) a moment
        // later, and it is what buys back the ~1.3GB avoided on every
        // Read/Bash/Grep/etc. tool call, which vastly outnumber Edit/Write.
        "post-tool-use" => !post_tool_use::is_acted_on_tool(input.tool_name.as_deref()),
        // prompt-submit: handle_inner's own first branch is `prompt.is_empty()
        // -> return Ok(())` (no stdout). The trailing import_current_transcript
        // call in handle() only has a transcript to chunk when a real prompt
        // triggered the turn, so an empty prompt leaves nothing new to import.
        "prompt-submit" => input.prompt.as_deref().unwrap_or("").is_empty(),
        _ => false,
    }
}

/// Log a skipped (no-op) hook invocation to hook-timing.log in the same
/// shape `dispatch_hook` uses for a real invocation, so the two are
/// comparable in post-session analysis.
pub fn log_noop_skip(hook_name: &str, elapsed: std::time::Duration) {
    let timing_line = format!(
        "CSR hook {} [skip]: total={}ms (no-op, engine skipped)",
        hook_name,
        elapsed.as_millis(),
    );
    eprintln!("{}", timing_line);
    crate::telemetry::append_timing_line(&timing_line);
}

/// Main hook dispatcher. Takes the already-parsed stdin JSON (parsed by the
/// caller before `Engine::new`, so the `hook_is_noop` short-circuit can run
/// pre-engine) and routes to the handler.
pub async fn dispatch_hook(hook_name: &str, engine: &Engine, input: HookInput) -> Result<()> {
    // Recursive-hook guard. The session-briefing hook spawns a nested `claude -p`
    // with CSR_DISABLE_RECURSIVE_HOOKS=1 in its env. That nested session inherits
    // the user's hook config and would otherwise fire CSR hooks — most damagingly
    // the Stop hook, which would store the analyst transcript as a `session_episode`
    // ("You are CSR Episode Analyst..."). Those meta-episodes then dominate the
    // episode store and feed back into the next briefing. Skip ALL hooks in any
    // process descended from the briefing subprocess. (The var propagates to the
    // nested `claude -p` and on to the `csr-engine hook ...` it spawns.)
    if std::env::var("CSR_DISABLE_RECURSIVE_HOOKS").as_deref() == Ok("1") {
        return Ok(());
    }

    let t0 = std::time::Instant::now();
    let t_stdin = t0.elapsed();

    // Determine CWD: prefer input.cwd, fall back to process CWD
    // Validate that cwd is a real directory under $HOME (S-3 fix)
    let cwd = if let Some(ref dir) = input.cwd {
        let p = std::path::PathBuf::from(dir);
        if p.is_dir() {
            let canonical = p.canonicalize()?;
            if let Some(home) = dirs::home_dir() {
                if !canonical.starts_with(&home) {
                    anyhow::bail!("cwd {} is outside home directory", canonical.display());
                }
            }
            canonical
        } else {
            std::env::current_dir()?
        }
    } else {
        std::env::current_dir()?
    };

    let t_setup = t0.elapsed();

    let result = match hook_name {
        "session-start" => session_start::handle(&input, engine, &cwd).await,
        "session-briefing" => session_briefing::handle(&input, engine, &cwd).await,
        "session-end" => session_end::handle(&input, engine, &cwd).await,
        "precompact" => precompact::handle(&input, engine, &cwd).await,
        "stop" => stop::handle(&input, engine, &cwd).await,
        "subagent-stop" => subagent_stop::handle(&input, engine, &cwd).await,
        "post-tool-use" => post_tool_use::handle(&input, engine, &cwd).await,
        "prompt-submit" => prompt_submit::handle(&input, engine, &cwd).await,
        _ => {
            eprintln!("unknown hook: {}", hook_name);
            Ok(())
        }
    };
    let t_hook = t0.elapsed();

    // Flush HNSW index if any hook modified it
    engine.flush_index().await;
    let t_total = t0.elapsed();

    // Resolve project name for logging
    let cwd_str = cwd.to_string_lossy();
    let project = resolve_project_from_cwd(&cwd_str).unwrap_or_else(|| "unknown".to_string());

    let timing_line = format!(
        "CSR hook {} [{}]: stdin={}ms setup={}ms hook={}ms flush={}ms total={}ms",
        hook_name,
        project,
        t_stdin.as_millis(),
        (t_setup - t_stdin).as_millis(),
        (t_hook - t_setup).as_millis(),
        (t_total - t_hook).as_millis(),
        t_total.as_millis(),
    );
    eprintln!("{}", timing_line);

    // Append to timing log file for post-session analysis
    crate::telemetry::append_timing_line(&timing_line);

    result
}

#[cfg(test)]
mod hook_is_noop_tests {
    use super::*;

    fn input_with_tool(tool_name: Option<&str>) -> HookInput {
        HookInput {
            tool_name: tool_name.map(str::to_string),
            ..Default::default()
        }
    }

    fn input_with_prompt(prompt: Option<&str>) -> HookInput {
        HookInput {
            prompt: prompt.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn post_tool_use_is_noop_for_non_acted_tool() {
        assert!(hook_is_noop(
            "post-tool-use",
            &input_with_tool(Some("Read"))
        ));
        assert!(hook_is_noop(
            "post-tool-use",
            &input_with_tool(Some("Bash"))
        ));
        assert!(hook_is_noop("post-tool-use", &input_with_tool(None)));
    }

    #[test]
    fn post_tool_use_is_not_noop_for_acted_tool() {
        assert!(!hook_is_noop(
            "post-tool-use",
            &input_with_tool(Some("Edit"))
        ));
        assert!(!hook_is_noop(
            "post-tool-use",
            &input_with_tool(Some("Write"))
        ));
        assert!(!hook_is_noop(
            "post-tool-use",
            &input_with_tool(Some("MultiEdit"))
        ));
    }

    #[test]
    fn prompt_submit_is_noop_for_empty_prompt() {
        assert!(hook_is_noop("prompt-submit", &input_with_prompt(None)));
        assert!(hook_is_noop("prompt-submit", &input_with_prompt(Some(""))));
    }

    #[test]
    fn prompt_submit_is_not_noop_for_real_prompt() {
        assert!(!hook_is_noop(
            "prompt-submit",
            &input_with_prompt(Some("hello"))
        ));
    }

    #[test]
    fn never_short_circuits_hooks_outside_the_reasoned_set() {
        // Empty/default input on every other dispatched hook name must still
        // route through the engine — these either always act (session-start,
        // session-briefing) or their no-op paths need engine-side
        // bookkeeping (stop, session-end, subagent-stop, precompact).
        let empty = HookInput::default();
        for name in [
            "session-start",
            "session-briefing",
            "session-end",
            "precompact",
            "stop",
            "subagent-stop",
            "unknown-hook-name",
        ] {
            assert!(!hook_is_noop(name, &empty), "{name} was short-circuited");
        }
    }
}
