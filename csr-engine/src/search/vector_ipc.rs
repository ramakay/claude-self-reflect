//! Daemon-served vector search for MCP servers: one process owns the index.
//!
//! Every MCP server (one per Claude Code session) used to load its own HNSW
//! index and embedding model: 1.3-2.2 GB each, nine at a time on the live
//! machine. When a daemon is configured (its heartbeat file is fresh), a
//! server leaves both to it: it forwards the vector primitives its tools use
//! — query embedding, chunk kNN (optionally restricted to an id set),
//! reflection kNN — over a private Unix socket beside the prompt socket, and
//! keeps reading everything else from SQLite itself. The daemon runs the very
//! same `SearchEngine`/`EmbeddingEngine` calls, so answers are the ones the
//! server would have computed from the same generation.
//!
//! Such a server never loads the index or the model, not even while the
//! daemon is restarting or gone: an in-process fallback cannot give its
//! memory back on macOS (60 MB -> 2,022 MB -> 1,094 MB after recovery, 81 MB
//! of it live), and nine servers falling back together brought the fleet
//! back. While the daemon does not answer, vector-backed tools answer from
//! SQLite's keyword index with a one-line note; every call tries the daemon
//! again, and when it answers they use it. A server whose data dir has no
//! fresh heartbeat (no daemon was ever run) is the full in-process engine,
//! unchanged.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Notify, RwLock, Semaphore};

use super::{SearchEngine, SearchResult};
use crate::embeddings::EmbeddingEngine;
use crate::storage::Storage;

const VERSION: u32 = 1;
/// A project-scoped search carries the scope's chunk ids: 174k ids (~7 MB of
/// JSON) for the largest project live.
const MAX_FRAME: usize = 64 << 20;
const PING_TIMEOUT: Duration = Duration::from_millis(1500);
/// Above the slowest in-process search (a filtered search that escalates to
/// the whole graph) so a busy daemon is not mistaken for a dead one. An
/// absolute deadline: connecting, writing and reading all count.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the daemon waits for its index sync to apply a stored change
/// before acknowledging a `Sync` anyway, and the caller's deadline for it.
const SYNC_WAIT: Duration = Duration::from_secs(3);
const SYNC_TIMEOUT: Duration = Duration::from_secs(4);
/// After a failed call, re-ping the daemon from this delay, doubling, for up
/// to [`BACKOFF_TOTAL`] before answering keyword-only. A restart leaves no
/// socket from the old daemon's exit until the new one has loaded its index
/// (5.7-12.3 s of cache load in the live daemon's log).
const BACKOFF_FIRST: Duration = Duration::from_millis(250);
const BACKOFF_TOTAL: Duration = Duration::from_secs(20);
/// Within this long of a failure, a call tries the daemon once and does not
/// wait out the backoff again: an outage costs one slow call, not all.
const DOWN_WINDOW: Duration = Duration::from_secs(15);
/// Requests the daemon serves at once; more wait for a slot.
const SERVER_SLOTS: usize = 8;
/// A heartbeat this recent means a daemon is configured for this data dir.
const CONFIGURED_WITHIN: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the daemon refreshes its heartbeat.
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(10 * 60);

/// What answers an MCP server's vector calls; recorded in its run record
/// (`csr-engine status` lists it under `mcp_servers`) and logged on change.
pub const PATH_DAEMON: &str = "daemon";
/// A server of a configured daemon that is not answering right now.
pub const PATH_KEYWORD_ONLY: &str = "keyword-only (daemon unavailable)";
/// A server with no configured daemon (or `CSR_NO_DAEMON_PROXY=1`).
pub const PATH_IN_PROCESS: &str = "in-process";

/// The line vector-backed tools put above keyword-only results.
pub const KEYWORD_ONLY_NOTE: &str =
    "semantic search paused: daemon restarting — keyword (FTS) results only";

pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("hook-ipc-v1").join("vector.sock")
}

pub fn heartbeat_path(data_dir: &Path) -> PathBuf {
    data_dir.join("hook-ipc-v1").join("daemon.alive")
}

/// The daemon's heartbeat: its pid and the time, rewritten at startup and
/// every [`HEARTBEAT_EVERY`]. Best-effort.
pub fn write_heartbeat(data_dir: &Path) {
    let path = heartbeat_path(data_dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        if let Some(dir) = path.parent() {
            // Same private directory the sockets require (0700).
            let _ = std::fs::DirBuilder::new().mode(0o700).create(dir);
        }
    }
    let body = format!(
        "{} {}\n",
        std::process::id(),
        chrono::Utc::now().to_rfc3339()
    );
    let tmp = path.with_extension("alive.tmp");
    if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Whether a daemon is configured for `data_dir`: `CSR_NO_DAEMON_PROXY=1`
/// says no, `CSR_DAEMON_PROXY=1` says yes, otherwise a heartbeat written in
/// the last 24 h. Configured means proxied, even while the daemon restarts.
pub fn daemon_configured(data_dir: &Path) -> bool {
    let set = |name: &str| std::env::var(name).is_ok_and(|v| v == "1" || v == "true");
    if set("CSR_NO_DAEMON_PROXY") {
        return false;
    }
    if set("CSR_DAEMON_PROXY") {
        return true;
    }
    std::fs::metadata(heartbeat_path(data_dir))
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age <= CONFIGURED_WITHIN)
}

