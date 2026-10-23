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
    session::{Ext, SessionRef, Sessions},
    timers::{HubTimers, TimerWork, Timers},
};
use crate::companion::{store::Store, wake};
use anyhow::Result;
use serde_json::{json, Map, Value};
use std::{
    collections::HashMap,
    fs::File,
    future::Future,
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
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
    /// The engine's timers; `after` and `every` below give their work a context.
    pub timers: Arc<dyn Timers>,
    /// What `kept` holds, one for the whole process.
    pub module: Arc<Mutex<Ext>>,
}

impl Ctx {
    /// The process-wide state of type `T`, made with `Default` the first time it is asked for: what
    /// the TypeScript kept in module variables, which lived as long as the Claude Code process, as
    /// this engine child does. A `/clear` keeps it; a respawn starts it again. `f` runs under a
    /// lock, so it must not wait on anything: copy out what an `.await` needs.
    pub fn kept<T: Default + Send + 'static, R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut module = self.module.lock().unwrap_or_else(|e| e.into_inner());
        f(module.get::<T>())
    }

    /// The context a timer's run gets: this one on `host`, with no conversation of its own.
    fn on_timer<F, Fut>(&self, work: F) -> TimerWork
    where
        F: Fn(Ctx) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let base = Ctx {
            id: String::new(),
            left: None,
            respawn: false,
            ..self.clone()
        };
        Arc::new(move |host| {
            let cx = Ctx {
                host,
                ..base.clone()
            };
            Box::pin(work(cx))
        })
    }

    /// Run `work` once, `ms` from now (`$.clock.after`). The id cancels it.
    pub fn after<F, Fut>(&self, ms: f64, work: F) -> u64
    where
        F: Fn(Ctx) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.timers.after(ms, self.on_timer(work))
    }

    /// Run `work` every `ms` (`$.clock.every`). The id cancels it.
    pub fn every<F, Fut>(&self, ms: f64, work: F) -> u64
    where
        F: Fn(Ctx) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.timers.every(ms, self.on_timer(work))
    }
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

/// What one feature does for one hook key.
#[derive(Clone)]
pub struct Entry {
    /// The feature module that registered it (`touch`, `carry`, ...), for logs and tests.
    pub feature: &'static str,
    /// `/event`: `e` is the event as Claude Code gave it.
    pub event: Option<Handler>,
    /// `/after`: `e` is what `next` returned.
    pub after: Option<Handler>,
}

/// The entries of every hook key, in the order they run.
///
/// Several features may act in one hook; `app::table()` registers them in the order the
/// TypeScript's handler runs its statement groups. How they compose depends on the hook's shape:
///
/// - Notify: `/event` runs every entry, in order; a failure is logged and the next one still
///   runs. No `/after`.
/// - Gate: `/event` runs the entries in order. The first `answer` ends the chain. A `next` is the
///   event of the entries after it, and the last one goes back. An entry's `left` reaches its own
///   `/after`, which runs every entry in order, each seeing the `result` the one before replaced.
/// - Render, Answer: exactly one entry per key (a second one panics in tests); its reply goes
///   back. No `/after`.
/// - Start, press: every entry, in order.
/// - Stream: no `/event` (the relay posts only `/after`); every entry runs, with `{ e, result }`.
#[derive(Default)]
pub struct Table {
    rows: HashMap<&'static str, Vec<Entry>>,
}

impl Table {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `entry` after the ones `key` has.
    pub fn add(&mut self, key: &'static str, entry: Entry) {
        #[cfg(test)]
        {
            let one = matches!(
                manifest::find(key).map(|hook| hook.shape),
                Some(Shape::Render | Shape::Answer)
            );
            if one && self.rows.get(key).is_some_and(|row| !row.is_empty()) {
                panic!("{key} takes one entry; {} is a second", entry.feature);
            }
        }
        self.rows.entry(key).or_default().push(entry);
    }

    /// An entry that has an `/event` handler only.
    pub fn on(&mut self, key: &'static str, feature: &'static str, event: Handler) {
        self.add(
            key,
            Entry {
                feature,
                event: Some(event),
                after: None,
            },
        );
    }

    /// An entry that has both.
    pub fn gate(
        &mut self,
        key: &'static str,
        feature: &'static str,
        event: Handler,
        after: Handler,
    ) {
        self.add(
            key,
            Entry {
                feature,
                event: Some(event),
                after: Some(after),
            },
        );
    }

    /// An entry that has an `/after` handler only.
    pub fn after(&mut self, key: &'static str, feature: &'static str, after: Handler) {
        self.add(
            key,
            Entry {
                feature,
                event: None,
                after: Some(after),
            },
        );
    }

    /// The entries of `key`, in the order they run.
    pub fn entries(&self, key: &str) -> &[Entry] {
        self.rows.get(key).map_or(&[], Vec::as_slice)
    }

    /// The features that act in `key`, in the order they run.
    pub fn features(&self, key: &str) -> Vec<&'static str> {
        self.entries(key)
            .iter()
            .map(|entry| entry.feature)
            .collect()
    }

    /// Every key with an entry, in no set order.
    pub fn keys(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.rows.keys().copied()
    }
}

