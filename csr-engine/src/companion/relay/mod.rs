//! The engine side of the relay: `csr-engine companion serve`.
//!
//! One child per Claude Code process. The generated `register.js` starts it with
//! `$.process.spawn` and talks to it over a Unix socket (`POST /event|/after|/step|/press`);
//! the child talks back by writing JSON lines on stdout, which the relay runs as `$` calls and
//! answers on `/step`. `host` is that trampoline, `session` and `store` are what handlers
//! remember, `dispatch` routes a request to the handler for its hook key.

pub mod dispatch;
pub mod emit;
pub mod host;
pub mod manifest;
pub mod session;

use anyhow::{bail, Context, Result};
use axum::{body::Bytes, extract::State, http::StatusCode, routing::post, Router};
use dispatch::{default_table, App, Log};
use host::{Hub, SystemClock};
use serde_json::{json, Value};
use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
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

/// A socket younger than this is left alone by the stale sweep: its owner may be between `bind`
/// and `listen`.
const STALE_AFTER: Duration = Duration::from_secs(2);

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.)
        .unwrap_or(0.)
}

async fn endpoint(
    app: Arc<App>,
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
    let reply = app.handle(path, &parsed).await;
    let handle_us = started.elapsed().as_micros() as u64;
    if app.log().enabled() {
        let mut row = json!({
            "at": now_ms(),
            "k": parsed.get("k"),
            "path": path,
            "sid": parsed.get("sid"),
            "bytes": body.len(),
            "handle_us": handle_us,
            "pid": std::process::id(),
        });
        if matches!(
            parsed.get("k").and_then(Value::as_str),
            Some("start" | "measure")
        ) {
            row["e"] = parsed.get("e").cloned().unwrap_or(Value::Null);
        }
        if let Some(respawn) = parsed.get("respawn") {
            row["respawn"] = respawn.clone();
        }
        app.log().row(row);
    }
    (StatusCode::OK, axum::Json(reply))
}

fn router(app: Arc<App>) -> Router {
    let route = |path: &'static str| {
        post(move |State(app): State<Arc<App>>, body: Bytes| endpoint(app, path, body))
    };
    Router::new()
        .route("/event", route("/event"))
        .route("/after", route("/after"))
        .route("/step", route("/step"))
        .route("/press", route("/press"))
        .with_state(app)
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

/// Remove sockets in `dir` that this user owns, that are at least `older_than` old, and that
/// refuse a connection: what a child that was killed left behind. Anything else in the folder is
/// left alone.
fn sweep_stale(dir: &Path, older_than: Duration) {
    let uid = unsafe { libc::getuid() };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age >= older_than);
        if meta.file_type().is_socket() && meta.uid() == uid && old {
            let refused = std::os::unix::net::UnixStream::connect(&path)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused);
            if refused {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// Make the socket's folder private, clear stale sockets from it, and clear a dead socket from
/// the path itself.
fn prepare(sock: &Path) -> Result<()> {
    let parent = sock.parent().context("the socket path has no folder")?;
    let root = super::private::default_root();
    super::private::ensure_dir(parent, &root)
        .with_context(|| format!("preparing {}", parent.display()))?;
    sweep_stale(parent, STALE_AFTER);
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
        .map_or_else(Log::default, Log::to);
    let (push, lines) = mpsc::unbounded_channel();
    let gone = Arc::new(Notify::new());
    // The store opens on the first handler that asks for it, not here.
    let app = App::new(
        &args.relay,
        Hub::new(push),
        Arc::new(super::store::Store::at(super::store::default_path())),
        Arc::new(SystemClock),
        Arc::new(log),
        default_table(),
    );

    let stopped = stop_signals()?;
    {
        let mut out = std::io::stdout().lock();
        writeln!(out, "ready")?;
        out.flush()?;
    }
    spawn_writer(lines, gone.clone());
    app.log()
        .row(json!({ "at": now_ms(), "k": "boot", "path": "", "pid": std::process::id() }));

    let listener = OwnerOnly {
        inner: listener,
        uid: unsafe { libc::getuid() },
    };
    let served = axum::serve(listener, router(app));
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

    #[test]
    fn the_sweep_removes_dead_sockets_only() {
        let dir = tempfile::tempdir().unwrap();
        let dead = dir.path().join("dead-1.sock");
        drop(std::os::unix::net::UnixListener::bind(&dead).unwrap());
        let live = dir.path().join("live-1.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&live).unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "keep").unwrap();
        let folder = dir.path().join("inner.sock");
        std::fs::create_dir(&folder).unwrap();
        let link = dir.path().join("link.sock");
        std::os::unix::fs::symlink(&dead, &link).unwrap();
        // A fresh socket is spared while its owner may still be starting up.
        sweep_stale(dir.path(), Duration::from_secs(3600));
        assert!(dead.exists());
        sweep_stale(dir.path(), Duration::ZERO);
        assert!(!dead.exists(), "a dead socket goes");
        assert!(live.exists(), "a socket that answers stays");
        assert!(file.exists() && folder.exists(), "other things stay");
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "a link to a socket is not a socket"
        );
    }
}
