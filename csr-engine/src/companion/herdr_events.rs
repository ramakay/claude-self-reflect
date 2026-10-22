//! herdr's event stream: the pane ends with its agent's pane, and draws when its status moves.
//!
//! Opt-in: only with `CSR_PANE_HERDR_EVENTS=1` and herdr's `HERDR_SOCKET_PATH` set. A thread subscribes over herdr's socket (newline JSON), and keeps two
//! facts for the watch loop:
//!
//! - wake: the agent's status changed (draw soon).
//! - gone: the agent pane closed or exited, and so does this pane.
//!
//! There is no "hidden": herdr says which tab its default focus is on, but each attached client
//! has a place of its own and the API does not list them, so a tab that is not the default focus
//! may be in front of someone. A pane that cannot know draws as it would without events; its own
//! gates keep a quiet one cheap.
//!
//! It fails open. Any socket error, an error answer to the subscription, `events_lost`, or an
//! EOF drops the thread back to plain polling, and it reconnects with a growing wait.
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

/// The socket, when the person asked for events and herdr is there.
pub fn config(var: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let nonempty = |name| var(name).filter(|v| !v.is_empty());
    if nonempty("CSR_PANE_HERDR_EVENTS")? != "1" {
        return None;
    }
    Some(nonempty("HERDR_SOCKET_PATH")?.into())
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
impl Watcher {
    /// A watcher of herdr's events, when asked for and possible. `agent` is the pane given on the
    /// command line; without one it is the left neighbour, found in the thread.
    pub fn from_env(agent: Option<String>) -> Option<Self> {
        let socket = config(|name| std::env::var(name).ok())?;
        Some(Self::start(socket, move || {
            send::agent_pane(agent.as_deref())
        }))
    }

    pub fn start(socket: PathBuf, agent: impl FnOnce() -> Option<String> + Send + 'static) -> Self {
        let shared = Arc::new(Shared::default());
        let thread = {
            let shared = shared.clone();
            thread::spawn(move || run(&shared, &socket, agent().as_deref()))
        };
        Self {
            shared,
            thread: Some(thread),
        }
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

fn run(shared: &Shared, socket: &Path, agent: Option<&str>) {
    let mut wait = RETRY;
    while !shared.stop.load(Ordering::Relaxed) && !shared.gone.load(Ordering::Relaxed) {
        let started = session(shared, socket, agent);
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

/// One subscription, until it ends. Ok(true): it was acknowledged before it ended.
fn session(shared: &Shared, socket: &Path, agent: Option<&str>) -> std::io::Result<bool> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(TICK))?;
    stream.set_write_timeout(Some(IO_LIMIT))?;
    (&stream).write_all(format!("{}\n", subscription(agent)).as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    let mut started = false;
    while !shared.stop.load(Ordering::Relaxed) {
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => return Ok(started),
            Ok(_) if line.ends_with(b"\n") => {
                let value: Value = serde_json::from_slice(&line).unwrap_or(Value::Null);
                line.clear();
                if !started {
                    if value["result"]["type"] != "subscription_started" {
                        return Ok(false);
                    }
                    started = true;
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
            Ok(_) => (),
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(started)
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
            };
            let (lines, requests, stop) =
                (fake.lines.clone(), fake.requests.clone(), fake.stop.clone());
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((conn, _)) => {
                            let (lines, requests, stop) =
                                (lines.clone(), requests.clone(), stop.clone());
                            thread::spawn(move || serve(conn, lines, requests, stop));
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
    ) {
        conn.set_nonblocking(false).unwrap();
        let mut request = String::new();
        BufReader::new(conn.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        let request: Value = serde_json::from_str(&request).unwrap();
        requests.lock().unwrap().push(request.clone());
        let mut out = conn;
        if request["method"] == "events.subscribe" {
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
    fn it_is_asked_for_by_the_environment_and_by_nothing_else() {
        fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
            move |name| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            }
        }
        let all = [
            ("CSR_PANE_HERDR_EVENTS", "1"),
            ("HERDR_SOCKET_PATH", "/s/herdr.sock"),
        ];
        assert_eq!(config(env(&all)), Some("/s/herdr.sock".into()));
        assert_eq!(config(env(&all[1..])), None, "not asked for");
        let off = [("CSR_PANE_HERDR_EVENTS", "0"), all[1]];
        assert_eq!(config(env(&off)), None);
        assert_eq!(config(env(&all[..1])), None, "no socket");
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
    fn events_lost_subscribes_again() {
        let fake = Fake::new();
        let _watch = watch(&fake);
        assert!(until(|| fake.subscriptions() == 1));
        fake.push(json!({ "id": "csr", "error": { "code": "events_lost" } }));
        assert!(until(|| fake.subscriptions() == 2), "a new subscription");
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
}
