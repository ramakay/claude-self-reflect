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
//! again, and when it answers they use it. Without a configured daemon the
//! server uses the full in-process engine.

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
const WAITING_CONNECTIONS: usize = 32;
const MAX_EMBED_TEXTS: usize = 256;
const MAX_EMBED_BYTES: usize = 1 << 20;
/// A heartbeat this recent means a daemon is configured for this data dir.
const CONFIGURED_WITHIN: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the daemon refreshes its heartbeat.
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(10 * 60);

/// What answers an MCP server's vector calls; recorded in its run record
/// (`csr-engine status` lists it under `mcp_servers`) and logged on change.
pub const PATH_DAEMON: &str = "daemon";
/// No request has established daemon availability yet.
pub const PATH_UNKNOWN: &str = "unknown (daemon unverified)";
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
    write_heartbeat_for_database(&data_dir.join("csr-engine.db"));
}

pub(crate) fn write_heartbeat_for_database(db_path: &Path) {
    let Some(data_dir) = db_path.parent() else {
        return;
    };
    let path = heartbeat_path(data_dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        if let Some(dir) = path.parent() {
            // Same private directory the sockets require (0700).
            let _ = std::fs::DirBuilder::new().mode(0o700).create(dir);
        }
    }
    let body = serde_json::json!({
        "pid": std::process::id(),
        "at": chrono::Utc::now().to_rfc3339(),
        "database": database_identity(db_path),
    })
    .to_string();
    let tmp = path.with_extension("alive.tmp");
    if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Whether a daemon is configured for `data_dir`: `CSR_NO_DAEMON_PROXY=1`
/// says no; an explicit opt-in, matching launchd job, database heartbeat or
/// verified ping/Busy reply says yes. Configuration survives outages and saturation.
pub fn daemon_configured(data_dir: &Path) -> bool {
    daemon_configured_for_database(&data_dir.join("csr-engine.db"))
}

pub(crate) fn daemon_configured_for_database(db_path: &Path) -> bool {
    let set = |name: &str| std::env::var(name).is_ok_and(|v| v == "1" || v == "true");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    daemon_configured_with(
        db_path,
        home.as_deref(),
        set("CSR_NO_DAEMON_PROXY"),
        set("CSR_DAEMON_PROXY"),
        |db| {
            VectorClient::for_database(db)
                .context("no vector client")?
                .ping_reply(Duration::from_millis(250))
        },
    )
}

/// Resolve existing ancestors too, so a not-yet-created database can be compared
/// through a symlinked data directory without requiring either file to exist.
fn database_identity(path: &Path) -> String {
    fn resolve(path: &Path) -> PathBuf {
        if let Ok(canonical) = path.canonicalize() {
            return canonical;
        }
        if path.components().next_back() == Some(std::path::Component::ParentDir) {
            let mut parent = resolve(path.parent().unwrap_or(Path::new("/")));
            parent.pop();
            return parent;
        }
        match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => resolve(parent).join(name),
            _ => path.to_path_buf(),
        }
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    resolve(&absolute).to_string_lossy().into_owned()
}

fn plist_skip_misc(input: &mut &str) -> Option<()> {
    loop {
        *input = input.trim_start();
        if input.starts_with("<!--") {
            *input = &input[input.find("-->")? + 3..];
        } else if input.starts_with("<?") {
            *input = &input[input.find("?>")? + 2..];
        } else if input.starts_with("<!DOCTYPE") {
            *input = &input[input.find('>')? + 1..];
        } else {
            break;
        }
    }
    Some(())
}

/// Read launchd's XML property-list values without executing the job. Unknown
/// or malformed configuration is not evidence that this database has a daemon.
fn plist_value(input: &mut &str) -> Option<serde_json::Value> {
    use serde_json::Value;
    plist_skip_misc(input)?;
    let end = input.find('>')?;
    let tag = input.get(1..end)?;
    let empty = tag.ends_with('/');
    let name = tag.trim_end_matches('/').split_whitespace().next()?;
    *input = &input[end + 1..];
    let close = format!("</{name}>");
    let value = match name {
        "plist" if !empty => plist_value(input)?,
        "dict" => {
            let mut map = serde_json::Map::new();
            while !empty && !input.is_empty() {
                plist_skip_misc(input)?;
                if input.starts_with(&close) {
                    break;
                }
                let key = plist_value(input)?.as_str()?.to_owned();
                let value = plist_value(input)?;
                map.insert(key, value);
            }
            Value::Object(map)
        }
        "array" => {
            let mut values = Vec::new();
            while !empty && !input.is_empty() {
                plist_skip_misc(input)?;
                if input.starts_with(&close) {
                    break;
                }
                values.push(plist_value(input)?);
            }
            Value::Array(values)
        }
        "string" | "key" | "integer" | "real" | "data" | "date" => {
            let end = if empty { 0 } else { input.find(&close)? };
            let text = &input[..end];
            let mut decoded = String::new();
            let mut rest = text;
            while let Some(at) = rest.find('&') {
                decoded.push_str(&rest[..at]);
                let end = rest[at..].find(';')? + at;
                let entity = &rest[at + 1..end];
                decoded.push(match entity {
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    _ => {
                        let number = if let Some(hex) = entity.strip_prefix("#x") {
                            u32::from_str_radix(hex, 16).ok()?
                        } else {
                            entity.strip_prefix('#')?.parse().ok()?
                        };
                        char::from_u32(number)?
                    }
                });
                rest = &rest[end + 1..];
            }
            decoded.push_str(rest);
            *input = &input[end..];
            Value::String(decoded)
        }
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => return None,
    };
    if !empty {
        // Whitespace inside strings is data; only skip XML miscellany for containers.
        if matches!(name, "plist" | "dict" | "array") {
            plist_skip_misc(input)?;
        }
        *input = input.strip_prefix(&close)?;
    }
    Some(value)
}

fn launchd_database(home: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(
        home.join("Library/LaunchAgents/com.ramakay.csr-engine.daemon.plist"),
    )
    .ok()?;
    let plist = plist_value(&mut text.as_str())?;
    plist.as_object()?;
    let args = plist
        .get("ProgramArguments")
        .and_then(serde_json::Value::as_array);
    let argument = args.and_then(|args| {
        args.iter().enumerate().find_map(|(i, arg)| {
            let arg = arg.as_str()?;
            if arg == "--db-path" {
                args.get(i + 1)?.as_str()
            } else {
                arg.strip_prefix("--db-path=")
            }
        })
    });
    let env = plist.get("EnvironmentVariables");
    let path = if let Some(path) = argument {
        PathBuf::from(path)
    } else {
        env.and_then(|env| env.get("HOME"))
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| home.to_path_buf())
            .join(".claude-self-reflect/csr-engine.db")
    };
    Some(if path.is_absolute() {
        path
    } else {
        Path::new(
            plist
                .get("WorkingDirectory")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("/"),
        )
        .join(path)
    })
}