#[derive(Serialize, Deserialize)]
struct Request {
    version: u32,
    database: String,
    op: Op,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    Ping,
    Embed {
        texts: Vec<String>,
    },
    SearchChunks {
        vector: Vec<f32>,
        limit: usize,
        min_score: f32,
        allowed: Option<Vec<String>>,
    },
    SearchReflections {
        vector: Vec<f32>,
        limit: usize,
        min_score: f32,
    },
    /// The caller committed embedding changes up to `seq`: apply them, then
    /// answer (or answer after [`SYNC_WAIT`] regardless).
    Sync {
        seq: i64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
enum Reply {
    Pong,
    Vectors { vectors: Vec<Vec<f32>> },
    Hits { hits: Vec<(String, f32)> },
    Synced { applied: bool },
}

#[derive(Serialize, Deserialize)]
struct Response {
    version: u32,
    reply: Reply,
}

/// Run blocking socket work without stalling the async runtime: on a
/// multi-threaded runtime worker the worker hands its other tasks (MCP stdio
/// and cancellation included) to a replacement first. Elsewhere (a blocking
/// thread, no runtime, a current-thread runtime) it just runs.
fn off_worker<T>(work: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

/// Blocking client for the daemon's vector socket. Every call has an
/// absolute deadline covering connect, write and read.
pub struct VectorClient {
    socket: PathBuf,
    database: String,
}

impl VectorClient {
    pub fn for_database(db_path: &Path) -> Option<Self> {
        Some(Self {
            socket: socket_path(db_path.parent()?),
            database: db_path.file_name()?.to_str()?.to_owned(),
        })
    }

    fn encode(&self, op: Op) -> Result<Vec<u8>> {
        let request = serde_json::to_vec(&Request {
            version: VERSION,
            database: self.database.clone(),
            op,
        })?;
        if request.len() > MAX_FRAME {
            bail!("request of {} bytes exceeds the frame cap", request.len());
        }
        Ok(request)
    }

    fn call(&self, op: Op, timeout: Duration) -> Result<Reply> {
        let request = self.encode(op)?;
        self.send(&request, Instant::now() + timeout)
    }

    #[cfg(unix)]
    fn send(&self, request: &[u8], deadline: Instant) -> Result<Reply> {
        let mut stream = connect_by(&self.socket, deadline)
            .with_context(|| format!("connecting {}", self.socket.display()))?;
        let mut frame = Vec::with_capacity(4 + request.len());
        frame.extend_from_slice(&(request.len() as u32).to_be_bytes());
        frame.extend_from_slice(request);
        io_by(&mut stream, deadline, |s| s.write_all(&frame)).context("writing request")?;
        let mut header = [0u8; 4];
        io_by(&mut stream, deadline, |s| s.read_exact(&mut header)).context("reading reply")?;
        let size = u32::from_be_bytes(header) as usize;
        if size > MAX_FRAME {
            bail!("reply of {size} bytes exceeds the frame cap");
        }
        let mut body = vec![0; size];
        io_by(&mut stream, deadline, |s| s.read_exact(&mut body)).context("reading reply")?;
        let response: Response = serde_json::from_slice(&body)?;
        if response.version != VERSION {
            bail!("daemon speaks vector protocol {}", response.version);
        }
        Ok(response.reply)
    }

    #[cfg(not(unix))]
    fn send(&self, _request: &[u8], _deadline: Instant) -> Result<Reply> {
        bail!("no vector socket on this platform")
    }

    /// The daemon answers for this database.
    pub fn ping(&self) -> Result<()> {
        match self.call(Op::Ping, PING_TIMEOUT)? {
            Reply::Pong => Ok(()),
            _ => bail!("unexpected reply to ping"),
        }
    }
}

/// Connect without blocking past `deadline` (a listener whose backlog is
/// full would otherwise hold a blocking connect).
#[cfg(unix)]
fn connect_by(path: &Path, deadline: Instant) -> std::io::Result<std::os::unix::net::UnixStream> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::FromRawFd;
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: plain socket calls on a descriptor this function owns; it is
    // closed on every error path and handed to UnixStream on success.
    unsafe {
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        if bytes.len() >= addr.sun_path.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "socket path too long",
            ));
        }
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (slot, byte) in addr.sun_path.iter_mut().zip(bytes) {
            *slot = *byte as libc::c_char;
        }
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let close_with = |error: std::io::Error| {
            libc::close(fd);
            Err(error)
        };
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return close_with(std::io::Error::last_os_error());
        }
        let len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
        if libc::connect(fd, &addr as *const _ as *const libc::sockaddr, len) < 0 {
            let error = std::io::Error::last_os_error();
            let pending = matches!(
                error.raw_os_error(),
                Some(libc::EINPROGRESS) | Some(libc::EAGAIN)
            );
            if !pending {
                return close_with(error);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            let millis = remaining.as_millis().min(i32::MAX as u128) as libc::c_int;
            if libc::poll(&mut poll, 1, millis) <= 0 {
                return close_with(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "connect timed out",
                ));
            }
            let mut so_error: libc::c_int = 0;
            let mut so_len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                &mut so_error as *mut _ as *mut libc::c_void,
                &mut so_len,
            );
            if so_error != 0 {
                return close_with(std::io::Error::from_raw_os_error(so_error));
            }
        }
        if libc::fcntl(fd, libc::F_SETFL, flags) < 0 {
            return close_with(std::io::Error::last_os_error());
        }
        let stream = std::os::unix::net::UnixStream::from_raw_fd(fd);
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        stream.set_read_timeout(Some(remaining))?;
        stream.set_write_timeout(Some(remaining))?;
        Ok(stream)
    }
}

/// Run one blocking stream operation with the time left until `deadline`
/// as its socket timeout, failing once the deadline has passed.
#[cfg(unix)]
fn io_by(
    stream: &mut std::os::unix::net::UnixStream,
    deadline: Instant,
    op: impl FnOnce(&mut std::os::unix::net::UnixStream) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "deadline passed",
        ));
    }
    // macOS refuses SO_RCVTIMEO/SO_SNDTIMEO (EINVAL) once the peer has
    // closed — i.e. after a fast reply. The timeouts set right after connect
    // still bound the operation then, so a refused tightening is ignored.
    let _ = stream.set_read_timeout(Some(remaining));
    let _ = stream.set_write_timeout(Some(remaining));
    op(stream)
}

