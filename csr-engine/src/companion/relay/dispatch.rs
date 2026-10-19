//! Routes what the relay posts to the handler for its hook key, by the hook's shape.
//!
//! The manifest says what each key is (`Shape`); `Table` says what the engine does for it. A
//! handler gets a `Ctx` (the conversation's host, the session, the store, `sid`, `cwd`) and
//! returns a `Reply`; the shape decides which fields of the reply reach the relay:
//!
//! | shape | `/event` replies | `/after` replies |
//! |---|---|---|
//! | Gate | `answer` or `next` (and `id` when it left something for `/after`) | `result` |
//! | Notify | `{}` at once; the handler runs on, its calls go out on stdout | nothing |
//! | Render | `tree` or nothing | nothing |
//! | Answer | `answer` | nothing |
//! | Start | nothing (session opened) | nothing |
//!
//! An unknown key, or a key with no handler, answers `{}`: the relay passes the call through.

use super::{
    host::{parse_results, BoxFuture, Clock, Host, Hub},
    manifest::{self, Shape},
    session::{SessionRef, Sessions},
};
use crate::companion::store::Store;
use anyhow::Result;
use serde_json::{json, Map, Value};
use std::{
    collections::HashMap,
    fs::File,
    future::Future,
    io::Write,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
};

pub const TOAST: &str = "CSR updated: run /reload-plugins";

/// The `CSR_RELAY_LOG` file: one JSON row per request, for measuring and for tests.
#[derive(Default)]
pub struct Log(Option<Mutex<File>>);

impl Log {
    pub fn to(file: File) -> Self {
        Self(Some(Mutex::new(file)))
    }

    pub fn enabled(&self) -> bool {
        self.0.is_some()
    }

    pub fn row(&self, row: Value) {
        if let Some(file) = &self.0 {
            if let Ok(mut file) = file.lock() {
                let _ = writeln!(file, "{row}");
            }
        }
    }
}

/// Everything a handler may use.
#[derive(Clone)]
pub struct Ctx {
    pub host: Arc<dyn Host>,
    pub session: SessionRef,
    pub store: Arc<Store>,
    pub clock: Arc<dyn Clock>,
    pub log: Arc<Log>,
    pub sid: String,
    pub cwd: String,
    /// The conversation id; for a gate, what its `/after` names. Empty for a Notify handler.
    pub id: String,
    /// On `/after`: what this key's `/event` left (`Reply::left`), if anything.
    pub left: Option<Value>,
    /// On the start event: the relay posted it again after a respawn.
    pub respawn: bool,
    pub pid: u32,
    /// `/event` requests so far, this one included.
    pub events: u64,
}

/// What a handler wants sent back. Which fields count depends on the hook's shape.
#[derive(Default, Debug, Clone)]
pub struct Reply {
    /// Return this instead of calling `next` (Gate, Answer).
    pub answer: Option<Value>,
    /// Call `next` with this event instead (Gate).
    pub next: Option<Value>,
    /// The plain element tree (Render).
    pub tree: Option<Value>,
    /// On `/after`: return this instead of what `next` returned (Gate).
    pub result: Option<Value>,
    /// Kept for this gate's `/after`, which reads it as `Ctx::left`.
    pub left: Option<Value>,
}

impl Reply {
    pub fn none() -> Self {
        Self::default()
    }
    pub fn answer(value: Value) -> Self {
        Self {
            answer: Some(value),
            ..Self::default()
        }
    }
    pub fn next(value: Value) -> Self {
        Self {
            next: Some(value),
            ..Self::default()
        }
    }
    pub fn tree(value: Value) -> Self {
        Self {
            tree: Some(value),
            ..Self::default()
        }
    }
    pub fn result(value: Value) -> Self {
        Self {
            result: Some(value),
            ..Self::default()
        }
    }
}

pub type Handler = Arc<dyn Fn(Ctx, Value) -> BoxFuture<Result<Reply>> + Send + Sync>;

