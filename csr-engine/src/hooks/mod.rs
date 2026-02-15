//! Hook dispatcher for Claude Code lifecycle events.
//!
//! Claude Code invokes hooks as shell commands at lifecycle events. Each hook:
//! 1. Receives JSON on stdin (session_id, transcript_path, cwd, reason)
//! 2. Performs work (search, store, file I/O)
//! 3. Writes text to stdout (injected into Claude's context)
//! 4. Exits with code 0 (never blocks the session)

pub mod install;
pub mod precompact;
pub mod ralph_state;
pub mod session_end;
pub mod session_start;

use std::io::Read;

use anyhow::Result;
use serde::Deserialize;

use crate::engine::Engine;

/// Input received from Claude Code via stdin JSON.
#[derive(Debug, Deserialize, Default)]
pub struct HookInput {
    pub session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub cwd: Option<String>,
    pub reason: Option<String>,
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
        Ok(_) if !buf.trim().is_empty() => {
            serde_json::from_str(&buf).unwrap_or_default()
        }
        _ => HookInput::default(),
    }
}

/// Main hook dispatcher. Parses stdin, detects Ralph state, routes to handler.
pub async fn dispatch_hook(hook_name: &str, engine: &Engine) -> Result<()> {
    let input = read_stdin_json();

    // Determine CWD: prefer input.cwd, fall back to process CWD
    // Validate that cwd is a real directory under $HOME (S-3 fix)
    let cwd = if let Some(ref dir) = input.cwd {
        let p = std::path::PathBuf::from(dir);
        if p.is_dir() {
            let canonical = p.canonicalize()?;
            if let Some(home) = dirs::home_dir() {
                if !canonical.starts_with(&home) {
                    anyhow::bail!(
                        "cwd {} is outside home directory",
                        canonical.display()
                    );
                }
            }
            canonical
        } else {
            std::env::current_dir()?
        }
    } else {
        std::env::current_dir()?
    };

    let ralph = ralph_state::RalphState::detect_in(&cwd)?;

    match hook_name {
        "session-start" => session_start::handle(&input, ralph.as_ref(), engine, &cwd).await,
        "session-end" => session_end::handle(&input, ralph.as_ref(), engine, &cwd).await,
        "precompact" => precompact::handle(&input, ralph.as_ref(), engine).await,
        _ => {
            eprintln!("unknown hook: {}", hook_name);
            Ok(())
        }
    }
}