/// Backoff and down-window timing, adjustable so tests need not wait 20 s.
#[derive(Clone, Copy, Debug)]
pub struct ProxyTimings {
    pub backoff_first: Duration,
    pub backoff_total: Duration,
    pub down_window: Duration,
}

impl Default for ProxyTimings {
    fn default() -> Self {
        Self {
            backoff_first: BACKOFF_FIRST,
            backoff_total: BACKOFF_TOTAL,
            down_window: DOWN_WINDOW,
        }
    }
}

/// The vector side of a proxied MCP server. It holds no index and no model:
/// every primitive goes to the daemon, and while the daemon does not answer
/// the primitives return nothing and [`Self::failures`] moves, which the
/// MCP layer turns into keyword-only answers.
pub struct VectorProxy {
    client: VectorClient,
    storage: Arc<Storage>,
    calls: AtomicU64,
    failures: AtomicU64,
    down_since: std::sync::Mutex<Option<Instant>>,
    timings: std::sync::Mutex<ProxyTimings>,
    /// The server's run record, rewritten with its current path.
    run_record: std::sync::Mutex<Option<PathBuf>>,
}

impl VectorProxy {
    pub fn new(client: VectorClient, storage: Arc<Storage>) -> Self {
        Self {
            client,
            storage,
            calls: AtomicU64::new(0),
            failures: AtomicU64::new(0),
            down_since: std::sync::Mutex::new(None),
            timings: std::sync::Mutex::new(ProxyTimings::default()),
            run_record: std::sync::Mutex::new(None),
        }
    }

    pub fn set_timings(&self, timings: ProxyTimings) {
        if let Ok(mut current) = self.timings.lock() {
            *current = timings;
        }
    }

    fn timings(&self) -> ProxyTimings {
        self.timings.lock().map(|t| *t).unwrap_or_default()
    }

    /// `(calls the daemon answered, calls it did not)`. The MCP layer
    /// compares `failures` before and after a tool call.
    pub fn stats(&self) -> (u64, u64) {
        (self.calls.load(Ordering::Relaxed), self.failures())
    }

    pub fn failures(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }

    /// Record this server's path in `record` from now on, starting now.
    pub fn attach_run_record(&self, record: PathBuf) {
        crate::binary_identity::set_run_record_search_path(&record, self.path());
        if let Ok(mut current) = self.run_record.lock() {
            *current = Some(record);
        }
    }

    /// The path answering this server's vector calls right now.
    pub fn path(&self) -> &'static str {
        if self.down_since.lock().is_ok_and(|down| down.is_some()) {
            PATH_KEYWORD_ONLY
        } else {
            PATH_DAEMON
        }
    }

    fn report_path(&self) {
        let path = self.path();
        tracing::info!(search_path = path, "MCP vector search path changed");
        if let Ok(record) = self.run_record.lock() {
            if let Some(record) = record.as_ref() {
                crate::binary_identity::set_run_record_search_path(record, path);
            }
        }
    }

    fn mark_up(&self) {
        let was_down = self
            .down_since
            .lock()
            .map(|mut down| down.take().is_some())
            .unwrap_or(false);
        if was_down {
            tracing::info!("daemon answers again: semantic search resumed");
            self.report_path();
        }
    }

    fn mark_down(&self, error: &anyhow::Error) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        let newly = self
            .down_since
            .lock()
            .map(|mut down| {
                let newly = down.is_none();
                *down = Some(Instant::now());
                newly
            })
            .unwrap_or(false);
        if newly {
            tracing::warn!(%error, "daemon vector search unavailable; answering keyword-only");
            self.report_path();
        }
    }

    fn recently_down(&self) -> bool {
        let window = self.timings().down_window;
        self.down_since
            .lock()
            .is_ok_and(|down| down.is_some_and(|since| since.elapsed() < window))
    }

    /// Re-ping the daemon with doubling backoff; `true` once it answers.
    fn await_daemon(&self) -> bool {
        let timings = self.timings();
        let started = Instant::now();
        let mut delay = timings.backoff_first;
        loop {
            let remaining = timings.backoff_total.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return false;
            }
            std::thread::sleep(delay.min(remaining));
            if self.client.ping().is_ok() {
                return true;
            }
            delay = delay.saturating_mul(2);
        }
    }

    /// Whether the daemon answers now, waiting out the backoff unless it
    /// failed within the down window. Blocking: call it off the runtime.
    pub fn available(&self) -> bool {
        if self.client.ping().is_ok() {
            self.mark_up();
            return true;
        }
        if !self.recently_down() && self.await_daemon() {
            self.mark_up();
            return true;
        }
        self.mark_down(&anyhow::anyhow!("daemon did not answer a ping"));
        false
    }

    fn remote(&self, op: Op, timeout: Duration) -> Option<Reply> {
        off_worker(|| {
            let request = self.client.encode(op).ok()?;
            let error = match self.client.send(&request, Instant::now() + timeout) {
                Ok(reply) => {
                    self.calls.fetch_add(1, Ordering::Relaxed);
                    self.mark_up();
                    return Some(reply);
                }
                Err(error) => error,
            };
            if !self.recently_down() && self.await_daemon() {
                if let Ok(reply) = self.client.send(&request, Instant::now() + timeout) {
                    self.calls.fetch_add(1, Ordering::Relaxed);
                    self.mark_up();
                    return Some(reply);
                }
            }
            self.mark_down(&error);
            None
        })
    }

    fn hits(reply: Option<Reply>) -> Vec<SearchResult> {
        match reply {
            Some(Reply::Hits { hits }) => hits
                .into_iter()
                .map(|(id, score)| SearchResult { id, score })
                .collect(),
            _ => Vec::new(),
        }
    }

    pub(crate) fn search_chunks(
        &self,
        vector: &[f32],
        limit: usize,
        min_score: f32,
    ) -> Vec<SearchResult> {
        let op = Op::SearchChunks {
            vector: vector.to_vec(),
            limit,
            min_score,
            allowed: None,
        };
        Self::hits(self.remote(op, CALL_TIMEOUT))
    }

    pub(crate) fn search_chunks_filtered(
        &self,
        vector: &[f32],
        limit: usize,
        min_score: f32,
        allowed: &HashSet<String>,
    ) -> Vec<SearchResult> {
        let op = Op::SearchChunks {
            vector: vector.to_vec(),
            limit,
            min_score,
            allowed: Some(allowed.iter().cloned().collect()),
        };
        Self::hits(self.remote(op, CALL_TIMEOUT))
    }

    pub(crate) fn search_reflections(
        &self,
        vector: &[f32],
        limit: usize,
        min_score: f32,
    ) -> Vec<SearchResult> {
        let op = Op::SearchReflections {
            vector: vector.to_vec(),
            limit,
            min_score,
        };
        Self::hits(self.remote(op, CALL_TIMEOUT))
    }

    /// The daemon's embedding of `texts`, or `None` while it is unavailable.
    pub(crate) fn embed(&self, texts: &[&str]) -> Option<Vec<Vec<f32>>> {
        let op = Op::Embed {
            texts: texts.iter().map(|text| text.to_string()).collect(),
        };
        match self.remote(op, CALL_TIMEOUT)? {
            Reply::Vectors { vectors } if vectors.len() == texts.len() => Some(vectors),
            _ => None,
        }
    }

    /// The server committed an embedding to SQLite. Ask the daemon to apply
    /// every change up to the latest one before answering, so a search right
    /// after a store finds it (bounded by [`SYNC_WAIT`]; the keyword index
    /// has it regardless). `true` when the daemon confirmed it applied them.
    pub(crate) fn sync_committed(&self) -> bool {
        let Ok(seq) = self.storage.latest_embedding_change_seq() else {
            return false;
        };
        matches!(
            self.remote(Op::Sync { seq }, SYNC_TIMEOUT),
            Some(Reply::Synced { applied: true })
        )
    }
}

