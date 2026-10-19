//! The engine side of the relay: `csr-engine companion serve`.
//!
//! One child per Claude Code process. The generated `register.js` starts it with
//! `$.process.spawn` and talks to it over a Unix socket (`POST /event|/after|/step|/press`);
//! the child talks back by writing JSON lines on stdout, which the relay runs as `$` calls and
//! answers on `/step`. The dispatcher here is a stub: WP1 replaces it.

pub mod emit;
pub mod manifest;

use anyhow::{bail, Context, Result};
use axum::{body::Bytes, extract::State, http::StatusCode, routing::post, Router};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::Write,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{mpsc, Notify},
};

#[derive(clap::Args, Debug)]
pub struct ServeArgs {
    /// Where to listen.
    #[arg(long)]
    pub sock: PathBuf,
    /// The version of the relay that started this child.
    #[arg(long)]
    pub relay: String,
}

const TOAST: &str = "CSR updated: run /reload-plugins";

/// Why a pushed call was made: what the stub does with the answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Registered,
    Usage,
}

struct Shared {
    relay: String,
    pid: u32,
    log: Option<Mutex<std::fs::File>>,
    push: mpsc::UnboundedSender<String>,
    events: AtomicU64,
    seq: AtomicU64,
    warned: AtomicBool,
    pending: Mutex<HashMap<String, Pending>>,
}

impl Shared {
    fn next_id(&self, prefix: &str) -> String {
        format!("{prefix}{}", self.seq.fetch_add(1, Ordering::Relaxed) + 1)
    }

    /// One `$` call, pushed to the relay.
    fn push_call(&self, id: &str, method: &str, args: Value) {
        let line = json!({ "id": id, "calls": [{ "id": "c1", "m": method, "a": args }] });
        let _ = self.push.send(line.to_string());
    }

    fn log(&self, row: Value) {
        if let Some(file) = &self.log {
            if let Ok(mut file) = file.lock() {
                let _ = writeln!(file, "{row}");
            }
        }
    }

    /// The stub dispatcher. Returns the reply and anything extra worth logging.
    fn dispatch(&self, path: &str, body: &Value) -> (Value, Value) {
        let sent = body.get("v").and_then(Value::as_str);
        if sent != Some(self.relay.as_str()) && !self.warned.swap(true, Ordering::Relaxed) {
            self.push_call(&self.next_id("p"), "ui.toast", json!([TOAST]));
        }
        match path {
            "/event" => {
                let events = self.events.fetch_add(1, Ordering::Relaxed) + 1;
                match body.get("k").and_then(Value::as_str) {
                    Some("start") => {
                        let id = self.next_id("s");
                        self.pending
                            .lock()
                            .unwrap()
                            .insert(id.clone(), Pending::Registered);
                        let reply = json!({ "id": id, "calls": [{
                            "id": "c1", "m": "command.register", "a": [{
                                "name": "csr",
                                "description": "Open the CSR pane beside Claude; \
                                    \"/csr dreams\" says how the dreamer is pacing itself",
                            }],
                        }] });
                        (reply, json!({ "e": body.get("e") }))
                    }
                    Some("csr") => {
                        let text = format!("csr relay ok pid={} events={events}", self.pid);
                        (json!({ "answer": { "text": text } }), Value::Null)
                    }
                    Some("measure") => (json!({}), json!({ "e": body.get("e") })),
                    _ => (json!({}), Value::Null),
                }
            }
            "/step" => {
                let id = body.get("id").and_then(Value::as_str).unwrap_or("");
                let kind = self.pending.lock().unwrap().remove(id);
                match kind {
                    Some(Pending::Registered) => {
                        let next = self.next_id("u");
                        self.pending
                            .lock()
                            .unwrap()
                            .insert(next.clone(), Pending::Usage);
                        self.push_call(&next, "session.usage", json!([]));
                        (
                            json!({}),
                            json!({ "step": "registered", "results": body.get("results") }),
                        )
                    }
                    Some(Pending::Usage) => (
                        json!({}),
                        json!({ "step": "usage", "usage": body.get("results") }),
                    ),
                    None => (json!({}), Value::Null),
                }
            }
            _ => (json!({}), Value::Null),
        }
    }
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.)
        .unwrap_or(0.)
}

async fn endpoint(
    shared: Arc<Shared>,
    path: &'static str,
    body: Bytes,
) -> (StatusCode, axum::Json<Value>) {
    let started = Instant::now();
    let Ok(parsed) = serde_json::from_slice::<Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({ "error": "not JSON" })),
        );
    };
    let (reply, extra) = shared.dispatch(path, &parsed);
    let handle_us = started.elapsed().as_micros() as u64;
    if shared.log.is_some() {
        let mut row = json!({
            "at": now_ms(),
            "k": parsed.get("k"),
            "path": path,
            "sid": parsed.get("sid"),
            "bytes": body.len(),
            "handle_us": handle_us,
            "pid": shared.pid,
        });
        if let (Some(row), Some(extra)) = (row.as_object_mut(), extra.as_object()) {
            row.extend(extra.clone());
        }
        shared.log(row);
    }
    (StatusCode::OK, axum::Json(reply))
}