/// The key a button press is routed by; it is not a hook, so the manifest does not list it.
pub const PRESS: &str = "press";

/// What the engine does: every feature's entries, in `app::table()`'s order.
pub fn default_table() -> Table {
    crate::companion::app::table()
}

/// Run one entry's handler apart, so a panic in it is one entry's failure, not the chain's.
async fn guarded(
    key: &str,
    feature: &str,
    log: &Log,
    run: &Handler,
    cx: Ctx,
    e: Value,
) -> Option<Reply> {
    let work = tokio::spawn(run(cx, e));
    let failed = match work.await {
        Ok(Ok(reply)) => return Some(reply),
        Ok(Err(error)) => format!("{error:#}"),
        Err(error) => format!("panicked: {error}"),
    };
    log.row(json!({ "k": "error", "for": key, "feature": feature, "error": failed }));
    None
}

/// What each entry of a gate left, as the one value `Session::leave` keeps: `[[index, left]]`.
fn pack(lefts: Vec<(usize, Value)>) -> Option<Value> {
    if lefts.is_empty() {
        return None;
    }
    Some(Value::Array(
        lefts
            .into_iter()
            .map(|(at, left)| json!([at, left]))
            .collect(),
    ))
}

/// What entry number `at` left, out of what `pack` made.
fn unpack(packed: &Option<Value>, at: usize) -> Option<Value> {
    let rows = packed.as_ref()?.as_array()?;
    let row = rows.iter().find(|row| row[0].as_u64() == Some(at as u64))?;
    Some(row[1].clone())
}

/// `/event` for every entry of `key`, by the hook's shape.
async fn chain_event(key: &str, shape: Shape, entries: &[Entry], cx: Ctx, e: Value) -> Reply {
    let mut current = e;
    let mut rewritten = false;
    let mut lefts = Vec::new();
    for (at, entry) in entries.iter().enumerate() {
        let Some(run) = &entry.event else { continue };
        let own = Ctx {
            left: None,
            ..cx.clone()
        };
        let Some(reply) = guarded(key, entry.feature, &cx.log, run, own, current.clone()).await
        else {
            continue;
        };
        if !matches!(shape, Shape::Gate | Shape::Render | Shape::Answer) {
            continue;
        }
        if reply.answer.is_some() || reply.tree.is_some() {
            return reply;
        }
        if let Some(next) = reply.next {
            current = next;
            rewritten = true;
        }
        if let Some(left) = reply.left {
            lefts.push((at, left));
        }
    }
    Reply {
        next: rewritten.then_some(current),
        left: pack(lefts),
        ..Reply::none()
    }
}

/// `/after` for every entry of `key`: each one sees the result the one before replaced.
async fn chain_after(
    key: &str,
    shape: Shape,
    entries: &[Entry],
    cx: Ctx,
    result: Value,
    left: Option<Value>,
) -> Reply {
    let mut current = result;
    let mut replaced = false;
    for (at, entry) in entries.iter().enumerate() {
        let Some(run) = &entry.after else { continue };
        let own = Ctx {
            left: unpack(&left, at),
            ..cx.clone()
        };
        let Some(reply) = guarded(key, entry.feature, &cx.log, run, own, current.clone()).await
        else {
            continue;
        };
        if let (Shape::Gate, Some(result)) = (shape, reply.result) {
            current = result;
            replaced = true;
        }
    }
    Reply {
        result: replaced.then_some(current),
        ..Reply::none()
    }
}

pub struct App {
    pid: u32,
    hub: Arc<Hub>,
    sessions: Mutex<Sessions>,
    store: Arc<Store>,
    clock: Arc<dyn Clock>,
    log: Arc<Log>,
    table: Table,
    events: AtomicU64,
    warned: AtomicBool,
    timers: Arc<dyn Timers>,
    module: Arc<Mutex<Ext>>,
    /// Where the panes of this relay's sessions keep their pid files. Unset: no pane is woken.
    keep: OnceLock<PathBuf>,
}

/// A pane to wake once the hook that calls for it has been handled.
struct Wake {
    keep: PathBuf,
    sid: String,
    kind: wake::Kind,
}
impl Wake {
    fn send(self) {
        wake::notify(&self.keep, &self.sid, self.kind);
    }
}

impl App {
    pub fn new(
        hub: Arc<Hub>,
        store: Arc<Store>,
        clock: Arc<dyn Clock>,
        log: Arc<Log>,
        table: Table,
    ) -> Arc<Self> {
        Arc::new(Self {
            pid: std::process::id(),
            timers: HubTimers::new(hub.clone()),
            module: Arc::default(),
            hub,
            sessions: Mutex::new(Sessions::default()),
            store,
            clock,
            log,
            table,
            events: AtomicU64::new(0),
            warned: AtomicBool::new(false),
            keep: OnceLock::new(),
        })
    }

    /// Wake the panes whose pid files are under `keep` when a hook changes what they show.
    pub fn wake_panes_in(&self, keep: PathBuf) {
        let _ = self.keep.set(keep);
    }

