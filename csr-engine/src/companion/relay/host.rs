//! The one way engine code asks Claude Code for something.
//!
//! `Host::call` is a `$` call the relay runs for the engine. `RelayHost` carries it over the
//! trampoline: each request is a *conversation* with an id, whose handler runs as a task. When the
//! handler awaits a host call, the reply in flight is `{ id, calls }`; the relay runs the calls and
//! posts `/step { id, results }`, which resumes the handler and is answered with its next yield,
//! more calls or the final reply. Engine-initiated work (`Host::detach`) is the same conversation
//! with no request waiting: its first yield goes out as one JSON line on stdout.
//!
//! `async-trait` is already a dependency, so `Host` is an ordinary async trait.

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::mpsc;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// How long a conversation waits for its `/step` before it is dropped.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// Milliseconds since the epoch, as `$.clock.now()` reads them.
pub trait Clock: Send + Sync {
    fn now(&self) -> f64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64() * 1000.)
            .unwrap_or(0.)
    }
}

/// One `$.noun.method(...args)` call.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub m: String,
    pub a: Vec<Value>,
}

impl Call {
    pub fn new(m: &str, a: Vec<Value>) -> Self {
        Self { m: m.into(), a }
    }
}

/// Work that runs apart from any request, with a host of its own.
pub type Detached = Box<dyn FnOnce(Arc<dyn Host>) -> BoxFuture<()> + Send>;

#[async_trait]
pub trait Host: Send + Sync {
    /// Several calls in one round trip. The outer error means the conversation is gone (dropped,
    /// timed out); each inner one is what Claude Code answered for that call.
    async fn batch(&self, calls: Vec<Call>) -> Result<Vec<Result<Value, String>>>;

    async fn call(&self, m: &str, a: Vec<Value>) -> Result<Value> {
        let mut done = self.batch(vec![Call::new(m, a)]).await?;
        done.pop()
            .unwrap_or_else(|| Err("no result".into()))
            .map_err(|e| anyhow!("{m}: {e}"))
    }

    /// Run `work` on its own, with a host that is not tied to the request that started it.
    fn detach(&self, work: Detached);
}

/// What the handler side of a conversation sends to the request side.
enum Yield {
    Calls(Vec<Value>),
    Done(Value),
}

/// What the relay answered for each call id of one batch.
type Results = Vec<(String, Result<Value, String>)>;

/// A conversation parked on its `/step`.
struct Slot {
    steps: mpsc::UnboundedSender<Results>,
    out: mpsc::UnboundedReceiver<Yield>,
}

/// Every open conversation, and the line writer pushes go through.
pub struct Hub {
    push: mpsc::UnboundedSender<String>,
    open: Mutex<HashMap<String, Slot>>,
    seq: AtomicU64,
    timeout: Duration,
}

type Work = Box<dyn FnOnce(Arc<RelayHost>) -> BoxFuture<Value> + Send>;

impl Hub {
    pub fn new(push: mpsc::UnboundedSender<String>) -> Arc<Self> {
        Arc::new(Self {
            push,
            open: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
            timeout: STEP_TIMEOUT,
        })
    }