fn router(shared: Arc<Shared>) -> Router {
    let route = |path: &'static str| {
        post(move |State(shared): State<Arc<Shared>>, body: Bytes| endpoint(shared, path, body))
    };
    Router::new()
        .route("/event", route("/event"))
        .route("/after", route("/after"))
        .route("/step", route("/step"))
        .route("/press", route("/press"))
        .with_state(shared)
}

/// A listener that hands the router only connections from this user.
struct OwnerOnly {
    inner: UnixListener,
    uid: u32,
}

/// Whether a connection from `peer` may reach the router.
fn same_user(peer: std::io::Result<tokio::net::unix::UCred>, uid: u32) -> bool {
    peer.is_ok_and(|cred| cred.uid() == uid)
}

impl axum::serve::Listener for OwnerOnly {
    type Io = UnixStream;
    type Addr = ();

    async fn accept(&mut self) -> (UnixStream, ()) {
        loop {
            match self.inner.accept().await {
                Ok((stream, _)) if same_user(stream.peer_cred(), self.uid) => return (stream, ()),
                Ok(_) => (),
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Make the socket's folder private and clear a dead socket from the path.
fn prepare(sock: &Path) -> Result<()> {
    let parent = sock.parent().context("the socket path has no folder")?;
    let root = super::private::default_root();
    super::private::ensure_dir(parent, &root)
        .with_context(|| format!("preparing {}", parent.display()))?;
    match std::fs::symlink_metadata(sock) {
        Ok(meta) if meta.file_type().is_socket() => {
            if std::os::unix::net::UnixStream::connect(sock).is_ok() {
                bail!("{} is served by another process", sock.display());
            }
            std::fs::remove_file(sock)?;
        }
        Ok(_) => bail!("{} exists and is not a socket", sock.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Print each pushed line on stdout. A write that fails means the relay is gone.
fn spawn_writer(mut lines: mpsc::UnboundedReceiver<String>, gone: Arc<Notify>) {
    std::thread::spawn(move || {
        let stdout = std::io::stdout();
        while let Some(line) = lines.blocking_recv() {
            let mut out = stdout.lock();
            if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                gone.notify_one();
                return;
            }
        }
    });
}

/// Resolves when the parent that started this process is gone or stdout has no reader.
async fn orphaned() {
    let parent = unsafe { libc::getppid() };
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if parent > 1 && unsafe { libc::getppid() } != parent {
            return;
        }
        let mut fd = libc::pollfd {
            fd: 1,
            events: libc::POLLOUT,
            revents: 0,
        };
        let polled = unsafe { libc::poll(&mut fd, 1, 0) };
        if polled > 0 && fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return;
        }
    }
}

/// Handlers for SIGTERM and SIGINT, installed before `ready` is printed so that a signal sent
/// the moment the relay sees it is never the default one.
fn stop_signals() -> Result<impl std::future::Future<Output = ()>> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! { _ = term.recv() => (), _ = int.recv() => () }
    })
}

/// Remove the socket file, but only when it is still the one this process bound.
fn remove_own_socket(sock: &Path, bound: (u64, u64)) {
    if let Ok(meta) = std::fs::symlink_metadata(sock) {
        if meta.file_type().is_socket() && (meta.dev(), meta.ino()) == bound {
            let _ = std::fs::remove_file(sock);
        }
    }
}

async fn run(args: &ServeArgs) -> Result<()> {
    prepare(&args.sock)?;
    let listener = UnixListener::bind(&args.sock)
        .with_context(|| format!("binding {}", args.sock.display()))?;
    std::fs::set_permissions(&args.sock, std::fs::Permissions::from_mode(0o600))?;
    let meta = std::fs::symlink_metadata(&args.sock)?;
    let bound = (meta.dev(), meta.ino());

    let log = std::env::var_os("CSR_RELAY_LOG")
        .filter(|path| !path.is_empty())
        .and_then(|path| OpenOptions::new().create(true).append(true).open(path).ok())
        .map(Mutex::new);
    let (push, lines) = mpsc::unbounded_channel();
    let gone = Arc::new(Notify::new());
    let shared = Arc::new(Shared {
        relay: args.relay.clone(),
        pid: std::process::id(),
        log,
        push,
        events: AtomicU64::new(0),
        seq: AtomicU64::new(0),
        warned: AtomicBool::new(false),
        pending: Mutex::new(HashMap::new()),
    });

    let stopped = stop_signals()?;
    {
        let mut out = std::io::stdout().lock();
        writeln!(out, "ready")?;
        out.flush()?;
    }
    spawn_writer(lines, gone.clone());
    shared.log(json!({ "at": now_ms(), "k": "boot", "path": "", "pid": shared.pid }));

    let listener = OwnerOnly {
        inner: listener,
        uid: unsafe { libc::getuid() },
    };
    let served = axum::serve(listener, router(shared));
    let result = tokio::select! {
        served = served => served.map_err(anyhow::Error::from),
        () = stopped => Ok(()),
        () = gone.notified() => Ok(()),
        () = orphaned() => Ok(()),
    };
    remove_own_socket(&args.sock, bound);
    result
}

/// `csr-engine companion serve`: runs until a signal, or until the relay is gone.
pub fn serve(args: &ServeArgs) -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run(args))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared(relay: &str) -> (Arc<Shared>, mpsc::UnboundedReceiver<String>) {
        let (push, lines) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            relay: relay.into(),
            pid: 7,
            log: None,
            push,
            events: AtomicU64::new(0),
            seq: AtomicU64::new(0),
            warned: AtomicBool::new(false),
            pending: Mutex::new(HashMap::new()),
        });
        (shared, lines)
    }

    #[test]
    fn start_asks_for_the_csr_command_and_the_answer_asks_for_usage() {
        let (shared, mut lines) = shared("1");
        let (reply, _) = shared.dispatch("/event", &json!({ "v": "1", "k": "start", "e": {} }));
        let call = &reply["calls"][0];
        assert_eq!(call["m"], "command.register");
        assert_eq!(call["a"][0]["name"], "csr");
        assert!(lines.try_recv().is_err());
        let id = reply["id"].as_str().unwrap();
        let (done, _) = shared.dispatch(
            "/step",
            &json!({ "v": "1", "id": id, "results": [{ "id": "c1", "v": { "command": "csr" } }] }),
        );
        assert_eq!(done, json!({}));
        let pushed: Value = serde_json::from_str(&lines.try_recv().unwrap()).unwrap();
        assert_eq!(pushed["calls"][0]["m"], "session.usage");
        let (done, extra) = shared.dispatch(
            "/step",
            &json!({ "v": "1", "id": pushed["id"], "results": [{ "id": "c1", "v": { "a": 1 } }] }),
        );
        assert_eq!(done, json!({}));
        assert_eq!(extra["usage"][0]["v"]["a"], 1);
    }

    #[test]
    fn csr_answers_with_pid_and_the_running_event_count() {
        let (shared, _lines) = shared("1");
        shared.dispatch("/event", &json!({ "v": "1", "k": "tool" }));
        let (reply, _) = shared.dispatch("/event", &json!({ "v": "1", "k": "csr" }));
        assert_eq!(reply["answer"]["text"], "csr relay ok pid=7 events=2");
    }

    #[test]
    fn everything_else_passes_through() {
        let (shared, _lines) = shared("1");
        for path in ["/event", "/after", "/press"] {
            let (reply, _) = shared.dispatch(path, &json!({ "v": "1", "k": "guard" }));
            assert_eq!(reply, json!({}));
        }
    }

    #[test]
    fn a_different_relay_version_toasts_once() {
        let (shared, mut lines) = shared("2");
        shared.dispatch("/event", &json!({ "v": "1", "k": "tool" }));
        shared.dispatch("/event", &json!({ "v": "1", "k": "tool" }));
        let pushed: Value = serde_json::from_str(&lines.try_recv().unwrap()).unwrap();
        assert_eq!(pushed["calls"][0]["m"], "ui.toast");
        assert_eq!(pushed["calls"][0]["a"][0], TOAST);
        assert!(lines.try_recv().is_err());
    }

    #[test]
    fn the_same_relay_version_never_toasts() {
        let (shared, mut lines) = shared("2");
        shared.dispatch("/event", &json!({ "v": "2", "k": "tool" }));
        assert!(lines.try_recv().is_err());
    }

    #[test]
    fn only_the_same_user_gets_through() {
        let err = || Err(std::io::Error::other("no credentials"));
        assert!(!same_user(err(), 501));
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        a.set_nonblocking(true).unwrap();
        let uid = unsafe { libc::getuid() };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let cred = rt.block_on(async { UnixStream::from_std(a).unwrap().peer_cred() });
        assert!(same_user(Ok(cred.unwrap()), uid));
        let cred = rt.block_on(async {
            let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
            a.set_nonblocking(true).unwrap();
            UnixStream::from_std(a).unwrap().peer_cred()
        });
        assert!(!same_user(Ok(cred.unwrap()), uid.wrapping_add(1)));
    }

    #[test]
    fn prepare_clears_a_dead_socket_and_refuses_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("run/a.sock");
        prepare(&sock).unwrap();
        let mode = std::fs::metadata(sock.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
        assert!(std::fs::symlink_metadata(&sock)
            .unwrap()
            .file_type()
            .is_socket());
        prepare(&sock).unwrap();
        assert!(!sock.exists());
        std::fs::write(&sock, "x").unwrap();
        assert!(prepare(&sock).is_err());
        assert!(sock.exists());
    }

    #[test]
    fn a_live_socket_is_not_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("a.sock");
        let _live = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        assert!(prepare(&sock).is_err());
        assert!(sock.exists());
    }
}
