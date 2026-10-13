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

pub const REQUEST_BUDGET: Duration = Duration::from_millis(800);
pub const PROCESS_BUDGET: Duration = Duration::from_millis(1400);
const VERSION: u32 = 1;
const MAX_FRAME: usize = 256 * 1024;

#[derive(Serialize, Deserialize)]
struct Request {
    version: u32,
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

/// Called before runtime/storage construction. The independent thread also
/// covers synchronous stdin, filesystem calls, and runtime shutdown. No Rust
/// timeout can preempt those operations. Leave scheduling margin below 3s.
pub fn run_client(db_path: &Path, projects_dir: &Path, started: Instant) {
    let Some(_operation) = super::try_start(db_path) else {
        return;
    };
    let timing_probe = std::env::var_os("CSR_HOOK_TIMING_PROBE").is_some();
    if std::thread::Builder::new()
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
        .is_err()
    {
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
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let setup_elapsed = started.elapsed().saturating_sub(stdin_elapsed);
    let request_started = Instant::now();
    let output = runtime.block_on(request_with_delivery(
        db_path,
        projects_dir,
        input,
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
        "CSR hook prompt-submit [{}]: stdin={}ms setup={}ms hook={}ms flush=0ms total={}ms transport={} includes_startup=1",
        project, stdin_elapsed.as_millis(), setup_elapsed.as_millis(),
        request_started.elapsed().as_millis(), started.elapsed().as_millis(),
        if output.is_some() {
            "ok"
        } else {
            "unavailable"
        },
    );
    eprintln!("{line}");
    crate::telemetry::append_timing_line(&line);
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

pub async fn request(db_path: &Path, projects_dir: &Path, input: HookInput) -> Option<String> {
    request_with_delivery(db_path, projects_dir, input, |_| true).await
}

async fn request_with_delivery(
    db_path: &Path,
    projects_dir: &Path,
    input: HookInput,
    deliver: impl FnOnce(&str) -> bool,
) -> Option<String> {
    #[cfg(unix)]
    {
        tokio::time::timeout(REQUEST_BUDGET, async {
            let data_dir = db_path.parent()?;
            let mut stream = tokio::net::UnixStream::connect(socket_path(data_dir))
                .await
                .ok()?;
            let bytes = serde_json::to_vec(&Request {
                version: VERSION,
                database: db_path.file_name()?.to_str()?.to_owned(),
                projects_dir: projects_dir
                    .canonicalize()
                    .unwrap_or_else(|_| projects_dir.to_path_buf()),
                policy: policy(),
                input,
            })
            .ok()?;
            write_frame(&mut stream, &bytes).await?;
            let bytes = read_frame(&mut stream).await?;
            let response: Response = serde_json::from_slice(&bytes).ok()?;
            if response.version != VERSION || !deliver(&response.injection) {
                return None;
            }
            // Acknowledge only a fully validated response accepted by the stdout
            // writer. Failed/partial/timed-out replies never commit exposure state.
            let _ = stream.write_u8(1).await;
            Some(response.injection)
        })
        .await
        .ok()
        .flatten()
    }
    #[cfg(not(unix))]
    {
        let _ = (db_path, projects_dir, input, deliver);
        None
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
            // Measure the frame vocabulary (`injection::specificity`) off the
            // prompt path: before the first prompt, then well inside each
            // validity window, so a render only ever reads the cached copy.
            {
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
                let database = database.clone();
                tokio::spawn(async move {
                    let deadline = tokio::time::Instant::now() + REQUEST_BUDGET;
                    let _ = tokio::time::timeout_at(deadline, async {
                        let bytes = read_frame(&mut stream).await?;
                        let request: Request = serde_json::from_slice(&bytes).ok()?;
                        if request.version != VERSION
                            || request.database != database
                            || request.policy != policy()
                            || request.projects_dir
                                != engine
                                    .projects_dir()
                                    .canonicalize()
                                    .unwrap_or_else(|_| engine.projects_dir().to_path_buf())
                        {
                            return None;
                        }
                        let cwd = PathBuf::from(request.input.cwd.as_deref()?);
                        if !cwd.is_absolute() {
                            return None;
                        }
                        let runtime = tokio::runtime::Handle::current();
                        tokio::task::spawn_blocking(move || {
                            // The worker owns admission until rendering, delivery,
                            // and receipt commit/rollback have actually finished.
                            let _slot = slot;
                            let _operation = engine.storage().operation_lock().ok()?;
                            if tokio::time::Instant::now() >= deadline {
                                return None;
                            }
                            let prepared = runtime
                                .block_on(super::prompt_submit::prepare(
                                    &request.input,
                                    &engine,
                                    &cwd,
                                ))
                                .ok()?;
                            if tokio::time::Instant::now() >= deadline {
                                return None;
                            }
                            let bytes = serde_json::to_vec(&Response {
                                version: VERSION,
                                injection: prepared.text.clone(),
                            })
                            .ok()?;
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
                                prepared.delivered();
                            }
                            // An unacknowledged plan drops here, on this blocking
                            // worker: speculative dream claims are released and
                            // unshown exposures are discarded, never on a Tokio worker.
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
        let storage = Arc::new(crate::storage::Storage::open_memory().unwrap());
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
                database: "csr-engine.db".into(),
                projects_dir: dir.path().canonicalize().unwrap(),
                policy: client_policy,
                input: input(dir.path()),
            })
            .unwrap();
            let mut stream = tokio::net::UnixStream::connect(socket_path(dir.path()))
                .await
                .unwrap();
            write_frame(&mut stream, &bytes).await.unwrap();
            assert!(
                tokio::time::timeout(REQUEST_BUDGET, read_frame(&mut stream))
                    .await
                    .unwrap()
                    .is_none(),
                "daemon must abstain when {key} differs"
            );
        }
        // Matching configuration still renders after all rejected requests.
        assert!(request(dir.path(), input(dir.path())).await.is_some());
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
