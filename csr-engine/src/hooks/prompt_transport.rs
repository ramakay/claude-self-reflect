//! Bounded, versioned prompt rendering over a private daemon Unix socket.
//!
//! The client never constructs an Engine. Missing/old/busy daemons fail open;
//! loading a gigabyte index is not a viable fallback within the hook budget.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;

use super::prompt_log::DecisionReason;
use super::HookInput;
use crate::engine::Engine;

#[path = "prompt_transport_completion.rs"]
mod completion;
use completion::completion_line;
#[cfg(test)]
use completion::decision_suffix;
#[path = "prompt_transport_server.rs"]
mod server;
#[cfg(all(test, unix))]
#[path = "prompt_transport_slow_tests.rs"]
mod slow_tests;

/// Connect plus reply over the daemon socket for the socket-only callers
/// ([`request`], [`post_compact`]). The prompt hook waits longer: see
/// [`resolve`].
pub const SOCKET_BUDGET: Duration = Duration::from_millis(250);
/// The daemon's deadline for one request from a client that does not say how
/// long it waits, from accept to the end of the reply, slot wait included. A
/// client that says so gets that wait less a small reply margin
/// (`server::REPLY_MARGIN`), so an answer it still waits for is delivered.
pub const REQUEST_DEADLINE: Duration = Duration::from_millis(420);
/// Deadline, measured from `main`, within which the in-process fallback
/// answers when the socket is refused or missing. The process wall adds the
/// exec and dynamic-link time before `main` (about 18 ms measured) and the
/// stdout flush after it, so 255 ms here keeps the whole hook at or under
/// 300 ms where 300 ms measured p95 318.5 ms.
pub const FALLBACK_BUDGET: Duration = Duration::from_millis(255);
/// Hard stop for the hook process. Nothing, stdin included, outlives it. At
/// 500 ms the three composed answers missed in build F ended 482-501 ms in.
pub const PROCESS_BUDGET: Duration = Duration::from_millis(560);
/// What the fallback leaves unspent of [`PROCESS_BUDGET`] after a stalled
/// socket: flush, exit, and scheduling jitter.
const FALLBACK_MARGIN: Duration = Duration::from_millis(40);
/// Bumped to 2 with `kind`. A daemon on another version never answers a
/// request of this one, and the client never accepts its answer.
const VERSION: u32 = 2;
const MAX_FRAME: usize = 256 * 1024;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    /// Render the UserPromptSubmit injection.
    #[default]
    Prompt,
    /// Store the PostCompact `compact_summary`.
    PostCompact,
}

#[derive(Serialize, Deserialize)]
struct Request {
    version: u32,
    #[serde(default)]
    kind: Kind,
    database: String,
    projects_dir: PathBuf,
    policy: Vec<Option<String>>,
    /// How long the client still waits for the reply, in ms, as it sends the
    /// request. Older clients send none.
    #[serde(default)]
    wait_ms: Option<u64>,
    input: HookInput,
}

// Rendering observes these switches. A differently configured daemon must
// abstain rather than silently use different ranking or injection policy.
const POLICY_KEYS: &[&str] = &[
    "CSR_NO_ANTI_PATTERNS",
    "CSR_NO_GRAPH_SLICES",
    "CSR_TRAINED_RERANK",
    "CSR_NO_DREAM_INJECT",
    "CSR_DREAM_CONSUMPTION",
    "CSR_NO_RECAP",
    "CSR_NO_VALIDITY_PARTITION",
    "CSR_NO_TIMING_DETAIL",
    crate::injection::send_gate::OFF_SWITCH,
];

fn policy() -> Vec<Option<String>> {
    POLICY_KEYS
        .iter()
        .map(|key| std::env::var(key).ok())
        .collect()
}

#[derive(Serialize, Deserialize)]
struct Response {
    version: u32,
    injection: String,
    #[serde(default)]
    reason: Option<DecisionReason>,
}

pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("hook-ipc-v1").join("prompt.sock")
}

/// Project and prompt id the watchdog logs if it kills the prompt hook.
static WATCHDOG_ROW: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
/// Set by whichever of the hook and the watchdog writes the timing line first.
static LINE_WRITTEN: AtomicBool = AtomicBool::new(false);

/// Start the independent deadline thread: at [`PROCESS_BUDGET`] after
/// `started` the process is gone. The thread also covers synchronous stdin,
/// filesystem calls, and runtime shutdown, which no Rust timeout can preempt.
/// Returns false when the thread cannot start (the caller then does nothing).
pub(super) fn spawn_watchdog(started: Instant) -> bool {
    let timing_probe = std::env::var_os("CSR_HOOK_TIMING_PROBE").is_some();
    std::thread::Builder::new()
        .name("prompt-deadline".into())
        .spawn(move || {
            let left = || PROCESS_BUDGET.saturating_sub(started.elapsed());
            std::thread::sleep(left().saturating_sub(WATCHDOG_LOG_WAIT));
            if timing_probe {
                eprintln!(
                    "CSR timing-probe deadline_total_ms={}",
                    started.elapsed().as_millis()
                );
            }
            // A kill before the hook wrote its own line would leave no trace of
            // the timeout, so the delivered share would not see it. Only the
            // prompt hook sets `WATCHDOG_ROW`; other users of this thread skip.
            // Opening the log can block (a FIFO nobody reads, a hung disk), so
            // the line gets what is left of the budget and no more.
            match WATCHDOG_ROW.get() {
                Some((project, id)) if !LINE_WRITTEN.swap(true, Ordering::SeqCst) => {
                    let line = watchdog_line(project, id, started.elapsed());
                    finish_within(move || crate::telemetry::append_timing_line(&line), left());
                }
                _ => std::thread::sleep(left()),
            }
            // Rust's exit cleanup flushes stdout and can deadlock behind a
            // writer holding its lock on a full pipe. This client owns no DB
            // or index state; bypass all cleanup to enforce its deadline.
            #[cfg(unix)]
            unsafe {
                libc::_exit(0)
            }
            #[cfg(not(unix))]
            std::process::exit(0);
        })
        .is_ok()
}

/// The end of the budget kept for the watchdog's timing line.
const WATCHDOG_LOG_WAIT: Duration = Duration::from_millis(15);

/// Run `work` on its own thread and wait for it at most `wait`; true when it
/// finished. A blocked `work` is left behind: the watchdog exits next.
fn finish_within(work: impl FnOnce() + Send + 'static, wait: Duration) -> bool {
    let (done, finished) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("prompt-deadline-log".into())
        .spawn(move || {
            work();
            let _ = done.send(());
        });
    spawned.is_ok() && finished.recv_timeout(wait).is_ok()
}

fn watchdog_line(project: &str, id: &str, elapsed: Duration) -> String {
    format!(
        "CSR hook prompt-submit [{project}]: total={}ms transport=unavailable prompt_id={id} includes_startup=1 injected=0 watchdog=1",
        elapsed.as_millis()
    )
}

/// How a prompt was answered, for the timing line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transport {
    /// The daemon answered over the socket.
    Socket,
    /// The in-process fallback answered.
    Fallback,
    /// Nothing was injected.
    Unavailable,
}

