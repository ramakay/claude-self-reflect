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

use super::HookInput;
use crate::engine::Engine;

/// Connect plus reply over the daemon socket. Also the daemon's own deadline
/// for a request: an answer later than this is one nobody is waiting for.
pub const SOCKET_BUDGET: Duration = Duration::from_millis(250);
/// Whole-process wall, measured from `main`, within which the in-process
/// fallback answers when the socket is refused or missing.
pub const FALLBACK_BUDGET: Duration = Duration::from_millis(300);
/// Hard stop for the hook process. Nothing, stdin included, outlives it.
pub const PROCESS_BUDGET: Duration = Duration::from_millis(500);
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
}

pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("hook-ipc-v1").join("prompt.sock")
}

/// Start the independent deadline thread: at [`PROCESS_BUDGET`] after
/// `started` the process is gone. The thread also covers synchronous stdin,
/// filesystem calls, and runtime shutdown, which no Rust timeout can preempt.
/// Returns false when the thread cannot start (the caller then does nothing).
pub(super) fn spawn_watchdog(started: Instant) -> bool {
    let timing_probe = std::env::var_os("CSR_HOOK_TIMING_PROBE").is_some();
    std::thread::Builder::new()
        .name("prompt-deadline".into())
        .spawn(move || {
            std::thread::sleep(PROCESS_BUDGET.saturating_sub(started.elapsed()));
            if timing_probe {
                eprintln!(
                    "CSR timing-probe deadline_total_ms={}",
                    started.elapsed().as_millis()
                );
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
    if matches!(
        super::prompt_submit::early_exit(input.prompt.as_deref().unwrap_or_default()),
        Some(super::prompt_submit::EarlyExit::Silent)
    ) {
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
    // The turn mark goes down before any request, so FS2 sees a prompt that
    // the daemon never answered.
    if let (Some(data_dir), Some(session)) = (db_path.parent(), input.session_id.as_deref()) {
        super::turn_marks::mark_prompt(data_dir, session);
    }
    let prompt_id = input.prompt_id.clone();
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let setup_elapsed = started.elapsed().saturating_sub(stdin_elapsed);
    let request_started = Instant::now();
    let (_, transport) =
        runtime.block_on(resolve(db_path, projects_dir, input, started, |output| {
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all(output.as_bytes())
                .and_then(|_| stdout.flush())
                .is_ok()
        }));
    runtime.shutdown_background();
    let line = format!(
        "CSR hook prompt-submit [{}]: stdin={}ms setup={}ms hook={}ms flush=0ms total={}ms transport={} prompt_id={} includes_startup=1",
        project, stdin_elapsed.as_millis(), setup_elapsed.as_millis(),
        request_started.elapsed().as_millis(), started.elapsed().as_millis(),
        transport.label(),
        prompt_id.as_deref().filter(|id| !id.is_empty()).unwrap_or("-"),
    );
    eprintln!("{line}");
    crate::telemetry::append_timing_line(&line);
}

/// Answer a prompt: the daemon socket first, the in-process fallback when the
/// socket cannot. `deliver` writes the answer to the hook's stdout and says
/// whether it landed; only a delivered answer counts.
///
/// - refused or missing socket: fallback, by `started + FALLBACK_BUDGET`;
/// - socket that accepts and stalls, drops, or garbles: after
///   [`SOCKET_BUDGET`], fallback with what remains of [`PROCESS_BUDGET`];
/// - reply from another protocol version, or a stdout that fails: say nothing.
pub(crate) async fn resolve(
    db_path: &Path,
    projects_dir: &Path,
    input: HookInput,
    started: Instant,
    deliver: impl FnOnce(&str) -> bool,
) -> (Option<String>, Transport) {
    let mut deliver = Some(deliver);
    let socket = exchange(
        db_path,
        projects_dir,
        Kind::Prompt,
        input.clone(),
        SOCKET_BUDGET,
        |text| deliver.take().is_some_and(|deliver| deliver(text)),
    )
    .await;
    let deadline = match socket {
        Socket::Answered(text) => return (Some(text), Transport::Socket),
        Socket::Abstained => return (None, Transport::Unavailable),
        Socket::Refused => started + FALLBACK_BUDGET,
        Socket::Slow => started + PROCESS_BUDGET.saturating_sub(FALLBACK_MARGIN),
    };
    let Some(deliver) = deliver else {
        return (None, Transport::Unavailable);
    };
    let db = db_path.to_path_buf();
    let rendered = super::prompt_fallback::render(&db, &input, deadline);
    match rendered {
        Some(text) if deliver(&text) => (Some(text), Transport::Fallback),
        _ => (None, Transport::Unavailable),
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
        Socket::Answered(text) => Some(text),
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
        Socket::Answered(_)
    )
}

/// What one socket exchange came to.
#[derive(Debug, PartialEq, Eq)]
enum Socket {
    /// A valid reply of this protocol version, accepted by `deliver`.
    Answered(String),
    /// The daemon replied but the answer cannot be used (other version) or the
    /// caller could not deliver it. Nothing more is tried.
    Abstained,
    /// Nobody is listening: the socket is missing or refuses connections.
    Refused,
    /// Someone is listening and gave no usable answer in time: stalled,
    /// dropped the connection, or sent garbage.
    Slow,
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
            Socket::Answered(response.injection)
        })
        .await
        .unwrap_or(Socket::Slow)
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
    spawn(engine, shutdown, false)
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
    spawn(engine, shutdown, true)
}

/// `PRAGMA query_only=ON` on a storage handle. Failure leaves it writable.
fn make_query_only(storage: &crate::storage::Storage) {
    let _ = storage.with_connection(|conn| {
        conn.execute_batch("PRAGMA query_only=ON")?;
        Ok(())
    });
}

fn spawn(
    engine: Arc<Engine>,
    shutdown: Arc<AtomicBool>,
    read_only: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let data_dir = engine.index_dir().parent().unwrap_or(Path::new("."));
            let database = engine.storage().with_connection(|conn| {
                Ok(conn
                    .path()
                    .and_then(|path| Path::new(path).file_name())
                    .and_then(|name| name.to_str())
                    .unwrap_or("csr-engine.db")
                    .to_owned())
            });
            let Ok(database) = database else { return };
            // Off the async workers: a contended lock waits (see `lock_within`).
            let owned_dir = data_dir.to_path_buf();
            let bound = match tokio::task::spawn_blocking(move || BoundSocket::bind(&owned_dir))
                .await
                .unwrap_or_else(|e| Err(std::io::Error::other(e)))
            {
                Ok(bound) => bound,
                Err(e) => {
                    tracing::warn!(%e, "prompt socket unavailable");
                    return;
                }
            };
            // Renders read through their own connection, so a background loop
            // holding the daemon's connection for a long statement cannot park
            // a prompt behind it (see `Storage::open_prompt_lane`). Writes that
            // must not be lost, and may wait, stay on the daemon's connection:
            // `receipts`.
            let receipts = engine.storage().clone();
            if read_only {
                make_query_only(&receipts);
            }
            let render_engine = match engine.storage().open_prompt_lane() {
                Ok(Some(lane)) => Arc::new(Engine::from_parts_with_index_dir(
                    {
                        if read_only {
                            make_query_only(&lane);
                        }
                        Arc::new(lane)
                    },
                    engine.embeddings().clone(),
                    engine.search().clone(),
                    engine.projects_dir().to_path_buf(),
                    engine.index_dir().to_path_buf(),
                )),
                Ok(None) => engine.clone(),
                Err(e) => {
                    tracing::warn!(%e, "prompt lane unavailable, renders share the daemon connection");
                    engine.clone()
                }
            };
            // Measure the frame vocabulary (`injection::specificity`) off the
            // prompt path: before the first prompt, then well inside each
            // validity window, so a render only ever reads the cached copy. It
            // runs on the daemon's connection (reads, then one meta write that
            // may wait for another writer) and publishes into the cache the lane
            // shares; the lane's own mutex is never held for that.
            if !read_only {
                let engine = engine.clone();
                let shutdown = shutdown.clone();
                tokio::spawn(async move {
                    while !shutdown.load(Ordering::SeqCst) {
                        let worker = engine.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            worker.storage().refresh_reflection_common_terms();
                        })
                        .await;
                        tokio::time::sleep(COMMON_TERMS_REFRESH).await;
                    }
                });
            }
            let engine = render_engine;
            let slots = Arc::new(Semaphore::new(1));
            while !shutdown.load(Ordering::SeqCst) {
                let accepted = tokio::select! {
                    accepted = bound.listener.accept() => accepted,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => continue,
                };
                let Ok((mut stream, _)) = accepted else {
                    continue;
                };
                let Ok(slot) = slots.clone().try_acquire_owned() else {
                    continue;
                };
                let engine = engine.clone();
                let receipts = receipts.clone();
                let database = database.clone();
                tokio::spawn(async move {
                    let deadline = tokio::time::Instant::now() + SOCKET_BUDGET;
                    let _ = tokio::time::timeout_at(deadline, async {
                        let bytes = read_frame(&mut stream).await?;
                        let request: Request = serde_json::from_slice(&bytes).ok()?;
                        if request.version != VERSION
                            || request.database != database
                            || (request.kind == Kind::Prompt && request.policy != policy())
                            || request.projects_dir
                                != engine
                                    .projects_dir()
                                    .canonicalize()
                                    .unwrap_or_else(|_| engine.projects_dir().to_path_buf())
                        {
                            return None;
                        }
                        if request.kind == Kind::PostCompact {
                            // Stored on the daemon's own connection: a write that
                            // may wait for another writer, never on the render lane.
                            let input = request.input;
                            let engine = engine.clone();
                            let stored = tokio::task::spawn_blocking(move || {
                                let _operation = engine.storage().operation_lock().ok()?;
                                receipts
                                    .with_connection(|conn| {
                                        super::post_compact::store(conn, &input)
                                    })
                                    .ok()
                            })
                            .await
                            .ok()
                            .flatten()?;
                            let bytes = serde_json::to_vec(&Response {
                                version: VERSION,
                                injection: if stored { "stored" } else { "duplicate" }.into(),
                            })
                            .ok()?;
                            write_frame(&mut stream, &bytes).await?;
                            return Some(());
                        }
                        let cwd = PathBuf::from(request.input.cwd.as_deref()?);
                        if !cwd.is_absolute() {
                            return None;
                        }
                        let runtime = tokio::runtime::Handle::current();
                        tokio::task::spawn_blocking(move || {
                            // The worker owns admission until rendering and delivery
                            // have finished. It lets go before anything that can wait
                            // for a database writer: the retrieval events and the
                            // release of an unshown dream claim, which go to the
                            // daemon's connection, not the render's.
                            let mut permit = Some(slot);
                            let _operation = engine.storage().operation_lock().ok()?;
                            if tokio::time::Instant::now() >= deadline {
                                return None;
                            }
                            let prepared =
                                runtime.block_on(super::prompt_submit::prepare_with_receipts(
                                    &request.input,
                                    &engine,
                                    &cwd,
                                    receipts,
                                ));
                            let prepared = prepared.ok()?;
                            if tokio::time::Instant::now() >= deadline {
                                drop(permit.take());
                                drop(prepared);
                                return None;
                            }
                            let Ok(bytes) = serde_json::to_vec(&Response {
                                version: VERSION,
                                injection: prepared.text.clone(),
                            }) else {
                                drop(permit.take());
                                drop(prepared);
                                return None;
                            };
                            let delivered = runtime.block_on(async {
                                tokio::time::timeout_at(deadline, async {
                                    write_frame(&mut stream, &bytes).await?;
                                    (stream.read_u8().await.ok()? == 1).then_some(())
                                })
                                .await
                                .ok()
                                .flatten()
                            });
                            if delivered.is_some() {
                                prepared.delivered_releasing(|| drop(permit.take()));
                            } else {
                                // An unacknowledged plan drops here, on this blocking
                                // worker: speculative dream claims are released and
                                // unshown exposures are discarded, never on a Tokio worker.
                                drop(permit.take());
                                drop(prepared);
                            }
                            delivered
                        })
                        .await
                        .ok()
                        .flatten()
                    })
                    .await;
                });
            }
        }
        #[cfg(not(unix))]
        let _ = (engine, shutdown);
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    async fn request(dir: &Path, input: HookInput) -> Option<String> {
        super::request(&dir.join("csr-engine.db"), dir, input).await
    }

    fn fixture(dir: &Path) -> Arc<Engine> {
        fixture_over(dir, crate::storage::Storage::open_memory().unwrap())
    }

    fn fixture_over(dir: &Path, storage: crate::storage::Storage) -> Arc<Engine> {
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

    fn input(dir: &Path) -> HookInput {
        HookInput {
            prompt: Some("continue".into()),
            cwd: Some(dir.to_string_lossy().into()),
            session_id: Some("current".into()),
            ..Default::default()
        }
    }

    async fn ready(dir: &Path) {
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
        let expected = super::super::prompt_submit::render(&input, &engine, dir.path())
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
        super::super::prompt_submit::render(&input(dir.path()), &engine, dir.path())
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
        engine
            .storage()
            .insert_reflection("solution", content, &["solution".into()], &vector)
            .unwrap();
        engine
            .search()
            .write()
            .await
            .insert_reflection("solution".into(), vector);
        let expected = super::super::prompt_submit::render(&input, &engine, dir.path())
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
        let expected = super::super::prompt_submit::render(&input(dir.path()), &engine, dir.path())
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
        engine
            .storage()
            .insert_reflection(
                "solution",
                "Token validation regression was fixed by refreshing expired credentials before retry.",
                &["solution".into()],
                &vector,
            )
            .unwrap();
        engine
            .search()
            .write()
            .await
            .insert_reflection("solution".into(), vector);
        let expected = super::super::prompt_submit::render(&input, &engine, dir.path())
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

    /// A file database holding one chunk the fallback can find by identifier
    /// and by words. The storage is returned to keep the database open.
    fn fallback_db(dir: &Path) -> (PathBuf, crate::storage::Storage) {
        let db = dir.join("csr-engine.db");
        let storage = crate::storage::Storage::open(&db).unwrap();
        storage
            .insert_chunk(
                &crate::import::ConversationChunk {
                    id: "fb-1".into(),
                    conversation_id: "abcdef12-0000-4000-8000-000000000000".into(),
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
        (db, storage)
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
    async fn stalled_listener_times_out_at_the_socket_budget_then_yields_the_fallback() {
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
        let (text, transport) =
            resolve(&db, dir.path(), fallback_input(dir.path()), started, |_| {
                true
            })
            .await;
        let elapsed = started.elapsed();
        assert_eq!(transport, Transport::Fallback);
        let text = text.expect("the fallback answers when the socket stalls");
        assert!(text.contains("src/auth/token.rs"), "{text}");
        assert!(elapsed >= SOCKET_BUDGET, "{elapsed:?}");
        assert!(
            elapsed < SOCKET_BUDGET + Duration::from_millis(100),
            "{elapsed:?}"
        );
        stalled.abort();
    }

    #[tokio::test]
    async fn refused_and_missing_sockets_go_straight_to_the_fallback() {
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

    #[tokio::test]
    async fn version_mismatch_daemon_makes_the_client_abstain() {
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
        let expected = super::super::prompt_submit::render(&input(dir.path()), &engine, dir.path())
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
