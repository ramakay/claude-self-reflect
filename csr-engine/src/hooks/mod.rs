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
pub mod prompt_transport;
pub mod reaction;
pub mod recap;
mod scope_folder;
pub mod session_briefing;
pub mod session_end;
pub mod session_start;
pub mod stop;

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;

use crate::engine::Engine;
use crate::search::cross_project::resolve_project_from_cwd;

/// Input received from Claude Code via stdin JSON.
#[derive(Debug, Clone, Deserialize, serde::Serialize, Default)]
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
    import_current_transcript_observing(input, engine, cwd, None).await;
}

pub(crate) async fn import_current_transcript_observing(
    input: &HookInput,
    engine: &Engine,
    cwd: &Path,
    observer: Option<&mut (dyn FnMut(&serde_json::Value) + Send)>,
) {
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

    match engine
        .import_file_observing(&canonical, &project, observer)
        .await
    {
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
        let saved = divert_fd(target, sink);
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

/// Save `target` and point it at `sink`, returning the saved descriptor.
/// Shared by [`StdoutQuarantine`] and [`ClaimedStdout`] so the fcntl/dup2 pair
/// exists in exactly one place.
///
/// All-or-nothing: a non-negative return means both steps landed, `target`
/// now points at `sink`, and the caller owns the returned descriptor (it must
/// eventually be restored or closed). A negative return means `target` was
/// left exactly as it was and there is nothing to close. Reporting success
/// after only the `fcntl` half worked would leave `target` on the *original*
/// stream while callers believe it is diverted — for [`ClaimedStdout`] that
/// means JSON-RPC frames written through the saved descriptor and library
/// `println!`s would both land on the same real fd 1, corrupting the
/// protocol stream, which is the exact failure this diversion exists to
/// prevent. A half-applied diversion is worse than none, so on a failed
/// `dup2` the saved copy is closed and the caller is told nothing happened:
/// the failure exit is a single unconditional `close` on the only path where
/// `dup2` did not succeed, so there is nowhere for the saved descriptor to
/// leak. The all-or-nothing contract, including that close-on-failure, is
/// covered deterministically through [`divert_fd_with`]'s injected effects —
/// what remains untestable is *observing* the leak from outside via the
/// process's open-fd high-water mark, which is process-global state that
/// other threads move arbitrarily under the parallel test harness.
#[cfg(unix)]
fn divert_fd(target: libc::c_int, sink: libc::c_int) -> libc::c_int {
    // SAFETY: fcntl/dup2 on this process's own descriptors. CLOEXEC keeps the
    // saved copy out of any child spawned while the guard is held.
    divert_fd_with(
        target,
        sink,
        |fd| unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) },
        |s, t| unsafe { libc::dup2(s, t) },
        |fd| unsafe {
            libc::close(fd);
        },
    )
}

/// The all-or-nothing decision behind [`divert_fd`], with every syscall
/// injected as a closure. Pure logic on top of the three effects — save,
/// dup2, close — so the failure-path cleanup can be proven deterministically
/// in a unit test without touching any real descriptor or depending on
/// process-global allocation state.
#[cfg(unix)]
fn divert_fd_with(
    target: libc::c_int,
    sink: libc::c_int,
    save: impl Fn(libc::c_int) -> libc::c_int,
    dup2: impl Fn(libc::c_int, libc::c_int) -> libc::c_int,
    close: impl Fn(libc::c_int),
) -> libc::c_int {
    let saved = save(target);
    if saved < 0 {
        return saved;
    }
    if dup2(sink, target) < 0 {
        // `target` is unchanged; the saved copy is now useless, close it
        // rather than leak it.
        close(saved);
        return -1;
    }
    saved
}

/// Claims real fd 1 for the JSON-RPC transport, for the lifetime of the MCP
/// server process, and sends everything else that would have landed on stdout
/// to stderr instead.
///
/// Unlike [`StdoutQuarantine`], which is scoped to a single hook invocation,
/// this has to hold for as long as the process serves MCP: the server is
/// long-lived, `Engine::new` rebuilds or backfills the HNSW index at startup
/// (the same `hnsw_rs` `println!` covered by `StdoutQuarantine`'s doc comment),
/// and `spawn_enrichment_loops` plus `--watch` importers keep inserting into
/// that index *while the server is running*, so the 50,000-point boundary can
/// fire mid-session, long after any scoped guard would have been dropped.
/// stdout is the MCP JSON-RPC channel, so any of those prints reaching it
/// corrupts the protocol stream. Claim it once, before `Engine::new` runs, and
/// hold it for the rest of the process; there is deliberately no restore.
///
/// Everything that used to reach stdout now reaches stderr instead, which
/// Claude Code captures into its own MCP server logs — the noise moves to
/// where it belongs rather than disappearing. The saved descriptor is
/// `CLOEXEC`, so children spawned after the claim (narrator `claude -p`
/// subprocesses, `--watch` helpers) never inherit the real JSON-RPC channel.
pub struct ClaimedStdout {
    #[cfg(unix)]
    saved: libc::c_int,
}