impl Transport {
    fn label(self) -> &'static str {
        match self {
            Self::Socket => "socket",
            Self::Fallback => "fallback",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Pre-dispatch step. Every prompt starts a model turn, so the mark goes down
/// before the early-exit check; precompact::decide_veto reads it. Returns false
/// when the early-exit check answers silently.
fn pre_dispatch(
    db_path: &Path,
    source: Option<&str>,
    prompt: &str,
    session_id: Option<&str>,
) -> bool {
    record_turn_start(db_path, session_id);
    !matches!(
        super::prompt_submit::early_exit_for(source, prompt),
        Some(super::prompt_submit::EarlyExit::Silent)
    )
}

/// The timing row for a prompt the client answered silently because a person
/// did not write it, so the abstain can be counted. `transport=skipped`: no
/// socket or fallback ran, and delivery shares (which read socket rows) are
/// unchanged. `None` for every other silent exit (slash command, empty prompt).
fn machine_prompt_line(input: &HookInput) -> Option<String> {
    let prompt = input.prompt.as_deref().unwrap_or_default();
    if !super::prompt_submit::is_machine_prompt(input.source.as_deref(), prompt) {
        return None;
    }
    let project = crate::search::cross_project::resolve_project_from_cwd(
        &super::resolve_hook_cwd(input.cwd.as_deref()).to_string_lossy(),
    )
    .unwrap_or_else(|| "unknown".into());
    Some(format!(
        "CSR hook prompt-submit [{project}]: transport=skipped prompt_id={} injected=0 decided=abstain reason={}",
        input
            .prompt_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .unwrap_or("-"),
        DecisionReason::MachinePrompt.name(),
    ))
}

/// Record the turn start for a prompt that carries a session id.
fn record_turn_start(db_path: &Path, session_id: Option<&str>) {
    if let (Some(data_dir), Some(session)) = (db_path.parent(), session_id) {
        super::turn_marks::mark_prompt(data_dir, session);
    }
}

/// Called before runtime/storage construction. Leave scheduling margin below 3s.
pub fn run_client(db_path: &Path, projects_dir: &Path, started: Instant) {
    let Some(_operation) = super::try_start(db_path) else {
        return;
    };
    if !spawn_watchdog(started) {
        return;
    }
    if std::env::var("CSR_DISABLE_RECURSIVE_HOOKS").as_deref() == Ok("1") {
        return;
    }
    let stdin_started = Instant::now();
    let mut input = super::read_stdin_json();
    let stdin_elapsed = stdin_started.elapsed();
    let dispatch = pre_dispatch(
        db_path,
        input.source.as_deref(),
        input.prompt.as_deref().unwrap_or_default(),
        input.session_id.as_deref(),
    );
    if !dispatch {
        if let Some(line) = machine_prompt_line(&input) {
            if !LINE_WRITTEN.swap(true, Ordering::SeqCst) {
                crate::telemetry::append_timing_line(&line);
            }
        }
        return;
    }
    // Resolve fallback cwd in the hook process, not in the daemon (whose cwd
    // is unrelated to the session). Carry the same canonical path as dispatch.
    input.cwd = Some(
        super::resolve_hook_cwd(input.cwd.as_deref())
            .to_string_lossy()
            .into_owned(),
    );
    let project = crate::search::cross_project::resolve_project_from_cwd(
        input.cwd.as_deref().unwrap_or_default(),
    )
    .unwrap_or_else(|| "unknown".into());
    let prompt_id = input.prompt_id.clone();
    let _ = WATCHDOG_ROW.set((
        project.clone(),
        prompt_id
            .clone()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| "-".into()),
    ));
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let setup_elapsed = started.elapsed().saturating_sub(stdin_elapsed);
    let request_started = Instant::now();
    let (answer, transport, reason) = runtime.block_on(resolve_decision(
        db_path,
        projects_dir,
        input,
        started,
        |output| {
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all(output.as_bytes())
                .and_then(|_| stdout.flush())
                .is_ok()
        },
    ));
    runtime.shutdown_background();
    let line = format!(
        "CSR hook prompt-submit [{}]: stdin={}ms setup={}ms hook={}ms flush=0ms total={}ms transport={} prompt_id={} includes_startup=1 injected={}",
        project, stdin_elapsed.as_millis(), setup_elapsed.as_millis(),
        request_started.elapsed().as_millis(), started.elapsed().as_millis(),
        transport.label(),
        prompt_id.as_deref().filter(|id| !id.is_empty()).unwrap_or("-"),
        answer.as_deref().map_or(0, str::len),
    );
    let Some(line) = completion_line(line, transport, reason, &LINE_WRITTEN) else {
        return;
    };
    eprintln!("{line}");
    crate::telemetry::append_timing_line(&line);
}

/// Answer a prompt: the daemon socket first, the in-process fallback when the
/// socket cannot. `deliver` writes the answer to the hook's stdout and says
/// whether it landed; only a delivered answer counts.
///
/// - refused or missing socket: fallback, by `started + FALLBACK_BUDGET`;
/// - socket that accepts and gives no answer in time: the socket gets until
///   `started + PROCESS_BUDGET - FALLBACK_MARGIN` (520 ms), so a render queued
///   behind another still lands; at that cutoff nothing is left for the
///   fallback, so the prompt goes ahead without recall;
/// - socket that accepts and drops, or garbles, at once: fallback with what
///   remains of [`PROCESS_BUDGET`];
/// - reply from another protocol version, or a stdout that fails: say nothing.
#[cfg(test)]
pub(crate) async fn resolve(
    db_path: &Path,
    projects_dir: &Path,
    input: HookInput,
    started: Instant,
    deliver: impl FnOnce(&str) -> bool,
) -> (Option<String>, Transport) {
    let (text, transport, _) =
        resolve_decision(db_path, projects_dir, input, started, deliver).await;
    (text, transport)
}

async fn resolve_decision(
    db_path: &Path,
    projects_dir: &Path,
    input: HookInput,
    started: Instant,
    deliver: impl FnOnce(&str) -> bool,
) -> (Option<String>, Transport, Option<DecisionReason>) {
    let mut deliver = Some(deliver);
    let cutoff = started + PROCESS_BUDGET.saturating_sub(FALLBACK_MARGIN);
    let socket = exchange(
        db_path,
        projects_dir,
        Kind::Prompt,
        input.clone(),
        cutoff.saturating_duration_since(Instant::now()),
        |text| deliver.take().is_some_and(|deliver| deliver(text)),
    )
    .await;
    let deadline = match socket {
        Socket::Answered(text, reason) => return (Some(text), Transport::Socket, reason),
        Socket::Abstained | Socket::TimedOut => return (None, Transport::Unavailable, None),
        Socket::Refused => started + FALLBACK_BUDGET,
        Socket::Slow => cutoff,
    };
    let Some(deliver) = deliver else {
        return (None, Transport::Unavailable, None);
    };
    let db = db_path.to_path_buf();
    let rendered = super::prompt_fallback::render(&db, &input, deadline);
    match rendered {
        Some(text) if deliver(&text) => (Some(text), Transport::Fallback, None),
        _ => (None, Transport::Unavailable, None),
    }
}

async fn read_frame<R: tokio::io::AsyncRead + Unpin>(reader: &mut R) -> Option<Vec<u8>> {
    let size = reader.read_u32().await.ok()? as usize;
    if size > MAX_FRAME {
        return None;
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).await.ok()?;
    Some(bytes)
}

async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(writer: &mut W, bytes: &[u8]) -> Option<()> {
    if bytes.len() > MAX_FRAME {
        return None;
    }
    writer.write_u32(bytes.len() as u32).await.ok()?;
    writer.write_all(bytes).await.ok()?;
    Some(())
}

/// Socket-only exchange for a prompt: the daemon's answer or nothing. No fallback.
pub async fn request(db_path: &Path, projects_dir: &Path, input: HookInput) -> Option<String> {
    match exchange(
        db_path,
        projects_dir,
        Kind::Prompt,
        input,
        SOCKET_BUDGET,
        |_| true,
    )
    .await
    {
        Socket::Answered(text, _) => Some(text),
        _ => None,
    }
}

/// Ask the daemon to store a PostCompact summary. True once it answered.
pub(crate) async fn post_compact(db_path: &Path, projects_dir: &Path, input: HookInput) -> bool {
    matches!(
        exchange(
            db_path,
            projects_dir,
            Kind::PostCompact,
            input,
            SOCKET_BUDGET,
            |_| true,
        )
        .await,
        Socket::Answered(..)
    )
}

/// What one socket exchange came to.
#[derive(Debug, PartialEq, Eq)]
enum Socket {
    /// A valid reply of this protocol version, accepted by `deliver`.
    Answered(String, Option<DecisionReason>),
    /// The daemon replied but the answer cannot be used (other version) or the
    /// caller could not deliver it. Nothing more is tried.
    Abstained,
    /// Nobody is listening: the socket is missing or refuses connections.
    Refused,
    /// Someone is listening and gave no usable answer at once: dropped the
    /// connection, failed a write, or sent garbage.
    Slow,
    /// The daemon accepted and gave no answer before the budget ran out.
    /// Nothing is left for a fallback.
    TimedOut,
}

async fn exchange(
    db_path: &Path,
    projects_dir: &Path,
    kind: Kind,
    input: HookInput,
    budget: Duration,
    deliver: impl FnOnce(&str) -> bool,
) -> Socket {
    #[cfg(unix)]
    {
        let waits_until = Instant::now() + budget;
        tokio::time::timeout(budget, async {
            let Some(data_dir) = db_path.parent() else {
                return Socket::Refused;
            };
            let Ok(mut stream) = tokio::net::UnixStream::connect(socket_path(data_dir)).await
            else {
                return Socket::Refused;
            };
            let Some(database) = db_path.file_name().and_then(|n| n.to_str()) else {
                return Socket::Abstained;
            };
            let Ok(bytes) = serde_json::to_vec(&Request {
                version: VERSION,
                kind,
                database: database.to_owned(),
                projects_dir: projects_dir
                    .canonicalize()
                    .unwrap_or_else(|_| projects_dir.to_path_buf()),
                policy: policy(),
                wait_ms: Some(
                    waits_until
                        .saturating_duration_since(Instant::now())
                        .as_millis() as u64,
                ),
                input,
            }) else {
                return Socket::Abstained;
            };
            if write_frame(&mut stream, &bytes).await.is_none() {
                return Socket::Slow;
            }
            let Some(bytes) = read_frame(&mut stream).await else {
                return Socket::Slow;
            };
            let Ok(response) = serde_json::from_slice::<Response>(&bytes) else {
                return Socket::Slow;
            };
            if response.version != VERSION || !deliver(&response.injection) {
                return Socket::Abstained;
            }
            // Acknowledge only a fully validated response accepted by the stdout
            // writer. Failed/partial/timed-out replies never commit exposure state.
            let _ = stream.write_u8(1).await;
            Socket::Answered(response.injection, response.reason)
        })
        .await
        .unwrap_or(Socket::TimedOut)
    }
    #[cfg(not(unix))]
    {
        let _ = (db_path, projects_dir, kind, input, budget, deliver);
        Socket::Refused
    }
}

#[cfg(unix)]
pub(crate) struct BoundSocket {
    pub(crate) listener: tokio::net::UnixListener,
    path: PathBuf,
    // Held until the listener and socket path have been removed. A second
    // daemon must never unlink the first daemon's active endpoint.
    _lock: std::fs::File,
}

#[cfg(unix)]
impl BoundSocket {
    fn bind(data_dir: &Path) -> std::io::Result<Self> {
        Self::bind_named(data_dir, "prompt.sock", "server.lock")
    }

