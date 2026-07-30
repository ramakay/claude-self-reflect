//! Hook dispatcher for Claude Code lifecycle events.
//!
//! Claude Code invokes hooks as shell commands at lifecycle events. Each hook:
//! 1. Receives JSON on stdin (session_id, transcript_path, cwd, reason)
//! 2. Performs work (search, store, file I/O)
//! 3. Writes text to stdout (injected into Claude's context)
//! 4. Exits with code 0 (never blocks the session)

pub mod install;
pub mod intent;
pub mod post_tool_use;
pub mod precompact;
pub mod prompt_submit;
pub mod recap;
pub mod session_briefing;
pub mod session_end;
pub mod session_start;
pub mod stop;

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
}

/// How long the hook waits for the piped JSON before giving up.
///
/// `read_to_string` returns at EOF, so a parent that opens the pipe, writes
/// nothing and keeps its write handle open would block the hook — and with it the
/// session — forever. A hook must never block Claude Code, so the read is bounded.
const STDIN_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2000);

/// What reading stdin produced. Separated from the read itself so the outcomes and
/// their logging are testable without a real pipe.
#[derive(Debug)]
enum StdinRead {
    Payload(String),
    Empty,
    TimedOut,
    Failed(String),
}

/// Read and parse JSON from stdin. Returns a default HookInput if stdin is empty,
/// unreadable, invalid or too slow: a hook never fails because of its input.
/// Guards against hanging when invoked from a terminal (S-4 fix) and against a
/// parent that never closes the pipe.
pub fn read_stdin_json() -> HookInput {
    use std::io::IsTerminal;

    // If stdin is a TTY (not piped), don't block waiting for input
    if std::io::stdin().is_terminal() {
        crate::telemetry::append_timing_line("CSR stdin: is a terminal, no piped input");
        return HookInput::default();
    }

    hook_input_from(read_bounded(STDIN_READ_TIMEOUT, || {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).map(|_| buf)
    }))
}

/// Run `read` on its own thread and give up after `timeout`.
///
/// The reader thread is deliberately detached rather than joined: when the timeout
/// fires it is still parked on a pipe nobody is going to close, and this process is
/// about to exit anyway.
fn read_bounded<F>(timeout: std::time::Duration, read: F) -> StdinRead
where
    F: FnOnce() -> std::io::Result<String> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = match read() {
            Ok(buf) if buf.trim().is_empty() => StdinRead::Empty,
            Ok(buf) => StdinRead::Payload(buf),
            Err(e) => StdinRead::Failed(e.to_string()),
        };
        let _ = tx.send(outcome);
    });

    match rx.recv_timeout(timeout) {
        Ok(outcome) => outcome,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => StdinRead::TimedOut,
        // The reader vanished without reporting. Unreadable is not the same as empty.
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            StdinRead::Failed("reader thread disconnected".to_string())
        }
    }
}