    /// The conversations waiting for a `/step`.
    pub fn waiting(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Slot>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn open(self: &Arc<Self>, prefix: &str, work: Work) -> (String, Slot) {
        let id = format!("{prefix}{}", self.seq.fetch_add(1, Ordering::Relaxed) + 1);
        let (out_tx, out) = mpsc::unbounded_channel();
        let (steps, steps_rx) = mpsc::unbounded_channel();
        let host = Arc::new(RelayHost {
            hub: self.clone(),
            id: id.clone(),
            calls: AtomicU64::new(0),
            out: out_tx.clone(),
            steps: tokio::sync::Mutex::new(steps_rx),
        });
        tokio::spawn(async move {
            let done = work(host).await;
            let _ = out_tx.send(Yield::Done(done));
        });
        (id, Slot { steps, out })
    }

    /// Wait for the handler's next yield: `{ id, calls }` (the conversation stays parked) or its
    /// final reply. A handler that died leaves `{}`.
    async fn advance(&self, id: String, mut slot: Slot) -> Value {
        match slot.out.recv().await {
            Some(Yield::Calls(calls)) => {
                self.lock().insert(id.clone(), slot);
                json!({ "id": id, "calls": calls })
            }
            Some(Yield::Done(reply)) => reply,
            None => json!({}),
        }
    }

    /// A request opens a conversation; the reply is its first yield.
    pub async fn converse(
        self: &Arc<Self>,
        work: impl FnOnce(Arc<RelayHost>) -> BoxFuture<Value> + Send + 'static,
    ) -> Value {
        let (id, slot) = self.open("e", Box::new(work));
        self.advance(id, slot).await
    }

    /// `POST /step`: hand the results to the parked handler and answer with its next yield. An id
    /// nobody waits for (a dropped conversation, a respawned engine) gets `{}`.
    pub async fn step(&self, id: &str, results: Results) -> Value {
        let Some(slot) = self.lock().remove(id) else {
            return json!({});
        };
        if slot.steps.send(results).is_err() {
            return json!({});
        }
        self.advance(id.to_string(), slot).await
    }

    /// Work with no request: its calls go out as one line on stdout, its results come on `/step`.
    pub fn detach(
        self: &Arc<Self>,
        work: impl FnOnce(Arc<dyn Host>) -> BoxFuture<()> + Send + 'static,
    ) {
        let (id, slot) = self.open(
            "p",
            Box::new(move |host| {
                Box::pin(async move {
                    work(host).await;
                    json!({})
                })
            }),
        );
        let hub = self.clone();
        tokio::spawn(async move {
            let reply = hub.advance(id, slot).await;
            if reply.get("calls").is_some() {
                let _ = hub.push.send(reply.to_string());
            }
        });
    }
}

/// The host of one conversation.
pub struct RelayHost {
    hub: Arc<Hub>,
    id: String,
    calls: AtomicU64,
    out: mpsc::UnboundedSender<Yield>,
    steps: tokio::sync::Mutex<mpsc::UnboundedReceiver<Results>>,
}

impl RelayHost {
    pub fn id(&self) -> &str {
        &self.id
    }
}

#[async_trait]
impl Host for RelayHost {
    async fn batch(&self, calls: Vec<Call>) -> Result<Vec<Result<Value, String>>> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<String> = calls
            .iter()
            .map(|_| format!("c{}", self.calls.fetch_add(1, Ordering::Relaxed) + 1))
            .collect();
        let wire = calls
            .iter()
            .zip(&ids)
            .map(|(call, id)| json!({ "id": id, "m": call.m, "a": call.a }))
            .collect();
        self.out
            .send(Yield::Calls(wire))
            .map_err(|_| anyhow!("conversation {} is closed", self.id))?;
        let mut steps = self.steps.lock().await;
        match tokio::time::timeout(self.hub.timeout, steps.recv()).await {
            Ok(Some(mut results)) => Ok(ids
                .iter()
                .map(|id| match results.iter().position(|(got, _)| got == id) {
                    Some(at) => results.swap_remove(at).1,
                    None => Err("no result".to_string()),
                })
                .collect()),
            Ok(None) => bail!("conversation {} is closed", self.id),
            Err(_) => {
                self.hub.lock().remove(&self.id);
                bail!(
                    "conversation {} dropped: no /step in {} s",
                    self.id,
                    self.hub.timeout.as_secs()
                )
            }
        }
    }

    fn detach(&self, work: Detached) {
        self.hub.detach(work);
    }
}