    /// Bind `<data_dir>/hook-ipc-v1/<socket>` under the same private
    /// directory and single-owner lock rules as the prompt socket.
    pub(crate) fn bind_named(data_dir: &Path, socket: &str, lock: &str) -> std::io::Result<Self> {
        use std::os::unix::fs::{
            DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt,
        };
        let path = socket_path(data_dir).with_file_name(socket);
        let dir = path.parent().expect("socket has parent");
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let meta = std::fs::symlink_metadata(dir)?;
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "private hook directory required",
            ));
        }
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(dir.join(lock))?;
        let lock = lock_within(lock, LOCK_WAIT)?;
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(&path)?,
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "non-socket at hook path",
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = tokio::net::UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            listener,
            path,
            _lock: lock,
        })
    }
}

/// How long a bind waits for the endpoint lock before deciding another
/// server holds it.
#[cfg(unix)]
const LOCK_WAIT: Duration = Duration::from_secs(2);

/// Take the exclusive endpoint lock before the deadline, releasing the file on failure.
#[cfg(unix)]
fn lock_within(file: std::fs::File, wait: Duration) -> std::io::Result<std::fs::File> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(error) if error.kind() != std::io::ErrorKind::WouldBlock => return Err(error),
            Err(error) => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return Err(error);
                }
                std::thread::sleep(remaining.min(Duration::from_millis(5)));
            }
        }
    }
}

#[cfg(unix)]
impl Drop for BoundSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Start an independent endpoint sharing the daemon's warm engine. Single
/// flight bounds work even if a client disappears while SQLite/ONNX is busy.
/// How often the daemon re-measures the reflection frame vocabulary; half its
/// 6 h validity, so renders never find it stale.
const COMMON_TERMS_REFRESH: Duration = Duration::from_secs(3 * 60 * 60);

/// Prompt work never takes the daemon heavy-work permit: a stuck renderer must
/// not prevent reconciliation, imports, or enrichment from making progress.
pub fn spawn_for_daemon(
    engine: Arc<Engine>,
    shutdown: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    server::spawn(engine, shutdown, false)
}