fn daemon_configured_with(
    db_path: &Path,
    home: Option<&Path>,
    disabled: bool,
    enabled: bool,
    ping: impl FnOnce(&Path) -> Result<Reply>,
) -> bool {
    if disabled {
        return false;
    }
    if enabled
        || home.is_some_and(|home| {
            launchd_database(home).is_some_and(|configured| {
                database_identity(&configured) == database_identity(db_path)
            })
        })
    {
        return true;
    }
    let Some(data_dir) = db_path.parent() else {
        return false;
    };
    let fresh = std::fs::metadata(heartbeat_path(data_dir))
        .and_then(|meta| meta.modified())
        .ok()
        .map(|modified| {
            SystemTime::now()
                .duration_since(modified)
                .unwrap_or_default()
        })
        .is_some_and(|age| age < CONFIGURED_WITHIN);
    fresh
        && std::fs::read_to_string(heartbeat_path(data_dir))
            .ok()
            .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
            .is_some_and(|body| {
                body["database"].as_str() == Some(database_identity(db_path).as_str())
            })
        || matches!(
            ping(db_path),
            Ok(Reply::Pong
                | Reply::Error {
                    code: ErrorCode::Busy,
                    ..
                })
        )
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
    Error { code: ErrorCode, message: String },
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ErrorCode {
    Busy,
    EmbeddingLimit,
}

fn validate_work(op: &Op) -> Option<Reply> {
    if let Op::Embed { texts } = op {
        let bytes = texts
            .iter()
            .try_fold(0usize, |sum, text| sum.checked_add(text.len()));
        if texts.len() > MAX_EMBED_TEXTS || bytes.is_none_or(|bytes| bytes > MAX_EMBED_BYTES) {
            return Some(Reply::Error {
                code: ErrorCode::EmbeddingLimit,
                message: "embedding request exceeds 256 texts or 1 MiB of text".into(),
            });
        }
    }
    None
}

#[derive(Serialize, Deserialize)]
struct Response {
    version: u32,
    /// Included even in pre-admission Busy replies. Old identity-free replies
    /// cannot establish that a daemon serves the caller's database.
    #[serde(default)]
    database: Option<String>,
    reply: Reply,
}

impl Response {
    fn for_database(self, database: &str) -> Result<Reply> {
        if self.version != VERSION {
            bail!("daemon speaks vector protocol {}", self.version);
        }
        if self.database.as_deref() != Some(database) {
            bail!("daemon database identity does not match");
        }
        Ok(self.reply)
    }
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
            database: database_identity(db_path),
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

    // Configuration accepts a verified Pong or Busy even under saturation.
    fn ping_reply(&self, timeout: Duration) -> Result<Reply> {
        let request = self.encode(Op::Ping)?;
        self.send_reply(&request, Instant::now() + timeout)
    }

    fn send(&self, request: &[u8], deadline: Instant) -> Result<Reply> {
        let reply = self.send_reply(request, deadline)?;
        if let Reply::Error { code, message } = reply {
            bail!("daemon rejected request ({code:?}): {message}");
        }
        Ok(reply)
    }

    #[cfg(unix)]
    fn send_reply(&self, request: &[u8], deadline: Instant) -> Result<Reply> {
        let mut stream = connect_by(&self.socket, deadline)
            .with_context(|| format!("connecting {}", self.socket.display()))?;
        let mut frame = Vec::with_capacity(4 + request.len());
        frame.extend_from_slice(&(request.len() as u32).to_be_bytes());
        frame.extend_from_slice(request);
        write_by(&mut stream, deadline, &frame).context("writing request")?;
        let mut header = [0u8; 4];
        read_by(&mut stream, deadline, &mut header).context("reading reply")?;
        let size = u32::from_be_bytes(header) as usize;
        if size > MAX_FRAME {
            bail!("reply of {size} bytes exceeds the frame cap");
        }
        let mut body = vec![0; size];
        read_by(&mut stream, deadline, &mut body).context("reading reply")?;
        let response: Response = serde_json::from_slice(&body)?;
        response.for_database(&self.database)
    }

    #[cfg(not(unix))]
    fn send_reply(&self, _request: &[u8], _deadline: Instant) -> Result<Reply> {
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
    if Instant::now() >= deadline {
        return Err(std::io::ErrorKind::TimedOut.into());
    }
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
        Ok(std::os::unix::net::UnixStream::from_raw_fd(fd))
    }
}

/// Poll and perform one nonblocking operation before the absolute deadline.
#[cfg(unix)]
fn io_by(
    stream: &mut std::os::unix::net::UnixStream,
    deadline: Instant,
    events: libc::c_short,
    mut op: impl FnMut(&mut std::os::unix::net::UnixStream) -> std::io::Result<usize>,
) -> std::io::Result<usize> {
    use std::os::fd::AsRawFd;
    stream.set_nonblocking(true)?;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        let mut descriptor = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        let millis = remaining.as_millis().max(1).min(i32::MAX as u128) as libc::c_int;
        // SAFETY: poll borrows the live stream descriptor for this call only.
        let ready = unsafe { libc::poll(&mut descriptor, 1, millis) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            continue;
        }
        if Instant::now() >= deadline {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        match op(stream) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            result => return result,
        }
    }
}