impl ClaimedStdout {
    /// Refuse to serve MCP unless its output channel was claimed.
    pub fn require(claim: Option<Self>) -> anyhow::Result<Self> {
        claim.ok_or_else(|| anyhow::anyhow!("cannot claim stdout for the MCP channel"))
    }

    /// Flushes Rust's stdout buffer, then diverts fd 1 to fd 2 and hands back a
    /// handle to the saved original. Returns `None` (nothing redirected, the
    /// caller falls back to the existing `rmcp::transport::io::stdio()` path)
    /// when `divert_fd` fails to fully apply the diversion, or unconditionally
    /// on non-unix, where Rust's stdout does not go through a CRT fd `dup2`
    /// can redirect.
    pub fn claim() -> Option<Self> {
        let _ = std::io::Write::flush(&mut std::io::stdout());
        #[cfg(unix)]
        {
            Self::claim_on(libc::STDOUT_FILENO, libc::STDERR_FILENO)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    #[cfg(unix)]
    fn claim_on(target: libc::c_int, sink: libc::c_int) -> Option<Self> {
        let saved = divert_fd(target, sink);
        if saved < 0 {
            return None;
        }
        Some(Self { saved })
    }

    /// The saved raw descriptor, for tests that claim on pipes instead of the
    /// real fd 1.
    #[cfg(all(unix, test))]
    fn saved_fd(&self) -> libc::c_int {
        self.saved
    }

    /// Hands ownership of the saved descriptor to a `tokio::fs::File` so it can
    /// back the MCP transport's write half. Consumes `self` on purpose: the
    /// claim is meant to last for the process lifetime, so there is no `Drop`
    /// impl to race with this handoff, and the descriptor must not be closed
    /// out from under the file that now owns it.
    #[cfg(unix)]
    pub fn into_async_writer(self) -> tokio::fs::File {
        use std::os::unix::io::FromRawFd;
        // SAFETY: `saved` came from `F_DUPFD_CLOEXEC` in `divert_fd` and is
        // owned solely by this `ClaimedStdout` (no `Drop` impl closes it), so
        // handing it to `std::fs::File` does not create a double-close or use
        // of a descriptor still held elsewhere.
        tokio::fs::File::from_std(unsafe { std::fs::File::from_raw_fd(self.saved) })
    }
}

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
/// need it loaded, except for the prompts [`hook_needs_index`] rules out.
pub fn is_import_only_hook(hook_name: &str) -> bool {
    matches!(
        hook_name,
        "precompact" | "session-end" | "stop" | "post-tool-use"
    )
}

/// Hooks that no longer do anything but may still be registered in a
/// settings.json that predates their removal. They stay accepted so Claude
/// Code never sees a failing command, and `main` returns before the lock, the
/// stdin read or any engine startup. `hook install --apply` drops the stale
/// registration.
///
/// `subagent-stop` captured nothing by design: `intent_events::is_foreign`
/// treats sidechain transcripts as foreign, so it cost a process spawn per
/// finished subagent for zero rows.
pub fn is_retired_hook(hook_name: &str) -> bool {
    matches!(hook_name, "subagent-stop")
}

/// Whether this argv is `hook <retired name>`, read straight from the OS
/// strings. `main` needs the answer before clap runs (the timing probe prints
/// first), and `std::env::args()` would panic on a non-UTF-8 `--db-path`, so
/// the walk is over `OsString` and mirrors clap: global flags may precede or
/// follow `hook`, and only the first non-flag token after it is the name — a
/// flag's value (`--db-path subagent-stop`) is never mistaken for it.
pub fn argv_names_retired_hook<I>(args: I) -> bool
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    const VALUE_FLAGS: [&str; 3] = ["--db-path", "--projects-dir", "--limit"];
    let mut args = args.into_iter();
    let mut seen_hook = false;
    while let Some(arg) = args.next() {
        let Some(text) = arg.to_str() else {
            if arg.as_encoded_bytes().starts_with(b"-") {
                continue;
            }
            return false;
        };
        if text.starts_with('-') {
            if VALUE_FLAGS.contains(&text) {
                args.next();
            }
            continue;
        }
        if !seen_hook {
            if text != "hook" {
                return false;
            }
            seen_hook = true;
            continue;
        }
        return is_retired_hook(text);
    }
    false
}

/// Whether this invocation will query the HNSW index, decided from the hook
/// name and its parsed input before the engine is built.
///
/// Loading the graph is the dominant cost of a hook process on a large corpus
/// (measured 2026-09-19 at 259k chunks: ~880ms of a 1.1s wall clock for a
/// `prompt-submit` whose handler returned in 0ms). `prompt-submit` leaves
/// before any search for harness turns, continuation prompts, slash commands
/// and bare acknowledgments, so those run on the import-only engine too.
pub fn hook_needs_index(hook_name: &str, input: &HookInput) -> bool {
    if is_import_only_hook(hook_name) {
        return false;
    }
    !(hook_name == "prompt-submit" && prompt_submit::exits_before_search(input.prompt.as_deref()))
}

/// Hooks never publish the HNSW cache. Imports remain durable in SQLite;
/// the daemon and normal search-engine startup reconcile those rows. Keep
/// this gate even for import-only engines so a future constructor change
/// cannot silently put multi-second index dumps back on the hook path.
pub fn hook_may_persist_index(_hook_name: &str) -> bool {
    false
}

/// Skip a busy maintenance run before stdin, storage, embeddings or index startup.
/// Hooks fail open with empty output if the lock is busy or unavailable.
pub fn try_start(db_path: &Path) -> Option<crate::maintenance::SharedLock> {
    crate::maintenance::try_shared(db_path.parent().unwrap_or(Path::new(".")))
        .ok()
        .flatten()
}

/// Main hook dispatcher: routes the parsed input to its handler.
///
/// `input` is read by the caller, ahead of engine construction, because
/// [`hook_needs_index`] picks the engine from it; `stdin_elapsed` is how long
/// that read took, kept for the timing line.
pub async fn dispatch_hook(
    hook_name: &str,
    engine: &Engine,
    input: HookInput,
    stdin_elapsed: std::time::Duration,
) -> Result<()> {
    let _operation = match engine.storage().operation_lock() {
        Ok(lock) => lock,
        Err(_) => return Ok(()),
    };
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

    // Phase marks stay cumulative from the start of the stdin read, as they
    // were when this function did the read itself.
    let t0 = std::time::Instant::now();
    let t_stdin = stdin_elapsed;
    let elapsed = || stdin_elapsed + t0.elapsed();

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

    let t_setup = elapsed();

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
    let t_hook = elapsed();

    // Persist HNSW changes only if this hook's role allows it — see
    // `hook_may_persist_index`, the sole allowlist. Injection-critical hooks
    // (prompt-submit, post-tool-use, session-start, session-briefing) skip
    // this entirely, so their `flush=` timing below is ~0ms even when
    // `Engine::new`'s additive backfill left the in-memory index dirty.
    // Nothing after the handler should reach stdout; see suppress_stdout.
    suppress_stdout();
    if hook_may_persist_index(hook_name) {
        engine.flush_index().await;
    }
    let t_total = elapsed();

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

    /// The MCP claim must behave like the hook quarantine's diversion half:
    /// once claimed, library writes to the target land on the sink, and the
    /// caller can still reach the original stream through the saved
    /// descriptor. Run on pipes so the test process's real fd 1 is untouched.
    #[cfg(unix)]
    #[test]
    fn claimed_stdout_diverts_target_to_sink() {
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

        let claimed = ClaimedStdout::claim_on(out_w, sink_w).expect("claim_on should succeed");

        // Library code still writing to the (now-diverted) target lands on the sink.
        write_fd(out_w, b"library noise");
        assert_eq!(read_fd(sink_r), "library noise");

        // The saved descriptor still reaches the original reader.
        let saved = claimed.saved_fd();
        write_fd(saved, b"jsonrpc");
        assert_eq!(read_fd(out_r), "jsonrpc");

        unsafe { libc::close(saved) };
        for fd in [out_r, out_w, sink_r, sink_w] {
            unsafe { libc::close(fd) };
        }
    }

    /// A failed `dup2` half of `divert_fd` must not be reported as a
    /// successful claim: `sink_w` is closed up front, which is a bad fd for
    /// `dup2`, so the `fcntl` save succeeds but the `dup2` fails with
    /// `EBADF`. `claim_on` must refuse the claim rather than hand back a
    /// `ClaimedStdout` whose saved descriptor still points at the *original*
    /// stream while `target` was never actually repointed — and `target`
    /// itself must be provably untouched: still readable by its original
    /// reader, not silently left on a stream nobody can read.
    #[cfg(unix)]
    #[test]
    fn claimed_stdout_refuses_a_partial_diversion() {
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
        // A sentinel that is definitionally invalid as a dup2 target, rather
        // than a real descriptor number recycled from a closed pipe: under
        // the parallel test harness other threads churn fds constantly, and
        // `fcntl(.., F_DUPFD_CLOEXEC, 3)` hands out the lowest free number,
        // so a closed-and-reused sink could collide with a fd another thread
        // just opened, making `dup2` succeed and the test flaky. `c_int::MAX`
        // can never be allocated by that call, so `dup2` fails with `EBADF`
        // deterministically.
        let sink_w = libc::c_int::MAX;

        let claimed = ClaimedStdout::claim_on(out_w, sink_w);
        assert!(
            claimed.is_none(),
            "a partial diversion (fcntl ok, dup2 EBADF) must not be reported as a successful claim"
        );

        // `target` must be exactly as it was: still reachable by its original
        // reader, not silently repointed at a stream nobody can read.
        write_fd(out_w, b"jsonrpc");
        assert_eq!(read_fd(out_r), "jsonrpc");

        for fd in [out_r, out_w] {
            unsafe { libc::close(fd) };
        }
    }

    /// `divert_fd`'s failure path (`fcntl`/save succeeds, `dup2` fails) must
    /// close the descriptor it saved rather than leak it — the property
    /// `claimed_stdout_refuses_a_partial_diversion` cannot observe from
    /// outside. Proven here through `divert_fd_with`'s injected effects: a
    /// fake `save` that hands back a fixed, unmistakable descriptor number,
    /// a fake `dup2` that always fails, and a `close` that records every
    /// descriptor it was asked to close. No real syscalls, no dependence on
    /// process-global descriptor state.
    #[cfg(unix)]
    #[test]
    fn divert_fd_with_closes_the_saved_copy_on_a_failed_dup2() {
        let closed = std::cell::RefCell::new(Vec::new());
        let result = divert_fd_with(
            1,
            2,
            |_target| 4242,
            |_sink, _target| -1,
            |fd| {
                closed.borrow_mut().push(fd);
            },
        );

        assert!(
            result < 0,
            "a failed dup2 must be reported as a failed diversion"
        );
        assert_eq!(
            closed.into_inner(),
            vec![4242],
            "the saved descriptor must be closed exactly once on the failed dup2 path"
        );
    }

    /// The mirror case: when `dup2` succeeds, the saved descriptor is handed
    /// back to the caller (who now owns it), not closed.
    #[cfg(unix)]
    #[test]
    fn divert_fd_with_does_not_close_the_saved_copy_on_success() {
        let closed = std::cell::RefCell::new(Vec::new());
        let result = divert_fd_with(
            1,
            2,
            |_target| 4242,
            |_sink, _target| 0,
            |fd| {
                closed.borrow_mut().push(fd);
            },
        );

        assert_eq!(
            result, 4242,
            "a successful diversion must return the saved descriptor"
        );
        assert!(
            closed.into_inner().is_empty(),
            "a successful diversion must not close the saved descriptor"
        );
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
    fn the_index_is_loaded_only_for_invocations_that_search() {
        let with_prompt = |prompt: &str| HookInput {
            prompt: Some(prompt.to_string()),
            ..Default::default()
        };
        assert!(hook_needs_index(
            "prompt-submit",
            &with_prompt("fix the statusline badge count")
        ));
        for index_free in [
            "continue",
            "/compact",
            "ok",
            "<task-notification>\n<status>completed</status>\n</task-notification>",
        ] {
            assert!(
                !hook_needs_index("prompt-submit", &with_prompt(index_free)),
                "{index_free}"
            );
        }
        assert!(!hook_needs_index("prompt-submit", &HookInput::default()));
        // The prompt carve-out belongs to prompt-submit alone: session-start
        // has no prompt and always searches.
        assert!(hook_needs_index("session-start", &HookInput::default()));
        assert!(hook_needs_index("session-briefing", &HookInput::default()));
        assert!(!hook_needs_index(
            "stop",
            &with_prompt("fix the statusline badge count")
        ));
    }

    #[test]
    fn argv_names_the_retired_hook_only_as_the_hook_name() {
        let argv = |parts: &[&str]| {
            parts
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
        };
        for yes in [
            &["csr-engine", "hook", "subagent-stop"][..],
            &["csr-engine", "hook", "subagent-stop", "--db-path", "/x"],
            &["csr-engine", "--db-path", "/x", "hook", "subagent-stop"],
            &[
                "csr-engine",
                "hook",
                "--projects-dir",
                "/x",
                "subagent-stop",
            ],
            &["csr-engine", "hook", "--db-path=/x", "subagent-stop"],
        ] {
            assert!(
                argv_names_retired_hook(argv(&yes[0..]).into_iter().skip(1)),
                "{yes:?}"
            );
        }
        for no in [
            &["csr-engine", "hook", "--db-path", "subagent-stop", "stop"][..],
            &["csr-engine", "--db-path", "subagent-stop", "hook", "stop"],
            &["csr-engine", "hook", "stop", "subagent-stop"],
            &["csr-engine", "status", "subagent-stop"],
            &["csr-engine", "subagent-stop"],
            &["csr-engine", "hook"],
            &["csr-engine"],
        ] {
            assert!(
                !argv_names_retired_hook(argv(&no[0..]).into_iter().skip(1)),
                "{no:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn argv_scan_survives_non_utf8_values() {
        use std::os::unix::ffi::OsStringExt;
        let bad = std::ffi::OsString::from_vec(vec![b'/', 0xff, 0xfe]);
        let argv = |name: &str| vec!["hook".into(), name.into(), "--db-path".into(), bad.clone()];
        assert!(argv_names_retired_hook(argv("subagent-stop")));
        assert!(!argv_names_retired_hook(argv("stop")));
        assert!(!argv_names_retired_hook(vec![
            "hook".into(),
            "--db-path".into(),
            bad.clone(),
            "stop".into()
        ]));
    }

    #[test]
    fn only_subagent_stop_is_retired() {
        assert!(is_retired_hook("subagent-stop"));
        for live in [
            "session-start",
            "session-briefing",
            "session-end",
            "precompact",
            "stop",
            "post-tool-use",
            "prompt-submit",
            "install",
        ] {
            assert!(!is_retired_hook(live), "{live} is a live hook");
        }
    }

    #[test]
    fn no_hook_may_persist_the_index() {
        for hook in [
            "prompt-submit",
            "post-tool-use",
            "stop",
            "session-start",
            "session-briefing",
            "precompact",
            "session-end",
            "future-hook",
        ] {
            assert!(
                !hook_may_persist_index(hook),
                "{hook} must never publish HNSW"
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

#[cfg(test)]
mod r3_stdout_tests {
    #[test]
    fn r3_mcp_refuses_an_unclaimed_stdout_channel() {
        #[cfg(unix)]
        let failed = super::ClaimedStdout::claim_on(-1, -1);
        #[cfg(not(unix))]
        let failed = None;
        let error = super::ClaimedStdout::require(failed).err().unwrap();
        assert!(error
            .to_string()
            .contains("cannot claim stdout for the MCP channel"));
    }
}

#[cfg(test)]
mod maintenance_probes {
    use super::*;

    #[test]
    fn round3_hook_skips_before_storage_and_cache_miss_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("large.db");
        // Opening this sparse non-database would fail; the hook must not open it.
        std::fs::File::create(&db)
            .unwrap()
            .set_len(4 * 1024 * 1024 * 1024)
            .unwrap();
        let owner = crate::maintenance::MaintenanceLock::acquire(dir.path()).unwrap();
        let start = std::time::Instant::now();
        // The constructor also gates before probing a configured daemon or opening SQLite.
        assert!(Engine::new_for_hook(&db, &dir.path().join("projects"))
            .err()
            .unwrap()
            .to_string()
            .contains("maintenance in progress"));
        let mut constructed = false;
        if let Some(_operation) = try_start(&db) {
            constructed = true;
            let _ = Engine::new_for_hook(&db, &dir.path().join("projects"));
        }
        assert!(!constructed);
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        assert!(!dir.path().join("index-v3").exists());
        drop(owner);
        assert!(try_start(&db).is_some());
    }
}
