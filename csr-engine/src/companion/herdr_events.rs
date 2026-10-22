//! herdr's event stream: a pane nobody can see draws nothing.
//!
//! Opt-in: only with `CSR_PANE_HERDR_EVENTS=1` and herdr's `HERDR_SOCKET_PATH` (and the pane's own
//! `HERDR_PANE_ID`) set. A thread subscribes over herdr's socket (newline JSON), and keeps three
//! facts for the watch loop:
//!
//! - hidden: this pane's tab is not the focused tab. Read from `session.snapshot` on a second
//!   connection, as herdr's docs ask, whenever a focus or layout event arrives.
//! - wake: it became visible (draw at once), or the agent's status changed (draw soon).
//! - gone: the agent pane closed or exited, and so does this pane.
//!
//! It fails open. Any socket error, an error answer to the subscription, `events_lost`, or an
//! EOF drops the thread back to "visible", where the pane draws as it does without events, and it
//! reconnects with a growing wait.
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

/// A hidden pane still draws this often, so a pane herdr failed to tell us about is not frozen.
pub const HIDDEN_FLOOR: Duration = Duration::from_secs(60);
/// The wait before the first reconnect, and the most it grows to.
const RETRY: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(10);
/// How long a read waits before the thread looks at its stop flag.
const TICK: Duration = Duration::from_millis(250);
/// Focus and layout events come in bursts: one snapshot per burst, at most this late.
const COALESCE: Duration = Duration::from_millis(300);
const IO_LIMIT: Duration = Duration::from_secs(2);

/// The socket and this pane's id, when the person asked for events and herdr is there.
pub fn config(var: impl Fn(&str) -> Option<String>) -> Option<(PathBuf, String)> {
    let nonempty = |name| var(name).filter(|v| !v.is_empty());
    if nonempty("CSR_PANE_HERDR_EVENTS")? != "1" {
        return None;
    }
    Some((
        nonempty("HERDR_SOCKET_PATH")?.into(),
        nonempty("HERDR_PANE_ID")?,
    ))
}

/// Whether the watch loop may draw now: always when visible; when hidden, once per `HIDDEN_FLOOR`.
pub fn may_draw(hidden: bool, since_last_draw: Duration) -> bool {
    !hidden || since_last_draw >= HIDDEN_FLOOR
}

