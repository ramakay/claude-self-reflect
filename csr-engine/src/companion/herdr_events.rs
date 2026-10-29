//! herdr's event stream: the pane ends with its agent's pane, and draws when its status moves.
//!
//! On by default whenever herdr's `HERDR_SOCKET_PATH` is set; `CSR_PANE_HERDR_EVENTS=0` turns it
//! off (any other value, or none, leaves it on). A thread subscribes over herdr's socket (newline
//! JSON), and keeps two facts for the watch loop:
//!
//! - wake: the agent's status changed (draw soon).
//! - gone: the agent pane closed or exited, and so does this pane. An event says so; and since a
//!   stream that was lost or reconnected replays nothing, each new subscription is followed by one
//!   `session.snapshot`, and an agent pane that is not in it counts as gone too.
//!
//! There is no "hidden": herdr says which tab its default focus is on, but each attached client
//! has a place of its own and the API does not list them, so a tab that is not the default focus
//! may be in front of someone. A pane that cannot know draws as it would without events; its own
//! gates keep a quiet one cheap.
//!
//! It fails open. A socket that is missing or refuses, an error or garbage answer to the
//! subscription, `events_lost`, an EOF, or a line too long to be herdr's drops the thread back to
//! plain polling, and it reconnects with a growing wait. Nothing it does can block or panic the
//! pane: the work is on its own thread, and leaving does not wait for it.
use super::send;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, ErrorKind, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// The wait before the first reconnect, and the most it grows to.
const RETRY: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(10);
/// How long a read waits before the thread looks at its stop flag.
const TICK: Duration = Duration::from_millis(250);
const IO_LIMIT: Duration = Duration::from_secs(2);

/// The longest line read from herdr. A snapshot of a big session is far below it; a stream that
/// goes past it without a newline is not herdr.
const LINE_LIMIT: u64 = 4 << 20;

/// The socket, when herdr is there and the kill switch (`CSR_PANE_HERDR_EVENTS=0`) is not set.
pub fn config(var: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if var("CSR_PANE_HERDR_EVENTS").as_deref() == Some("0") {
        return None;
    }
    var("HERDR_SOCKET_PATH")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[derive(Default)]
struct Shared {
    status: AtomicBool,
    gone: AtomicBool,
    stop: AtomicBool,
}

pub struct Watcher {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}
/// A second opinion on "the agent pane is not in herdr's snapshot": true when the pane is still
/// there. herdr can change a pane's id (a move to another workspace) and keeps the old one as an
/// alias, so absence from the snapshot alone does not say the pane ended.
type StillThere = Arc<dyn Fn(&str) -> bool + Send + Sync>;

impl Watcher {
    /// A watcher of herdr's events, when herdr is there and it is not switched off. `agent` is the pane given on the
    /// command line; without one it is the left neighbour, found in the thread.
    pub fn from_env(agent: Option<String>) -> Option<Self> {
        let socket = config(|name| std::env::var(name).ok())?;
        let still_there: StillThere = Arc::new(|pane| {
            (send::herdr(&["pane", "get", pane]))
                .and_then(|said| serde_json::from_str::<Value>(&said).ok())
                .is_some_and(|said| said["result"]["pane"].is_object())
        });
        Some(Self::start_with(
            socket,
            move || send::agent_pane(agent.as_deref()),
            still_there,
        ))
    }

    pub fn start(socket: PathBuf, agent: impl FnOnce() -> Option<String> + Send + 'static) -> Self {
        Self::start_with(socket, agent, Arc::new(|_| false))
    }

    fn start_with(
        socket: PathBuf,
        agent: impl FnOnce() -> Option<String> + Send + 'static,
        still_there: StillThere,
    ) -> Self {
        let shared = Arc::new(Shared::default());
        // A thread that cannot be made leaves a watcher that sees nothing: the pane polls.
        let thread = {
            let shared = shared.clone();
            thread::Builder::new()
                .name("herdr-events".into())
                .spawn(move || run(&shared, &socket, agent().as_deref(), &*still_there))
                .ok()
        };
        Self { shared, thread }
    }

    /// The agent pane is closed or exited.
    pub fn agent_gone(&self) -> bool {
        self.shared.gone.load(Ordering::Relaxed)
    }
    /// The agent's status changed since the last call.
    pub fn take_status(&self) -> bool {
        self.shared.status.swap(false, Ordering::Relaxed)
    }
}
impl Drop for Watcher {
    /// The thread sees the flag within a tick and ends on its own: leaving does not wait for a
    /// herdr that is slow to answer.
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        drop(self.thread.take());
    }
}