/// The socket server of `csr-engine daemon --serve-only`: the same endpoint
/// and renderer as the daemon's, on a warm engine, with every database
/// connection set `query_only` and no background refresh, so serving a prompt
/// changes nothing on disk. Receipt writes (retrieval events, exposure
/// impressions) and dream claims therefore fail open instead of landing.
pub fn spawn_serve_only(
    engine: Arc<Engine>,
    shutdown: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    server::spawn(engine, shutdown, true)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The watchdog's line goes to a log whose open blocks (a FIFO nobody
    /// reads): the wait ends on time and the watchdog can still exit.
    #[test]
    #[cfg(unix)]
    fn a_blocking_timing_log_cannot_hold_the_watchdog_past_its_wait() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("timing.log");
        let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let path = fifo.clone();
        let started = Instant::now();
        let open = move || drop(crate::private_files::append_private(&path));
        assert!(!finish_within(open, WATCHDOG_LOG_WAIT));
        assert!(
            started.elapsed() < WATCHDOG_LOG_WAIT * 4,
            "{:?}",
            started.elapsed()
        );
        // A reader frees the blocked open; a quick write finishes in time.
        let mut reader = std::fs::OpenOptions::new();
        std::os::unix::fs::OpenOptionsExt::custom_flags(reader.read(true), libc::O_NONBLOCK);
        let _reader = reader.open(&fifo);
        assert!(finish_within(|| {}, Duration::from_secs(1)));
    }

    #[tokio::test]
    async fn completed_socket_decisions_keep_each_reason_on_the_timing_row() {
        for (reason, fields) in [
            (DecisionReason::OnTopic, "decided=block reason=on_topic"),
            (
                DecisionReason::LookalikeOnly,
                "decided=abstain reason=lookalike_only",
            ),
            (
                DecisionReason::BelowFloor,
                "decided=abstain reason=below_floor",
            ),
            (DecisionReason::ThinPool, "decided=abstain reason=thin_pool"),
            (
                DecisionReason::NoCandidates,
                "decided=abstain reason=no_candidates",
            ),
            (DecisionReason::TooShort, "decided=abstain reason=too_short"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let bound = BoundSocket::bind(dir.path()).unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = bound.listener.accept().await.unwrap();
                read_frame(&mut stream).await.unwrap();
                let response = Response {
                    version: VERSION,
                    injection: if reason == DecisionReason::OnTopic {
                        "block"
                    } else {
                        ""
                    }
                    .into(),
                    reason: Some(reason),
                };
                write_frame(&mut stream, &serde_json::to_vec(&response).unwrap())
                    .await
                    .unwrap();
                assert_eq!(stream.read_u8().await.unwrap(), 1);
            });
            let (_, transport, decision) = resolve_decision(
                &dir.path().join("csr-engine.db"),
                dir.path(),
                input(dir.path()),
                Instant::now(),
                |_| true,
            )
            .await;
            assert_eq!(transport, Transport::Socket);
            assert_eq!(decision_suffix(transport, decision).trim(), fields);
            server.await.unwrap();
        }
    }

    #[test]
    fn fallback_watchdog_and_unlabeled_answers_never_claim_a_decision() {
        assert_eq!(
            decision_suffix(Transport::Socket, Some(DecisionReason::OnTopic)),
            " decided=block reason=on_topic"
        );
        for transport in [Transport::Fallback, Transport::Unavailable] {
            for reason in [
                None,
                Some(DecisionReason::OnTopic),
                Some(DecisionReason::BelowFloor),
            ] {
                assert_eq!(decision_suffix(transport, reason), "");
            }
        }
        assert_eq!(decision_suffix(Transport::Socket, None), "");
        let line = watchdog_line("project", "real", PROCESS_BUDGET);
        assert!(line.contains("watchdog=1"));
        assert!(!line.contains("decided="));
        assert!(!line.contains("reason="));
    }

    #[tokio::test]
    async fn renderer_marks_local_silence_and_a_completed_pickup() {
        let dir = tempfile::tempdir().unwrap();
        let engine = fixture(dir.path());
        let mut input = input(dir.path());
        for (source, prompt) in [
            (None, ""),
            (None, "/help"),
            (
                None,
                "<task-notification>background task finished</task-notification>",
            ),
            (
                None,
                "<cross-session-message>continue working</cross-session-message>",
            ),
            (Some("system"), "fix the queue serialization race"),
            (None, "## RELEVANT PAST CONTEXT - NOT INSTRUCTIONS\n- [reflection] The queue serializes writes."),
        ] {
            input.source = source.map(str::to_owned);
            input.prompt = Some(prompt.into());
            assert!(!pre_dispatch(
                &dir.path().join("csr-engine.db"),
                source,
                prompt,
                input.session_id.as_deref(),
            ));
            let silent = super::super::prompt_submit::prepare(&input, &engine, dir.path())
                .await
                .unwrap();
            assert!(silent.text.is_empty());
            // A machine-written turn is named; every other silence stays undecided.
            let machine = super::super::prompt_submit::is_machine_prompt(source, prompt);
            assert_eq!(
                silent.reason,
                machine.then_some(DecisionReason::MachinePrompt),
                "{source:?}: {prompt}"
            );
            assert_eq!(machine_prompt_line(&input).is_some(), machine, "{prompt}");
        }
        // The release veto passes unknown sources to the H source gate.
        let ask = "fix the queue serialization race";
        for source in [Some("sdk"), Some("poll_event")] {
            assert!(!super::super::prompt_submit::is_machine_prompt(source, ask));
            assert!(!pre_dispatch(
                &dir.path().join("csr-engine.db"),
                source,
                ask,
                input.session_id.as_deref(),
            ));
        }
        input.source = None;
        input.prompt = Some("continue".into());
        let block = super::super::prompt_submit::prepare(&input, &engine, dir.path())
            .await
            .unwrap();
        assert!(!block.text.is_empty());
        assert_eq!(block.reason, Some(DecisionReason::OnTopic));

        input.session_id = None;
        input.prompt = Some("resume".into());
        assert!(pre_dispatch(
            &dir.path().join("csr-engine.db"),
            None,
            "resume",
            None,
        ));
        let resumed = super::super::prompt_submit::prepare(&input, &engine, dir.path())
            .await
            .unwrap();
        assert_eq!(resumed.text, block.text);
        assert_eq!(resumed.reason, Some(DecisionReason::OnTopic));
    }

    async fn request(dir: &Path, input: HookInput) -> Option<String> {
        super::request(&dir.join("csr-engine.db"), dir, input).await
    }

    pub(super) fn fixture(dir: &Path) -> Arc<Engine> {
        fixture_over(dir, crate::storage::Storage::open_memory().unwrap())
    }

    pub(super) fn fixture_over(dir: &Path, storage: crate::storage::Storage) -> Arc<Engine> {
        let storage = Arc::new(storage);
        let project =
            crate::search::cross_project::resolve_project_from_cwd(&dir.to_string_lossy()).unwrap();
        let episode = serde_json::json!({
            "schema": "csr_episode_v1", "session_id": "previous", "project": project,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "request": "Fix token validation in the authentication middleware",
            "investigated": [], "completed": "Fixed token validation and verified the regression",
            "next_steps": "Deploy to staging", "blockers": null, "outcome": "partial",
            "error_signatures": [], "tools_used": [], "files_modified": [],
            "message_count": 10, "duration_minutes": 5
        });
        storage
            .insert_reflection(
                "episode",
                &episode.to_string(),
                &["session_episode".into(), format!("project_{project}")],
                &[0.0; 384],
            )
            .unwrap();
        Arc::new(Engine::from_parts_with_index_dir(
            storage,
            Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
            Arc::new(tokio::sync::RwLock::new(crate::search::SearchEngine::new(
                10,
            ))),
            dir.to_path_buf(),
            dir.join("index-v3"),
        ))
    }

    pub(super) fn input(dir: &Path) -> HookInput {
        HookInput {
            prompt: Some("continue".into()),
            cwd: Some(dir.to_string_lossy().into()),
            session_id: Some("current".into()),
            ..Default::default()
        }
    }

    /// One fresh `chunk|external` item from an earlier session in the
    /// project of `dir`, the kind the automatic block carries.
    async fn seed_solution(engine: &Engine, dir: &Path, content: &str, vector: Vec<f32>) {
        let chunk = crate::import::ConversationChunk {
            id: "solution".into(),
            conversation_id: "earlier".into(),
            project_name: crate::search::cross_project::resolve_project_from_cwd(
                &dir.to_string_lossy(),
            )
            .unwrap_or_default(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::Assistant,
            seq: 0,
            is_sidechain: false,
        };
        engine.storage().insert_chunk(&chunk, &vector).unwrap();
        engine
            .storage()
            .with_connection(|conn| Ok(conn.execute("UPDATE chunks SET min_trust = 1", [])?))
            .unwrap();
        engine.search().write().await.insert_chunk(chunk.id, vector);
    }

    pub(super) async fn ready(dir: &Path) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !socket_path(dir).exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
    async fn daemon_returns_exact_nonempty_render_and_never_imports_or_dumps() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let engine = fixture(dir.path());
        let mut input = input(dir.path());
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(&transcript, "do not import this transcript").unwrap();
        input.transcript_path = Some(transcript.to_string_lossy().into());
        // Uncommitted: a committed pickup is not sent to the session again.
        let expected = super::super::prompt_submit::render_uncommitted(&input, &engine, dir.path())
            .await
            .unwrap();
        assert!(expected.starts_with("CSR PICKUP —"), "{expected}");
        assert!(expected.contains("Fixed token validation"));
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = spawn_for_daemon(engine.clone(), shutdown.clone());
        ready(dir.path()).await;
        assert_eq!(request(dir.path(), input).await, Some(expected));
        assert!(!engine.embeddings().is_loaded());
        assert!(!dir.path().join("index-v3").exists());
        assert_eq!(
            std::fs::read_to_string(transcript).unwrap(),
            "do not import this transcript"
        );
        shutdown.store(true, Ordering::SeqCst);
        server.await.unwrap();
        assert!(!socket_path(dir.path()).exists());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
    async fn absent_and_stale_daemons_fail_open_and_stale_socket_recovers() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        let dir = tempfile::tempdir().unwrap();
        assert!(request(dir.path(), input(dir.path())).await.is_none());
        let bound = BoundSocket::bind(dir.path()).unwrap();
        // A second server must not steal an active endpoint.
        assert!(BoundSocket::bind(dir.path()).is_err());
        drop(bound);
        let stale = std::os::unix::net::UnixListener::bind(socket_path(dir.path())).unwrap();
        drop(stale);
        assert!(request(dir.path(), input(dir.path())).await.is_none());
        let recovered = BoundSocket::bind(dir.path()).unwrap();
        drop(recovered);
        // Prime the renderer in-process first: the request below measures the
        // endpoint's recovery, not a cold first render against the budget.
        let engine = fixture(dir.path());
        super::super::prompt_submit::render_uncommitted(&input(dir.path()), &engine, dir.path())
            .await
            .unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = spawn_for_daemon(engine, shutdown.clone());
        ready(dir.path()).await;
        assert!(request(dir.path(), input(dir.path()))
            .await
            .unwrap()
            .contains("CSR PICKUP"));
        shutdown.store(true, Ordering::SeqCst);
        server.await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
    async fn warm_semantic_search_matches_direct_renderer_exactly() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        let _gate = crate::injection::send_gate::Switch::off(); // one item is a thin pool
        let dir = tempfile::tempdir().unwrap();
        let engine = fixture(dir.path());
        let mut input = input(dir.path());
        input.prompt =
            Some("authentication timeout regression in token validation needs a fix".into());
        let vector = engine
            .embeddings()
            .embed_single(input.prompt.as_deref().unwrap())
            .unwrap();
        let content =
            "Token validation regression was fixed by refreshing expired credentials before retry.";
        seed_solution(&engine, dir.path(), content, vector).await;
        // Uncommitted: a committed render would mark the item delivered to this
        // session, and the socket render would rightly not send it again.
        let expected = super::super::prompt_submit::render_uncommitted(&input, &engine, dir.path())
            .await
            .unwrap();
        assert!(
            expected.contains(content),
            "semantic fixture must inject content: {expected}"
        );
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = spawn_for_daemon(engine, shutdown.clone());
        ready(dir.path()).await;
        assert_eq!(request(dir.path(), input).await, Some(expected));
        shutdown.store(true, Ordering::SeqCst);
        server.await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
    async fn timeout_retains_admission_until_blocked_renderer_finishes() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let engine = fixture(dir.path());
        let storage = engine.storage().clone();
        let evidence = storage.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = spawn_for_daemon(engine, shutdown.clone());
        ready(dir.path()).await;
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            storage
                .with_connection(|_| {
                    locked_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok(())
                })
                .unwrap();
        });
        locked_rx.await.unwrap();
        let started = Instant::now();
        assert!(request(dir.path(), input(dir.path())).await.is_none());
        assert!(started.elapsed() < Duration::from_millis(1200));
        let started = Instant::now();
        assert!(request(dir.path(), input(dir.path())).await.is_none());
        assert!(started.elapsed() < Duration::from_millis(500));
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        let impressions: i64 = evidence
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM rerank_exposure_impressions",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(
            impressions, 0,
            "timed-out context must not be recorded as shown"
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if request(dir.path(), input(dir.path())).await.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        shutdown.store(true, Ordering::SeqCst);
        server.await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
    async fn render_is_served_while_the_daemon_connection_is_held() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        assert!(crate::storage::Storage::open_memory()
            .unwrap()
            .open_prompt_lane()
            .unwrap()
            .is_none());
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::storage::Storage::open(&dir.path().join("csr-engine.db")).unwrap();
        let engine = fixture_over(dir.path(), storage);
        // Prime the renderer so the request below measures the held
        // connection, not a cold first render against the budget.
        let ask = input(dir.path());
        let expected = super::super::prompt_submit::render_uncommitted(&ask, &engine, dir.path())
            .await
            .unwrap();
        let held = engine.storage().clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = spawn_for_daemon(engine, shutdown.clone());
        ready(dir.path()).await;
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        // A background loop in the middle of a long statement.
        let holder = std::thread::spawn(move || {
            held.with_connection(|_| {
                locked_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(20)).unwrap();
                Ok(())
            })
            .unwrap();
        });
        locked_rx.await.unwrap();
        let mut served = None;
        // The daemon drops a connection that arrives while it is still
        // finishing the last one, so a loaded host may need a few tries.
        for _ in 0..40 {
            served = request(dir.path(), input(dir.path())).await;
            if served.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(served, Some(expected));
        shutdown.store(true, Ordering::SeqCst);
        server.await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
    async fn receipts_wait_for_the_writer_without_holding_admission() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        let _gate = crate::injection::send_gate::Switch::off(); // one item is a thin pool
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("csr-engine.db");
        let engine = fixture_over(dir.path(), crate::storage::Storage::open(&db).unwrap());
        let mut input = input(dir.path());
        input.prompt =
            Some("authentication timeout regression in token validation needs a fix".into());
        let vector = engine
            .embeddings()
            .embed_single(input.prompt.as_deref().unwrap())
            .unwrap();
        seed_solution(
            &engine,
            dir.path(),
            "Token validation regression was fixed by refreshing expired credentials before retry.",
            vector,
        )
        .await;
        // Uncommitted, so the socket render below is not a repeat delivery.
        let expected = super::super::prompt_submit::render_uncommitted(&input, &engine, dir.path())
            .await
            .unwrap();
        assert!(
            expected.contains("refreshing expired credentials"),
            "{expected}"
        );
        let storage = engine.storage().clone();
        let events = |storage: &crate::storage::Storage| -> i64 {
            storage
                .with_connection(|conn| {
                    Ok(
                        conn.query_row("SELECT COUNT(*) FROM retrieval_events", [], |row| {
                            row.get(0)
                        })?,
                    )
                })
                .unwrap()
        };
        let before = events(&storage);
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = spawn_for_daemon(engine, shutdown.clone());
        ready(dir.path()).await;

        // A daemon write transaction is open. Receipts for the first reply have
        // to wait for it; the second prompt must not.
        let writer = rusqlite::Connection::open(&db).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        let mut first = None;
        for _ in 0..40 {
            first = request(dir.path(), input.clone()).await;
            if first.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(first, Some(expected));
        let started = Instant::now();
        let mut second = None;
        while started.elapsed() < Duration::from_millis(1500) {
            second = request(dir.path(), input.clone()).await;
            if second.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            second.is_some(),
            "a prompt was refused while receipts waited for the writer"
        );
        assert_eq!(events(&storage), before, "receipts cannot have landed yet");

        // Once the writer commits, the receipts land: none were dropped.
        writer.execute_batch("COMMIT").unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while events(&storage) <= before {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the retrieval events of the first reply were written");
        shutdown.store(true, Ordering::SeqCst);
        server.await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // Stabilize policy reads without mutating process environment.
    async fn mismatched_render_policy_abstains_for_every_switch() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = spawn_for_daemon(fixture(dir.path()), shutdown.clone());
        ready(dir.path()).await;
        assert!(POLICY_KEYS.contains(&"CSR_NO_VALIDITY_PARTITION"));
        for (index, key) in POLICY_KEYS.iter().enumerate() {
            let mut client_policy = policy();
            client_policy[index] = if client_policy[index].as_deref() == Some("1") {
                None
            } else {
                Some("1".into())
            };
            let bytes = serde_json::to_vec(&Request {
                version: VERSION,
                kind: Kind::Prompt,
                database: "csr-engine.db".into(),
                projects_dir: dir.path().canonicalize().unwrap(),
                policy: client_policy,
                wait_ms: None,
                input: input(dir.path()),
            })
            .unwrap();
            // The socket file appears at bind(2), before listen(2), so `ready` can return while
            // connect is still refused. Probing with a connect would take the daemon's only
            // request slot, so the retry lives here instead.
            let connect_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match tokio::net::UnixStream::connect(socket_path(dir.path())).await {
                    Ok(stream) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                        assert!(
                            tokio::time::Instant::now() < connect_deadline,
                            "the daemon socket still refused connections after 10 s ({key})"
                        );
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            write_frame(&mut stream, &bytes).await.unwrap();
            // This asserts that the daemon abstains, not that it answers within the render
            // budget, so the wait tolerates a loaded host.
            assert!(
                tokio::time::timeout(Duration::from_secs(60), read_frame(&mut stream))
                    .await
                    .unwrap()
                    .is_none(),
                "daemon must abstain when {key} differs"
            );
        }
        // Matching configuration still renders after all rejected requests. The daemon serves one
        // request at a time and drops a connection that arrives while the last is still being
        // torn down, so a loaded host may need a few tries.
        let mut rendered = None;
        for _ in 0..40 {
            rendered = request(dir.path(), input(dir.path())).await;
            if rendered.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(rendered.is_some());
        shutdown.store(true, Ordering::SeqCst);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_wrong_version_oversized_and_stalled_replies_are_silent() {
        for case in 0..5 {
            let dir = tempfile::tempdir().unwrap();
            let bound = BoundSocket::bind(dir.path()).unwrap();
            let peer = tokio::spawn(async move {
                let (mut stream, _) = bound.listener.accept().await.unwrap();
                read_frame(&mut stream).await.unwrap();
                match case {
                    0 => {
                        write_frame(&mut stream, b"not json").await;
                    }
                    1 => {
                        write_frame(&mut stream, br#"{"version":0,"injection":"wrong"}"#).await;
                    }
                    2 => {
                        stream.write_u32(MAX_FRAME as u32 + 1).await.unwrap();
                    }
                    3 => {
                        tokio::time::sleep(Duration::from_millis(1000)).await;
                    }
                    _ => {
                        stream.write_u32(100).await.unwrap();
                        stream.write_all(b"partial").await.unwrap();
                    }
                }
            });
            assert!(
                request(dir.path(), input(dir.path())).await.is_none(),
                "case {case}"
            );
            peer.await.unwrap();
        }
    }

    #[tokio::test]
    async fn socket_is_private_and_rejects_non_socket_paths() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bound = BoundSocket::bind(dir.path()).unwrap();
        assert_eq!(
            std::fs::metadata(&bound.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(bound.path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        drop(bound);
        std::fs::write(socket_path(dir.path()), "keep me").unwrap();
        assert!(BoundSocket::bind(dir.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(socket_path(dir.path())).unwrap(),
            "keep me"
        );
    }
    // ---- FS1: stalled and old daemons, fallback, PostCompact over the socket ----

    const FALLBACK_TEXT: &str =
        "The refresh path in src/auth/token.rs rejects expired tokens before the retry middleware runs.";

    /// A file database holding chunks the fallback can find by identifier and
    /// by words, one in each of `sessions` sessions (three clear the send
    /// gate). The storage is returned to keep the database open.
    fn fallback_db_of(dir: &Path, sessions: usize) -> (PathBuf, crate::storage::Storage) {
        let db = dir.join("csr-engine.db");
        let storage = crate::storage::Storage::open(&db).unwrap();
        for n in 0..sessions {
            storage
                .insert_chunk(
                    &crate::import::ConversationChunk {
                        id: format!("fb-{n}"),
                        conversation_id: format!("abcdef1{n}-0000-4000-8000-000000000000"),
                        project_name: "proj".into(),
                        timestamp: "2026-10-01T00:00:00Z".into(),
                        content: FALLBACK_TEXT.into(),
                        message_count: 2,
                        summary: None,
                        author: crate::provenance::Speaker::User,
                        seq: 0,
                        is_sidechain: false,
                    },
                    &[0.0; 384],
                )
                .unwrap();
        }
        // The `external` floor, the kind the automatic block carries.
        storage
            .with_connection(|conn| Ok(conn.execute("UPDATE chunks SET min_trust = 1", [])?))
            .unwrap();
        (db, storage)
    }

    fn fallback_db(dir: &Path) -> (PathBuf, crate::storage::Storage) {
        fallback_db_of(dir, 3)
    }

    fn fallback_input(dir: &Path) -> HookInput {
        HookInput {
            prompt: Some(FALLBACK_TEXT.into()),
            cwd: Some(dir.to_string_lossy().into()),
            session_id: Some("some-other-session".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn stalled_listener_gets_the_whole_socket_budget_and_no_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _storage) = fallback_db(dir.path());
        let bound = BoundSocket::bind(dir.path()).unwrap();
        // Accepts, reads the request, never replies.
        let stalled = tokio::spawn(async move {
            let (mut stream, _) = bound.listener.accept().await.unwrap();
            read_frame(&mut stream).await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(bound);
        });
        let started = Instant::now();
        let fallback_delivered = std::cell::Cell::new(false);
        let (text, transport) =
            resolve(&db, dir.path(), fallback_input(dir.path()), started, |_| {
                fallback_delivered.set(true);
                true
            })
            .await;
        let elapsed = started.elapsed();
        assert_eq!((text, transport), (None, Transport::Unavailable));
        assert!(!fallback_delivered.get(), "the fallback must not run");
        assert!(elapsed >= Duration::from_millis(400), "{elapsed:?}");
        assert!(elapsed < PROCESS_BUDGET, "{elapsed:?}");
        stalled.abort();
    }

    #[tokio::test]
    async fn an_answer_at_350_ms_is_delivered_over_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _storage) = fallback_db(dir.path());
        let bound = BoundSocket::bind(dir.path()).unwrap();
        let late = tokio::spawn(async move {
            let (mut stream, _) = bound.listener.accept().await.unwrap();
            read_frame(&mut stream).await.unwrap();
            tokio::time::sleep(Duration::from_millis(350)).await;
            let reply = serde_json::to_vec(&Response {
                version: VERSION,
                injection: "late but here".into(),
                reason: Some(DecisionReason::OnTopic),
            })
            .unwrap();
            write_frame(&mut stream, &reply).await.unwrap();
            let _ = stream.read_u8().await;
        });
        let started = Instant::now();
        let (text, transport) =
            resolve(&db, dir.path(), fallback_input(dir.path()), started, |_| {
                true
            })
            .await;
        assert_eq!(
            (text.as_deref(), transport),
            (Some("late but here"), Transport::Socket)
        );
        assert!(started.elapsed() >= Duration::from_millis(340));
        late.await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_listener_that_accepts_and_drops_still_gets_the_fallback_at_once() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        let _mode = super::super::prompt_fallback::tests::SearchModeVar::set(Some("fts"));
        let dir = tempfile::tempdir().unwrap();
        let (db, _storage) = fallback_db(dir.path());
        let bound = BoundSocket::bind(dir.path()).unwrap();
        let dropper = tokio::spawn(async move {
            let (mut stream, _) = bound.listener.accept().await.unwrap();
            read_frame(&mut stream).await.unwrap();
        });
        let started = Instant::now();
        let (text, transport) =
            resolve(&db, dir.path(), fallback_input(dir.path()), started, |_| {
                true
            })
            .await;
        dropper.await.unwrap();
        assert_eq!(transport, Transport::Fallback);
        assert!(text.is_some());
        assert!(
            started.elapsed() < PROCESS_BUDGET,
            "{:?}",
            started.elapsed()
        );
    }

    /// One prompt to a running daemon, holding its render slot for `delay_ms`.
    async fn ask(dir: &Path, delay_ms: u64, budget: Duration) -> Socket {
        let mut input = input(dir);
        input.session_id = Some(format!("render-delay-ms={delay_ms}"));
        exchange(
            &dir.join("csr-engine.db"),
            dir,
            Kind::Prompt,
            input,
            budget,
            |_| true,
        )
        .await
    }

    /// Waits until the daemon at `dir` answers a plain prompt.
    pub(super) async fn warm(dir: &Path) {
        for _ in 0..100 {
            if matches!(
                ask(dir, 0, Duration::from_secs(2)).await,
                Socket::Answered(..)
            ) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the daemon never answered");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
    async fn simultaneous_prompts_queue_for_the_render_slot_and_all_get_a_reply() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let engine = fixture(dir.path());
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = spawn_for_daemon(engine, shutdown.clone());
        ready(dir.path()).await;
        warm(dir.path()).await;
        let started = Instant::now();
        let asks: Vec<_> = (0..4)
            .map(|_| {
                let path = dir.path().to_path_buf();
                tokio::spawn(async move { ask(&path, 50, Duration::from_secs(2)).await })
            })
            .collect();
        for ask in asks {
            let reply = ask.await.unwrap();
            assert!(matches!(reply, Socket::Answered(..)), "{reply:?}");
        }
        // One slot: the four 50 ms renders ran one after another.
        assert!(
            started.elapsed() >= Duration::from_millis(190),
            "{:?}",
            started.elapsed()
        );
        shutdown.store(true, Ordering::SeqCst);
        server.await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
    async fn a_request_that_outwaits_the_deadline_gets_eof_and_the_next_one_is_served() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let engine = fixture(dir.path());
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = spawn_for_daemon(engine, shutdown.clone());
        ready(dir.path()).await;
        warm(dir.path()).await;
        // The first request holds the slot for 700 ms, past the deadline.
        let path = dir.path().to_path_buf();
        let holder = tokio::spawn(async move { ask(&path, 700, Duration::from_secs(2)).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        // The second waits for the slot and runs out of deadline: EOF, not a stall.
        let started = Instant::now();
        let waiter = ask(dir.path(), 0, Duration::from_secs(2)).await;
        assert_eq!(waiter, Socket::Slow);
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(250), "{waited:?}");
        assert!(waited < Duration::from_millis(700), "{waited:?}");
        assert!(!matches!(holder.await.unwrap(), Socket::Answered(..)));
        // The slot is free again: the next request is served.
        let next = ask(dir.path(), 0, Duration::from_secs(2)).await;
        assert!(matches!(next, Socket::Answered(..)), "{next:?}");
        shutdown.store(true, Ordering::SeqCst);
        server.await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn refused_and_missing_sockets_go_straight_to_the_fallback() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        let _mode = super::super::prompt_fallback::tests::SearchModeVar::set(Some("fts"));
        let dir = tempfile::tempdir().unwrap();
        let (db, _storage) = fallback_db(dir.path());
        // Missing socket.
        let started = Instant::now();
        let (text, transport) =
            resolve(&db, dir.path(), fallback_input(dir.path()), started, |_| {
                true
            })
            .await;
        assert_eq!(transport, Transport::Fallback);
        assert!(text.is_some());
        assert!(
            started.elapsed() < FALLBACK_BUDGET,
            "{:?}",
            started.elapsed()
        );
        // A socket file nobody listens on: connection refused.
        std::fs::create_dir_all(socket_path(dir.path()).parent().unwrap()).unwrap();
        let stale = std::os::unix::net::UnixListener::bind(socket_path(dir.path())).unwrap();
        drop(stale);
        let started = Instant::now();
        let (text, transport) =
            resolve(&db, dir.path(), fallback_input(dir.path()), started, |_| {
                true
            })
            .await;
        assert_eq!(transport, Transport::Fallback);
        assert!(text.is_some());
        assert!(
            started.elapsed() < FALLBACK_BUDGET,
            "{:?}",
            started.elapsed()
        );
        // The fallback also stays quiet when stdout cannot take the answer.
        let (text, transport) = resolve(
            &db,
            dir.path(),
            fallback_input(dir.path()),
            Instant::now(),
            |_| false,
        )
        .await;
        assert_eq!((text, transport), (None, Transport::Unavailable));
    }

    /// The send gate holds without a daemon: one session's hit is a thin pool
    /// and the fallback says nothing, as the daemon would. Switched off, it
    /// answers; three sessions clear the gate (the other fallback tests).
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn the_fallback_abstains_on_a_thin_pool_like_the_daemon() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        let _mode = super::super::prompt_fallback::tests::SearchModeVar::set(Some("fts"));
        let dir = tempfile::tempdir().unwrap();
        let (db, _storage) = fallback_db_of(dir.path(), 1);
        let delivered = std::cell::Cell::new(false);
        let _on = crate::injection::send_gate::Switch::on();
        let started = Instant::now();
        let (text, transport) =
            resolve(&db, dir.path(), fallback_input(dir.path()), started, |_| {
                delivered.set(true);
                true
            })
            .await;
        assert_eq!((text, transport), (None, Transport::Unavailable));
        assert!(!delivered.get());
        let _off = crate::injection::send_gate::Switch::off();
        let started = Instant::now();
        let (text, transport) =
            resolve(&db, dir.path(), fallback_input(dir.path()), started, |_| {
                true
            })
            .await;
        assert_eq!(transport, Transport::Fallback);
        assert!(text.is_some());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn version_mismatch_daemon_makes_the_client_abstain() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        let _mode = super::super::prompt_fallback::tests::SearchModeVar::set(Some("fts"));
        let dir = tempfile::tempdir().unwrap();
        let (db, _storage) = fallback_db(dir.path());
        for reply in [
            // An old daemon that did understand the frame.
            Some(br#"{"version":1,"injection":"CSR PICKUP \u2014 from an old daemon\n"}"#.to_vec()),
            // A future one.
            Some(br#"{"version":3,"injection":"from the future"}"#.to_vec()),
        ] {
            let bound = BoundSocket::bind(dir.path()).unwrap();
            let peer = tokio::spawn(async move {
                let (mut stream, _) = bound.listener.accept().await.unwrap();
                let request = read_frame(&mut stream).await.unwrap();
                let json: serde_json::Value = serde_json::from_slice(&request).unwrap();
                assert_eq!(json["version"], 2);
                assert_eq!(json["kind"], "prompt");
                write_frame(&mut stream, &reply.unwrap()).await.unwrap();
            });
            let started = Instant::now();
            let delivered = std::cell::Cell::new(false);
            let (text, transport) =
                resolve(&db, dir.path(), fallback_input(dir.path()), started, |_| {
                    delivered.set(true);
                    true
                })
                .await;
            peer.await.unwrap();
            // Abstain: nothing delivered, and the fallback did not run in its place.
            assert_eq!((text, transport), (None, Transport::Unavailable));
            assert!(!delivered.get());
            assert!(started.elapsed() < SOCKET_BUDGET);
        }
        // A daemon that drops a frame it does not understand is not "abstain":
        // the client moves on to the fallback and still never hangs.
        let bound = BoundSocket::bind(dir.path()).unwrap();
        let dropper = tokio::spawn(async move {
            let (mut stream, _) = bound.listener.accept().await.unwrap();
            read_frame(&mut stream).await.unwrap();
        });
        let started = Instant::now();
        let (_, transport) = resolve(&db, dir.path(), fallback_input(dir.path()), started, |_| {
            true
        })
        .await;
        dropper.await.unwrap();
        assert_eq!(transport, Transport::Fallback);
        assert!(started.elapsed() < PROCESS_BUDGET);
    }

    #[test]
    fn request_frames_default_to_a_prompt_and_name_their_kind() {
        let bytes = serde_json::to_vec(&Request {
            version: VERSION,
            kind: Kind::PostCompact,
            database: "csr-engine.db".into(),
            projects_dir: PathBuf::from("/p"),
            policy: Vec::new(),
            wait_ms: None,
            input: HookInput::default(),
        })
        .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["kind"], "post_compact");
        assert_eq!(VERSION, 2);
        let mut without_kind = json;
        without_kind.as_object_mut().unwrap().remove("kind");
        let request: Request = serde_json::from_value(without_kind).unwrap();
        assert_eq!(request.kind, Kind::Prompt);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
    async fn post_compact_over_the_socket_stores_one_row_and_a_repeat_adds_none() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("csr-engine.db");
        let engine = fixture_over(dir.path(), crate::storage::Storage::open(&db).unwrap());
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = spawn_for_daemon(engine.clone(), shutdown.clone());
        ready(dir.path()).await;
        let payload = HookInput {
            session_id: Some("sess-pc".into()),
            compact_summary: Some("Compacted: the importer now dedupes by uuid.".into()),
            trigger: Some("auto".into()),
            prompt_id: Some("p-1".into()),
            cwd: Some(dir.path().to_string_lossy().into()),
            ..Default::default()
        };
        let mut sent = 0;
        for _ in 0..40 {
            if super::post_compact(&db, dir.path(), payload.clone()).await {
                sent += 1;
                if sent == 2 {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(sent, 2, "the daemon never answered both PostCompact fires");
        let rows: i64 = engine
            .storage()
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM chunks WHERE source = 'native_compact'",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(rows, 1);
        shutdown.store(true, Ordering::SeqCst);
        server.await.unwrap();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
    async fn serve_only_renders_without_writing_and_removes_its_socket() {
        let _env = crate::daemon::dream_cadence::env_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("csr-engine.db");
        let engine = fixture_over(dir.path(), crate::storage::Storage::open(&db).unwrap());
        let ask = input(dir.path());
        let expected = super::super::prompt_submit::render_uncommitted(&ask, &engine, dir.path())
            .await
            .unwrap();
        let counts = |engine: &Engine| -> (i64, i64, i64) {
            engine
                .storage()
                .with_connection(|conn| {
                    let count = |table: &str| -> i64 {
                        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                            .unwrap()
                    };
                    Ok((
                        count("retrieval_events"),
                        count("rerank_exposure_impressions"),
                        count("reflections"),
                    ))
                })
                .unwrap()
        };
        let before = counts(&engine);
        let shutdown = Arc::new(AtomicBool::new(false));
        let server = spawn_serve_only(engine.clone(), shutdown.clone());
        ready(dir.path()).await;
        let mut served = None;
        for _ in 0..40 {
            served = request(dir.path(), input(dir.path())).await;
            if served.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(served, Some(expected));
        // Let any receipt write that was going to land do so, then compare.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(counts(&engine), before);
        shutdown.store(true, Ordering::SeqCst);
        server.await.unwrap();
        assert!(!socket_path(dir.path()).exists());
    }
}

#[cfg(all(test, unix))]
mod endpoint_lock_tests {
    use super::*;

    /// A released endpoint can be bound again at once even while other
    /// threads spawn child processes (see `lock_within`).
    #[tokio::test]
    async fn rebinding_a_released_endpoint_survives_concurrent_spawns() {
        let stop = Arc::new(AtomicBool::new(false));
        let spawners: Vec<_> = (0..4)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        let _ = std::process::Command::new("/usr/bin/true").status();
                    }
                })
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let outcome = (0..300).try_for_each(|_| BoundSocket::bind(dir.path()).map(drop));
        stop.store(true, Ordering::SeqCst);
        for spawner in spawners {
            spawner.join().unwrap();
        }
        outcome.unwrap();
    }

    /// A live endpoint still refuses a second server.
    #[tokio::test]
    async fn a_held_endpoint_is_refused_after_the_wait() {
        let dir = tempfile::tempdir().unwrap();
        let held = BoundSocket::bind(dir.path()).unwrap();
        let started = std::time::Instant::now();
        let error = BoundSocket::bind(dir.path())
            .err()
            .expect("second bind refused");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert!(started.elapsed() >= LOCK_WAIT);
        drop(held);
        BoundSocket::bind(dir.path()).unwrap();
    }

    #[test]
    fn r3_endpoint_lock_timeout_closes_the_attempt_descriptor() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("held.lock");
        let held = std::fs::File::create(&path).unwrap();
        fs2::FileExt::lock_exclusive(&held).unwrap();
        let attempt = std::fs::File::open(&path).unwrap();
        let fd = attempt.as_raw_fd();
        let start = std::time::Instant::now();
        assert!(lock_within(attempt, Duration::from_millis(30)).is_err());
        assert!(start.elapsed() < Duration::from_millis(200));
        // SAFETY: fcntl only probes the descriptor and does not take ownership.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
    }
}

#[cfg(test)]
mod s9r_turn_mark_tests {
    use super::*;

    fn marked_after(prompt: &str) -> bool {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("csr.db");
        let proceed = pre_dispatch(&db, None, prompt, Some("sess-1"));
        assert!(!proceed, "{prompt:?} should still answer silently");
        super::super::turn_marks::read(dir.path(), "sess-1")
            .last_prompt
            .is_some()
    }

    #[test]
    fn s9r_slash_prompt_marks_turn_start() {
        assert!(marked_after("/help"));
    }

    #[test]
    fn s9r_short_prompt_marks_turn_start() {
        assert!(marked_after("ok"));
    }
}
