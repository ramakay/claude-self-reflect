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
fn policy() -> Vec<Option<String>> {
    [
        "CSR_NO_ANTI_PATTERNS",
        "CSR_NO_GRAPH_SLICES",
        "CSR_TRAINED_RERANK",
        "CSR_NO_DREAM_INJECT",
        "CSR_DREAM_CONSUMPTION",
        "CSR_NO_RECAP",
    ]
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
    if std::thread::Builder::new()
        .name("prompt-deadline".into())
        .spawn(move || {
            std::thread::sleep(PROCESS_BUDGET.saturating_sub(started.elapsed()));
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
struct BoundSocket {
    listener: tokio::net::UnixListener,
    path: PathBuf,
    // Held until the listener and socket path have been removed. A second
    // daemon must never unlink the first daemon's active endpoint.
    _lock: std::fs::File,
}

#[cfg(unix)]
impl BoundSocket {
    fn bind(data_dir: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::{
            DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt,
        };
        let path = socket_path(data_dir);
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
            .open(dir.join("server.lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock)?;
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

#[cfg(unix)]
impl Drop for BoundSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Start an independent endpoint sharing the daemon's warm engine. Single
/// flight bounds work even if a client disappears while SQLite/ONNX is busy.
pub fn spawn_for_daemon(
    engine: Arc<Engine>,
    heavy_work: Arc<Semaphore>,
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
            let bound = match BoundSocket::bind(data_dir) {
                Ok(bound) => bound,
                Err(e) => {
                    tracing::warn!(%e, "prompt socket unavailable");
                    return;
                }
            };
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
                let Ok(heavy) = heavy_work.clone().try_acquire_owned() else {
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
                            let (_slot, _heavy) = (slot, heavy);
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
        let _ = (engine, heavy_work, shutdown);
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
            dir.join("index"),
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
        let server = spawn_for_daemon(
            engine.clone(),
            Arc::new(Semaphore::new(1)),
            shutdown.clone(),
        );
        ready(dir.path()).await;
        assert_eq!(request(dir.path(), input).await, Some(expected));
        assert!(!engine.embeddings().is_loaded());
        assert!(!dir.path().join("index").exists());
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
    async fn absent_stale_and_busy_daemons_fail_open() {
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
        let shutdown = Arc::new(AtomicBool::new(false));
        let heavy = Arc::new(Semaphore::new(1));
        let permit = heavy.clone().acquire_owned().await.unwrap();
        let server = spawn_for_daemon(fixture(dir.path()), heavy, shutdown.clone());
        ready(dir.path()).await;
        let started = Instant::now();
        assert!(request(dir.path(), input(dir.path())).await.is_none());
        assert!(started.elapsed() < Duration::from_millis(500));
        drop(permit);
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
        let server = spawn_for_daemon(engine, Arc::new(Semaphore::new(1)), shutdown.clone());
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
        let heavy = Arc::new(Semaphore::new(1));
        let server = spawn_for_daemon(engine, heavy.clone(), shutdown.clone());
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
        assert_eq!(heavy.available_permits(), 0);
        let started = Instant::now();
        assert!(request(dir.path(), input(dir.path())).await.is_none());
        assert!(started.elapsed() < Duration::from_millis(500));
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while heavy.available_permits() == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
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
