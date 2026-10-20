//! The scripted Claude Code and the fake outside: the twin of `world()` in
//! the removed `compact.test.ts`. `TestHost` answers `$` (usage, config, compaction, append, log),
//! `FakeOutside` what the TypeScript asked of `process.run`, `fs` and `env`, and `Kit` drives the
//! compactor's entries the way the relay does.

use crate::companion::{
    app::compactor::{
        compact_band,
        flow::Out,
        on_press_with,
        outside::{Outside, Redact},
        register_with,
        rules::Running,
    },
    relay::{
        dispatch::{testing::fixture_in, Ctx, Table},
        host::{
            fake::{FakeClock, World as FakeWorld},
            Call, Detached, Host,
        },
        manifest,
        timers::fake::FakeTimers,
    },
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

/// `Date.UTC(2026, 9, 12, 12)`.
pub const NOW: f64 = 1_791_806_400_000.;
pub const HOME: &str = "/home/u";
pub const PLAN_FILE: &str = "/home/u/.claude-self-reflect/csr-companion/compact-plan/sess-1.json";
pub const MARKER: &str = "/home/u/.claude-self-reflect/csr-companion/compacting/sess-1.json";
pub const LANDED: &str = "Done. Codex committed 6369e6b on the study branch; the gates are green.";
pub const DREAMER_SAID: &str = "CSR's dreamer asked this while you worked";
pub const AUTO_KEY: &str = "csr-companion.autoCompact";
pub const GO_ON: &str = "go on";
pub const KEEP_GOING: &str = "keep going";

pub fn agent() -> Running {
    Running::new("subagent", "a427c126", "Explore CapCut draft automation")
}

pub fn some(text: &str) -> Option<String> {
    Some(text.to_string())
}

pub fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

pub fn width(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The Stop hook's row for a task, as Claude Code lists it.
pub fn task_row(task: &Running) -> Value {
    json!({ "id": task.id, "type": task.kind, "status": "running",
        "description": task.description })
}

/// `askClaude` of the removed `dreamer.ts` (the band's fill, forward grammar v2): the question in
/// its quotes, the attribution, the reasoning quoted. Its `cleanForDraft` changes nothing in the
/// texts these tests send (no stray surrogate, invisible character, `@`, paste marker or
/// `ultra` word), so it is left out; the dreamer's own port is not in this module.
pub fn ask_claude(line: &str, title: &str, text: &str) -> String {
    let straight = crate::companion::send::straight;
    let quoted = text.split('\n').map(|row| {
        if row.trim().is_empty() {
            ">".to_string()
        } else {
            format!("> {}", row.replace(['“', '”'], "\""))
        }
    });
    let mut rows = vec![
        format!("“{}”", straight(line)),
        String::new(),
        format!("{DREAMER_SAID}, quoted. Its reasoning:"),
        String::new(),
        format!("> **{}**", straight(title)),
    ];
    rows.extend(quoted);
    rows.join("\n")
}

pub fn forward() -> String {
    ask_claude(
        "What if we migrated the queue first?",
        "Migrate the queue first",
        "* We must disable retries before migrating.\n* Never run both queues at once.",
    )
}

/// Whether every key of `want` is in `actual` with an equal value (`expect.objectContaining`).
pub fn has(actual: &Value, want: Value) -> bool {
    let Some(want) = want.as_object() else {
        return same(actual, &want);
    };
    want.iter()
        .all(|(key, value)| actual.get(key).is_some_and(|got| same(got, value)))
}

/// Deep equality where numbers compare by value (`17` is `17.0`).
pub fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

// ---------------------------------------------------------------- the world

pub type Said = Arc<dyn Fn(&Value) -> String + Send + Sync>;
pub type Fails = Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;

/// What the test's Claude Code and outside do (the TypeScript's `World`, and the setting).
#[derive(Clone)]
pub struct Setup {
    pub ctx: f64,
    /// The `/config` row's value: `auto`, `suggest`, `off`; none for a person who never chose.
    pub setting: Option<&'static str>,
    pub headless: bool,
    /// What the removed `pane.py --compact-plan` prints: none for the usual plan, null for no
    /// answer.
    pub plan: Option<Value>,
    pub lost: Vec<Value>,
    /// What `git -C dir <args>` prints, by the args.
    pub git: Vec<(&'static str, &'static str)>,
    /// A hook vetoes the compaction.
    pub skip: Option<&'static str>,
    pub compact_fails: bool,
    /// What a tool call answers.
    pub said: Option<Said>,
    /// The engine's private writer fails for these.
    pub private_fails: Option<Fails>,
    /// `rm` takes the file away (it did not in the TypeScript's `world()`).
    pub rm_removes: bool,
}

impl Setup {
    /// The tests that drive the engine are about auto, which a person opts into; suggest and off
    /// say so themselves.
    pub fn auto(ctx: f64) -> Self {
        Self {
            ctx,
            setting: Some("auto"),
            headless: false,
            plan: None,
            lost: Vec::new(),
            git: Vec::new(),
            skip: None,
            compact_fails: false,
            said: None,
            private_fails: None,
            rm_removes: false,
        }
    }
}

pub fn usual_plan(ctx: usize) -> Value {
    json!({ "ctx": ctx, "floor": 65_000, "measured": true, "payback": 8.2, "per_turn": 17,
        "worth": true, "trend": 20_000, "story": null })
}

#[derive(Default)]
struct HostState {
    ctx: f64,
    setting: Option<&'static str>,
    headless: bool,
    skip: Option<&'static str>,
    compact_fails: bool,
    compacted: Vec<Value>,
    appended: Vec<String>,
    logged: Vec<String>,
    invalidated: usize,
}

/// `$` as the TypeScript's `world()` answered it.
#[derive(Clone)]
pub struct TestHost {
    state: Arc<Mutex<HostState>>,
}

impl TestHost {
    fn new(setup: &Setup) -> Self {
        Self {
            state: Arc::new(Mutex::new(HostState {
                ctx: setup.ctx,
                setting: setup.setting,
                headless: setup.headless,
                skip: setup.skip,
                compact_fails: setup.compact_fails,
                ..HostState::default()
            })),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, HostState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn answer(&self, call: &Call) -> Result<Value, String> {
        let mut s = self.state();
        let first = call.a.first().cloned().unwrap_or(Value::Null);
        match call.m.as_str() {
            "session.usage" => Ok(json!({ "startedAt": 0, "rateLimits": [], "context": {
                "tokens": s.ctx as u64, "window": 1_000_000, "percent": (s.ctx / 10_000.).round() } })),
            "config.list" => Ok(json!([{ "key": AUTO_KEY, "kind": "enum", "value": s.setting }])),
            "session.compact" => {
                s.compacted.push(first);
                if s.compact_fails {
                    return Err("summary failed".into());
                }
                if let Some(skip) = s.skip {
                    return Ok(json!({ "skip": skip }));
                }
                Ok(json!({ "tokensBefore": s.ctx as u64, "tokensAfter": 21_000 }))
            }
            "session.append" => {
                let text = first["message"]["content"][0]["text"]
                    .as_str()
                    .unwrap_or("");
                s.appended.push(text.to_string());
                Ok(json!({ "appended": true }))
            }
            "session.surfaces" => Ok(json!(if s.headless { vec![] } else { vec!["terminal"] })),
            "ui.log" => {
                s.logged.push(first.as_str().unwrap_or("").to_string());
                Ok(Value::Null)
            }
            "ui.invalidate" => {
                s.invalidated += 1;
                Ok(Value::Null)
            }
            _ => Ok(Value::Null),
        }
    }
}

#[async_trait]
impl Host for TestHost {
    async fn batch(&self, calls: Vec<Call>) -> Result<Vec<Result<Value, String>>> {
        Ok(calls.iter().map(|call| self.answer(call)).collect())
    }

    fn detach(&self, work: Detached) {
        tokio::spawn(work(Arc::new(self.clone())));
    }
}

/// Where a test holds the compactor until it lets go (the TypeScript's `hold` and `holdHome`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    /// The log of the plan.
    Plan,
    /// `git status`, in the keep list.
    GitStatus,
    /// The HOME lookup, once the plan is logged.
    Home,
}

pub struct Hold {
    at: Point,
    paused: AtomicBool,
    gate: tokio::sync::Semaphore,
}

impl Hold {
    pub fn at(at: Point) -> Arc<Self> {
        Arc::new(Self {
            at,
            paused: AtomicBool::new(false),
            gate: tokio::sync::Semaphore::new(0),
        })
    }

    async fn wait(&self, at: Point) {
        if self.at == at && !self.paused.swap(true, Ordering::SeqCst) {
            if let Ok(permit) = self.gate.acquire().await {
                permit.forget();
            }
        }
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub fn release(&self) {
        self.gate.add_permits(1);
    }
}

/// `process.run`, `fs` and `env.get`, answered from the test's world.
pub struct FakeOutside {
    setup: Setup,
    pub kept: Mutex<Vec<Value>>,
    /// `(sid, ctx)` of each call of the removed `pane.py --compact-plan`.
    pub plans: Mutex<Vec<(String, usize)>>,
    pub lost_asked: Mutex<Vec<(String, String)>>,
    /// The args of each `git`.
    pub gits: Mutex<Vec<String>>,
    pub files: Mutex<HashMap<String, String>>,
    /// The path of each write tried, those that failed too.
    pub writes: Mutex<Vec<String>>,
    pub hold: Mutex<Option<Arc<Hold>>>,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl FakeOutside {
    pub fn new(setup: Setup) -> Self {
        Self {
            setup,
            kept: Mutex::default(),
            plans: Mutex::default(),
            lost_asked: Mutex::default(),
            gits: Mutex::default(),
            files: Mutex::default(),
            writes: Mutex::default(),
            hold: Mutex::default(),
        }
    }

    /// The rows of the dream log of kind `compact`.
    pub fn compacts(&self) -> Vec<Value> {
        let kept = locked(&self.kept);
        kept.iter()
            .filter(|row| row["kind"] == "compact")
            .cloned()
            .collect()
    }

    pub fn file(&self, path: &str) -> Option<String> {
        locked(&self.files).get(path).cloned()
    }

    fn held(&self) -> Option<Arc<Hold>> {
        locked(&self.hold).clone()
    }
}

#[async_trait]
impl Outside for FakeOutside {
    async fn plan(&self, sid: &str, ctx: usize) -> Option<Value> {
        locked(&self.plans).push((sid.to_string(), ctx));
        match &self.setup.plan {
            Some(Value::Null) => None,
            Some(plan) => Some(plan.clone()),
            None => Some(usual_plan(ctx)),
        }
    }

    async fn keep(&self, _cwd: &str, record: Value) {
        if record["event"] == "plan" {
            if let Some(hold) = self.held() {
                hold.wait(Point::Plan).await;
            }
        }
        locked(&self.kept).push(record);
    }

    async fn lost(&self, cwd: &str, sid: &str) -> Option<Value> {
        locked(&self.lost_asked).push((cwd.to_string(), sid.to_string()));
        Some(Value::Array(self.setup.lost.clone()))
    }

    async fn git(&self, _cwd: &str, args: &[&str]) -> Option<String> {
        let key = args.join(" ");
        locked(&self.gits).push(key.clone());
        if args.contains(&"status") {
            if let Some(hold) = self.held() {
                hold.wait(Point::GitStatus).await;
            }
        }
        let said = self.setup.git.iter().find(|(asked, _)| *asked == key);
        said.map(|(_, text)| text.to_string())
    }

    async fn home(&self) -> Option<String> {
        if self.compacts().iter().any(|row| row["event"] == "plan") {
            if let Some(hold) = self.held() {
                hold.wait(Point::Home).await;
            }
        }
        Some(HOME.to_string())
    }

    async fn write(&self, path: &str, text: &str, _redact: Redact) -> Result<()> {
        locked(&self.writes).push(path.to_string());
        if self
            .setup
            .private_fails
            .as_ref()
            .is_some_and(|fails| fails(path, text))
        {
            return Err(anyhow!("engine private write exited 1"));
        }
        locked(&self.files).insert(path.to_string(), text.to_string());
        Ok(())
    }

    async fn remove(&self, path: &str) {
        if self.setup.rm_removes {
            locked(&self.files).remove(path);
        }
    }

    async fn exists(&self, path: &str) -> bool {
        locked(&self.files).contains_key(path)
    }
}

// ---------------------------------------------------------------- the kit

/// What the band shows: its text lines and the labels of its buttons.
pub struct Band {
    pub texts: Vec<String>,
    pub labels: Vec<String>,
}

impl Band {
    fn of(tree: &Value, into: &mut Band) {
        if tree["type"] == "Button" {
            if let Some(label) = tree["props"]["label"].as_str() {
                into.labels.push(label.to_string());
            }
        }
        for child in tree["children"].as_array().into_iter().flatten() {
            match child {
                Value::String(text) => into.texts.push(text.clone()),
                node => Band::of(node, into),
            }
        }
    }

    pub fn has(&self, text: &str) -> bool {
        self.texts.iter().any(|shown| shown == text)
    }

    pub fn has_like(&self, pattern: &str) -> bool {
        let re = regex::Regex::new(pattern).unwrap();
        self.texts.iter().any(|shown| re.is_match(shown))
    }

    pub fn has_button(&self, label: &str) -> bool {
        self.labels.iter().any(|shown| shown == label)
    }
}

#[derive(Clone)]
pub struct Kit {
    pub cx: Ctx,
    pub table: Arc<Table>,
    pub out: Arc<FakeOutside>,
    pub host: TestHost,
    pub timers: Arc<FakeTimers>,
    pub clock: Arc<FakeClock>,
    said: Option<Said>,
    /// The marker file as each `session.compact` ran.
    pub markers: Arc<Mutex<Vec<Option<String>>>>,
    _dir: Arc<tempfile::TempDir>,
}

impl Kit {
    pub fn new(setup: Setup) -> Self {
        let fx = fixture_in(FakeWorld::default(), "sess-1");
        fx.clock.set(NOW);
        let host = TestHost::new(&setup);
        let said = setup.said.clone();
        let headless = setup.headless;
        let out = Arc::new(FakeOutside::new(setup));
        let mut table = Table::new();
        register_with(&mut table, out.clone());
        let cx = Ctx {
            host: Arc::new(host.clone()),
            ..fx.cx
        };
        if headless {
            // a `claude -p` child: the session has not been told it draws anywhere
            cx.session.lock().unwrap().interactive = None;
        }
        Self {
            cx,
            table: Arc::new(table),
            out,
            host,
            timers: fx.timers,
            clock: fx.clock,
            said,
            markers: Arc::default(),
            _dir: Arc::new(fx.dir),
        }
    }

    pub fn auto(ctx: f64) -> Self {
        Self::new(Setup::auto(ctx))
    }

    pub fn set_ctx(&self, ctx: f64) {
        self.host.state().ctx = ctx;
    }

    /// A Notify hook: every entry of `key` runs, a failure fails the test.
    pub async fn notify(&self, key: &str, e: Value) {
        for entry in self.table.entries(key) {
            if let Some(run) = &entry.event {
                run(self.cx.clone(), e.clone()).await.unwrap();
            }
        }
    }

    /// A Gate hook around `inner` (what `next` does): every entry's `/event`, then `/after` with
    /// what `inner` came to and what that entry's `/event` left.
    pub async fn gate(&self, key: &str, e: Value, inner: impl Future<Output = Value>) -> Value {
        let mut lefts = Vec::new();
        for entry in self.table.entries(key) {
            let mut left = None;
            if let Some(run) = &entry.event {
                left = run(self.cx.clone(), e.clone()).await.unwrap().left;
            }
            lefts.push(left);
        }
        let result = inner.await;
        for (entry, left) in self.table.entries(key).iter().zip(lefts) {
            if let Some(after) = &entry.after {
                let own = Ctx {
                    left,
                    ..self.cx.clone()
                };
                after(own, result.clone()).await.unwrap();
            }
        }
        result
    }

    /// `$.session.compact(args)` through the hooks that match its trigger, as the engine makes the
    /// call: the marker's gate around the call itself. A failure is the call's `Err`.
    pub async fn compact(&self, args: Value) -> Result<Value, String> {
        let trigger = args["trigger"].as_str().unwrap_or("").to_string();
        let (host, out, markers) = (self.host.clone(), self.out.clone(), self.markers.clone());
        let call = args.clone();
        let inner = async move {
            locked(&markers).push(out.file(MARKER));
            match host.call("session.compact", vec![call]).await {
                Ok(done) => done,
                Err(error) => json!({ "deny": error.to_string() }),
            }
        };
        let matcher = manifest::find("marker")
            .and_then(|hook| hook.matcher)
            .unwrap_or("");
        let done = if matcher.contains(&format!("'{trigger}'")) {
            self.gate("marker", args, inner).await
        } else {
            inner.await
        };
        match done.get("deny").and_then(Value::as_str) {
            Some(error) => Err(error.to_string()),
            None => Ok(done),
        }
    }

    pub async fn submitted(&self, kind: &str, text: &str) {
        self.notify("seen", json!({ "text": text, "origin": { "kind": kind } }))
            .await;
    }

    /// A prompt typed at the terminal.
    pub async fn typed(&self, text: &str) {
        self.submitted("composer", text).await;
    }

    /// A prompt that came through Remote Control.
    pub async fn phone_sent(&self, text: &str) {
        self.submitted("bridge", text).await;
    }

    /// One main-loop turn: it starts, the Stop lists the background work in flight, it ends with
    /// this answer.
    pub async fn turn_listing(&self, answer: &str, running: Vec<Value>) {
        self.notify("turn_start", json!({ "text": "", "turnId": "t" }))
            .await;
        self.notify(
            "stop",
            json!({ "stop_hook_active": false, "background_tasks": running }),
        )
        .await;
        let done = json!({ "answer": answer, "durationMs": 1000, "isAborted": false,
            "turnId": "t", "reason": "answer" });
        self.notify("turn_complete", done).await;
    }

    pub async fn turn(&self, answer: &str, running: &[Running]) {
        self.turn_listing(answer, running.iter().map(task_row).collect())
            .await;
    }

    /// The end of a subagent's turn.
    pub async fn turn_of(&self, answer: &str, agent_id: &str) {
        let done = json!({ "answer": answer, "durationMs": 1000, "isAborted": false,
            "turnId": "t", "reason": "answer", "agentId": agent_id });
        self.notify("turn_complete", done).await;
    }

    /// A tool call, answered with what the test's `said` makes of it.
    pub async fn tool(&self, e: Value) {
        let text = self.said.as_ref().map(|said| said(&e)).unwrap_or_default();
        self.gate("watched", e, async {
            json!({ "result": text, "text": text })
        })
        .await;
    }

    pub async fn edit(&self, e: Value) {
        self.notify("edit", e).await;
    }

    /// `$.config.set` of the setting.
    pub async fn config_set(&self, value: &str) {
        let e = json!({ "key": AUTO_KEY, "value": value });
        self.gate("config", e, async { json!({ "value": value }) })
            .await;
    }

    pub async fn start(&self) {
        let e = json!({ "cwd": "/repo", "surface": "terminal", "isInteractive": true,
            "startedAt": 0 });
        self.notify("start", e).await;
    }

    /// The clock moves on, and what falls due runs.
    pub async fn advance(&self, ms: f64) {
        self.timers.advance(ms, Arc::new(self.host.clone())).await;
    }

    pub async fn band(&self) -> Band {
        let mut band = Band {
            texts: Vec::new(),
            labels: Vec::new(),
        };
        if let Some(tree) = compact_band(&self.cx).await {
            Band::of(&tree, &mut band);
        }
        band
    }

    pub async fn press(&self, key: &str) {
        let out: Out = self.out.clone();
        on_press_with(&self.cx, &out, key).await.unwrap();
    }

    /// The instructions of each compaction that ran.
    pub fn compacted(&self) -> Vec<Option<String>> {
        let state = self.host.state();
        let said = |args: &Value| args["instructions"].as_str().map(str::to_string);
        state.compacted.iter().map(said).collect()
    }

    pub fn compactions(&self) -> usize {
        self.host.state().compacted.len()
    }

    pub fn appended(&self) -> Vec<String> {
        self.host.state().appended.clone()
    }

    /// The rows of the dream log of kind `compact`.
    pub fn compacts(&self) -> Vec<Value> {
        self.out.compacts()
    }

    pub fn events(&self) -> Vec<String> {
        let event = |row: &Value| row["event"].as_str().unwrap_or("").to_string();
        self.compacts().iter().map(event).collect()
    }

    pub fn last(&self) -> Value {
        self.compacts().last().cloned().unwrap_or(Value::Null)
    }

    /// The plan file the pane reads.
    pub fn plan(&self) -> Option<Value> {
        let text = self.out.file(PLAN_FILE)?;
        serde_json::from_str(&text).ok()
    }

    pub fn plan_text(&self) -> String {
        let plan = self.plan().unwrap_or(Value::Null);
        plan["text"].as_str().unwrap_or("").to_string()
    }

    pub fn hold_at(&self, at: Point) -> Arc<Hold> {
        let hold = Hold::at(at);
        *locked(&self.out.hold) = Some(hold.clone());
        hold
    }
}