    fn wake_for(&self, path: &str, key: &str, sid: &str) -> Option<Wake> {
        let keep = self.keep.get()?.clone();
        let kind = wake::classify(path, key)?;
        Some(Wake {
            keep,
            sid: sid.to_string(),
            kind,
        })
    }

    /// No conversation with the relay is parked waiting for its next step.
    pub fn idle(&self) -> bool {
        self.hub.waiting() == 0
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
        let (key, mut e) = match path {
            "/press" => (PRESS.to_string(), body.clone()),
            _ => (text("k"), body.get("e").cloned().unwrap_or(Value::Null)),
        };
        // The seat of a call (a fork's `plugin` and `tier`) is on the hook's `next` in Claude Code,
        // not in the event; the relay posts it beside the event when it is set. Handlers read it as
        // `e["origin"]`, unless the event has an origin of its own (a prompt's).
        let seat = body.get("origin").filter(|seat| !seat.is_null());
        let seat = match (path, seat, e.as_object_mut()) {
            ("/event", Some(seat), Some(fields)) if !fields.contains_key("origin") => {
                fields.insert("origin".to_string(), seat.clone());
                Some(seat.clone())
            }
            _ => None,
        };
        let at = Request {
            sid,
            cwd,
            session,
            events,
            respawn: body.get("respawn").and_then(Value::as_bool) == Some(true),
        };
        match path {
            "/event" => without_seat(self.event(&key, e, at).await, seat),
            "/after" => {
                let id = body.get("id").and_then(Value::as_str).unwrap_or("");
                let left = at.session.lock().map_or(None, |mut s| s.take_left(id));
                let result = body.get("result").cloned().unwrap_or(Value::Null);
                // A stream's `/after` names what went in beside what came out.
                let told = match manifest::find(&key).map(|hook| hook.shape) {
                    Some(Shape::Stream) => json!({ "e": e, "result": result }),
                    _ => result,
                };
                self.after(&key, told, left, at).await
            }
            "/press" => self.press(e, at).await,
            _ => json!({}),
        }
    }

    /// The relay's version differs from this engine's: one toast, once. The engine's own version
    /// is the one the binary was built as: a binary replaced under a running relay is another.
    fn check_version(&self, body: &Value) {
        let sent = body.get("v").and_then(Value::as_str);
        if sent != Some(env!("CARGO_PKG_VERSION")) && !self.warned.swap(true, Ordering::Relaxed) {
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
            timers: self.timers.clone(),
            module: self.module.clone(),
        }
    }

    async fn event(&self, key: &str, e: Value, at: Request) -> Value {
        let Some(hook) = manifest::find(key) else {
            return json!({});
        };
        let wake = self.wake_for("/event", key, &at.sid);
        if hook.shape == Shape::Start {
            if let Ok(mut session) = at.session.lock() {
                session.interactive = e.get("isInteractive").and_then(Value::as_bool);
            }
        }
        let entries: Vec<Entry> = (self.table.entries(key).iter())
            .filter(|entry| entry.event.is_some())
            .cloned()
            .collect();
        if entries.is_empty() {
            return json!({});
        }
        if hook.shape == Shape::Notify {
            let cx_of = self.cx_maker(at);
            self.hub.detach(move |host| {
                Box::pin(async move {
                    // One host for the whole chain: its calls go out in the order the entries ran.
                    chain_event(hook.key, hook.shape, &entries, cx_of(host, ""), e).await;
                    if let Some(wake) = wake {
                        wake.send();
                    }
                })
            });
            return json!({});
        }
        let reply = self
            .converse(hook.key, hook.shape, false, entries, e, None, at)
            .await;
        if let Some(wake) = wake {
            wake.send();
        }
        reply
    }

    async fn after(&self, key: &str, result: Value, left: Option<Value>, at: Request) -> Value {
        let Some(hook) = manifest::find(key) else {
            return json!({});
        };
        let entries: Vec<Entry> = self.table.entries(key).to_vec();
        if entries.iter().all(|entry| entry.after.is_none()) {
            return json!({});
        }
        let wake = self.wake_for("/after", key, &at.sid);
        let reply = self
            .converse(hook.key, hook.shape, true, entries, result, left, at)
            .await;
        if let Some(wake) = wake {
            wake.send();
        }
        reply
    }