#[derive(Default)]
struct Shared {
    hidden: AtomicBool,
    visible: AtomicBool,
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
        let (socket, me) = config(|name| std::env::var(name).ok())?;
        Some(Self::start(socket, me, move || {
            send::agent_pane(agent.as_deref())
        }))
    }

    pub fn start(
        socket: PathBuf,
        me: String,
        agent: impl FnOnce() -> Option<String> + Send + 'static,
    ) -> Self {
        let shared = Arc::new(Shared::default());
        let thread = {
            let shared = shared.clone();
            thread::spawn(move || run(&shared, &socket, &me, agent().as_deref()))
        };
        Self {
            shared,
            thread: Some(thread),
        }
    }

    /// This pane's tab is not the one on screen.
    pub fn hidden(&self) -> bool {
        self.shared.hidden.load(Ordering::Relaxed)
    }
    /// The agent pane is closed or exited.
    pub fn agent_gone(&self) -> bool {
        self.shared.gone.load(Ordering::Relaxed)
    }
    /// Wakes since the last call: (became visible, agent status changed).
    pub fn take_wakes(&self) -> (bool, bool) {
        (
            self.shared.visible.swap(false, Ordering::Relaxed),
            self.shared.status.swap(false, Ordering::Relaxed),
        )
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
    let mut subscriptions = vec![
        json!({ "type": "tab.focused" }),
        json!({ "type": "workspace.focused" }),
        json!({ "type": "layout.updated" }),
    ];
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

fn run(shared: &Shared, socket: &Path, me: &str, agent: Option<&str>) {
    let mut wait = RETRY;
    while !shared.stop.load(Ordering::Relaxed) && !shared.gone.load(Ordering::Relaxed) {
        let started = session(shared, socket, me, agent);
        // Not connected: nobody is telling us the pane is hidden.
        shared.hidden.store(false, Ordering::Relaxed);
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
fn session(shared: &Shared, socket: &Path, me: &str, agent: Option<&str>) -> std::io::Result<bool> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(TICK))?;
    stream.set_write_timeout(Some(IO_LIMIT))?;
    (&stream).write_all(format!("{}\n", subscription(agent)).as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    let mut started = false;
    let mut pending: Option<Instant> = None;
    while !shared.stop.load(Ordering::Relaxed) {
        let quiet = match reader.read_until(b'\n', &mut line) {
            Ok(0) => return Ok(started),
            Ok(_) if line.ends_with(b"\n") => {
                let value: Value = serde_json::from_slice(&line).unwrap_or(Value::Null);
                line.clear();
                if !started {
                    if value["result"]["type"] != "subscription_started" {
                        return Ok(false);
                    }
                    started = true;
                    refresh(shared, socket, me);
                    continue;
                }
                // `events_lost`, or any other error: this subscription is over.
                if value["error"].is_object() {
                    return Ok(started);
                }
                match event(shared, &value, agent) {
                    Event::Focus => {
                        pending.get_or_insert_with(Instant::now);
                    }
                    Event::Gone => return Ok(started),
                    Event::Other => (),
                }
                false
            }
            Ok(_) => false,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => true,
            Err(e) if e.kind() == ErrorKind::Interrupted => false,
            Err(e) => return Err(e),
        };
        if pending.is_some_and(|since| quiet || since.elapsed() >= COALESCE) {
            pending = None;
            refresh(shared, socket, me);
        }
    }
    Ok(started)
}

enum Event {
    /// The focus or a layout moved: read the snapshot again.
    Focus,
    /// The agent pane is gone.
    Gone,
    Other,
}

fn event(shared: &Shared, value: &Value, agent: Option<&str>) -> Event {
    let name = value["event"].as_str().unwrap_or("").replace('.', "_");
    let pane = value["data"]["pane_id"].as_str();
    match name.as_str() {
        "tab_focused" | "workspace_focused" | "layout_updated" => Event::Focus,
        "pane_agent_status_changed" if pane.is_some() && pane == agent => {
            shared.status.store(true, Ordering::Relaxed);
            Event::Other
        }
        "pane_closed" | "pane_exited" if pane.is_some() && pane == agent => {
            shared.gone.store(true, Ordering::Relaxed);
            Event::Gone
        }
        _ => Event::Other,
    }
}

/// Read who is on screen and set `hidden`. A snapshot that cannot be read means visible.
fn refresh(shared: &Shared, socket: &Path, me: &str) {
    let hidden = snapshot(socket, me).unwrap_or(false);
    let was = shared.hidden.swap(hidden, Ordering::Relaxed);
    if was && !hidden {
        shared.visible.store(true, Ordering::Relaxed);
    }
}

/// True when `me` is in a tab that is not the focused one.
fn snapshot(socket: &Path, me: &str) -> Option<bool> {
    let stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(IO_LIMIT)).ok()?;
    stream.set_write_timeout(Some(IO_LIMIT)).ok()?;
    (&stream)
        .write_all(b"{\"id\":\"csr-snapshot\",\"method\":\"session.snapshot\",\"params\":{}}\n")
        .ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    let value: Value = serde_json::from_str(&line).ok()?;
    let snapshot = &value["result"]["snapshot"];
    let focused = snapshot["focused_tab_id"].as_str()?;
    let tab = snapshot["panes"]
        .as_array()?
        .iter()
        .find(|pane| pane["pane_id"] == me)?["tab_id"]
        .as_str()?;
    Some(tab != focused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, io::Read, os::unix::net::UnixListener, sync::Mutex};

    /// A herdr that answers on a socket: the focused tab is whatever the test says, and the events
    /// of the subscription are the lines the test pushes.
    struct Fake {
        dir: tempfile::TempDir,
        focused: Arc<Mutex<String>>,
        lines: Arc<Mutex<VecDeque<String>>>,
        requests: Arc<Mutex<Vec<Value>>>,
        stop: Arc<AtomicBool>,
    }
    impl Fake {
        fn new(focused: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let listener = UnixListener::bind(dir.path().join("herdr.sock")).unwrap();
            listener.set_nonblocking(true).unwrap();
            let fake = Self {
                dir,
                focused: Arc::new(Mutex::new(focused.into())),
                lines: Arc::default(),
                requests: Arc::default(),
                stop: Arc::default(),
            };
            let (focused, lines, requests, stop) = (
                fake.focused.clone(),
                fake.lines.clone(),
                fake.requests.clone(),
                fake.stop.clone(),
            );
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((conn, _)) => {
                            let (focused, lines, requests, stop) = (
                                focused.clone(),
                                lines.clone(),
                                requests.clone(),
                                stop.clone(),
                            );
                            thread::spawn(move || serve(conn, focused, lines, requests, stop));
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
        fn focus(&self, tab: &str) {
            *self.focused.lock().unwrap() = tab.into();
        }
        fn subscriptions(&self) -> usize {
            (self.requests.lock().unwrap().iter())
                .filter(|r| r["method"] == "events.subscribe")
                .count()
        }
        fn snapshots(&self) -> usize {
            (self.requests.lock().unwrap().iter())
                .filter(|r| r["method"] == "session.snapshot")
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
        focused: Arc<Mutex<String>>,
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
        match request["method"].as_str() {
            Some("session.snapshot") => {
                let focused = focused.lock().unwrap().clone();
                let snapshot = json!({
                    "id": request["id"],
                    "result": { "type": "session_snapshot", "snapshot": {
                        "focused_tab_id": focused,
                        "panes": [
                            { "pane_id": "w1:p2", "tab_id": "w1:t1" },
                            { "pane_id": "w1:p3", "tab_id": "w1:t2" },
                        ],
                    }},
                });
                let _ = writeln!(out, "{snapshot}");
            }
            Some("events.subscribe") => {
                let ack =
                    json!({ "id": request["id"], "result": { "type": "subscription_started" }});
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
            _ => (),
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
        Watcher::start(fake.socket(), "w1:p3".into(), || Some("w1:p2".into()))
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
            ("HERDR_PANE_ID", "w1:p3"),
        ];
        assert_eq!(
            config(env(&all)),
            Some(("/s/herdr.sock".into(), "w1:p3".into()))
        );
        assert_eq!(config(env(&all[1..])), None, "not asked for");
        let off = [("CSR_PANE_HERDR_EVENTS", "0"), all[1], all[2]];
        assert_eq!(config(env(&off)), None);
        let no_socket = [all[0], all[2]];
        assert_eq!(config(env(&no_socket)), None, "no socket");
        assert_eq!(config(env(&all[..2])), None, "no pane");
    }

    #[test]
    fn a_hidden_pane_draws_nothing_until_its_floor_or_until_it_is_seen() {
        let s = Duration::from_secs;
        assert!(may_draw(false, Duration::ZERO));
        assert!(!may_draw(true, Duration::ZERO));
        assert!(!may_draw(true, s(59)));
        assert!(may_draw(true, s(60)));
        assert!(may_draw(false, s(3600)));
    }

    #[test]
    fn it_subscribes_with_the_events_a_pane_needs() {
        let fake = Fake::new("w1:t1");
        let _watch = watch(&fake);
        assert!(until(|| fake.subscriptions() == 1));
        let request = fake.requests.lock().unwrap()[0].clone();
        assert_eq!(
            request,
            json!({
                "id": "csr",
                "method": "events.subscribe",
                "params": { "subscriptions": [
                    { "type": "tab.focused" },
                    { "type": "workspace.focused" },
                    { "type": "layout.updated" },
                    { "type": "pane.agent_status_changed", "pane_id": "w1:p2" },
                    { "type": "pane.closed" },
                    { "type": "pane.exited" },
                ]},
            })
        );
    }

    #[test]
    fn hidden_follows_the_focused_tab_and_becoming_visible_is_a_wake() {
        // The pane (w1:p3) is in tab w1:t2; the agent's tab w1:t1 is on screen.
        let fake = Fake::new("w1:t1");
        let watch = watch(&fake);
        assert!(until(|| watch.hidden()), "hidden at the start");
        assert_eq!(watch.take_wakes(), (false, false));
        fake.focus("w1:t2");
        fake.push(json!({ "event": "tab_focused", "data": { "tab_id": "w1:t2" } }));
        assert!(until(|| !watch.hidden()));
        assert!(until(|| watch.take_wakes().0), "visible is a wake");
        assert!(!watch.take_wakes().0, "once");
        fake.focus("w1:t1");
        fake.push(json!({ "event": "workspace_focused", "data": { "workspace_id": "w2" } }));
        assert!(until(|| watch.hidden()));
        assert_eq!(watch.take_wakes(), (false, false), "hiding is not a wake");
    }

    #[test]
    fn a_burst_of_layout_events_reads_one_snapshot_not_one_each() {
        let fake = Fake::new("w1:t2");
        let watch = watch(&fake);
        assert!(until(|| fake.snapshots() == 1));
        for _ in 0..30 {
            fake.push(json!({ "event": "layout_updated", "data": { "tab_id": "w1:t2" } }));
        }
        assert!(until(|| fake.snapshots() >= 2));
        thread::sleep(Duration::from_millis(600));
        assert!(fake.snapshots() <= 4, "{} snapshots", fake.snapshots());
        assert!(!watch.hidden());
    }

    #[test]
    fn the_agent_s_status_is_a_wake_and_another_panes_is_not() {
        let fake = Fake::new("w1:t2");
        let watch = watch(&fake);
        assert!(until(|| fake.subscriptions() == 1));
        fake.push(json!({ "event": "pane_agent_status_changed", "data": { "pane_id": "w1:p9" } }));
        fake.push(json!({ "event": "pane.agent_status_changed", "data": { "pane_id": "w1:p2" } }));
        assert!(until(|| watch.take_wakes().1));
    }

    #[test]
    fn the_pane_ends_when_the_agent_pane_closes_or_exits_and_not_for_another() {
        for event in ["pane_closed", "pane_exited"] {
            let fake = Fake::new("w1:t2");
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
    fn events_lost_resubscribes_and_reads_the_focus_again() {
        let fake = Fake::new("w1:t1");
        let watch = watch(&fake);
        assert!(until(|| watch.hidden()));
        fake.focus("w1:t2");
        fake.push(json!({ "id": "csr", "error": { "code": "events_lost" } }));
        assert!(until(|| fake.subscriptions() == 2), "a new subscription");
        assert!(until(|| !watch.hidden()), "and a new snapshot");
    }

    #[test]
    fn a_socket_that_is_not_there_or_not_herdr_leaves_the_pane_visible() {
        let d = tempfile::tempdir().unwrap();
        let none = Watcher::start(d.path().join("none.sock"), "w1:p3".into(), || None);
        // A listener that answers the subscription with an error, and one that says nothing sensible.
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
        let bad = Watcher::start(d.path().join("bad.sock"), "w1:p3".into(), || None);
        thread::sleep(Duration::from_millis(400));
        for watch in [&none, &bad] {
            assert!(!watch.hidden());
            assert!(!watch.agent_gone());
            assert_eq!(watch.take_wakes(), (false, false));
        }
    }

    #[test]
    fn a_snapshot_that_does_not_name_this_pane_means_visible() {
        let fake = Fake::new("w1:t1");
        let watch = Watcher::start(fake.socket(), "w9:p9".into(), || None);
        assert!(until(|| fake.snapshots() == 1));
        thread::sleep(Duration::from_millis(100));
        assert!(!watch.hidden());
    }
}
