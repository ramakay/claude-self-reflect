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
use std::path::{Path, PathBuf};

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

/// Resolve the directory a hook operates on. Prefers the cwd Claude Code sent,
/// falls back to the process cwd, and **never fails**.
///
/// Every step here used to abort the whole hook with `?`: `canonicalize` fails if
/// the directory is renamed or removed between `is_dir()` and the call, and
/// `current_dir` fails outright once the process's own working directory is gone.
/// Aborting is the failure this module keeps having to fix — it silently disables
/// both context injection and live transcript import — so each step now degrades to
/// the next best path and says so, rather than taking the session down with it.
///
/// Also performs the S-3 home-directory check (Windows-fixed: `canonicalize()`
/// returns a `\\?\`-prefixed path, so the raw `home_dir()` never prefix-matched and
/// this bailed on EVERY hook that carried a cwd — compare canonical vs canonical).
/// Outside-`$HOME` is a warning, not an error: projects legitimately live on other
/// drives.
fn resolve_hook_cwd(input_cwd: Option<&str>) -> PathBuf {
    if let Some(dir) = input_cwd {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            let resolved = match p.canonicalize() {
                Ok(canonical) => canonical,
                Err(e) => {
                    crate::telemetry::append_timing_line(&format!(
                        "CSR cwd: canonicalize({}) failed ({}) — using the path as given",
                        p.display(),
                        e
                    ));
                    p
                }
            };
            let home_ok = dirs::home_dir()
                .and_then(|h| h.canonicalize().ok())
                .map(|h| resolved.starts_with(&h))
                .unwrap_or(false);
            if !home_ok {
                eprintln!("CSR: cwd {} outside home (allowed)", resolved.display());
            }
            return resolved;
        }
        // A cwd arrived but does not name a directory. Falling back is the right
        // behaviour, but doing it silently is exactly how the Windows breakage
        // above survived unnoticed for so long — leave a line so the next one is
        // diagnosable from the log alone.
        // Quoted, not `Display`: an empty cwd is a real case and prints as nothing,
        // which reads as a corrupt log line rather than the anomaly it is.
        crate::telemetry::append_timing_line(&format!(
            "CSR cwd: input cwd {dir:?} is not a directory — falling back to current_dir()"
        ));
    }

    std::env::current_dir().unwrap_or_else(|e| {
        // The process cwd itself is unreadable (deleted out from under us). A
        // relative path keeps the hook alive; whatever it touches will fail on its
        // own terms instead of killing the session here.
        crate::telemetry::append_timing_line(&format!(
            "CSR cwd: current_dir() failed ({}) — falling back to \".\"",
            e
        ));
        PathBuf::from(".")
    })
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
/// Whether a hook can run on an import-only engine that skips loading and
/// persisting the HNSW index (see [`crate::engine::Engine::new_import_only`]).
///
/// `precompact` and `session-end` only import a transcript (and, for
/// session-end, write reflections to SQLite); neither queries the HNSW, so
/// skipping the load is safe and the new chunks are reconciled on the next
/// search-time load.
///
/// `stop` and `post-tool-use` are here for the same reason. `post-tool-use`
/// never touches the index at all; `stop` matches resolutions via SQLite fts5
/// and its single index interaction is an *insert* whose durable copy already
/// went to SQLite via `insert_derived_reflection_ranges` — the additive
/// backfill in [`Engine::new`] replays it into the next search-constructing
/// process. They were held out of the original write-only split to bound its
/// scope, not because they search, and they are the two hottest hooks on the
/// bus: `stop` fires on every response and `post-tool-use` after every
/// Edit/Write. On a 300k-chunk corpus each invocation was paying a full
/// `HnswIo::load_hnsw` walk plus a ~644MB dump of the graph and data files.
///
/// Everything else stays on the full engine. `session-start` and
/// `prompt-submit` query the index (recap, predictive injection) and genuinely
/// need it loaded.
pub fn is_import_only_hook(hook_name: &str) -> bool {
    matches!(
        hook_name,
        "precompact" | "session-end" | "stop" | "post-tool-use"
    )
}

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

    let cwd = resolve_hook_cwd(input.cwd.as_deref());

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

    // Nothing after the handler should reach stdout; see suppress_stdout.
    suppress_stdout();
    // Hooks never persist the index. `Engine::new` marks it dirty whenever the
    // on-disk cache trails SQLite (every `stop` writes reflections that no
    // long-lived process reconciles until it restarts), and one dirty flag
    // covers both indexes, so flushing here rewrote the whole chunk graph and
    // data (~1GB on a 150k-chunk corpus) on every prompt. With a few sessions
    // open that ran past Claude Code's 30s hook budget and the injection was
    // discarded. A hook only needs its in-memory reconciliation; the daemon's
    // watcher and the MCP server own persistence.
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

    /// The guard must divert writes while held and hand the descriptor back
    /// intact afterwards. Run on a pair of pipes so the test process's own
    /// stdout is never touched.
    #[cfg(unix)]
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
    use std::time::{Duration, Instant};

    #[test]
    fn only_write_only_hooks_skip_the_index() {
        // Write-only importers may run on the import-only engine.
        assert!(is_import_only_hook("precompact"));
        assert!(is_import_only_hook("session-end"));
        // stop and post-tool-use never query the HNSW either: post-tool-use
        // does not touch it, and stop's only interaction is an insert whose
        // durable copy went to SQLite first.
        assert!(is_import_only_hook("stop"));
        assert!(is_import_only_hook("post-tool-use"));
        // session-start/prompt-submit query the index and MUST keep it loaded.
        // A regression that added either here would make it search an empty
        // index and silently return nothing.
        for full in ["session-start", "prompt-submit"] {
            assert!(
                !is_import_only_hook(full),
                "{full} must not be treated as import-only"
            );
        }
    }

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
    fn resolve_hook_cwd_never_fails() {
        // A real directory is taken and canonicalized.
        let real = std::env::temp_dir();
        assert_eq!(
            resolve_hook_cwd(Some(&real.to_string_lossy())),
            real.canonicalize().unwrap()
        );

        // A cwd that is not a directory, and no cwd at all, both fall back to the
        // process directory instead of aborting the hook.
        let here = std::env::current_dir().unwrap();
        assert_eq!(
            resolve_hook_cwd(Some("/definitely/not/a/directory/csr-test")),
            here
        );
        assert_eq!(resolve_hook_cwd(None), here);
        assert_eq!(resolve_hook_cwd(Some("")), here);
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
