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

/// Redirect this process's stdout (fd 1) onto stderr (fd 2) for the rest of the
/// hook.
///
/// Hook stdout is Claude Code's context-injection channel on `UserPromptSubmit`
/// and `SessionStart`, so a stray library print lands in the model's context as
/// if it were retrieved memory. `hnsw_rs` 0.3.4 has a bare `println!` in
/// `generate_new_point` (hnsw.rs:520) that fires on every 50,000th point insert,
/// so any indexing performed *after* a hook has emitted its injection can leak
/// `" setting number of points N "` into the prompt.
///
/// Call this once a hook has finished writing its intended output. There is no
/// restore: the hook process exits immediately afterwards.
///
/// Unix only. On Windows Rust's stdout writes to the Win32 standard handle, not
/// CRT fd 1, so `_dup2` would not redirect it; the call is a no-op there.
pub fn suppress_stdout() {
    // Anything still sitting in Rust's stdout buffer (a `print!` with no trailing
    // newline) belongs to the injection. Push it out before fd 1 moves, or it
    // would be flushed to stderr at exit and the injection would be lost.
    let _ = std::io::Write::flush(&mut std::io::stdout());

    #[cfg(unix)]
    // SAFETY: dup2 on this process's own standard descriptors. A failure here is
    // non-fatal — the worst case is the pre-existing leak — so the result is
    // deliberately ignored.
    unsafe {
        libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO);
    }
}

/// Points stdout (fd 1) at stderr for as long as it is alive, then puts the real
/// stdout back on drop.
///
/// [`suppress_stdout`] covers prints made *after* a hook has written its
/// injection. This covers the ones made *before* it: `Engine::new` rebuilds or
/// backfills an index whenever the on-disk cache is missing or stale, and that
/// is the same `hnsw_rs` insert path with the same bare `println!`. Those lines
/// reached the model ahead of the injection, e.g. `" setting number of points
/// 50000 "` on every prompt that followed a reflection insert, because the
/// ~59k-point reflection index was rebuilt in-process. `HnswIo::init` also
/// prints to stdout when a cache file is missing.
///
/// Hold one across engine construction in the hook path, and drop it before the
/// handler runs so the injection still reaches Claude Code.
///
/// Unix only, for the reason given on [`suppress_stdout`].
pub struct StdoutQuarantine {
    #[cfg(unix)]
    target: libc::c_int,
    #[cfg(unix)]
    saved: libc::c_int,
}

impl StdoutQuarantine {
    pub fn begin() -> Self {
        let _ = std::io::Write::flush(&mut std::io::stdout());
        #[cfg(unix)]
        {
            Self::begin_on(libc::STDOUT_FILENO, libc::STDERR_FILENO)
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    #[cfg(unix)]
    fn begin_on(target: libc::c_int, sink: libc::c_int) -> Self {
        // SAFETY: fcntl/dup2 on this process's own descriptors. CLOEXEC keeps the
        // saved copy out of any child spawned while the guard is held. If the
        // save fails nothing is redirected: a leak beats a lost injection.
        let saved = unsafe { libc::fcntl(target, libc::F_DUPFD_CLOEXEC, 3) };
        if saved >= 0 {
            unsafe {
                libc::dup2(sink, target);
            }
        }
        Self { target, saved }
    }
}

impl Drop for StdoutQuarantine {
    fn drop(&mut self) {
        // Whatever is still buffered was printed under quarantine; send it to the
        // sink before the real stdout comes back.
        let _ = std::io::Write::flush(&mut std::io::stdout());
        #[cfg(unix)]
        if self.saved >= 0 {
            // SAFETY: restores the descriptor saved in `begin_on`, then closes
            // the spare copy.
            unsafe {
                libc::dup2(self.saved, self.target);
                libc::close(self.saved);
            }
        }
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
        "post-tool-use" => post_tool_use::handle(&input, engine, &cwd).await,
        "prompt-submit" => prompt_submit::handle(&input, engine, &cwd).await,
        _ => {
            eprintln!("unknown hook: {}", hook_name);
            Ok(())
        }
    };
    let t_hook = t0.elapsed();

    // Flush HNSW index if any hook modified it
    // Nothing after the handler should reach stdout; see suppress_stdout.
    suppress_stdout();
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The guard must divert writes while held and hand the descriptor back
    /// intact afterwards. Run on a pair of pipes so the test process's own
    /// stdout is never touched.
    #[test]
    fn stdout_quarantine_diverts_then_restores() {
        fn pipe() -> (libc::c_int, libc::c_int) {
            let mut fds = [0 as libc::c_int; 2];
            assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
            (fds[0], fds[1])
        }
        fn write_fd(fd: libc::c_int, bytes: &[u8]) {
            let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
            assert_eq!(n, bytes.len() as isize);
        }
        fn read_fd(fd: libc::c_int) -> String {
            let mut buf = [0u8; 64];
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            assert!(n > 0, "nothing arrived on fd {fd}");
            String::from_utf8_lossy(&buf[..n as usize]).into_owned()
        }

        let (out_r, out_w) = pipe();
        let (sink_r, sink_w) = pipe();

        let guard = StdoutQuarantine::begin_on(out_w, sink_w);
        write_fd(out_w, b"library noise");
        assert_eq!(read_fd(sink_r), "library noise");
        drop(guard);

        write_fd(out_w, b"injection");
        assert_eq!(read_fd(out_r), "injection");

        for fd in [out_r, out_w, sink_r, sink_w] {
            unsafe { libc::close(fd) };
        }
    }
}