/// The `{ id, v } | { id, err }` list a `/step` carries, as results by call id.
pub fn parse_results(body: &Value) -> Results {
    let list = body.get("results").and_then(Value::as_array);
    list.map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|row| {
            let id = row.get("id")?.as_str()?.to_string();
            let result = match row.get("err") {
                Some(err) => Err(err.as_str().map_or_else(|| err.to_string(), str::to_string)),
                None => Ok(row.get("v").cloned().unwrap_or(Value::Null)),
            };
            Some((id, result))
        })
        .collect()
}

/// A scripted stand-in for Claude Code, the twin of `host()` in
/// `plugins/csr-companion/tests/companion.test.ts`.
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::{HashSet, VecDeque};

    /// A clock that moves only when a test moves it.
    pub struct FakeClock(Mutex<f64>);

    impl FakeClock {
        pub fn new(now: f64) -> Arc<Self> {
            Arc::new(Self(Mutex::new(now)))
        }
        pub fn advance(&self, ms: f64) {
            *self.0.lock().unwrap() += ms;
        }
        pub fn set(&self, now: f64) {
            *self.0.lock().unwrap() = now;
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> f64 {
            *self.0.lock().unwrap()
        }
    }

    /// What a fork cost where a test gives no cost of its own.
    pub fn usage_default() -> Value {
        json!({ "input_tokens": 900, "output_tokens": 100,
            "cache_creation_input_tokens": 0, "cache_read_input_tokens": 40_000 })
    }

    /// What the fake Claude Code answers. Defaults are the TypeScript test's.
    #[derive(Default)]
    pub struct World {
        /// `$.agent.list()` answers.
        pub agents: Vec<Value>,
        /// `$.prompt.read()` text.
        pub prompt: String,
        /// `model.fork` replies in order; `dream: none` where left out.
        pub replies: Vec<String>,
        /// What each fork cost, in order; `usage_default()` where left out.
        pub fork_usage: Vec<Value>,
        /// A fork that comes back without text, for this reason.
        pub fork_reason: Option<String>,
        pub fork_fails: bool,
        pub compact_fails: bool,
        /// A `/config` row the plugin may not change.
        pub config_refused: bool,
        /// A `claude -p` child draws on no surface.
        pub headless: bool,
        /// Answers per method, first in first out; they win over the defaults above.
        pub script: std::collections::HashMap<String, VecDeque<Result<Value, String>>>,
        /// Methods that never settle (`agentListHangs`).
        pub hangs: HashSet<String>,
    }

    /// What the plugin did, in the shape of the TypeScript test's `seen`.
    #[derive(Default, Debug)]
    pub struct Seen {
        /// Every call, in order.
        pub calls: Vec<Call>,
        pub toasts: Vec<String>,
        pub statuses: Vec<Value>,
        pub opened: Vec<Value>,
        pub filled: Vec<String>,
        pub forks: Vec<String>,
        pub configured: Vec<Value>,
        pub compacted: Vec<Value>,
        pub logged: Vec<String>,
        pub registered: Vec<Value>,
        pub invalidated: usize,
        pub batches: usize,
        pub detached: usize,
    }

    #[derive(Clone)]
    pub struct FakeHost {
        pub world: Arc<Mutex<World>>,
        pub seen: Arc<Mutex<Seen>>,
        pub clock: Arc<FakeClock>,
    }

    impl FakeHost {
        pub fn new(world: World) -> Self {
            Self {
                world: Arc::new(Mutex::new(world)),
                seen: Arc::new(Mutex::new(Seen::default())),
                clock: FakeClock::new(1_791_547_200_000.),
            }
        }

        pub fn calls(&self) -> Vec<(String, Vec<Value>)> {
            let seen = self.seen.lock().unwrap();
            seen.calls
                .iter()
                .map(|c| (c.m.clone(), c.a.clone()))
                .collect()
        }

        fn answer(&self, call: &Call) -> Result<Value, String> {
            let mut seen = self.seen.lock().unwrap();
            let first = call.a.first().cloned().unwrap_or(Value::Null);
            let text = |v: &Value| v.get("text").and_then(Value::as_str).map(str::to_string);
            match call.m.as_str() {
                "ui.toast" => seen.toasts.push(
                    text(&first)
                        .or(first.as_str().map(str::to_string))
                        .unwrap_or_default(),
                ),
                "ui.status" => seen.statuses.push(first.clone()),
                "ui.open" => seen.opened.push(first.clone()),
                "prompt.fill" => seen.filled.push(text(&first).unwrap_or_default()),
                "ui.invalidate" => seen.invalidated += 1,
                "ui.log" => seen
                    .logged
                    .push(first.as_str().unwrap_or_default().to_string()),
                "command.register" => seen.registered.push(first.clone()),
                "session.compact" => seen.compacted.push(first.clone()),
                "config.set" => seen.configured.push(first.clone()),
                "model.fork" => seen.forks.push(
                    first
                        .get("prompt")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                ),
                _ => (),
            }
            let nth = seen.forks.len().saturating_sub(1);
            drop(seen);
            let mut world = self.world.lock().unwrap();
            if let Some(next) = world.script.get_mut(&call.m).and_then(VecDeque::pop_front) {
                return next;
            }
            let usage = |world: &World| {
                world
                    .fork_usage
                    .get(nth)
                    .cloned()
                    .unwrap_or_else(usage_default)
            };
            match call.m.as_str() {
                "agent.list" => Ok(Value::Array(world.agents.clone())),
                "prompt.read" => Ok(json!({ "text": world.prompt, "cursor": 0 })),
                "prompt.fill" => Ok(json!({ "isFilled": true })),
                "command.register" => Ok(json!({ "command": first.get("name") })),
                "config.list" => Ok(json!([{ "key": "csr-companion.dreams", "label": "Dreams",
                    "kind": "boolean", "value": false }])),
                "config.set" if world.config_refused => Err("set by policy".into()),
                "config.set" => Ok(first.get("value").cloned().unwrap_or(Value::Null)),
                "session.compact" if world.compact_fails => Err("summary failed".into()),
                "session.compact" => Ok(first),
                "session.surfaces" => Ok(json!(if world.headless {
                    vec![]
                } else {
                    vec!["terminal"]
                })),
                "ui.open" => Ok(json!({ "isPlaced": true })),
                "model.fork" if world.fork_fails => Err("timed out after 60000 ms".into()),
                "model.fork" => Ok(match &world.fork_reason {
                    Some(reason) => {
                        json!({ "isAnswered": false, "reason": reason, "usage": usage(&world) })
                    }
                    None => {
                        let text = world
                            .replies
                            .get(nth)
                            .cloned()
                            .unwrap_or_else(|| "dream: none".into());
                        json!({ "isAnswered": true, "text": text, "usage": usage(&world) })
                    }
                }),
                _ => Ok(Value::Null),
            }
        }
    }

    #[async_trait]
    impl Host for FakeHost {
        async fn batch(&self, calls: Vec<Call>) -> Result<Vec<Result<Value, String>>> {
            {
                let mut seen = self.seen.lock().unwrap();
                seen.batches += 1;
                seen.calls.extend(calls.iter().cloned());
            }
            let mut out = Vec::new();
            for call in &calls {
                if self.world.lock().unwrap().hangs.contains(&call.m) {
                    std::future::pending::<()>().await;
                }
                out.push(self.answer(call));
            }
            Ok(out)
        }

        fn detach(&self, work: Detached) {
            self.seen.lock().unwrap().detached += 1;
            tokio::spawn(work(Arc::new(self.clone())));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeClock, FakeHost, World};
    use super::*;

    #[tokio::test]
    async fn the_fake_answers_by_method_and_records_every_call_in_order() {
        let host = FakeHost::new(World {
            replies: vec!["dream: one".into(), "dream: two".into()],
            agents: vec![json!({ "id": "a1" })],
            ..World::default()
        });
        let listed = host.call("agent.list", vec![]).await.unwrap();
        assert_eq!(listed, json!([{ "id": "a1" }]));
        let first = host
            .call("model.fork", vec![json!({ "prompt": "p1" })])
            .await
            .unwrap();
        let second = host
            .call("model.fork", vec![json!({ "prompt": "p2" })])
            .await
            .unwrap();
        assert_eq!(first["text"], "dream: one");
        assert_eq!(second["text"], "dream: two");
        assert_eq!(first["usage"]["input_tokens"], 900);
        host.call("ui.toast", vec![json!("hello")]).await.unwrap();
        host.call("prompt.fill", vec![json!({ "text": "hi" })])
            .await
            .unwrap();
        let names: Vec<String> = host.calls().into_iter().map(|(m, _)| m).collect();
        assert_eq!(
            names,
            [
                "agent.list",
                "model.fork",
                "model.fork",
                "ui.toast",
                "prompt.fill"
            ]
        );
        let seen = host.seen.lock().unwrap();
        assert_eq!(seen.forks, ["p1", "p2"]);
        assert_eq!(seen.toasts, ["hello"]);
        assert_eq!(seen.filled, ["hi"]);
    }

    #[tokio::test]
    async fn a_script_beats_the_defaults_and_failures_come_back_as_errors() {
        let mut world = World::default();
        let answers = [Ok(json!({ "a": 1 })), Err("gone".to_string())];
        world.script.insert("session.usage".into(), answers.into());
        world.fork_fails = true;
        world.config_refused = true;
        let host = FakeHost::new(world);
        let first = host.call("session.usage", vec![]).await.unwrap();
        assert_eq!(first, json!({ "a": 1 }));
        let second = host.call("session.usage", vec![]).await.unwrap_err();
        assert!(second.to_string().contains("gone"));
        assert!(host
            .call("model.fork", vec![json!({ "prompt": "p" })])
            .await
            .is_err());
        assert!(host
            .call("config.set", vec![json!({ "key": "k" })])
            .await
            .is_err());
        let pair = vec![
            Call::new("session.surfaces", vec![]),
            Call::new("ui.status", vec![]),
        ];
        assert_eq!(host.batch(pair).await.unwrap().len(), 2);
        assert_eq!(host.seen.lock().unwrap().batches, 5);
    }

    #[tokio::test]
    async fn a_method_that_hangs_never_settles_and_the_clock_moves_when_told() {
        let host = FakeHost::new(World::default());
        host.world.lock().unwrap().hangs.insert("agent.list".into());
        let wait = Duration::from_millis(50);
        let waited = tokio::time::timeout(wait, host.call("agent.list", vec![])).await;
        assert!(waited.is_err());
        let before = host.clock.now();
        host.clock.advance(1500.);
        assert_eq!(host.clock.now() - before, 1500.);
        assert_eq!(FakeClock::new(5.).now(), 5.);
    }

    #[tokio::test]
    async fn the_fake_runs_detached_work_with_itself_as_the_host() {
        let host = FakeHost::new(World::default());
        host.detach(Box::new(|h| {
            Box::pin(async move {
                let _ = h.call("ui.invalidate", vec![]).await;
            })
        }));
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
        assert_eq!(host.seen.lock().unwrap().invalidated, 1);
        assert_eq!(host.seen.lock().unwrap().detached, 1);
    }

    #[test]
    fn step_results_parse_values_errors_and_skip_rows_without_an_id() {
        let got = parse_results(&json!({ "results": [
            { "id": "c1", "v": { "a": 1 } }, { "id": "c2", "err": "no" }, { "id": "c3" },
            { "v": 1 }, "junk" ] }));
        let want = vec![
            ("c1".to_string(), Ok(json!({ "a": 1 }))),
            ("c2".to_string(), Err("no".to_string())),
            ("c3".to_string(), Ok(Value::Null)),
        ];
        assert_eq!(got, want);
        assert!(parse_results(&json!({})).is_empty());
    }
}