/// Serve the vector socket from the daemon's warm engine until `shutdown`.
/// Never takes the daemon's heavy-work permit: searches only read the index.
pub fn spawn_for_daemon(
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    db_path: &Path,
    shutdown: Arc<AtomicBool>,
    sync_nudge: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    spawn_for_daemon_with(
        embeddings,
        search,
        db_path,
        shutdown,
        sync_nudge,
        CALL_TIMEOUT,
        Arc::new(Semaphore::new(SERVER_SLOTS)),
    )
}

/// [`spawn_for_daemon`] giving up on a connection after `serve_timeout`,
/// admitting requests through `slots`.
fn spawn_for_daemon_with(
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    db_path: &Path,
    shutdown: Arc<AtomicBool>,
    sync_nudge: Arc<Notify>,
    serve_timeout: Duration,
    slots: Arc<Semaphore>,
) -> tokio::task::JoinHandle<()> {
    let data_dir = db_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let database = db_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("csr-engine.db")
        .to_owned();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            // Off the async workers: a contended lock waits.
            let owned_dir = data_dir.clone();
            let bound = match tokio::task::spawn_blocking(move || {
                crate::hooks::prompt_transport::BoundSocket::bind_named(
                    &owned_dir,
                    "vector.sock",
                    "vector.lock",
                )
            })
            .await
            .unwrap_or_else(|e| Err(std::io::Error::other(e)))
            {
                Ok(bound) => bound,
                Err(error) => {
                    tracing::warn!(%error, "vector socket unavailable");
                    return;
                }
            };
            while !shutdown.load(Ordering::SeqCst) {
                let accepted = tokio::select! {
                    accepted = bound.listener.accept() => accepted,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => continue,
                };
                let Ok((stream, _)) = accepted else {
                    continue;
                };
                let (embeddings, search) = (embeddings.clone(), search.clone());
                let (database, nudge, slots) =
                    (database.clone(), sync_nudge.clone(), slots.clone());
                tokio::spawn(async move {
                    // Admission before anything is read or allocated: at most
                    // SERVER_SLOTS requests are buffered or computing, and a
                    // slot is held until its blocking work has finished, not
                    // just until this connection gives up waiting for it.
                    let Ok(Ok(slot)) =
                        tokio::time::timeout(serve_timeout, slots.acquire_owned()).await
                    else {
                        return;
                    };
                    let _ = tokio::time::timeout(
                        serve_timeout,
                        serve_one(stream, embeddings, search, database, nudge, slot),
                    )
                    .await;
                });
            }
        }
        #[cfg(not(unix))]
        let _ = (embeddings, search, data_dir, database, shutdown, sync_nudge);
    })
}