#[cfg(unix)]
fn read_by(
    stream: &mut std::os::unix::net::UnixStream,
    deadline: Instant,
    mut bytes: &mut [u8],
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        match io_by(stream, deadline, libc::POLLIN, |s| s.read(bytes)) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn write_by(
    stream: &mut std::os::unix::net::UnixStream,
    deadline: Instant,
    mut bytes: &[u8],
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        match io_by(stream, deadline, libc::POLLOUT, |s| s.write(bytes)) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn wait_for_sync(
    data_dir: &Path,
    search: &RwLock<SearchEngine>,
    seq: i64,
    nudge: &Notify,
    deadline: Instant,
) -> bool {
    // Hold exclusion through the acknowledgement, including already-applied/no-op cursors.
    let Ok(_operation) = crate::maintenance::shared(data_dir) else {
        return false;
    };
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        if search
            .try_read()
            .is_ok_and(|index| index.change_cursor() >= seq)
        {
            return true;
        }
        nudge.notify_one();
        std::thread::sleep(remaining.min(Duration::from_millis(5)));
    }
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
    answered: AtomicBool,
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
            answered: AtomicBool::new(false),
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
        } else if self.answered.load(Ordering::Relaxed) {
            PATH_DAEMON
        } else {
            PATH_UNKNOWN
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
        let first_reply = !self.answered.swap(true, Ordering::Relaxed);
        let was_down = self
            .down_since
            .lock()
            .map(|mut down| down.take().is_some())
            .unwrap_or(false);
        if was_down || first_reply {
            tracing::info!("daemon answers: semantic search available");
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
    fn await_daemon(&self, deadline: Instant) -> bool {
        let timings = self.timings();
        let started = Instant::now();
        let mut delay = timings.backoff_first;
        loop {
            let remaining = timings
                .backoff_total
                .saturating_sub(started.elapsed())
                .min(deadline.saturating_duration_since(Instant::now()));
            if remaining.is_zero() {
                return false;
            }
            std::thread::sleep(delay.min(remaining));
            if matches!(
                self.client.call(
                    Op::Ping,
                    PING_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()))
                ),
                Ok(Reply::Pong)
            ) {
                return true;
            }
            delay = delay.saturating_mul(2);
        }
    }

    /// Whether the daemon answers now, waiting out the backoff unless it
    /// failed within the down window. Blocking: call it off the runtime.
    pub fn available(&self) -> bool {
        let deadline = Instant::now() + self.timings().backoff_total;
        if matches!(
            self.client.call(
                Op::Ping,
                PING_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()))
            ),
            Ok(Reply::Pong)
        ) {
            self.mark_up();
            return true;
        }
        if !self.recently_down() && self.await_daemon(deadline) {
            self.mark_up();
            return true;
        }
        self.mark_down(&anyhow::anyhow!("daemon did not answer a ping"));
        false
    }

    fn remote(&self, op: Op, timeout: Duration) -> Option<Reply> {
        off_worker(|| {
            let deadline = Instant::now() + timeout;
            let request = match self.client.encode(op) {
                Ok(request) => request,
                Err(error) => {
                    self.mark_down(&error);
                    return None;
                }
            };
            let error = match self.client.send(&request, deadline) {
                Ok(reply) => {
                    self.calls.fetch_add(1, Ordering::Relaxed);
                    self.mark_up();
                    return Some(reply);
                }
                Err(error) => error,
            };
            if !self.recently_down() && self.await_daemon(deadline) {
                if let Ok(reply) = self.client.send(&request, deadline) {
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
    let database = database_identity(db_path);
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
            let waiting = Arc::new(Semaphore::new(WAITING_CONNECTIONS));
            while !shutdown.load(Ordering::SeqCst) {
                let accepted = tokio::select! {
                    accepted = bound.listener.accept() => accepted,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => continue,
                };
                let Ok((stream, _)) = accepted else {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                };
                let Ok(waiting_slot) = waiting.clone().try_acquire_owned() else {
                    let bytes = serde_json::to_vec(&Response {
                        version: VERSION,
                        database: Some(database.clone()),
                        reply: Reply::Error {
                            code: ErrorCode::Busy,
                            message: "vector daemon is busy".into(),
                        },
                    })
                    .expect("busy reply encodes");
                    let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
                    frame.extend_from_slice(&bytes);
                    let _ = stream
                        .into_std()
                        .and_then(|mut stream| stream.write_all(&frame));
                    continue;
                };
                let (embeddings, search) = (embeddings.clone(), search.clone());
                let data_dir = data_dir.clone();
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
                    drop(waiting_slot);
                    let _ = tokio::time::timeout(
                        serve_timeout,
                        serve_one(stream, embeddings, search, database, data_dir, nudge, slot),
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
    data_dir: PathBuf,
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
    let reply = if let Some(error) = validate_work(&request.op) {
        error
    } else {
        tokio::task::spawn_blocking(move || -> Result<Reply> {
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
                Op::Sync { seq } => Reply::Synced {
                    applied: wait_for_sync(
                        &data_dir,
                        &search,
                        seq,
                        &nudge,
                        Instant::now() + SYNC_WAIT,
                    ),
                },
            })
        })
        .await
        .ok()?
        .ok()?
    };
    let bytes = serde_json::to_vec(&Response {
        version: VERSION,
        database: Some(database),
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
            let index =
                SearchEngine::load_from_disk(&data_dir.join("index-v3"), chunks, reflections)
                    .unwrap()
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
        std::fs::create_dir_all(to.join("index-v3")).unwrap();
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
        for entry in std::fs::read_dir(from.join("index-v3")).unwrap().flatten() {
            std::fs::copy(entry.path(), to.join("index-v3").join(entry.file_name())).unwrap();
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
        let local = Engine::new(
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
        let ping = serde_json::to_string(&Request {
            version: VERSION,
            database: database_identity(&db),
            op: Op::Ping,
        })
        .unwrap();
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
            (answered, raw(&dir, &frame(&ping), false))
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
            &tmp.path().join("index-v3"),
            storage.count_chunk_embeddings().unwrap(),
            storage.count_reflection_embeddings().unwrap(),
        )
        .unwrap()
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
        tokio::time::timeout(Duration::from_secs(3), async {
            while !socket_path(tmp.path()).exists() {
                assert!(!server.is_finished(), "vector socket bind failed");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("vector socket became ready");
        // Every search now blocks on the index lock.
        let held = search.write().await;
        let dir = tmp.path().to_path_buf();
        let vector: Vec<f32> = direction(1);
        let search_frame = frame(
            &serde_json::to_string(&Request {
                version: VERSION,
                database: database_identity(&db),
                op: Op::SearchChunks {
                    vector,
                    limit: 3,
                    min_score: -1.0,
                    allowed: None,
                },
            })
            .unwrap(),
        );
        let ping = frame(
            &serde_json::to_string(&Request {
                version: VERSION,
                database: database_identity(&db),
                op: Op::Ping,
            })
            .unwrap(),
        );
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
    fn a_fresh_heartbeat_configures_an_otherwise_unconfigured_daemon() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("csr-engine.db");
        let projects = tmp.path().join("projects");
        let offline = |_: &Path| anyhow::bail!("no daemon reply");
        // No dependency on the user's launchd installation or proxy environment.
        assert!(!daemon_configured_with(&db, None, false, false, offline));
        write_heartbeat(tmp.path());
        assert!(daemon_configured_with(&db, None, false, false, |_| panic!(
            "a fresh heartbeat does not require a ping"
        )));
        assert!(
            Engine::new_proxied(&db, &projects).unwrap().is_some(),
            "socket absent is fine"
        );
        // Without durable configuration or a typed reply, an old heartbeat expires.
        let stale = SystemTime::now() - Duration::from_secs(25 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(heartbeat_path(tmp.path()))
            .unwrap()
            .set_modified(stale)
            .unwrap();
        assert!(!daemon_configured_with(&db, None, false, false, offline));
    }

    #[test]
    fn r3_dripping_reads_obey_one_absolute_deadline() {
        let (mut client, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let writer = std::thread::spawn(move || {
            for _ in 0..12 {
                std::thread::sleep(Duration::from_millis(25));
                if peer.write_all(&[1]).is_err() {
                    break;
                }
            }
        });
        let start = Instant::now();
        let result = read_by(
            &mut client,
            start + Duration::from_millis(100),
            &mut [0u8; 12],
        );
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_millis(220));
        drop(client);
        writer.join().unwrap();
    }

    #[test]
    fn merged_sync_refuses_an_applied_cursor_during_maintenance() {
        let dir = tempfile::tempdir().unwrap();
        let search = RwLock::new(SearchEngine::new(8));
        search.blocking_write().set_change_cursor(7);
        let owner = crate::maintenance::MaintenanceLock::acquire(dir.path()).unwrap();
        let wait = || {
            wait_for_sync(
                dir.path(),
                &search,
                7,
                &Notify::new(),
                Instant::now() + Duration::from_millis(50),
            )
        };
        assert!(!wait());
        drop(owner);
        assert!(wait());
    }

    #[test]
    fn r3_sync_wait_does_not_block_on_a_writer() {
        let search = Arc::new(RwLock::new(SearchEngine::new(8)));
        let held = search.blocking_write();
        let other = search.clone();
        let start = Instant::now();
        let waiter = std::thread::spawn(move || {
            let dir = tempfile::tempdir().unwrap();
            wait_for_sync(
                dir.path(),
                &other,
                1,
                &Notify::new(),
                Instant::now() + Duration::from_millis(50),
            )
        });
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            waiter.is_finished(),
            "Sync must expire while the write lock is still held"
        );
        drop(held);
        assert!(!waiter.join().unwrap());
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn r3_future_heartbeat_is_fresh() {
        let dir = tempfile::tempdir().unwrap();
        write_heartbeat(dir.path());
        std::fs::File::options()
            .write(true)
            .open(heartbeat_path(dir.path()))
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(3600))
            .unwrap();
        assert!(daemon_configured(dir.path()));
        let hook = Engine::new_for_hook(
            &dir.path().join("csr-engine.db"),
            &dir.path().join("projects"),
        )
        .unwrap();
        assert!(hook.is_proxied());
        assert!(!hook.embeddings().is_loaded());
        assert_eq!(hook.search().blocking_read().chunk_count(), 0);
        assert!(!hook.index_dir().exists());
    }

    #[test]
    fn r3_embedding_work_is_rejected_before_dispatch() {
        for op in [
            Op::Embed {
                texts: vec![String::new(); 257],
            },
            Op::Embed {
                texts: vec!["x".repeat(MAX_EMBED_BYTES + 1)],
            },
        ] {
            assert!(matches!(
                validate_work(&op),
                Some(Reply::Error {
                    code: ErrorCode::EmbeddingLimit,
                    ..
                })
            ));
        }
        assert!(validate_work(&Op::Embed {
            texts: vec![String::new(); 256]
        })
        .is_none());
        assert!(validate_work(&Op::Embed {
            texts: vec!["x".repeat(MAX_EMBED_BYTES)]
        })
        .is_none());
    }

    #[test]
    fn r3_encode_failure_counts_as_a_failed_keyword_only_call() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let client = VectorClient::for_database(&dir.path().join("csr-engine.db")).unwrap();
        let proxy = VectorProxy::new(client, storage);
        let reply = proxy.remote(
            Op::Embed {
                texts: vec!["x".repeat(MAX_FRAME)],
            },
            Duration::from_millis(1),
        );
        assert!(reply.is_none());
        assert_eq!(proxy.failures(), 1);
        assert_eq!(proxy.path(), PATH_KEYWORD_ONLY);
    }

    #[test]
    fn r5_plist_configuration_matches_only_its_canonical_database() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let plist = home.join("Library/LaunchAgents/com.ramakay.csr-engine.daemon.plist");
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(
            &plist,
            include_str!("../../../scripts/launchd/com.ramakay.csr-engine.daemon.plist"),
        )
        .unwrap();
        assert_eq!(
            launchd_database(home),
            Some(home.join(".claude-self-reflect/csr-engine.db"))
        );
        let default = home.join(".claude-self-reflect/csr-engine.db");
        let other = home.join("other.db");
        let offline = |_: &Path| anyhow::bail!("offline");
        std::fs::write(&plist, "<plist><dict><key>ProgramArguments</key><array><string>csr-engine</string><string>daemon</string><!-- array end --></array><!-- dict end --></dict></plist>").unwrap();
        assert!(daemon_configured_with(
            &default,
            Some(home),
            false,
            false,
            offline
        ));
        assert!(!daemon_configured_with(
            &other,
            Some(home),
            false,
            false,
            offline
        ));
        for arguments in [
            format!("<key>ProgramArguments</key><array><string>csr-engine</string><string>--db-path</string><string>{}</string><string>daemon</string></array>", other.display()),
            format!("<key>ProgramArguments</key><array><string>csr-engine</string><string>--db-path={}</string><string>daemon</string></array>", other.display()),
        ] {
            std::fs::write(&plist, format!("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>{arguments}</dict></plist>")).unwrap();
            assert!(daemon_configured_with(&other, Some(home), false, false, offline));
            assert!(!daemon_configured_with(&default, Some(home), false, false, offline));
        }
        let actual = home.join("actual & data");
        std::fs::create_dir(&actual).unwrap();
        let alias = home.join("alias");
        std::os::unix::fs::symlink(&actual, &alias).unwrap();
        std::fs::write(&plist, format!("<plist><dict><key>ProgramArguments</key><array><string>csr-engine</string><string>--db-path={}/test.db</string><string>daemon</string></array><key>EnvironmentVariables</key><dict><key>HOME</key><string>{}</string></dict></dict></plist>", actual.display().to_string().replace('&', "&amp;"), home.display())).unwrap();
        assert!(daemon_configured_with(
            &alias.join("test.db"),
            Some(home),
            false,
            false,
            offline
        ));
        std::fs::write(&plist, "not a plist").unwrap();
        assert!(!daemon_configured_with(
            &default,
            Some(home),
            false,
            false,
            offline
        ));
        // A heartbeat in the same directory is also scoped to its database.
        write_heartbeat_for_database(&other);
        assert!(!daemon_configured_with(
            &home.join("unrelated.db"),
            Some(home),
            false,
            false,
            offline
        ));
    }

    #[test]
    fn r5_busy_and_ping_require_matching_database_identity() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        let identity = database_identity(&db);
        for pong in [false, true] {
            for database in [
                None,
                Some(database_identity(&dir.path().join("other"))),
                Some(identity.clone()),
            ] {
                let matches = database.as_deref() == Some(identity.as_str());
                let response = Response {
                    version: VERSION,
                    database,
                    reply: if pong {
                        Reply::Pong
                    } else {
                        Reply::Error {
                            code: ErrorCode::Busy,
                            message: "busy before admission".into(),
                        }
                    },
                };
                // Exercise the wire representation, including an old reply with
                // no identity: it must never select another database's daemon.
                let bytes = serde_json::to_vec(&response).unwrap();
                let response: Response = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    daemon_configured_with(&db, None, false, false, |_| response
                        .for_database(&identity)),
                    matches
                );
            }
        }
    }

    #[test]
    fn r5_proxy_path_stays_unknown_until_a_successful_reply() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        let proxy = VectorProxy::new(
            VectorClient::for_database(&db).unwrap(),
            Arc::new(Storage::open(&db).unwrap()),
        );
        let record = dir.path().join("run.json");
        let run = crate::binary_identity::RunRecord {
            pid: std::process::id(),
            version: "test".into(),
            git_sha: "test".into(),
            exe_identity: crate::binary_identity::FileIdentity::capture(&db).unwrap(),
            role: "mcp_stdio".into(),
            started_at: "2026-10-13T00:00:00Z".into(),
            parent_cmd: None,
            search_path: None,
        };
        std::fs::write(&record, serde_json::to_vec(&run).unwrap()).unwrap();
        proxy.attach_run_record(record.clone());
        let recorded = || {
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&record).unwrap())
                .unwrap()["search_path"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(proxy.path(), PATH_UNKNOWN);
        assert_eq!(recorded(), PATH_UNKNOWN);
        assert_eq!(
            crate::binary_identity::live_mcp_servers(dir.path())[0]
                .search_path
                .as_deref(),
            Some(PATH_UNKNOWN)
        );
        proxy.mark_up();
        assert_eq!(proxy.path(), PATH_DAEMON);
        assert_eq!(recorded(), PATH_DAEMON);
        proxy.mark_down(&anyhow::anyhow!("offline"));
        assert_eq!(recorded(), PATH_KEYWORD_ONLY);
        proxy.mark_up();
        assert_eq!(recorded(), PATH_DAEMON);
    }

    #[test]
    fn r3_missing_or_stale_heartbeat_pings_the_actual_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("alternate.db");
        for stale in [false, true] {
            if stale {
                write_heartbeat(dir.path());
                std::fs::File::options()
                    .write(true)
                    .open(heartbeat_path(dir.path()))
                    .unwrap()
                    .set_modified(SystemTime::now() - Duration::from_secs(25 * 3600))
                    .unwrap();
            }
            assert!(daemon_configured_with(&db, None, false, false, |actual| {
                assert_eq!(actual, db);
                Ok(Reply::Pong)
            }));
        }
    }

    #[test]
    fn r3_durable_daemon_configuration_survives_stale_heartbeat_and_busy_reply() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("alternate.db");
        write_heartbeat(dir.path());
        std::fs::File::options()
            .write(true)
            .open(heartbeat_path(dir.path()))
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(25 * 3600))
            .unwrap();
        assert!(daemon_configured_with(&db, None, false, false, |_| Ok(
            Reply::Error {
                code: ErrorCode::Busy,
                message: "all serving slots occupied".into(),
            }
        )));
        let plist = dir
            .path()
            .join("Library/LaunchAgents/com.ramakay.csr-engine.daemon.plist");
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, format!("<plist><dict><key>ProgramArguments</key><array><string>csr-engine</string><string>--db-path</string><string>{}</string><string>daemon</string></array></dict></plist>", db.display())).unwrap();
        assert!(daemon_configured_with(
            &db,
            Some(dir.path()),
            false,
            false,
            |_| panic!("installed daemon must not need a ping")
        ));
        assert!(!daemon_configured_with(
            &db,
            Some(dir.path()),
            true,
            true,
            |_| panic!("opt-out wins")
        ));
        assert!(daemon_configured_with(&db, None, false, true, |_| panic!(
            "explicit proxy wins"
        )));
        assert!(!daemon_configured_with(
            &db,
            None,
            false,
            false,
            |_| anyhow::bail!("offline")
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn r3_waiting_connections_are_capped_with_a_busy_reply() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("csr-engine.db");
        let shutdown = Arc::new(AtomicBool::new(false));
        let slots = Arc::new(Semaphore::new(0));
        let server = spawn_for_daemon_with(
            Arc::new(EmbeddingEngine::new().unwrap()),
            Arc::new(RwLock::new(SearchEngine::new(8))),
            &db,
            shutdown.clone(),
            Arc::new(Notify::new()),
            Duration::from_secs(5),
            slots,
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            while !socket_path(dir.path()).exists() {
                assert!(!server.is_finished(), "vector socket bind failed");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let mut waiting = Vec::new();
        for _ in 0..WAITING_CONNECTIONS {
            waiting.push(
                tokio::net::UnixStream::connect(socket_path(dir.path()))
                    .await
                    .unwrap(),
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut extra = tokio::net::UnixStream::connect(socket_path(dir.path()))
            .await
            .unwrap();
        let size = tokio::time::timeout(Duration::from_secs(1), extra.read_u32())
            .await
            .unwrap()
            .unwrap();
        let mut bytes = vec![0; size as usize];
        extra.read_exact(&mut bytes).await.unwrap();
        let response: Response = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            response.database.as_deref(),
            Some(database_identity(&db).as_str())
        );
        assert!(matches!(
            response.reply,
            Reply::Error {
                code: ErrorCode::Busy,
                ..
            }
        ));
        shutdown.store(true, Ordering::SeqCst);
        server.await.unwrap();
    }
    #[test]
    fn r3_draining_writes_obey_one_absolute_deadline() {
        let (mut client, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0; 4096];
            while !stopped.load(Ordering::SeqCst) {
                if peer.read(&mut bytes).unwrap_or(0) == 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let start = Instant::now();
        let result = write_by(
            &mut client,
            start + Duration::from_millis(100),
            &vec![0; 2 << 20],
        );
        stop.store(true, Ordering::SeqCst);
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_millis(300));
        drop(client);
        reader.join().unwrap();
    }

    #[test]
    fn r3_retry_backoff_shares_the_original_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let proxy = VectorProxy::new(
            VectorClient::for_database(&dir.path().join("csr-engine.db")).unwrap(),
            Arc::new(Storage::open_memory().unwrap()),
        );
        proxy.set_timings(ProxyTimings {
            backoff_first: Duration::from_millis(10),
            backoff_total: Duration::from_secs(1),
            down_window: Duration::from_secs(30),
        });
        let start = Instant::now();
        assert!(proxy.remote(Op::Ping, Duration::from_millis(60)).is_none());
        assert!(start.elapsed() < Duration::from_millis(250));
        assert_eq!(proxy.failures(), 1);
    }
}