    async fn press(&self, e: Value, at: Request) -> Value {
        let entries: Vec<Entry> = (self.table.entries(PRESS).iter())
            .filter(|entry| entry.event.is_some())
            .cloned()
            .collect();
        if entries.is_empty() {
            return json!({});
        }
        self.converse(PRESS, Shape::Start, false, entries, e, None, at)
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

    /// Run a key's entries as one conversation: their host calls become `{ id, calls }` replies.
    #[allow(clippy::too_many_arguments)]
    async fn converse(
        &self,
        key: &'static str,
        shape: Shape,
        after: bool,
        entries: Vec<Entry>,
        e: Value,
        left: Option<Value>,
        at: Request,
    ) -> Value {
        let cx_of = self.cx_maker(at);
        self.hub
            .converse(move |host| {
                Box::pin(async move {
                    let id = host.id().to_string();
                    let cx = cx_of(host, &id);
                    let session = cx.session.clone();
                    let reply = if after {
                        chain_after(key, shape, &entries, cx, e, left).await
                    } else {
                        chain_event(key, shape, &entries, cx, e).await
                    };
                    let keep = shape == Shape::Gate && !after && reply.answer.is_none();
                    if let (true, Some(value), Ok(mut session)) =
                        (keep, reply.left.clone(), session.lock())
                    {
                        session.leave(&id, value);
                    }
                    shaped(shape, after, &reply, &id)
                })
            })
            .await
    }
}

/// The reply with the seat the engine added to the event taken out of the `next` it carries: the
/// relay passes `next` on to Claude Code, and the seat was never part of the event.
fn without_seat(mut reply: Value, seat: Option<Value>) -> Value {
    let carried = reply.get_mut("next").and_then(Value::as_object_mut);
    if let (Some(seat), Some(next)) = (seat, carried) {
        if next.get("origin") == Some(&seat) {
            next.remove("origin");
        }
    }
    reply
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

/// A `Ctx` on the scripted `FakeHost`, for feature tests.
#[cfg(test)]
pub mod testing {
    use super::super::host::fake::{FakeClock, FakeHost, World};
    use super::super::timers::fake::FakeTimers;
    use super::*;

    pub struct Fixture {
        pub cx: Ctx,
        pub host: FakeHost,
        pub clock: Arc<FakeClock>,
        /// The context's timers: `timers.advance(ms, host)` fires what falls due.
        pub timers: Arc<FakeTimers>,
        pub dir: tempfile::TempDir,
        _clear: ClearTimers,
    }

    /// Clears the timers when the fixture goes (see `FakeTimers::clear`). A field of its own, not
    /// a `Drop` on `Fixture`, so a test may still take the fixture's parts one by one.
    pub struct ClearTimers(Arc<super::super::timers::fake::FakeTimers>);

    impl Drop for ClearTimers {
        fn drop(&mut self) {
            self.0.clear();
        }
    }

    /// A context in session `s` at `/repo`, with a store in a temp folder of its own.
    pub fn fixture(world: World) -> Fixture {
        fixture_in(world, "s")
    }

    /// The same, in session `sid`.
    pub fn fixture_in(world: World, sid: &str) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let host = FakeHost::new(world);
        let clock = host.clock.clone();
        let session = Sessions::default().touch(sid, "/repo", clock.now()).0;
        session.lock().unwrap().interactive = Some(true);
        let timers = FakeTimers::new(clock.clone());
        let cx = Ctx {
            host: Arc::new(host.clone()),
            session,
            store: Arc::new(Store::at(dir.path().join("csr-companion/companion.db"))),
            clock: clock.clone(),
            log: Arc::new(Log::default()),
            sid: sid.into(),
            cwd: "/repo".into(),
            id: "e1".into(),
            left: None,
            respawn: false,
            pid: 1,
            events: 1,
            timers: timers.clone(),
            module: Arc::default(),
        };
        Fixture {
            cx,
            host,
            clock,
            _clear: ClearTimers(timers.clone()),
            timers,
            dir,
        }
    }

    /// `/event` for `key` through the entries `table` holds, as the app runs them; a key that is
    /// not a hook (a press) runs as a start does.
    pub async fn run(table: &Table, key: &str, cx: &Ctx, e: Value) -> Reply {
        let shape = manifest::find(key).map_or(Shape::Start, |hook| hook.shape);
        chain_event(key, shape, table.entries(key), cx.clone(), e).await
    }

    /// Let tasks the runtime holds run until `done` holds, or give up after a second.
    pub async fn until(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..1000 {
            if done() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        done()
    }
}

#[cfg(test)]
mod tests {
    use super::super::host::fake::FakeClock;
    use super::*;
    use crate::companion::app::guard::{self, Machine, Ran};
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

    /// A machine with nothing on it: no HOME, no files, no git. The guard asked of it finds no
    /// guard, so no test that runs the guard entry reads the person's own guard.json.
    struct Nowhere;

    impl Machine for Nowhere {
        fn home(&self) -> Option<String> {
            None
        }
        fn exists(&self, _: &str) -> bool {
            false
        }
        fn read(&self, _: &str) -> Option<String> {
            None
        }
        fn write(&self, _: &str, _: &str) -> bool {
            false
        }
        fn keep(&self, _: &str, _: &str) -> bool {
            false
        }
        fn variable(&self, _: &str) -> Option<String> {
            None
        }
        fn git(&self, _: &str, _: &[String], _: &[(String, String)], _: &[&str]) -> Option<Ran> {
            None
        }
        fn coach(&self, _: f64) -> Vec<Value> {
            Vec::new()
        }
    }

    /// The two entries of the `start` the engine answers a session with: the `/csr` command and the
    /// usage call. Nothing here reads or writes the real HOME, as the rest of the table does.
    fn the_start() -> Table {
        let mut t = Table::new();
        crate::companion::app::pane_cmd::register(&mut t);
        crate::companion::app::usage::register(&mut t);
        t
    }

    /// Let the tasks the runtime holds run until they wait again.
    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    fn table(rows: Vec<(&'static str, Entry)>) -> Table {
        let mut table = Table::new();
        for (key, entry) in rows {
            table.add(key, entry);
        }
        table
    }

    fn event(h: Handler) -> Entry {
        Entry {
            feature: "test",
            event: Some(h),
            after: None,
        }
    }

    async fn post(app: &Arc<App>, path: &str, body: Value) -> Value {
        let mut body = body;
        body["v"] = json!(env!("CARGO_PKG_VERSION"));
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
                feature: "test",
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
        // The guard's own entry, on a machine with nothing on it: `default_table()` would read the
        // person's guard.json, and write pane-errors.log beside it when that file is malformed.
        let mut pretend = Table::new();
        guard::register_with(&mut pretend, Arc::new(Nowhere));
        let mut rig = rig(pretend);
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
        let mut rig = rig(the_start());
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
        let mut rig = rig(the_start());
        let mut body = ev("s1", "start", json!({ "isInteractive": false }));
        body["respawn"] = json!(true);
        assert_eq!(post(&rig.app, "/event", body).await, json!({}));
        assert!(rig.lines.try_recv().is_err());
        let session = rig.app.sessions.lock().unwrap().get("s1").unwrap();
        assert_eq!(session.lock().unwrap().interactive, Some(false));
    }

    #[tokio::test]
    async fn a_step_for_an_unknown_id_replies_empty() {
        let rig = rig(Table::new());
        for body in [
            json!({ "id": "e99", "results": [] }),
            json!({ "results": [] }),
            json!({ "id": 5 }),
        ] {
            assert_eq!(post(&rig.app, "/step", body).await, json!({}));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_conversation_with_no_step_in_eighty_seconds_is_dropped_and_its_handler_errs() {
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
        tokio::time::advance(Duration::from_secs(79)).await;
        settle().await;
        assert!(
            outcome.lock().unwrap().is_none(),
            "79 s is inside the limit"
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
    async fn kept_state_outlives_a_clear_and_a_sessions_own_state_does_not() {
        #[derive(Default)]
        struct Count(u32);
        let rig = rig(table(vec![(
            "tool",
            event(handler(|cx, _| async move {
                let process = cx.kept(|c: &mut Count| {
                    c.0 += 1;
                    c.0
                });
                let own = cx.session.lock().map_or(0, |mut s| {
                    let c = s.ext::<Count>();
                    c.0 += 1;
                    c.0
                });
                Ok(Reply::answer(json!([process, own])))
            })),
        )]));
        let mut said = Vec::new();
        for sid in ["before", "before", "after"] {
            said.push(post(&rig.app, "/event", ev(sid, "tool", json!({}))).await["answer"].clone());
        }
        assert_eq!(said, vec![json!([1, 1]), json!([2, 2]), json!([3, 1])]);
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
        let mut pretend = Table::new();
        crate::companion::app::pane_cmd::register(&mut pretend);
        let rig = rig(pretend);
        post(&rig.app, "/event", ev("s", "tool", json!({}))).await;
        let reply = post(&rig.app, "/event", ev("s", "csr", json!({}))).await;
        let pid = std::process::id();
        assert_eq!(
            reply["answer"]["text"],
            format!("csr relay ok pid={pid} events=2")
        );
    }

    #[tokio::test]
    async fn a_request_of_another_version_than_the_engine_s_toasts_once_and_its_own_never() {
        let quiet = || {
            table(vec![(
                "tool",
                event(handler(|_, _| async { Ok(Reply::none()) })),
            )])
        };
        let mut old = rig(quiet());
        let stale = json!({ "v": "0", "sid": "s", "k": "tool" });
        old.app.handle("/event", &stale).await;
        old.app.handle("/event", &stale).await;
        let line: Value = serde_json::from_str(&old.lines.recv().await.unwrap()).unwrap();
        assert_eq!(line["calls"][0]["m"], "ui.toast");
        assert_eq!(line["calls"][0]["a"][0], TOAST);
        settle().await;
        assert!(old.lines.try_recv().is_err());
        // The relay and the child come from one file, so they always agree with each other: what
        // counts is this engine's own version.
        let mut same = rig(quiet());
        let own = json!({ "v": env!("CARGO_PKG_VERSION"), "sid": "s", "k": "tool" });
        same.app.handle("/event", &own).await;
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

    // The registry: several features in one hook, on the scripted FakeHost.

    use super::super::host::fake::World;
    use super::testing::fixture;

    fn entry(feature: &'static str, event: Option<Handler>, after: Option<Handler>) -> Entry {
        Entry {
            feature,
            event,
            after,
        }
    }

    /// An `/event` handler that records its feature and the event it saw into `trail`.
    fn noting(trail: &Arc<Mutex<Vec<String>>>, feature: &'static str, reply: Reply) -> Handler {
        let trail = trail.clone();
        handler(move |_, e| {
            let (trail, reply) = (trail.clone(), reply.clone());
            async move {
                trail.lock().unwrap().push(format!("{feature} saw {e}"));
                Ok(reply)
            }
        })
    }

    fn entries_of(table: &Table, key: &str) -> Vec<Entry> {
        table.entries(key).to_vec()
    }

    #[tokio::test]
    async fn the_first_gate_entry_to_answer_ends_the_chain() {
        let trail = Arc::new(Mutex::new(Vec::new()));
        let mut t = Table::new();
        t.on("guard", "a", noting(&trail, "a", Reply::none()));
        t.on(
            "guard",
            "b",
            noting(&trail, "b", Reply::answer(json!({ "deny": "b" }))),
        );
        t.on(
            "guard",
            "c",
            noting(&trail, "c", Reply::answer(json!({ "deny": "c" }))),
        );
        let fx = fixture(World::default());
        let reply = chain_event(
            "guard",
            Shape::Gate,
            &entries_of(&t, "guard"),
            fx.cx,
            json!(1),
        )
        .await;
        assert_eq!(reply.answer, Some(json!({ "deny": "b" })));
        assert_eq!(*trail.lock().unwrap(), ["a saw 1", "b saw 1"]);
        assert_eq!(t.features("guard"), ["a", "b", "c"]);
    }

    #[tokio::test]
    async fn a_rewrite_becomes_the_event_of_the_entries_after_it_and_the_last_one_goes_back() {
        let trail = Arc::new(Mutex::new(Vec::new()));
        let mut t = Table::new();
        t.on(
            "tool",
            "a",
            noting(&trail, "a", Reply::next(json!({ "n": 2 }))),
        );
        t.on("tool", "b", noting(&trail, "b", Reply::none()));
        t.on(
            "tool",
            "c",
            noting(&trail, "c", Reply::next(json!({ "n": 3 }))),
        );
        let fx = fixture(World::default());
        let reply = chain_event(
            "tool",
            Shape::Gate,
            &entries_of(&t, "tool"),
            fx.cx,
            json!({ "n": 1 }),
        )
        .await;
        assert_eq!(reply.next, Some(json!({ "n": 3 })));
        assert_eq!(
            *trail.lock().unwrap(),
            [r#"a saw {"n":1}"#, r#"b saw {"n":2}"#, r#"c saw {"n":2}"#]
        );
        let quiet = table(vec![("tool", event(noting(&trail, "q", Reply::none())))]);
        let fx = fixture(World::default());
        let reply = chain_event("tool", Shape::Gate, quiet.entries("tool"), fx.cx, json!(1)).await;
        assert_eq!(reply.next, None, "no rewrite, no next");
    }

    #[tokio::test]
    async fn each_entrys_left_reaches_its_own_after_and_no_other() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let leaves = |feature: &'static str, left: Option<Value>| {
            handler(move |_, _| {
                let left = left.clone();
                async move {
                    let _ = feature;
                    Ok(Reply {
                        left,
                        ..Reply::none()
                    })
                }
            })
        };
        let reads = |feature: &'static str| {
            let seen = seen.clone();
            handler(move |cx, _| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push((feature, cx.left.clone()));
                    Ok(Reply::none())
                }
            })
        };
        let mut t = Table::new();
        t.add(
            "tool",
            entry(
                "a",
                Some(leaves("a", Some(json!("from a")))),
                Some(reads("a")),
            ),
        );
        t.add(
            "tool",
            entry("b", Some(leaves("b", None)), Some(reads("b"))),
        );
        t.add(
            "tool",
            entry(
                "c",
                Some(leaves("c", Some(json!({ "c": 1 })))),
                Some(reads("c")),
            ),
        );
        let fx = fixture(World::default());
        let entries = entries_of(&t, "tool");
        let first = chain_event("tool", Shape::Gate, &entries, fx.cx.clone(), json!({})).await;
        assert_eq!(shaped(Shape::Gate, false, &first, "e7")["id"], "e7");
        chain_after("tool", Shape::Gate, &entries, fx.cx, json!({}), first.left).await;
        assert_eq!(
            *seen.lock().unwrap(),
            [
                ("a", Some(json!("from a"))),
                ("b", None),
                ("c", Some(json!({ "c": 1 })))
            ]
        );
    }

    #[tokio::test]
    async fn after_runs_every_entry_in_order_and_each_sees_the_result_the_one_before_replaced() {
        let trail = Arc::new(Mutex::new(Vec::new()));
        let replacing = |feature: &'static str, with: Option<Value>| {
            let trail = trail.clone();
            handler(move |_, result| {
                let (trail, with) = (trail.clone(), with.clone());
                async move {
                    trail
                        .lock()
                        .unwrap()
                        .push(format!("{feature} saw {result}"));
                    Ok(Reply {
                        result: with,
                        ..Reply::none()
                    })
                }
            })
        };
        let mut t = Table::new();
        t.after("tool", "a", replacing("a", Some(json!("A"))));
        t.after("tool", "b", replacing("b", None));
        t.after("tool", "c", replacing("c", Some(json!("C"))));
        t.after("tool", "d", replacing("d", None));
        let fx = fixture(World::default());
        let reply = chain_after(
            "tool",
            Shape::Gate,
            &entries_of(&t, "tool"),
            fx.cx,
            json!("raw"),
            None,
        )
        .await;
        assert_eq!(reply.result, Some(json!("C")));
        assert_eq!(
            *trail.lock().unwrap(),
            [
                r#"a saw "raw""#,
                r#"b saw "A""#,
                r#"c saw "A""#,
                r#"d saw "C""#
            ]
        );
    }

    #[tokio::test]
    async fn a_failing_or_panicking_notify_entry_does_not_stop_the_next_one() {
        let mut t = Table::new();
        t.on(
            "stop",
            "fails",
            handler(|_, _| async { anyhow::bail!("boom") }),
        );
        t.on("stop", "panics", handler(|_, _| async { panic!("worse") }));
        t.on(
            "stop",
            "last",
            handler(|cx, _| async move {
                cx.host.call("ui.invalidate", vec![]).await?;
                Ok(Reply::none())
            }),
        );
        let fx = fixture(World::default());
        chain_event(
            "stop",
            Shape::Notify,
            &entries_of(&t, "stop"),
            fx.cx,
            json!({}),
        )
        .await;
        assert_eq!(fx.host.seen.lock().unwrap().invalidated, 1);
    }

    #[tokio::test]
    async fn a_notify_chain_through_the_app_runs_one_conversation_in_order() {
        let mut t = Table::new();
        t.on(
            "stop",
            "fails",
            handler(|_, _| async { anyhow::bail!("boom") }),
        );
        for (feature, method) in [("one", "ui.invalidate"), ("two", "ui.toast")] {
            t.on(
                "stop",
                feature,
                handler(move |cx, _| async move {
                    cx.host.call(method, vec![]).await?;
                    Ok(Reply::none())
                }),
            );
        }
        let mut rig = rig(t);
        assert_eq!(
            post(&rig.app, "/event", ev("s", "stop", json!({}))).await,
            json!({})
        );
        let first: Value = serde_json::from_str(&rig.lines.recv().await.unwrap()).unwrap();
        assert_eq!(first["calls"][0]["m"], "ui.invalidate");
        let id = first["id"].clone();
        let next = post(
            &rig.app,
            "/step",
            json!({ "id": id, "results": [{ "id": "c1", "v": null }] }),
        )
        .await;
        assert_eq!(next["calls"][0]["m"], "ui.toast");
        assert_eq!(next["id"], id, "the same conversation goes on");
    }

    #[tokio::test]
    async fn a_start_runs_every_entry_and_a_stream_after_names_what_went_in_and_came_out() {
        let trail = Arc::new(Mutex::new(Vec::new()));
        let mut t = Table::new();
        for feature in ["a", "b"] {
            t.on(
                "start",
                feature,
                noting(&trail, feature, Reply::answer(json!("x"))),
            );
            t.after("step", feature, noting(&trail, feature, Reply::none()));
        }
        let rig = rig(t);
        let started = post(
            &rig.app,
            "/event",
            ev("s", "start", json!({ "isInteractive": true })),
        )
        .await;
        assert_eq!(started, json!({}), "a start answers nothing");
        let body = json!({ "sid": "s", "k": "step", "e": { "n": 1 }, "result": { "m": 2 } });
        assert_eq!(post(&rig.app, "/after", body).await, json!({}));
        let trail = trail.lock().unwrap();
        assert_eq!(
            trail[..2],
            [
                r#"a saw {"isInteractive":true}"#,
                r#"b saw {"isInteractive":true}"#
            ]
        );
        assert_eq!(trail[2], r#"a saw {"e":{"n":1},"result":{"m":2}}"#);
        assert_eq!(trail.len(), 4);
    }

    #[tokio::test]
    async fn the_seat_the_relay_forwards_reaches_the_handler_in_the_event_and_not_the_next() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let spy = seen.clone();
        let rig = rig(table(vec![(
            "fork",
            event(handler(move |_, e| {
                let spy = spy.clone();
                async move {
                    spy.lock().unwrap().push(e["origin"].clone());
                    // A handler that rewrites the event hands the whole of it on.
                    Ok(Reply {
                        next: Some(e),
                        ..Reply::none()
                    })
                }
            })),
        )]));
        let seat = json!({ "plugin": "you-should-know", "tier": "builtin" });
        let mut body = ev("s", "fork", json!({ "prompt": "p" }));
        body["origin"] = seat.clone();
        let reply = post(&rig.app, "/event", body).await;
        assert_eq!(seen.lock().unwrap()[0], seat, "the handler reads e.origin");
        assert_eq!(
            reply["next"],
            json!({ "prompt": "p" }),
            "the seat is not passed on to Claude Code"
        );
        // No seat forwarded: the event is as the relay posted it.
        let reply = post(
            &rig.app,
            "/event",
            ev("s", "fork", json!({ "prompt": "p" })),
        )
        .await;
        assert_eq!(seen.lock().unwrap()[1], Value::Null);
        assert_eq!(reply["next"], json!({ "prompt": "p" }));
        // An event with an origin of its own keeps it.
        let mut body = ev("s", "fork", json!({ "origin": { "kind": "human" } }));
        body["origin"] = seat;
        let reply = post(&rig.app, "/event", body).await;
        assert_eq!(seen.lock().unwrap()[2], json!({ "kind": "human" }));
        assert_eq!(reply["next"], json!({ "origin": { "kind": "human" } }));
    }

    #[test]
    #[should_panic(expected = "takes one entry")]
    fn a_second_entry_for_a_render_key_panics() {
        let mut t = Table::new();
        t.on("band", "a", handler(|_, _| async { Ok(Reply::none()) }));
        t.on("band", "b", handler(|_, _| async { Ok(Reply::none()) }));
    }

    #[test]
    #[should_panic(expected = "takes one entry")]
    fn a_second_entry_for_an_answer_key_panics() {
        let mut t = Table::new();
        t.on("csr", "a", handler(|_, _| async { Ok(Reply::none()) }));
        t.on("csr", "b", handler(|_, _| async { Ok(Reply::none()) }));
    }

    #[test]
    fn the_app_table_keeps_the_wp0_start_and_csr_as_the_first_entries() {
        let t = default_table();
        assert_eq!(t.features("start")[0], "pane_cmd");
        assert_eq!(t.features("csr"), ["pane_cmd"]);
        assert_eq!(t.features("tool")[0], "touch");
    }
    /// A pane listening at the pipe the relay writes to.
    fn listening_pane(keep: &std::path::Path, sid: &str) -> wake::Listener {
        std::fs::create_dir_all(keep.join("panes")).unwrap();
        wake::Listener::open(keep, sid).unwrap()
    }

    /// The wakes that arrive within a second, polled as the pane's loop polls.
    async fn woken(pane: &mut wake::Listener) -> (bool, bool) {
        let mut got = (false, false);
        for _ in 0..500 {
            let (now, soon) = pane.take();
            got = (got.0 | now, got.1 | soon);
            if got != (false, false) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        got
    }

    #[tokio::test]
    async fn a_hook_that_changes_the_pane_wakes_it_once_the_handler_has_run_and_the_rest_do_not() {
        let ran = Arc::new(AtomicBool::new(false));
        let mark = ran.clone();
        let mut t = Table::new();
        t.on(
            "stop",
            "test",
            handler(move |_, _| {
                let mark = mark.clone();
                async move {
                    mark.store(true, Ordering::SeqCst);
                    Ok(Reply::none())
                }
            }),
        );
        t.gate(
            "tool",
            "test",
            handler(|_, _| async { Ok(Reply::none()) }),
            handler(|_, _| async { Ok(Reply::none()) }),
        );
        let rig = rig(t);
        let keep = rig._dir.path().join("keep");
        rig.app.wake_panes_in(keep.clone());
        let mut pane = listening_pane(&keep, "s");
        // Before a tool runs there is nothing new to show.
        post(&rig.app, "/event", ev("s", "tool", json!({}))).await;
        settle().await;
        assert_eq!(pane.take(), (false, false), "woken before the tool ran");
        // Another session's hook does not reach this pane.
        post(&rig.app, "/event", ev("other", "stop", json!({}))).await;
        settle().await;
        assert_eq!(pane.take(), (false, false), "woken by another session");
        post(&rig.app, "/event", ev("s", "stop", json!({}))).await;
        assert_eq!(woken(&mut pane).await, (true, false), "Stop wakes the pane");
        assert!(
            ran.load(Ordering::SeqCst),
            "the wake came after the handler ran"
        );
        // After a tool: the burst wake.
        let after = json!({ "sid": "s", "cwd": "/repo", "k": "tool", "id": "", "result": {} });
        post(&rig.app, "/after", after).await;
        assert_eq!(
            woken(&mut pane).await,
            (false, true),
            "a finished tool wakes the pane, soon"
        );
    }

    #[tokio::test]
    async fn a_relay_without_a_keep_directory_wakes_nobody() {
        let mut t = Table::new();
        t.on("stop", "test", handler(|_, _| async { Ok(Reply::none()) }));
        let rig = rig(t);
        let mut pane = listening_pane(&rig._dir.path().join("keep"), "s");
        post(&rig.app, "/event", ev("s", "stop", json!({}))).await;
        settle().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(pane.take(), (false, false));
    }
    /// The wake only fires where a handler is dispatched, so every hook that wakes a pane must have
    /// an entry of the shape `classify` expects in the table the relay really runs.
    #[test]
    fn every_hook_that_wakes_a_pane_has_the_entry_the_wake_rides_on() {
        let t = default_table();
        let keys = [
            ("/event", "stop"),
            ("/event", "turn_start"),
            ("/event", "turn_complete"),
            ("/event", "seen"),
            ("/event", "digit"),
            ("/event", "chapter"),
            ("/event", "marker"),
            ("/event", "measure"),
            ("/after", "spawned"),
            ("/after", "tool"),
        ];
        for (path, key) in keys {
            assert!(wake::classify(path, key).is_some(), "{path} {key}");
            let entries = t.entries(key);
            let has = |after: bool| {
                entries.iter().any(|e| {
                    if after {
                        e.after.is_some()
                    } else {
                        e.event.is_some()
                    }
                })
            };
            assert!(has(path == "/after"), "{key} has no {path} entry");
        }
        // And nothing else wakes a pane by accident.
        for hook in manifest::HOOKS {
            for path in ["/event", "/after"] {
                if wake::classify(path, hook.key).is_some() {
                    assert!(keys.contains(&(path, hook.key)), "{path} {}", hook.key);
                }
            }
        }
    }
}