#[cfg(unix)]
async fn serve_one(
    mut stream: tokio::net::UnixStream,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    database: String,
    nudge: Arc<Notify>,
    slot: tokio::sync::OwnedSemaphorePermit,
) -> Option<()> {
    let size = stream.read_u32().await.ok()? as usize;
    if size > MAX_FRAME {
        return None;
    }
    let mut body = vec![0; size];
    stream.read_exact(&mut body).await.ok()?;
    let request: Request = serde_json::from_slice(&body).ok()?;
    // A server for another database or protocol gets no answer and falls
    // back; it never gets results from the wrong index.
    if request.version != VERSION || request.database != database {
        return None;
    }
    let reply = tokio::task::spawn_blocking(move || -> Result<Reply> {
        let _slot = slot;
        Ok(match request.op {
            Op::Ping => Reply::Pong,
            Op::Embed { texts } => {
                let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                Reply::Vectors {
                    vectors: embeddings.embed(&refs)?,
                }
            }
            Op::SearchChunks {
                vector,
                limit,
                min_score,
                allowed,
            } => {
                let index = search.blocking_read();
                let hits = match allowed {
                    Some(ids) => {
                        let allowed: HashSet<String> = ids.into_iter().collect();
                        index.search_chunks_filtered(&vector, limit, min_score, &allowed)
                    }
                    None => index.search_chunks(&vector, limit, min_score),
                };
                Reply::Hits {
                    hits: hits.into_iter().map(|hit| (hit.id, hit.score)).collect(),
                }
            }
            Op::SearchReflections {
                vector,
                limit,
                min_score,
            } => Reply::Hits {
                hits: search
                    .blocking_read()
                    .search_reflections(&vector, limit, min_score)
                    .into_iter()
                    .map(|hit| (hit.id, hit.score))
                    .collect(),
            },
            Op::Sync { seq } => {
                // Answer once the index sync has applied the caller's
                // committed changes, or after SYNC_WAIT regardless.
                let deadline = Instant::now() + SYNC_WAIT;
                loop {
                    if search.blocking_read().change_cursor() >= seq {
                        break Reply::Synced { applied: true };
                    }
                    if Instant::now() >= deadline {
                        break Reply::Synced { applied: false };
                    }
                    nudge.notify_one();
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        })
    })
    .await
    .ok()?
    .ok()?;
    let bytes = serde_json::to_vec(&Response {
        version: VERSION,
        reply,
    })
    .ok()?;
    if bytes.len() > MAX_FRAME {
        return None;
    }
    stream.write_u32(bytes.len() as u32).await.ok()?;
    stream.write_all(&bytes).await.ok()?;
    Some(())
}

#[cfg(all(test, unix))]
pub(crate) mod test_support {
    use super::*;
    use crate::engine::Engine;
    use crate::import::ConversationChunk;

    /// A deterministic pseudo-random 384-dim direction per seed.
    pub(crate) fn direction(seed: usize) -> Vec<f32> {
        let mut state = (seed as u64 + 11).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        (0..384)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state % 2001) as f32 / 1000.0 - 1.0
            })
            .collect()
    }

    pub(crate) const TOPICS: [&str; 6] = [
        "authentication token refresh before retry",
        "HNSW index dump written to disk",
        "embedding batch padded to the longest text",
        "daemon lock and socket handshake",
        "SQLite temp b-tree spilled during grouping",
        "release checklist and npm publish",
    ];

    /// 600 chunks over two projects, past `EXACT_SCAN_THRESHOLD` so HNSW
    /// (not the exact scan) answers, and 40 reflections; then a published
    /// generation, as the daemon leaves it.
    pub(crate) fn seed(dir: &Path) -> (PathBuf, PathBuf) {
        let db = dir.join("csr-engine.db");
        let projects = dir.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let storage = Storage::open(&db).unwrap();
        for i in 0..600 {
            storage
                .insert_chunk(
                    &ConversationChunk {
                        id: format!("chunk-{i}"),
                        conversation_id: format!("conv-{}", i % 30),
                        project_name: if i % 2 == 0 { "alpha" } else { "beta" }.into(),
                        timestamp: format!("2026-10-{:02}T00:00:00Z", 1 + i % 11),
                        content: format!("{} (note {i})", TOPICS[i % TOPICS.len()]),
                        message_count: 2,
                        summary: None,
                        author: crate::provenance::Speaker::User,
                        seq: 0,
                        is_sidechain: false,
                    },
                    &direction(i),
                )
                .unwrap();
        }
        for i in 0..40 {
            storage
                .insert_reflection(
                    &format!("reflection-{i}"),
                    TOPICS[i % TOPICS.len()],
                    &["solution".into()],
                    &direction(10_000 + i),
                )
                .unwrap();
        }
        drop(storage);
        // Cache miss: rebuilds from SQLite and publishes the generation.
        drop(Engine::new(&db, &projects).unwrap());
        (db, projects)
    }

    pub(crate) struct TestDaemon {
        shutdown: Arc<AtomicBool>,
        handles: Vec<tokio::task::JoinHandle<()>>,
        pub(crate) search: Arc<RwLock<SearchEngine>>,
        pub(crate) embeddings: Arc<EmbeddingEngine>,
    }

    impl TestDaemon {
        pub(crate) async fn start(db: &Path) -> Self {
            let data_dir = db.parent().unwrap();
            let storage = Arc::new(Storage::open(db).unwrap());
            let chunks = storage.count_chunk_embeddings().unwrap();
            let reflections = storage.count_reflection_embeddings().unwrap();
            let index = SearchEngine::load_from_disk(&data_dir.join("index"), chunks, reflections)
                .expect("published generation");
            let search = Arc::new(RwLock::new(index));
            let shutdown = Arc::new(AtomicBool::new(false));
            let nudge = Arc::new(Notify::new());
            let sync = crate::daemon::index_sync::spawn_with_nudge(
                storage,
                search.clone(),
                shutdown.clone(),
                nudge.clone(),
            );
            let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
            let server = spawn_for_daemon(
                embeddings.clone(),
                search.clone(),
                db,
                shutdown.clone(),
                nudge,
            );
            tokio::time::timeout(Duration::from_secs(5), async {
                while !socket_path(data_dir).exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            std::fs::write(data_dir.join("daemon.lock"), std::process::id().to_string()).unwrap();
            write_heartbeat(data_dir);
            Self {
                shutdown,
                handles: vec![sync, server],
                search,
                embeddings,
            }
        }

        pub(crate) async fn stop(self) {
            self.shutdown.store(true, Ordering::SeqCst);
            for handle in self.handles {
                handle.await.unwrap();
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::engine::Engine;

    fn same(label: &str, proxied: &[SearchResult], local: &[SearchResult]) {
        let ids = |hits: &[SearchResult]| hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>();
        assert!(!local.is_empty(), "{label}: empty fixture answer");
        assert_eq!(ids(proxied), ids(local), "{label}");
        for (p, l) in proxied.iter().zip(local) {
            assert!(
                (p.score - l.score).abs() < 1e-5,
                "{label}: {} vs {}",
                p.score,
                l.score
            );
        }
    }

    async fn compare_primitives(proxied: &Engine, local: &Engine, tag: &str) {
        let alpha: HashSet<String> = local
            .storage()
            .get_chunk_ids_for_project("alpha")
            .unwrap()
            .into_iter()
            .collect();
        let p = proxied.search().read().await;
        let l = local.search().read().await;
        for q in 0..25 {
            let v = direction(50_000 + q);
            same(
                &format!("{tag} chunks q{q}"),
                &p.search_chunks(&v, 10, -1.0),
                &l.search_chunks(&v, 10, -1.0),
            );
            same(
                &format!("{tag} alpha q{q}"),
                &p.search_chunks_filtered(&v, 10, -1.0, &alpha),
                &l.search_chunks_filtered(&v, 10, -1.0, &alpha),
            );
            same(
                &format!("{tag} reflections q{q}"),
                &p.search_reflections(&v, 10, -1.0),
                &l.search_reflections(&v, 10, -1.0),
            );
        }
    }

    /// Tool output without the timings it reports.
    fn untimed(text: &str) -> String {
        let perf = regex::Regex::new(r"(?s)<perf>.*?</perf>|⚡ PERFORMANCE: \d+ms").unwrap();
        perf.replace_all(text, "").into_owned()
    }

    fn copy_data_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to.join("index")).unwrap();
        std::fs::create_dir_all(to.join("projects")).unwrap();
        for entry in std::fs::read_dir(from).unwrap().flatten() {
            if entry.path().is_file()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("csr-engine.db")
            {
                std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
            }
        }
        for entry in std::fs::read_dir(from.join("index")).unwrap().flatten() {
            std::fs::copy(entry.path(), to.join("index").join(entry.file_name())).unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn proxied_answers_equal_the_in_process_index() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (db, projects) = seed(tmp.path());
        // The in-process side reads its own copy, so tool calls that record
        // retrievals evolve two identical databases identically.
        let local_dir = tempfile::TempDir::new().unwrap();
        copy_data_dir(tmp.path(), local_dir.path());
        let local = Engine::new_for_hook(
            &local_dir.path().join("csr-engine.db"),
            &local_dir.path().join("projects"),
        )
        .unwrap();

        let daemon = TestDaemon::start(&db).await;
        let proxied = Engine::new_proxied(&db, &projects)
            .unwrap()
            .expect("the daemon answers");
        assert!(proxied.is_proxied());
        let proxy = proxied.search().read().await.proxy().unwrap().clone();

        compare_primitives(&proxied, &local, "daemon").await;
        assert_eq!(proxy.failures(), 0, "every call answered by the daemon");

        for query in ["authentication token refresh", "index dump to disk"] {
            let (s, e, x) = (proxied.storage(), proxied.embeddings(), proxied.search());
            let (ls, le, lx) = (local.storage(), local.embeddings(), local.search());
            let tools = crate::mcp::tools::reflect_on_past(s, e, x, query, 5, 0.0, Some("all"))
                .await
                .unwrap();
            let expected =
                crate::mcp::tools::reflect_on_past(ls, le, lx, query, 5, 0.0, Some("all"))
                    .await
                    .unwrap();
            assert_eq!(untimed(&tools), untimed(&expected), "{query}");
            assert!(tools.contains("<id>"), "{tools}");
            let tools = crate::mcp::tools::quick_check(s, e, x, query, 0.0, Some("alpha"))
                .await
                .unwrap();
            let expected = crate::mcp::tools::quick_check(ls, le, lx, query, 0.0, Some("alpha"))
                .await
                .unwrap();
            assert_eq!(untimed(&tools), untimed(&expected), "{query}");
        }
        assert!(
            !proxied.embeddings().is_loaded(),
            "queries were embedded by the daemon"
        );

        // A reflection stored through the server is in the daemon's index
        // when the store returns (review finding #7), not at its next poll.
        crate::mcp::tools::store_reflection_internal(
            proxied.storage(),
            proxied.embeddings(),
            proxied.search(),
            "proxied servers write SQLite and wait for the daemon to apply it",
            &["solution".into()],
        )
        .await
        .unwrap();
        assert_eq!(daemon.search.read().await.reflection_count(), 41);
        assert!(!proxied.embeddings().is_loaded());
        daemon.stop().await;
    }

    /// Review finding #7: a store returns only after the daemon applied it,
    /// so the very next vector search finds the stored reflection. The
    /// daemon's sync loop polls every 10 s; only the acknowledged Sync can
    /// make this immediate.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_stored_reflection_is_searchable_when_the_store_returns() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (db, projects) = seed(tmp.path());
        let daemon = TestDaemon::start(&db).await;
        let proxied = Engine::new_proxied(&db, &projects).unwrap().unwrap();
        for round in 0..5 {
            let content = format!("stored then searched, round {round}");
            crate::mcp::tools::store_reflection_internal(
                proxied.storage(),
                proxied.embeddings(),
                proxied.search(),
                &content,
                &["solution".into()],
            )
            .await
            .unwrap();
            let vector = daemon_embedding(&daemon, &content).await;
            let hits = proxied
                .search()
                .read()
                .await
                .search_reflections(&vector, 1, 0.99);
            assert_eq!(hits.len(), 1, "round {round}: found right after the store");
        }
        // The acknowledgement itself: a change committed to SQLite is in the
        // daemon's index when Sync answers `applied`.
        let proxy = proxied.search().read().await.proxy().unwrap().clone();
        proxied
            .storage()
            .insert_reflection("acked", "acked", &[], &direction(77_777))
            .unwrap();
        let synced = tokio::task::spawn_blocking(move || proxy.sync_committed())
            .await
            .unwrap();
        assert!(synced, "the daemon confirms it applied the change");
        assert!(daemon.search.read().await.has_reflection("acked"));
        daemon.stop().await;
    }

    async fn daemon_embedding(daemon: &TestDaemon, text: &str) -> Vec<f32> {
        let embeddings = daemon.embeddings.clone();
        let text = text.to_string();
        tokio::task::spawn_blocking(move || embeddings.embed_single(&text).unwrap())
            .await
            .unwrap()
    }

    fn fast(proxy: &VectorProxy) {
        proxy.set_timings(ProxyTimings {
            backoff_first: Duration::from_millis(20),
            backoff_total: Duration::from_millis(100),
            down_window: Duration::from_secs(30),
        });
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_daemon_back_within_the_backoff_answers_the_call() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (db, projects) = seed(tmp.path());
        let daemon = TestDaemon::start(&db).await;
        let proxied = Engine::new_proxied(&db, &projects).unwrap().unwrap();
        let proxy = proxied.search().read().await.proxy().unwrap().clone();
        proxy.set_timings(ProxyTimings {
            backoff_first: Duration::from_millis(50),
            backoff_total: Duration::from_secs(5),
            down_window: Duration::from_secs(30),
        });
        daemon.stop().await;
        // The daemon restarts while the call is backing off.
        let restart = {
            let db = db.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(400)).await;
                TestDaemon::start(&db).await
            })
        };
        let hits = proxied
            .search()
            .read()
            .await
            .search_chunks(&direction(7), 5, -1.0);
        assert_eq!(hits.len(), 5);
        assert_eq!(
            proxy.failures(),
            0,
            "a restart inside the backoff is not an outage"
        );
        assert_eq!(proxy.path(), PATH_DAEMON);
        restart.await.unwrap().stop().await;
    }

    /// Review findings #2, #3, #4, #10: an outage never loads anything into
    /// the server — no index to go stale, no model to race, nothing for the
    /// allocator to keep — and recovery needs no release step.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_outage_loads_nothing_and_the_daemon_is_used_again_when_it_answers() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (db, projects) = seed(tmp.path());
        let run_dir = tmp.path().join("run");
        let identity = crate::binary_identity::FileIdentity::capture(&db).unwrap();
        let record =
            crate::binary_identity::write_run_record(&run_dir, "mcp_stdio", identity).unwrap();
        let recorded = || -> Option<String> {
            let json: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&record).ok()?).ok()?;
            json["search_path"].as_str().map(str::to_owned)
        };

        // Started while the daemon is down (heartbeat fresh, no socket):
        // proxied all the same (finding #4).
        write_heartbeat(tmp.path());
        let proxied = Engine::new_proxied(&db, &projects)
            .unwrap()
            .expect("configured");
        let proxy = proxied.search().read().await.proxy().unwrap().clone();
        fast(&proxy);
        proxy.attach_run_record(record.clone());
        let search = || {
            let search = proxied.search().clone();
            async move { search.read().await.search_chunks(&direction(9), 5, -1.0) }
        };
        assert!(
            search().await.is_empty(),
            "nothing to answer with, nothing loaded"
        );
        assert_eq!(proxy.path(), PATH_KEYWORD_ONLY);
        assert_eq!(recorded().as_deref(), Some(PATH_KEYWORD_ONLY));
        let embedded = proxied
            .embeddings()
            .embed_single("daemon lock and socket handshake");
        assert!(embedded.is_err());
        assert!(
            !proxied.embeddings().is_loaded(),
            "the model is never loaded in-process"
        );
        // Within the down window a call does not wait out the backoff again.
        let started = Instant::now();
        assert!(!proxy.available());
        assert!(started.elapsed() < Duration::from_secs(2));

        let daemon = TestDaemon::start(&db).await;
        let check = proxy.clone();
        assert!(tokio::task::spawn_blocking(move || check.available())
            .await
            .unwrap());
        assert_eq!(search().await.len(), 5);
        assert_eq!(proxy.path(), PATH_DAEMON);
        assert_eq!(recorded().as_deref(), Some(PATH_DAEMON));
        assert!(proxied.embeddings().embed_single("daemon lock").is_ok());
        assert!(!proxied.embeddings().is_loaded());
        let servers = crate::binary_identity::live_mcp_servers(&run_dir);
        assert_eq!(servers[0].search_path.as_deref(), Some(PATH_DAEMON));

        // It dies mid-session: keyword-only again, then back.
        daemon.stop().await;
        assert!(search().await.is_empty());
        assert_eq!(recorded().as_deref(), Some(PATH_KEYWORD_ONLY));
        let daemon = TestDaemon::start(&db).await;
        let check = proxy.clone();
        assert!(tokio::task::spawn_blocking(move || check.available())
            .await
            .unwrap());
        assert_eq!(search().await.len(), 5);
        daemon.stop().await;
    }

    /// Send raw bytes; `Some(reply)` if the server answered a full frame.
    fn raw(data_dir: &Path, bytes: &[u8], close_after: bool) -> Option<Vec<u8>> {
        let mut stream = std::os::unix::net::UnixStream::connect(socket_path(data_dir)).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
        stream.write_all(bytes).ok()?;
        if close_after {
            stream.shutdown(std::net::Shutdown::Write).ok()?;
        }
        let mut header = [0u8; 4];
        stream.read_exact(&mut header).ok()?;
        let mut body = vec![0; u32::from_be_bytes(header) as usize];
        stream.read_exact(&mut body).ok()?;
        Some(body)
    }

    fn frame(json: &str) -> Vec<u8> {
        let mut bytes = (json.len() as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(json.as_bytes());
        bytes
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn malformed_and_oversized_frames_get_no_answer_and_break_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (db, _) = seed(tmp.path());
        let daemon = TestDaemon::start(&db).await;
        let dir = tmp.path().to_path_buf();
        let ping = r#"{"version":1,"database":"csr-engine.db","op":{"op":"ping"}}"#;
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("not json", frame("not json")),
            (
                "wrong version",
                frame(r#"{"version":9,"database":"csr-engine.db","op":{"op":"ping"}}"#),
            ),
            (
                "other database",
                frame(r#"{"version":1,"database":"other.db","op":{"op":"ping"}}"#),
            ),
            (
                "unknown op",
                frame(r#"{"version":1,"database":"csr-engine.db","op":{"op":"drop"}}"#),
            ),
            ("oversized", ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec()),
            ("truncated", {
                let mut bytes = 100u32.to_be_bytes().to_vec();
                bytes.extend_from_slice(b"{\"version\"");
                bytes
            }),
        ];
        let answered = tokio::task::spawn_blocking(move || {
            let mut answered = Vec::new();
            for (name, bytes) in cases {
                answered.push((name, raw(&dir, &bytes, true).is_some()));
            }
            (answered, raw(&dir, &frame(ping), false))
        })
        .await
        .unwrap();
        for (name, got_reply) in answered.0 {
            assert!(!got_reply, "{name} must get no answer");
        }
        let pong: Response = serde_json::from_slice(&answered.1.expect("still serving")).unwrap();
        assert!(matches!(pong.reply, Reply::Pong));
        daemon.stop().await;
    }

    /// Review finding #6: a connection that gives up must not free its slot
    /// while its blocking work still runs, or waves of slow requests pile up
    /// unbounded work.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn slots_stay_held_until_the_blocking_work_finishes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (db, _) = seed(tmp.path());
        let storage = Arc::new(Storage::open(&db).unwrap());
        let index = SearchEngine::load_from_disk(
            &tmp.path().join("index"),
            storage.count_chunk_embeddings().unwrap(),
            storage.count_reflection_embeddings().unwrap(),
        )
        .unwrap();
        let search = Arc::new(RwLock::new(index));
        let shutdown = Arc::new(AtomicBool::new(false));
        let slots = Arc::new(Semaphore::new(SERVER_SLOTS));
        let serve_timeout = Duration::from_millis(200);
        let server = spawn_for_daemon_with(
            Arc::new(EmbeddingEngine::new().unwrap()),
            search.clone(),
            &db,
            shutdown.clone(),
            Arc::new(Notify::new()),
            serve_timeout,
            slots.clone(),
        );
        while !socket_path(tmp.path()).exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Every search now blocks on the index lock.
        let held = search.write().await;
        let dir = tmp.path().to_path_buf();
        let vector: Vec<f32> = direction(1);
        let search_frame = frame(
            &serde_json::to_string(&Request {
                version: VERSION,
                database: "csr-engine.db".into(),
                op: Op::SearchChunks {
                    vector,
                    limit: 3,
                    min_score: -1.0,
                    allowed: None,
                },
            })
            .unwrap(),
        );
        let ping = frame(r#"{"version":1,"database":"csr-engine.db","op":{"op":"ping"}}"#);
        let stuck: Vec<_> = (0..SERVER_SLOTS)
            .map(|_| {
                let (dir, bytes) = (dir.clone(), search_frame.clone());
                std::thread::spawn(move || raw(&dir, &bytes, false))
            })
            .collect();
        // All eight admitted (and blocked on the lock)...
        tokio::time::timeout(Duration::from_secs(10), async {
            while slots.available_permits() > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("eight searches admitted");
        // ...then well past the server's give-up time for each of them (the
        // old code freed every slot then), none has come back.
        tokio::time::sleep(serve_timeout * 3).await;
        assert_eq!(
            slots.available_permits(),
            0,
            "slots held by unfinished work"
        );
        let (d, p) = (dir.clone(), ping.clone());
        let refused = tokio::task::spawn_blocking(move || raw(&d, &p, false))
            .await
            .unwrap();
        assert!(
            refused.is_none(),
            "no slot while eight jobs are still blocked"
        );
        drop(held);
        for thread in stuck {
            let _ = thread.join();
        }
        let (d, p) = (dir.clone(), ping.clone());
        let answered = tokio::task::spawn_blocking(move || raw(&d, &p, false))
            .await
            .unwrap();
        assert!(answered.is_some(), "slots come back once the work finishes");
        shutdown.store(true, Ordering::SeqCst);
        server.await.unwrap();
    }

    #[test]
    fn a_daemon_is_configured_by_a_fresh_heartbeat_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (db, projects) = seed(tmp.path());
        assert!(!daemon_configured(tmp.path()), "never ran a daemon");
        assert!(Engine::new_proxied(&db, &projects).unwrap().is_none());
        write_heartbeat(tmp.path());
        assert!(daemon_configured(tmp.path()));
        assert!(
            Engine::new_proxied(&db, &projects).unwrap().is_some(),
            "socket absent is fine"
        );
        // A heartbeat older than a day: the daemon was retired.
        let stale = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(heartbeat_path(tmp.path()))
            .unwrap()
            .set_modified(stale)
            .unwrap();
        assert!(!daemon_configured(tmp.path()));
        assert!(Engine::new_proxied(&db, &projects).unwrap().is_none());
    }
}