/// Wrap an async closure as a `Handler`.
pub fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(Ctx, Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Reply>> + Send + 'static,
{
    Arc::new(move |cx, e| Box::pin(f(cx, e)))
}

/// What the engine does for one hook key.
#[derive(Clone, Default)]
pub struct Entry {
    /// `/event`: `e` is the event as Claude Code gave it.
    pub event: Option<Handler>,
    /// `/after`: `e` is what `next` returned.
    pub after: Option<Handler>,
}

pub type Table = HashMap<&'static str, Entry>;

/// The key a button press is routed by; it is not a hook, so the manifest does not list it.
pub const PRESS: &str = "press";

fn start() -> Handler {
    handler(|cx, _e| async move {
        if cx.respawn {
            // The registration lives in the module, not in the child that died.
            return Ok(Reply::none());
        }
        let command = json!({
            "name": "csr",
            "description": "Open the CSR pane beside Claude; \
                \"/csr dreams\" says how the dreamer is pacing itself",
        });
        let _ = cx.host.call("command.register", vec![command]).await;
        let log = cx.log.clone();
        cx.host.detach(Box::new(move |host| {
            Box::pin(async move {
                let usage = host.call("session.usage", vec![]).await.ok();
                log.row(json!({ "k": "usage", "usage": usage }));
            })
        }));
        Ok(Reply::none())
    })
}

fn csr() -> Handler {
    handler(|cx, _e| async move {
        let text = format!("csr relay ok pid={} events={}", cx.pid, cx.events);
        Ok(Reply::answer(json!({ "text": text })))
    })
}

/// What the engine does today: the WP0 behaviour, nothing ported yet.
pub fn default_table() -> Table {
    let only = |h: Handler| Entry {
        event: Some(h),
        after: None,
    };
    HashMap::from([
        ("start", only(start())),
        ("csr", only(csr())),
        ("measure", only(crate::companion::usage::measure())),
    ])
}

pub struct App {
    relay: String,
    pid: u32,
    hub: Arc<Hub>,
    sessions: Mutex<Sessions>,
    store: Arc<Store>,
    clock: Arc<dyn Clock>,
    log: Arc<Log>,
    table: Table,
    events: AtomicU64,
    warned: AtomicBool,
}

impl App {
    pub fn new(
        relay: &str,
        hub: Arc<Hub>,
        store: Arc<Store>,
        clock: Arc<dyn Clock>,
        log: Arc<Log>,
        table: Table,
    ) -> Arc<Self> {
        Arc::new(Self {
            relay: relay.to_string(),
            pid: std::process::id(),
            hub,
            sessions: Mutex::new(Sessions::default()),
            store,
            clock,
            log,
            table,
            events: AtomicU64::new(0),
            warned: AtomicBool::new(false),
        })
    }

    pub fn log(&self) -> &Log {
        &self.log
    }

    pub fn session_count(&self) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// One request from the relay. `/step` carries no `sid`; the rest open their session here.
    pub async fn handle(self: &Arc<Self>, path: &str, body: &Value) -> Value {
        self.check_version(body);
        if path == "/step" {
            let id = body.get("id").and_then(Value::as_str).unwrap_or("");
            return self.hub.step(id, parse_results(body)).await;
        }
        let text = |name: &str| {
            body.get(name)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let (sid, cwd) = (text("sid"), text("cwd"));
        let session = {
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions.touch(&sid, &cwd, self.clock.now()).0
        };
        let events = if path == "/event" {
            self.events.fetch_add(1, Ordering::Relaxed) + 1
        } else {
            self.events.load(Ordering::Relaxed)
        };
        let (key, e) = match path {
            "/press" => (PRESS.to_string(), body.clone()),
            _ => (text("k"), body.get("e").cloned().unwrap_or(Value::Null)),
        };
        let at = Request {
            sid,
            cwd,
            session,
            events,
            respawn: body.get("respawn").and_then(Value::as_bool) == Some(true),
        };
        match path {
            "/event" => self.event(&key, e, at).await,
            "/after" => {
                let id = body.get("id").and_then(Value::as_str).unwrap_or("");
                let left = at.session.lock().map_or(None, |mut s| s.take_left(id));
                self.after(
                    &key,
                    body.get("result").cloned().unwrap_or(Value::Null),
                    left,
                    at,
                )
                .await
            }
            "/press" => self.press(e, at).await,
            _ => json!({}),
        }
    }

    /// The relay's version differs from this engine's: one toast, once.
    fn check_version(&self, body: &Value) {
        let sent = body.get("v").and_then(Value::as_str);
        if sent != Some(self.relay.as_str()) && !self.warned.swap(true, Ordering::Relaxed) {
            self.hub.detach(|host| {
                Box::pin(async move {
                    let _ = host.call("ui.toast", vec![json!(TOAST)]).await;
                })
            });
        }
    }

    fn ctx(&self, host: Arc<dyn Host>, at: &Request, id: &str, left: Option<Value>) -> Ctx {
        Ctx {
            host,
            session: at.session.clone(),
            store: self.store.clone(),
            clock: self.clock.clone(),
            log: self.log.clone(),
            sid: at.sid.clone(),
            cwd: at.cwd.clone(),
            id: id.to_string(),
            left,
            respawn: at.respawn,
            pid: self.pid,
            events: at.events,
        }
    }

    async fn event(&self, key: &str, e: Value, at: Request) -> Value {
        let Some(hook) = manifest::find(key) else {
            return json!({});
        };
        if hook.shape == Shape::Start {
            if let Ok(mut session) = at.session.lock() {
                session.interactive = e.get("isInteractive").and_then(Value::as_bool);
            }
        }
        let Some(run) = self.table.get(key).and_then(|entry| entry.event.clone()) else {
            return json!({});
        };
        if hook.shape == Shape::Notify {
            let (cx_of, log) = (self.cx_maker(at), self.log.clone());
            self.hub.detach(move |host| {
                Box::pin(async move {
                    if let Err(error) = run(cx_of(host, ""), e).await {
                        log.row(
                            json!({ "k": "error", "for": hook.key, "error": format!("{error:#}") }),
                        );
                    }
                })
            });
            return json!({});
        }
        self.converse(hook.key, hook.shape, false, run, e, None, at)
            .await
    }

    async fn after(&self, key: &str, result: Value, left: Option<Value>, at: Request) -> Value {
        let Some(hook) = manifest::find(key) else {
            return json!({});
        };
        let Some(run) = self.table.get(key).and_then(|entry| entry.after.clone()) else {
            return json!({});
        };
        self.converse(hook.key, hook.shape, true, run, result, left, at)
            .await
    }

    async fn press(&self, e: Value, at: Request) -> Value {
        let Some(run) = self.table.get(PRESS).and_then(|entry| entry.event.clone()) else {
            return json!({});
        };
        self.converse(PRESS, Shape::Start, false, run, e, None, at)
            .await
    }

    fn cx_maker(&self, at: Request) -> impl FnOnce(Arc<dyn Host>, &str) -> Ctx + Send + 'static {
        let base = self.ctx(Arc::new(NoHost), &at, "", None);
        move |host, id| Ctx {
            host,
            id: id.to_string(),
            ..base
        }
    }

    /// Run a handler as a conversation: its host calls become `{ id, calls }` replies.
    #[allow(clippy::too_many_arguments)]
    async fn converse(
        &self,
        key: &'static str,
        shape: Shape,
        after: bool,
        run: Handler,
        e: Value,
        left: Option<Value>,
        at: Request,
    ) -> Value {
        let cx_of = self.cx_maker(at);
        let log = self.log.clone();
        self.hub
            .converse(move |host| {
                Box::pin(async move {
                    let id = host.id().to_string();
                    let mut cx = cx_of(host, &id);
                    cx.left = left;
                    let session = cx.session.clone();
                    match run(cx, e).await {
                        Ok(reply) => {
                            let keep = shape == Shape::Gate && !after && reply.answer.is_none();
                            if let (true, Some(value), Ok(mut session)) =
                                (keep, reply.left.clone(), session.lock())
                            {
                                session.leave(&id, value);
                            }
                            shaped(shape, after, &reply, &id)
                        }
                        Err(error) => {
                            log.row(
                                json!({ "k": "error", "for": key, "error": format!("{error:#}") }),
                            );
                            json!({})
                        }
                    }
                })
            })
            .await
    }
}

struct Request {
    sid: String,
    cwd: String,
    session: SessionRef,
    events: u64,
    respawn: bool,
}

/// The reply fields the hook's shape allows.
fn shaped(shape: Shape, after: bool, reply: &Reply, id: &str) -> Value {
    let mut out = Map::new();
    let mut put = |name: &str, value: &Option<Value>| {
        if let Some(value) = value {
            out.insert(name.to_string(), value.clone());
        }
    };
    match (shape, after) {
        (Shape::Gate, false) => {
            if reply.answer.is_some() {
                put("answer", &reply.answer);
            } else {
                put("next", &reply.next);
                if reply.left.is_some() {
                    put("id", &Some(json!(id)));
                }
            }
        }
        (Shape::Gate, true) => put("result", &reply.result),
        (Shape::Render, false) => put("tree", &reply.tree),
        (Shape::Answer, false) => put("answer", &reply.answer),
        _ => (),
    }
    Value::Object(out)
}

/// A host for a context that has not met its conversation yet; never reached.
struct NoHost;

#[async_trait::async_trait]
impl Host for NoHost {
    async fn batch(
        &self,
        _calls: Vec<super::host::Call>,
    ) -> Result<Vec<std::result::Result<Value, String>>> {
        anyhow::bail!("no conversation")
    }
    fn detach(&self, _work: super::host::Detached) {}
}

#[cfg(test)]
mod tests {
    use super::super::host::fake::FakeClock;
    use super::*;
    use std::time::Duration;
    use tokio::sync::mpsc;

    struct Rig {
        app: Arc<App>,
        lines: mpsc::UnboundedReceiver<String>,
        clock: Arc<FakeClock>,
        _dir: tempfile::TempDir,
    }

    fn rig(table: Table) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let (push, lines) = mpsc::unbounded_channel();
        let clock = FakeClock::new(1_000.);
        let app = App::new(
            "1",
            Hub::new(push),
            Arc::new(Store::at(dir.path().join("csr-companion/companion.db"))),
            clock.clone(),
            Arc::new(Log::default()),
            table,
        );
        Rig {
            app,
            lines,
            clock,
            _dir: dir,
        }
    }

    /// Let the tasks the runtime holds run until they wait again.
    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    fn table(rows: Vec<(&'static str, Entry)>) -> Table {
        rows.into_iter().collect()
    }

    fn event(h: Handler) -> Entry {
        Entry {
            event: Some(h),
            after: None,
        }
    }

    async fn post(app: &Arc<App>, path: &str, body: Value) -> Value {
        let mut body = body;
        body["v"] = json!("1");
        app.handle(path, &body).await
    }

    fn ev(sid: &str, k: &str, e: Value) -> Value {
        json!({ "sid": sid, "cwd": "/repo", "k": k, "e": e })
    }

    #[tokio::test]
    async fn a_handler_that_awaits_twice_makes_two_rounds_and_sees_each_answer() {
        let got = Arc::new(Mutex::new(Vec::new()));
        let seen = got.clone();
        let rig = rig(table(vec![(
            "csr",
            event(handler(move |cx, _| {
                let seen = seen.clone();
                async move {
                    let usage = cx.host.call("session.usage", vec![]).await?;
                    let pair = cx
                        .host
                        .batch(vec![
                            super::super::host::Call::new("session.model", vec![]),
                            super::super::host::Call::new("session.surfaces", vec![]),
                        ])
                        .await?;
                    seen.lock().unwrap().push((usage, pair));
                    Ok(Reply::answer(json!({ "text": "done" })))
                }
            })),
        )]));
        let first = post(&rig.app, "/event", ev("s", "csr", json!({}))).await;
        assert_eq!(first["calls"].as_array().unwrap().len(), 1);
        assert_eq!(first["calls"][0]["m"], "session.usage");
        let id = first["id"].as_str().unwrap();
        let second = post(
            &rig.app,
            "/step",
            json!({ "id": id, "results": [{ "id": "c1", "v": { "tokens": 9 } }] }),
        )
        .await;
        assert_eq!(second["id"], id);
        let calls = second["calls"].as_array().unwrap();
        assert_eq!(calls.len(), 2, "two calls in one round trip");
        assert_eq!(
            (calls[0]["id"].as_str(), calls[1]["id"].as_str()),
            (Some("c2"), Some("c3"))
        );
        let done = post(
            &rig.app,
            "/step",
            json!({ "id": id, "results": [
                { "id": "c3", "err": "no surface" }, { "id": "c2", "v": "opus" } ] }),
        )
        .await;
        assert_eq!(done, json!({ "answer": { "text": "done" } }));
        let got = got.lock().unwrap();
        assert_eq!(got[0].0, json!({ "tokens": 9 }));
        assert_eq!(
            got[0].1,
            vec![Ok(json!("opus")), Err("no surface".to_string())]
        );
        assert_eq!(rig.app.hub.waiting(), 0);
    }

    #[tokio::test]
    async fn a_gate_may_answer() {
        let rig = rig(table(vec![(
            "guard",
            event(handler(|_, e| async move {
                Ok(Reply {
                    answer: Some(json!({ "deny": e["tool"] })),
                    next: Some(json!("ignored")),
                    left: Some(json!(1)),
                    ..Reply::default()
                })
            })),
        )]));
        let reply = post(
            &rig.app,
            "/event",
            ev("s", "guard", json!({ "tool": "Bash" })),
        )
        .await;
        assert_eq!(reply, json!({ "answer": { "deny": "Bash" } }));
    }

    #[tokio::test]
    async fn a_gate_may_rewrite_and_its_after_reads_what_the_event_left() {
        let after = handler(|cx, result| async move {
            let left = cx.left.clone().unwrap_or(Value::Null);
            cx.store
                .set(&cx.sid, "last", &json!({ "left": left, "result": result }))?;
            Ok(Reply::result(json!({ "skip": true })))
        });
        let rig = rig(table(vec![(
            "guard",
            Entry {
                event: Some(handler(|_, e| async move {
                    Ok(Reply {
                        next: Some(json!({ "rewritten": e["n"] })),
                        left: Some(json!({ "n": e["n"] })),
                        ..Reply::default()
                    })
                })),
                after: Some(after),
            },
        )]));
        let first = post(&rig.app, "/event", ev("s", "guard", json!({ "n": 7 }))).await;
        assert_eq!(first["next"], json!({ "rewritten": 7 }));
        let id = first["id"].clone();
        let second = post(
            &rig.app,
            "/after",
            json!({ "sid": "s", "k": "guard", "id": id, "result": { "ok": 1 } }),
        )
        .await;
        assert_eq!(second, json!({ "result": { "skip": true } }));
        let stored = rig.app.store.get("s", "last").unwrap().unwrap();
        assert_eq!(stored, json!({ "left": { "n": 7 }, "result": { "ok": 1 } }));
        // The second /after for the same id finds nothing left.
        post(
            &rig.app,
            "/after",
            json!({ "sid": "s", "k": "guard", "id": id, "result": 1 }),
        )
        .await;
        let stored = rig.app.store.get("s", "last").unwrap().unwrap();
        assert_eq!(stored["left"], Value::Null);
    }

    #[tokio::test]
    async fn a_gate_that_leaves_nothing_replies_empty_and_the_shape_filters_fields() {
        let all = || {
            handler(|_, _| async {
                Ok(Reply {
                    answer: Some(json!("a")),
                    next: Some(json!("n")),
                    tree: Some(json!("t")),
                    result: Some(json!("r")),
                    left: None,
                })
            })
        };
        let rig = rig(table(vec![
            ("tool", event(handler(|_, _| async { Ok(Reply::none()) }))),
            ("band", event(all())),
            ("csr", event(all())),
            ("stop", event(all())),
        ]));
        assert_eq!(
            post(&rig.app, "/event", ev("s", "tool", json!({}))).await,
            json!({})
        );
        // Render keeps the tree only, Answer the answer only, Notify nothing.
        assert_eq!(
            post(&rig.app, "/event", ev("s", "band", json!({}))).await,
            json!({ "tree": "t" })
        );
        assert_eq!(
            post(&rig.app, "/event", ev("s", "csr", json!({}))).await,
            json!({ "answer": "a" })
        );
        assert_eq!(
            post(&rig.app, "/event", ev("s", "stop", json!({}))).await,
            json!({})
        );
    }

    #[tokio::test]
    async fn an_unknown_key_and_a_key_with_no_handler_pass_through() {
        let mut rig = rig(default_table());
        for k in ["no-such-hook", "guard", "band", "tool"] {
            assert_eq!(
                post(&rig.app, "/event", ev("s", k, json!({}))).await,
                json!({}),
                "{k}"
            );
        }
        for path in ["/after", "/press", "/nothing"] {
            let body = json!({ "sid": "s", "k": "guard", "id": "x" });
            assert_eq!(post(&rig.app, path, body).await, json!({}), "{path}");
        }
        assert!(rig.lines.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_notify_replies_at_once_and_its_calls_go_out_as_a_pushed_line() {
        let done = Arc::new(Mutex::new(None));
        let seen = done.clone();
        let mut rig = rig(table(vec![(
            "stop",
            event(handler(move |cx, _| {
                let seen = seen.clone();
                async move {
                    let drawn = cx.host.call("ui.invalidate", vec![]).await?;
                    *seen.lock().unwrap() = Some(drawn);
                    Ok(Reply::none())
                }
            })),
        )]));
        assert_eq!(
            post(&rig.app, "/event", ev("s", "stop", json!({}))).await,
            json!({})
        );
        let line: Value = serde_json::from_str(&rig.lines.recv().await.unwrap()).unwrap();
        assert_eq!(line["calls"][0]["m"], "ui.invalidate");
        assert!(done.lock().unwrap().is_none());
        let id = line["id"].as_str().unwrap();
        let back = post(
            &rig.app,
            "/step",
            json!({ "id": id, "results": [{ "id": "c1", "v": "ok" }] }),
        )
        .await;
        assert_eq!(
            back,
            json!({}),
            "the pushed conversation ends with no reply"
        );
        assert_eq!(*done.lock().unwrap(), Some(json!("ok")));
    }

    #[tokio::test]
    async fn a_pushed_call_from_the_default_start_is_resumed_by_step() {
        let mut rig = rig(default_table());
        let start = post(
            &rig.app,
            "/event",
            ev("s1", "start", json!({ "isInteractive": true })),
        )
        .await;
        assert_eq!(start["calls"][0]["m"], "command.register");
        assert_eq!(start["calls"][0]["a"][0]["name"], "csr");
        assert!(rig.lines.try_recv().is_err());
        let done = post(
            &rig.app,
            "/step",
            json!({ "id": start["id"], "results": [{ "id": "c1", "v": {} }] }),
        )
        .await;
        assert_eq!(done, json!({}));
        let pushed: Value = serde_json::from_str(&rig.lines.recv().await.unwrap()).unwrap();
        assert_eq!(pushed["calls"][0]["m"], "session.usage");
        let end = post(
            &rig.app,
            "/step",
            json!({ "id": pushed["id"], "results": [{ "id": "c1", "v": { "a": 1 } }] }),
        )
        .await;
        assert_eq!(end, json!({}));
        let session = rig.app.sessions.lock().unwrap().get("s1").unwrap();
        assert_eq!(session.lock().unwrap().interactive, Some(true));
    }

    #[tokio::test]
    async fn a_respawned_start_reopens_the_session_and_does_not_register_again() {
        let mut rig = rig(default_table());
        let mut body = ev("s1", "start", json!({ "isInteractive": false }));
        body["respawn"] = json!(true);
        assert_eq!(post(&rig.app, "/event", body).await, json!({}));
        assert!(rig.lines.try_recv().is_err());
        let session = rig.app.sessions.lock().unwrap().get("s1").unwrap();
        assert_eq!(session.lock().unwrap().interactive, Some(false));
    }

    #[tokio::test]
    async fn a_step_for_an_unknown_id_replies_empty() {
        let rig = rig(default_table());
        for body in [
            json!({ "id": "e99", "results": [] }),
            json!({ "results": [] }),
            json!({ "id": 5 }),
        ] {
            assert_eq!(post(&rig.app, "/step", body).await, json!({}));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_conversation_with_no_step_in_thirty_seconds_is_dropped_and_its_handler_errs() {
        let outcome = Arc::new(Mutex::new(None));
        let seen = outcome.clone();
        let rig = rig(table(vec![(
            "csr",
            event(handler(move |cx, _| {
                let seen = seen.clone();
                async move {
                    let result = cx.host.call("session.usage", vec![]).await;
                    *seen.lock().unwrap() = Some(result.map_err(|e| e.to_string()));
                    Ok(Reply::answer(json!("late")))
                }
            })),
        )]));
        let first = post(&rig.app, "/event", ev("s", "csr", json!({}))).await;
        let id = first["id"].as_str().unwrap().to_string();
        assert_eq!(rig.app.hub.waiting(), 1);
        tokio::time::advance(Duration::from_secs(29)).await;
        settle().await;
        assert!(
            outcome.lock().unwrap().is_none(),
            "29 s is inside the limit"
        );
        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
        let outcome = outcome
            .lock()
            .unwrap()
            .take()
            .expect("the handler was told");
        assert!(outcome.unwrap_err().contains("dropped"));
        assert_eq!(rig.app.hub.waiting(), 0);
        let late = post(&rig.app, "/step", json!({ "id": id, "results": [] })).await;
        assert_eq!(late, json!({}));
    }

    #[tokio::test]
    async fn a_session_opens_on_the_first_request_and_a_clear_opens_a_second() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let rig = rig(table(vec![(
            "tool",
            event(handler(move |cx, _| {
                let log = log.clone();
                async move {
                    log.lock().unwrap().push((cx.sid.clone(), cx.cwd.clone()));
                    let n = cx
                        .store
                        .get(&cx.sid, "n")?
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    cx.store.set(&cx.sid, "n", &json!(n + 1))?;
                    Ok(Reply::none())
                }
            })),
        )]));
        assert_eq!(rig.app.session_count(), 0);
        post(&rig.app, "/event", ev("before", "tool", json!({}))).await;
        assert_eq!(rig.app.session_count(), 1, "opened with no session.start");
        post(&rig.app, "/event", ev("before", "tool", json!({}))).await;
        let mut cleared = ev("after", "tool", json!({}));
        cleared["cwd"] = json!("/other");
        post(&rig.app, "/event", cleared).await;
        assert_eq!(rig.app.session_count(), 2);
        assert_eq!(rig.app.store.get("before", "n").unwrap(), Some(json!(2)));
        assert_eq!(rig.app.store.get("after", "n").unwrap(), Some(json!(1)));
        assert_eq!(
            seen.lock().unwrap()[2],
            ("after".to_string(), "/other".to_string())
        );
        rig.clock.advance(super::super::session::IDLE_MS + 1.);
        post(&rig.app, "/event", ev("after", "tool", json!({}))).await;
        assert_eq!(rig.app.session_count(), 1, "the idle one is gone");
    }

    #[tokio::test]
    async fn a_failing_or_panicking_handler_lets_the_call_through() {
        let rig = rig(table(vec![
            (
                "guard",
                event(handler(|_, _| async { anyhow::bail!("boom") })),
            ),
            ("tool", event(handler(|_, _| async { panic!("worse") }))),
        ]));
        assert_eq!(
            post(&rig.app, "/event", ev("s", "guard", json!({}))).await,
            json!({})
        );
        assert_eq!(
            post(&rig.app, "/event", ev("s", "tool", json!({}))).await,
            json!({})
        );
    }

    #[tokio::test]
    async fn csr_answers_with_pid_and_the_running_event_count() {
        let rig = rig(default_table());
        post(&rig.app, "/event", ev("s", "tool", json!({}))).await;
        let reply = post(&rig.app, "/event", ev("s", "csr", json!({}))).await;
        let pid = std::process::id();
        assert_eq!(
            reply["answer"]["text"],
            format!("csr relay ok pid={pid} events=2")
        );
    }

    #[tokio::test]
    async fn a_different_relay_version_toasts_once_and_the_same_one_never() {
        let mut old = rig(default_table());
        let stale = json!({ "v": "0", "sid": "s", "k": "tool" });
        old.app.handle("/event", &stale).await;
        old.app.handle("/event", &stale).await;
        let line: Value = serde_json::from_str(&old.lines.recv().await.unwrap()).unwrap();
        assert_eq!(line["calls"][0]["m"], "ui.toast");
        assert_eq!(line["calls"][0]["a"][0], TOAST);
        settle().await;
        assert!(old.lines.try_recv().is_err());
        let mut same = rig(default_table());
        post(&same.app, "/event", ev("s", "tool", json!({}))).await;
        settle().await;
        assert!(same.lines.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_press_runs_the_press_handler_inline() {
        let rig = rig(table(vec![(
            PRESS,
            event(handler(|cx, e| async move {
                cx.host
                    .call("prompt.fill", vec![json!({ "text": e["key"] })])
                    .await?;
                Ok(Reply::none())
            })),
        )]));
        let first = post(
            &rig.app,
            "/press",
            json!({ "sid": "s", "key": "1", "requestId": "csr" }),
        )
        .await;
        assert_eq!(first["calls"][0]["a"][0], json!({ "text": "1" }));
        let done = post(
            &rig.app,
            "/step",
            json!({ "id": first["id"], "results": [{ "id": "c1", "v": {} }] }),
        )
        .await;
        assert_eq!(done, json!({}));
    }
}