/// Turn a stdin read into a HookInput, recording *why* an input was unusable —
/// empty, unreadable, too slow and unparseable used to be one indistinguishable
/// `hook=0ms` line.
fn hook_input_from(read: StdinRead) -> HookInput {
    match read {
        StdinRead::Payload(buf) => match serde_json::from_str(&buf) {
            Ok(input) => input,
            Err(e) => {
                // A parse failure here silently degrades to Default (no prompt,
                // no transcript_path) — the hook then no-ops. Log it loudly.
                crate::telemetry::append_timing_line(&format!(
                    "CSR stdin: {}B received but JSON parse FAILED: {}",
                    buf.len(),
                    e
                ));
                HookInput::default()
            }
        },
        StdinRead::Empty => {
            crate::telemetry::append_timing_line("CSR stdin: empty (no JSON piped)");
            HookInput::default()
        }
        StdinRead::TimedOut => {
            crate::telemetry::append_timing_line(&format!(
                "CSR stdin: no EOF within {}ms, giving up (parent still holds the pipe)",
                STDIN_READ_TIMEOUT.as_millis()
            ));
            HookInput::default()
        }
        StdinRead::Failed(e) => {
            crate::telemetry::append_timing_line(&format!("CSR stdin: read FAILED: {}", e));
            HookInput::default()
        }
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

/// Main hook dispatcher. Parses stdin, routes to handler.
pub async fn dispatch_hook(hook_name: &str, engine: &Engine) -> Result<()> {
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
    let input = read_stdin_json();
    let t_stdin = t0.elapsed();

    // Field-presence diagnostic: hook=0ms exits are indistinguishable from a
    // missing prompt without this (live debugging 2026-07-30).
    crate::telemetry::append_timing_line(&format!(
        "CSR hook {} input: prompt_len={} cwd={:?} transcript={} session={}",
        hook_name,
        input.prompt.as_deref().map(str::len).unwrap_or(0),
        input.cwd,
        input.transcript_path.is_some(),
        input.session_id.is_some(),
    ));

    // Determine CWD: prefer input.cwd, fall back to process CWD
    // Validate that cwd is a real directory under $HOME (S-3 fix)
    let cwd = if let Some(ref dir) = input.cwd {
        let p = std::path::PathBuf::from(dir);
        if p.is_dir() {
            let canonical = p.canonicalize()?;
            // S-3 check, Windows-fixed: canonicalize() returns a \\?\-prefixed
            // path, so the raw home_dir() never prefix-matched and this bailed
            // on EVERY hook that carried a cwd — compare canonical vs canonical.
            // And outside-home is a warning, not an error: projects legitimately
            // live on other drives (D:\...), and a hook that dies here silently
            // disables both injection and live transcript import.
            let home_ok = dirs::home_dir()
                .and_then(|h| h.canonicalize().ok())
                .map(|h| canonical.starts_with(&h))
                .unwrap_or(false);
            if !home_ok {
                eprintln!("CSR: cwd {} outside home (allowed)", canonical.display());
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
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn read_bounded_gives_up_instead_of_hanging() {
        // Stands in for a parent that opened the pipe and never closes it.
        let started = Instant::now();
        let outcome = read_bounded(Duration::from_millis(20), || {
            std::thread::sleep(Duration::from_secs(30));
            Ok(String::new())
        });
        assert!(
            matches!(outcome, StdinRead::TimedOut),
            "a read that never finishes must time out, got {outcome:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the hook must not wait for the reader"
        );
    }

    #[test]
    fn read_bounded_reports_io_errors_separately_from_empty() {
        let outcome = read_bounded(Duration::from_secs(5), || {
            Err(std::io::Error::other("pipe exploded"))
        });
        match outcome {
            StdinRead::Failed(e) => assert!(e.contains("pipe exploded")),
            other => panic!("an I/O failure must not be reported as empty, got {other:?}"),
        }

        let outcome = read_bounded(Duration::from_secs(5), || Ok("  \n".to_string()));
        assert!(
            matches!(outcome, StdinRead::Empty),
            "whitespace-only input is empty, not a payload"
        );
    }

    #[test]
    fn hook_input_from_parses_a_payload_and_degrades_otherwise() {
        let input = hook_input_from(StdinRead::Payload(
            r#"{"prompt":"hola","cwd":"/tmp/proj"}"#.to_string(),
        ));
        assert_eq!(input.prompt.as_deref(), Some("hola"));
        assert_eq!(input.cwd.as_deref(), Some("/tmp/proj"));

        // Every unusable outcome degrades to a default input rather than failing:
        // the hook still runs, it just has nothing to work with.
        for unusable in [
            StdinRead::Payload("not json".to_string()),
            StdinRead::Empty,
            StdinRead::TimedOut,
            StdinRead::Failed("boom".to_string()),
        ] {
            let input = hook_input_from(unusable);
            assert!(input.prompt.is_none() && input.transcript_path.is_none());
        }
    }
}