fn subscription(agent: Option<&str>) -> String {
    let mut subscriptions = vec![];
    if let Some(agent) = agent {
        subscriptions.push(json!({ "type": "pane.agent_status_changed", "pane_id": agent }));
    }
    subscriptions.push(json!({ "type": "pane.closed" }));
    subscriptions.push(json!({ "type": "pane.exited" }));
    json!({
        "id": "csr",
        "method": "events.subscribe",
        "params": { "subscriptions": subscriptions },
    })
    .to_string()
}

fn run(shared: &Shared, socket: &Path, agent: Option<&str>, still_there: &dyn Fn(&str) -> bool) {
    let mut wait = RETRY;
    while !shared.stop.load(Ordering::Relaxed) && !shared.gone.load(Ordering::Relaxed) {
        let started = session(shared, socket, agent, still_there);
        wait = if matches!(started, Ok(true)) {
            RETRY
        } else {
            (wait * 2).min(RETRY_MAX)
        };
        let until = Instant::now() + wait;
        while Instant::now() < until && !shared.stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// What one read from the stream brought.
enum Got {
    /// A whole line is in the buffer.
    Line,
    /// Some of one.
    Part,
    /// The stream ended.
    End,
}

/// One read from `reader` into `line`, up to the end of a line. It never reads twice, so the
/// caller looks at its stop flag between reads however slowly or quickly bytes arrive, and it
/// refuses a line past `LINE_LIMIT`.
fn read_some(reader: &mut BufReader<UnixStream>, line: &mut Vec<u8>) -> std::io::Result<Got> {
    let buffered = reader.fill_buf()?;
    if buffered.is_empty() {
        return Ok(Got::End);
    }
    let (taken, whole) = match buffered.iter().position(|b| *b == b'\n') {
        Some(end) => (end + 1, true),
        None => (buffered.len(), false),
    };
    line.extend_from_slice(&buffered[..taken]);
    reader.consume(taken);
    if line.len() as u64 > LINE_LIMIT {
        return Err(ErrorKind::InvalidData.into());
    }
    Ok(if whole { Got::Line } else { Got::Part })
}

/// After this many lines in a row that are not JSON the stream is not herdr.
const GARBAGE: usize = 64;

/// One subscription, until it ends. Ok(true): it was acknowledged before it ended.
fn session(
    shared: &Shared,
    socket: &Path,
    agent: Option<&str>,
    still_there: &dyn Fn(&str) -> bool,
) -> std::io::Result<bool> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(TICK))?;
    stream.set_write_timeout(Some(IO_LIMIT))?;
    (&stream).write_all(format!("{}\n", subscription(agent)).as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    let mut started = false;
    let mut garbage = 0;
    while !shared.stop.load(Ordering::Relaxed) {
        match read_some(&mut reader, &mut line) {
            Ok(Got::End) => return Ok(started),
            Ok(Got::Part) => (),
            Ok(Got::Line) => {
                let parsed = serde_json::from_slice::<Value>(&line);
                line.clear();
                garbage = if parsed.is_ok() { 0 } else { garbage + 1 };
                if garbage >= GARBAGE {
                    return Ok(false);
                }
                let value = parsed.unwrap_or(Value::Null);
                if !started {
                    if value["result"]["type"] != "subscription_started" {
                        return Ok(false);
                    }
                    started = true;
                    // The stream does not replay what happened while there was none: read the
                    // layout once now, after the subscription so nothing falls between.
                    if agent_pane_missing(shared, socket, agent, still_there) {
                        shared.gone.store(true, Ordering::Relaxed);
                        return Ok(started);
                    }
                    continue;
                }
                // `events_lost`, or any other error: this subscription is over.
                if value["error"].is_object() {
                    return Ok(started);
                }
                if event(shared, &value, agent) {
                    return Ok(started);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) => {}
            // A line too long, or the socket broke.
            Err(e) if e.kind() == ErrorKind::InvalidData => return Ok(false),
            Err(e) => return Err(e),
        }
    }
    Ok(started)
}

/// How long the snapshot is waited for, all told.
const SNAPSHOT_LIMIT: Duration = Duration::from_secs(4);

/// True when herdr's snapshot is readable, the agent pane is not in it, and herdr does not know
/// the pane under another id either: closed or exited (herdr drops a pane that ends) while no
/// event reached us. Unreadable, or no agent to look for, is not missing.
fn agent_pane_missing(
    shared: &Shared,
    socket: &Path,
    agent: Option<&str>,
    still_there: &dyn Fn(&str) -> bool,
) -> bool {
    let Some(agent) = agent else {
        return false;
    };
    let read = || -> Option<bool> {
        let stream = UnixStream::connect(socket).ok()?;
        stream.set_read_timeout(Some(TICK)).ok()?;
        stream.set_write_timeout(Some(IO_LIMIT)).ok()?;
        (&stream)
            .write_all(b"{\"id\":\"csr-snapshot\",\"method\":\"session.snapshot\",\"params\":{}}\n")
            .ok()?;
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        let until = Instant::now() + SNAPSHOT_LIMIT;
        loop {
            if shared.stop.load(Ordering::Relaxed) || Instant::now() >= until {
                return None;
            }
            match read_some(&mut reader, &mut line) {
                Ok(Got::Line) => break,
                Ok(Got::Part) => (),
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                    ) => {}
                Ok(Got::End) | Err(_) => return None,
            }
        }
        let value: Value = serde_json::from_slice(&line).ok()?;
        let panes = value["result"]["snapshot"]["panes"].as_array()?;
        Some(!panes.iter().any(|pane| pane["pane_id"] == agent))
    };
    read().unwrap_or(false) && !still_there(agent)
}

/// True when the agent pane is gone.
fn event(shared: &Shared, value: &Value, agent: Option<&str>) -> bool {
    let name = value["event"].as_str().unwrap_or("").replace('.', "_");
    let pane = value["data"]["pane_id"].as_str();
    match name.as_str() {
        "pane_agent_status_changed" if pane.is_some() && pane == agent => {
            shared.status.store(true, Ordering::Relaxed);
            false
        }
        "pane_closed" | "pane_exited" if pane.is_some() && pane == agent => {
            shared.gone.store(true, Ordering::Relaxed);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, io::Read, os::unix::net::UnixListener, sync::Mutex};

    /// A herdr that answers on a socket; the events of the subscription are the lines the test
    /// pushes.
    struct Fake {
        dir: tempfile::TempDir,
        lines: Arc<Mutex<VecDeque<String>>>,
        requests: Arc<Mutex<Vec<Value>>>,
        stop: Arc<AtomicBool>,
        /// The panes a snapshot lists; None: herdr cannot be read.
        panes: Arc<Mutex<Option<Vec<&'static str>>>>,
    }
    impl Fake {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let listener = UnixListener::bind(dir.path().join("herdr.sock")).unwrap();
            listener.set_nonblocking(true).unwrap();
            let fake = Self {
                dir,
                lines: Arc::default(),
                requests: Arc::default(),
                stop: Arc::default(),
                panes: Arc::new(Mutex::new(Some(vec!["w1:p2", "w1:p3"]))),
            };
            let (lines, requests, stop, panes) = (
                fake.lines.clone(),
                fake.requests.clone(),
                fake.stop.clone(),
                fake.panes.clone(),
            );
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((conn, _)) => {
                            let (lines, requests, stop, panes) =
                                (lines.clone(), requests.clone(), stop.clone(), panes.clone());
                            thread::spawn(move || serve(conn, lines, requests, stop, panes));
                        }
                        Err(_) => thread::sleep(Duration::from_millis(5)),
                    }
                }
            });
            fake
        }
        fn socket(&self) -> PathBuf {
            self.dir.path().join("herdr.sock")
        }
        fn push(&self, line: Value) {
            self.lines.lock().unwrap().push_back(line.to_string());
        }
        fn snapshots(&self) -> usize {
            (self.requests.lock().unwrap().iter())
                .filter(|r| r["method"] == "session.snapshot")
                .count()
        }
        fn subscriptions(&self) -> usize {
            (self.requests.lock().unwrap().iter())
                .filter(|r| r["method"] == "events.subscribe")
                .count()
        }
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }
    fn serve(
        conn: UnixStream,
        lines: Arc<Mutex<VecDeque<String>>>,
        requests: Arc<Mutex<Vec<Value>>>,
        stop: Arc<AtomicBool>,
        panes: Arc<Mutex<Option<Vec<&'static str>>>>,
    ) {
        conn.set_nonblocking(false).unwrap();
        let mut request = String::new();
        BufReader::new(conn.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        let request: Value = serde_json::from_str(&request).unwrap();
        requests.lock().unwrap().push(request.clone());
        let mut out = conn;
        if request["method"] == "session.snapshot" {
            let answer = match panes.lock().unwrap().clone() {
                Some(panes) => json!({ "id": request["id"], "result": {
                    "type": "session_snapshot",
                    "snapshot": { "panes": panes.iter()
                        .map(|p| json!({ "pane_id": p, "tab_id": "w1:t1" })).collect::<Vec<_>>() },
                }}),
                None => json!({ "id": request["id"], "error": { "code": "unavailable" } }),
            };
            let _ = writeln!(out, "{answer}");
        } else if request["method"] == "events.subscribe" {
            let ack = json!({ "id": request["id"], "result": { "type": "subscription_started" }});
            if writeln!(out, "{ack}").is_err() {
                return;
            }
            let mut sink = [0u8; 64];
            out.set_read_timeout(Some(Duration::from_millis(1)))
                .unwrap();
            while !stop.load(Ordering::Relaxed) {
                let next = lines.lock().unwrap().pop_front();
                match next {
                    Some(line) => {
                        if writeln!(out, "{line}").is_err() || line.contains("events_lost") {
                            return;
                        }
                    }
                    None => {
                        // The client hung up.
                        if matches!(out.read(&mut sink), Ok(0)) {
                            return;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                }
            }
        }
    }
    fn until(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..600 {
            if done() {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        done()
    }
    fn watch(fake: &Fake) -> Watcher {
        Watcher::start(fake.socket(), || Some("w1:p2".into()))
    }

    #[test]
    fn it_is_on_whenever_herdr_is_there_and_off_only_by_the_kill_switch() {
        fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
            move |name| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            }
        }
        let socket = ("HERDR_SOCKET_PATH", "/s/herdr.sock");
        assert_eq!(
            config(env(&[socket])),
            Some("/s/herdr.sock".into()),
            "unset"
        );
        for value in ["1", "", "true", "off", "00", " 0", "2"] {
            let on = [("CSR_PANE_HERDR_EVENTS", value), socket];
            assert_eq!(config(env(&on)), Some("/s/herdr.sock".into()), "{value:?}");
        }
        let off = [("CSR_PANE_HERDR_EVENTS", "0"), socket];
        assert_eq!(config(env(&off)), None, "the kill switch");
        assert_eq!(config(env(&[])), None, "not in herdr");
        assert_eq!(
            config(env(&[("CSR_PANE_HERDR_EVENTS", "1")])),
            None,
            "no socket"
        );
        assert_eq!(
            config(env(&[("HERDR_SOCKET_PATH", "")])),
            None,
            "empty socket"
        );
    }

    #[test]
    fn it_subscribes_with_the_events_a_pane_needs() {
        let fake = Fake::new();
        let _watch = watch(&fake);
        assert!(until(|| fake.subscriptions() == 1));
        let request = fake.requests.lock().unwrap()[0].clone();
        assert_eq!(
            request,
            json!({
                "id": "csr",
                "method": "events.subscribe",
                "params": { "subscriptions": [
                    { "type": "pane.agent_status_changed", "pane_id": "w1:p2" },
                    { "type": "pane.closed" },
                    { "type": "pane.exited" },
                ]},
            })
        );
    }

    #[test]
    fn the_agent_s_status_is_a_wake_and_another_panes_is_not() {
        let fake = Fake::new();
        let watch = watch(&fake);
        assert!(until(|| fake.subscriptions() == 1));
        fake.push(json!({ "event": "pane_agent_status_changed", "data": { "pane_id": "w1:p9" } }));
        thread::sleep(Duration::from_millis(100));
        assert!(!watch.take_status(), "another pane's status");
        fake.push(json!({ "event": "pane.agent_status_changed", "data": { "pane_id": "w1:p2" } }));
        assert!(until(|| watch.take_status()));
        assert!(!watch.take_status(), "once");
    }

    #[test]
    fn the_pane_ends_when_the_agent_pane_closes_or_exits_and_not_for_another() {
        for event in ["pane_closed", "pane_exited"] {
            let fake = Fake::new();
            let watch = watch(&fake);
            assert!(until(|| fake.subscriptions() == 1));
            fake.push(json!({ "event": event, "data": { "pane_id": "w1:p9" } }));
            thread::sleep(Duration::from_millis(100));
            assert!(!watch.agent_gone(), "{event} of another pane");
            fake.push(json!({ "event": event, "data": { "pane_id": "w1:p2" } }));
            assert!(until(|| watch.agent_gone()), "{event}");
        }
    }

    #[test]
    fn events_lost_subscribes_again_and_reads_the_layout_again() {
        let fake = Fake::new();
        let watch = watch(&fake);
        assert!(until(|| fake.subscriptions() == 1));
        assert!(
            until(|| fake.snapshots() == 1),
            "read after the first subscription"
        );
        fake.push(json!({ "id": "csr", "error": { "code": "events_lost" } }));
        assert!(until(|| fake.subscriptions() == 2), "a new subscription");
        assert!(until(|| fake.snapshots() == 2), "and a new snapshot");
        assert!(!watch.agent_gone(), "the agent pane is still there");
    }

    #[test]
    fn an_agent_pane_that_closed_while_no_event_reached_us_ends_the_pane() {
        // Already gone when the pane first looks.
        let fake = Fake::new();
        *fake.panes.lock().unwrap() = Some(vec!["w1:p3"]);
        let watch = watch(&fake);
        assert!(
            until(|| watch.agent_gone()),
            "missing at the first snapshot"
        );
        // Gone in the gap between a lost stream and the next one.
        let fake = Fake::new();
        let watch = self::watch(&fake);
        assert!(until(|| fake.snapshots() == 1));
        assert!(!watch.agent_gone());
        *fake.panes.lock().unwrap() = Some(vec!["w1:p3"]);
        fake.push(json!({ "id": "csr", "error": { "code": "events_lost" } }));
        assert!(
            until(|| watch.agent_gone()),
            "missing after the resubscription"
        );
    }

    #[test]
    fn a_snapshot_that_cannot_be_read_does_not_end_the_pane() {
        let fake = Fake::new();
        *fake.panes.lock().unwrap() = None;
        let watch = watch(&fake);
        assert!(until(|| fake.snapshots() == 1));
        thread::sleep(Duration::from_millis(200));
        assert!(!watch.agent_gone());
        // And with no agent to look for there is nothing to be missing.
        let fake = Fake::new();
        *fake.panes.lock().unwrap() = Some(vec![]);
        let watch = Watcher::start(fake.socket(), || None);
        assert!(until(|| fake.subscriptions() == 1));
        thread::sleep(Duration::from_millis(200));
        assert!(!watch.agent_gone());
    }

    #[test]
    fn a_socket_that_is_not_there_or_not_herdr_leaves_the_pane_as_it_was() {
        let d = tempfile::tempdir().unwrap();
        let none = Watcher::start(d.path().join("none.sock"), || None);
        // A listener that answers the subscription with an error.
        let listener = UnixListener::bind(d.path().join("bad.sock")).unwrap();
        thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut request = String::new();
                let _ = BufReader::new(conn.try_clone().unwrap()).read_line(&mut request);
                let _ = writeln!(
                    &conn,
                    "{}",
                    json!({ "id": "csr", "error": { "code": "bad" } })
                );
            }
        });
        let bad = Watcher::start(d.path().join("bad.sock"), || None);
        thread::sleep(Duration::from_millis(400));
        for watch in [&none, &bad] {
            assert!(!watch.agent_gone());
            assert!(!watch.take_status());
        }
    }

    /// A listener that does what `serve` is told with each connection, and counts them.
    fn listener(
        dir: &Path,
        name: &str,
        serve: impl Fn(UnixStream) + Send + Sync + 'static,
    ) -> (PathBuf, Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::AtomicUsize;
        let path = dir.join(name);
        let listener = UnixListener::bind(&path).unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let counted = count.clone();
        let serve = Arc::new(serve);
        thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                counted.fetch_add(1, Ordering::SeqCst);
                let serve = serve.clone();
                thread::spawn(move || serve(conn));
            }
        });
        (path, count)
    }
    fn read_request(conn: &UnixStream) {
        let mut request = String::new();
        let _ = BufReader::new(conn.try_clone().unwrap()).read_line(&mut request);
    }
    fn ack(conn: &UnixStream) {
        let _ = writeln!(
            &*conn,
            "{}",
            json!({ "id": "csr", "result": { "type": "subscription_started" } })
        );
    }
    /// The pane as it is with no watcher: nothing gone, nothing to draw for.
    fn quiet(watch: &Watcher) {
        assert!(!watch.agent_gone());
        assert!(!watch.take_status());
    }

    #[test]
    fn a_socket_that_is_stale_or_not_a_socket_leaves_the_pane_as_it_was() {
        let d = tempfile::tempdir().unwrap();
        // A socket file left behind by a herdr that is gone: bound, then nobody listens.
        let path = d.path().join("stale.sock");
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let started = Instant::now();
        let watch = Watcher::start(path, || Some("w1:p2".into()));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "start does not wait"
        );
        thread::sleep(Duration::from_millis(400));
        quiet(&watch);
        // And a path that is a plain file, not a socket.
        let file = d.path().join("plain");
        std::fs::write(&file, b"not a socket").unwrap();
        let watch = Watcher::start(file, || Some("w1:p2".into()));
        thread::sleep(Duration::from_millis(400));
        quiet(&watch);
    }

    #[test]
    fn a_refusing_socket_is_not_hammered() {
        let d = tempfile::tempdir().unwrap();
        // Every connection is closed at once, before a word: the retry waits 1 s, then 2 s.
        let (path, count) = listener(d.path(), "shut.sock", drop);
        let watch = Watcher::start(path, || Some("w1:p2".into()));
        thread::sleep(Duration::from_millis(2500));
        quiet(&watch);
        let tries = count.load(Ordering::SeqCst);
        assert!((1..=3).contains(&tries), "{tries} connections in 2.5 s");
    }

    #[test]
    fn a_herdr_that_closes_mid_stream_is_found_again_and_the_pane_goes_on() {
        let d = tempfile::tempdir().unwrap();
        // Acknowledges, then hangs up. Only the subscriptions are counted.
        let subscriptions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = subscriptions.clone();
        let (path, _) = listener(d.path(), "half.sock", move |conn| {
            let mut request = String::new();
            let _ = BufReader::new(conn.try_clone().unwrap()).read_line(&mut request);
            if request.contains("events.subscribe") {
                counted.fetch_add(1, Ordering::SeqCst);
                ack(&conn);
            }
        });
        let watch = Watcher::start(path, || Some("w1:p2".into()));
        assert!(until(|| subscriptions.load(Ordering::SeqCst) >= 1));
        quiet(&watch);
        // It subscribes again after the wait, not at once and not never.
        assert!(
            until(|| subscriptions.load(Ordering::SeqCst) >= 2),
            "no second subscription"
        );
        thread::sleep(Duration::from_millis(500));
        let tries = subscriptions.load(Ordering::SeqCst);
        assert!(tries <= 4, "{tries} subscriptions in about 2 s");
        quiet(&watch);
    }

    #[test]
    fn garbage_leaves_the_pane_as_it_was_and_a_good_event_after_it_still_counts() {
        let d = tempfile::tempdir().unwrap();
        // Not JSON at all, instead of the acknowledgement.
        let (path, _) = listener(d.path(), "junk.sock", |conn| {
            read_request(&conn);
            let _ = (&conn).write_all(b"\xff\xfe not json {{{\n[1,2,3]\n");
            thread::sleep(Duration::from_millis(300));
        });
        let watch = Watcher::start(path, || Some("w1:p2".into()));
        thread::sleep(Duration::from_millis(500));
        quiet(&watch);
        // Garbage after a good acknowledgement is skipped line by line.
        let (path, _) = listener(d.path(), "mixed.sock", |conn| {
            read_request(&conn);
            ack(&conn);
            let mut out = &conn;
            let _ = out.write_all(b"\xff\xfe\n\n{\"event\": 5}\n[[[[[[\n\"str\"\n{\"error\": 7}\n");
            let _ = writeln!(
                out,
                "{}",
                json!({ "event": "pane.agent_status_changed", "data": { "pane_id": "w1:p2" } })
            );
            thread::sleep(Duration::from_secs(5));
        });
        let watch = Watcher::start(path, || Some("w1:p2".into()));
        assert!(until(|| watch.take_status()), "the event after the garbage");
        assert!(!watch.agent_gone());
        // The same nesting that overflows a recursive parser is only an error line.
        let deep = format!("{}{}\n", "[".repeat(100_000), "]".repeat(100_000));
        let (path, _) = listener(d.path(), "deep.sock", move |conn| {
            read_request(&conn);
            ack(&conn);
            let _ = (&conn).write_all(deep.as_bytes());
            let _ = writeln!(
                &conn,
                "{}",
                json!({ "event": "pane.agent_status_changed", "data": { "pane_id": "w1:p2" } })
            );
            thread::sleep(Duration::from_secs(2));
        });
        let watch = Watcher::start(path, || Some("w1:p2".into()));
        assert!(until(|| watch.take_status()), "alive after the deep line");
        assert!(!watch.agent_gone());
    }

    #[test]
    fn a_stream_that_never_ends_a_line_is_dropped_and_not_kept_in_memory() {
        let d = tempfile::tempdir().unwrap();
        let (path, _) = listener(d.path(), "flood.sock", |conn| {
            read_request(&conn);
            ack(&conn);
            let chunk = vec![b'x'; 1 << 20];
            let mut out = &conn;
            for _ in 0..(LINE_LIMIT >> 20) + 4 {
                if out.write_all(&chunk).is_err() {
                    return;
                }
            }
            thread::sleep(Duration::from_secs(3));
        });
        // The subscription ends at the limit, well before the stream's own end, as one that was
        // not acknowledged would.
        let shared = Shared::default();
        let started = Instant::now();
        let ended = session(&shared, &path, Some("w1:p2"), &|_| false);
        assert!(matches!(ended, Ok(false)), "{ended:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert!(!shared.gone.load(Ordering::Relaxed));
        assert!(!shared.status.load(Ordering::Relaxed));
    }

    #[test]
    fn a_herdr_that_never_answers_costs_the_pane_nothing_and_leaving_does_not_wait() {
        let d = tempfile::tempdir().unwrap();
        let hung_up = Arc::new(AtomicBool::new(false));
        let seen = hung_up.clone();
        let (path, count) = listener(d.path(), "mute.sock", move |conn| {
            read_request(&conn);
            // Silent until the client lets go.
            let mut sink = [0u8; 64];
            while !matches!((&conn).read(&mut sink), Ok(0) | Err(_)) {}
            seen.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let watch = Watcher::start(path, || {
            // The lookup of the agent pane is itself slow.
            thread::sleep(Duration::from_millis(1200));
            Some("w1:p2".into())
        });
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "start does not wait"
        );
        let polled = Instant::now();
        for _ in 0..1000 {
            quiet(&watch);
        }
        assert!(
            polled.elapsed() < Duration::from_millis(200),
            "polling is a load"
        );
        assert!(
            until(|| count.load(Ordering::SeqCst) == 1),
            "it is waiting on herdr"
        );
        assert!(!hung_up.load(Ordering::SeqCst));
        let leaving = Instant::now();
        drop(watch);
        assert!(
            leaving.elapsed() < Duration::from_millis(200),
            "drop does not wait"
        );
        assert!(
            until(|| hung_up.load(Ordering::SeqCst)),
            "the socket is let go"
        );
    }

    #[test]
    fn a_herdr_that_trickles_a_line_is_let_go_when_the_pane_leaves() {
        let d = tempfile::tempdir().unwrap();
        let hung_up = Arc::new(AtomicBool::new(false));
        let seen = hung_up.clone();
        // A byte every 20 ms, never a newline: no read ever times out.
        let (path, _) = listener(d.path(), "trickle.sock", move |conn| {
            read_request(&conn);
            ack(&conn);
            while (&conn).write_all(b" ").is_ok() {
                thread::sleep(Duration::from_millis(20));
            }
            seen.store(true, Ordering::SeqCst);
        });
        let watch = Watcher::start(path, || Some("w1:p2".into()));
        thread::sleep(Duration::from_millis(600));
        quiet(&watch);
        drop(watch);
        assert!(
            until(|| hung_up.load(Ordering::SeqCst)),
            "the socket is let go"
        );
    }

    #[test]
    fn endless_short_garbage_ends_the_subscription_and_waits_before_the_next() {
        let d = tempfile::tempdir().unwrap();
        let subscriptions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = subscriptions.clone();
        let (path, _) = listener(d.path(), "spam.sock", move |conn| {
            let mut request = String::new();
            let _ = BufReader::new(conn.try_clone().unwrap()).read_line(&mut request);
            if !request.contains("events.subscribe") {
                return;
            }
            counted.fetch_add(1, Ordering::SeqCst);
            ack(&conn);
            let line = b"not json\n".repeat(1000);
            while (&conn).write_all(&line).is_ok() {}
        });
        let watch = Watcher::start(path, || Some("w1:p2".into()));
        assert!(until(|| subscriptions.load(Ordering::SeqCst) >= 1));
        thread::sleep(Duration::from_millis(1500));
        quiet(&watch);
        let tries = subscriptions.load(Ordering::SeqCst);
        assert!(tries <= 2, "{tries} subscriptions in 1.5 s");
    }

    #[test]
    fn an_agent_pane_herdr_still_knows_under_another_id_does_not_end_the_pane() {
        // Not in the snapshot, but herdr answers for it (a move gives a pane a new id and keeps
        // the old one as an alias): the second opinion says it is there.
        let fake = Fake::new();
        *fake.panes.lock().unwrap() = Some(vec!["w1:p3"]);
        let asked = Arc::new(Mutex::new(vec![]));
        let record = asked.clone();
        let watch = Watcher::start_with(
            fake.socket(),
            || Some("w1:p2".into()),
            Arc::new(move |pane| {
                record.lock().unwrap().push(pane.to_string());
                true
            }),
        );
        assert!(until(|| !asked.lock().unwrap().is_empty()), "asked");
        thread::sleep(Duration::from_millis(200));
        assert_eq!(*asked.lock().unwrap(), ["w1:p2"]);
        assert!(!watch.agent_gone());
        // And when herdr does not know it either, the pane ends.
        let fake = Fake::new();
        *fake.panes.lock().unwrap() = Some(vec!["w1:p3"]);
        let watch =
            Watcher::start_with(fake.socket(), || Some("w1:p2".into()), Arc::new(|_| false));
        assert!(until(|| watch.agent_gone()));
    }

    #[test]
    fn a_snapshot_that_is_garbage_or_has_no_panes_does_not_end_the_pane() {
        let d = tempfile::tempdir().unwrap();
        for answer in [
            b"\xff\xfe not json\n".to_vec(),
            b"{\"result\":{\"snapshot\":{\"panes\":\"no\"}}}\n".to_vec(),
            b"{\"result\":{\"snapshot\":{}}}\n".to_vec(),
            b"\n".to_vec(),
        ] {
            let name = format!("snap{}.sock", answer.len());
            let (path, count) = listener(d.path(), &name, move |conn| {
                let mut request = String::new();
                let _ = BufReader::new(conn.try_clone().unwrap()).read_line(&mut request);
                if request.contains("events.subscribe") {
                    ack(&conn);
                    thread::sleep(Duration::from_secs(3));
                } else {
                    let _ = (&conn).write_all(&answer);
                }
            });
            let watch = Watcher::start(path, || Some("w1:p2".into()));
            assert!(
                until(|| count.load(Ordering::SeqCst) >= 2),
                "snapshot asked for"
            );
            thread::sleep(Duration::from_millis(300));
            assert!(!watch.agent_gone(), "{name}");
        }
    }
}
