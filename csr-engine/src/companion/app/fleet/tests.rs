//! The 43 fleet tests: `tests/agents.test.ts` (26) and `tests/companion.test.ts` 2825-3080 (17),
//! by their snake-case names.
//!
//! The pace and the dreamer are other packages, and here they are the real ones: the table holds
//! the fleet's entries, the pace's and the dreamer's, in the manifest's order, and the dreamer
//! runs on a stand-in machine (`dreamer::outside::fake`: a brief a test sets, the lease on files
//! of its own, no git). A look is made by the dreamer's own code, so the budget check, the
//! brief's envelope and news, the fork, the pace and chapter rows and the claim's settling are
//! the real ones. A fork waits at a gate when a test holds it (`dreamer::outside::fake::Gated`).

use super::model::{
    admit_completion, associate_handoff, claim_completions, completion_key, completion_news,
    is_stalled, mark_disposition, parse_agent_envelope, pending_completions, plan_write,
    project_agents_file, reconcile, reduce, settle_completions, step_key, Act, AgentHandoff,
    AgentRec, AgentState, Disposition, Event, HandoffEvent, Ledger, Listed, Status, FILE_BYTES,
    FILE_EVERY_MS, LEDGER_MAX, STALL_MS,
};
use super::text::{fingerprint, normalize_path, redact_agent_value, utf8_length};
use super::*;
use crate::companion::{
    agents as pane,
    app::{
        dreamer::{
            self,
            outside::fake::{Fake, Gated},
            text::own_words,
        },
        pace::{self, facts},
    },
    relay::{
        dispatch::testing::{fixture_in, run, until, Fixture},
        host::fake::{FakeHost, World},
    },
};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// The fake host's clock at the start (the TypeScript's `T0`).
const T0: i64 = 1_791_547_200_000;
const SESSION: &str = "sess-a";
const SID: &str = "sess-1";
fn usage() -> Value {
    json!({ "input_tokens": 10, "output_tokens": 5, "cache_creation_input_tokens": 1,
        "cache_read_input_tokens": 100, "model": "opus" })
}
/// All four counts: 41,000 (the TypeScript's `LOOK`).
const LOOK: u64 = 41_000;
const REPORT: &str = "The importer retries three times and then drops the batch silently.";
const BRIEF: &str = "What CSR holds of this work:\n* The importer was edited three days ago.\n\
    * It retries its batch.";
const DREAM_REPLY: &str = "dream: Should the importer keep its retries?\nreceipt: abc1234";

fn look_usage() -> Value {
    json!({ "input_tokens": 4500, "output_tokens": 500, "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 36_000, "model": "opus" })
}

// ---------------------------------------------------------------- rows a test reads or sets

type Kept = Arc<Mutex<Vec<(String, Value)>>>;

fn row(cx: &Ctx, name: &str) -> Value {
    cx.store
        .get(&cx.sid, name)
        .ok()
        .flatten()
        .unwrap_or(Value::Null)
}

fn put(cx: &Ctx, name: &str, value: Value) {
    cx.store.set(&cx.sid, name, &value).unwrap();
}

fn num(value: &Value, name: &str) -> f64 {
    value[name].as_f64().unwrap_or(0.)
}

fn fresh_pace(sid: &str) -> Value {
    json!({ "session": sid, "claude": 0, "dreamer": 0, "looks": 0, "dreams": 0, "lastLook": 0 })
}

fn fresh_chapter(sid: &str, started: i64) -> Value {
    json!({ "session": sid, "started": started, "steps": 0, "due": 6, "looks": 0, "shown": 0,
        "adopted": 0 })
}

fn merge(base: Value, more: Value) -> Value {
    let (Value::Object(mut base), Value::Object(more)) = (base, more) else {
        panic!("two objects")
    };
    base.extend(more);
    Value::Object(base)
}

/// `/config` says dreams are `dreams`, for the next 3000 reads.
fn script_dreams(host: &FakeHost, dreams: bool) {
    let rows = json!([{ "key": "csr-companion.dreams", "label": "Dreams", "kind": "boolean",
        "value": dreams }]);
    let list: VecDeque<Result<Value, String>> = (0..3000).map(|_| Ok(rows.clone())).collect();
    let mut world = host.world.lock().unwrap();
    world.script.insert("config.list".into(), list);
}

// ---------------------------------------------------------------- the rig

fn unpack(packed: &Option<Value>, at: usize) -> Option<Value> {
    let rows = packed.as_ref()?.as_array()?;
    let row = rows.iter().find(|row| row[0].as_u64() == Some(at as u64))?;
    Some(row[1].clone())
}

struct Rig {
    fx: Fixture,
    table: Table,
    /// The machine around the dreamer: its brief, its lease files.
    out: Arc<Fake>,
    /// The `--keep` records the pace and the dreamer wrote.
    kept: Kept,
    /// The fork gate: `hold_forks` holds the next forks until `release`.
    gate: Gated,
    home: PathBuf,
    main_next: AtomicU64,
}

impl Rig {
    fn new() -> Self {
        Self::build(World::default(), SESSION, true)
    }

    /// The session of the companion tests, with dreams on.
    fn dreaming(world: World) -> Self {
        Self::build(world, SID, true)
    }

    fn build(world: World, sid: &str, dreams: bool) -> Self {
        let mut fx = fixture_in(world, sid);
        let home = fx.dir.path().join("home");
        let kept: Kept = Arc::default();
        let seen = kept.clone();
        let keep: pace::Keep = Arc::new(move |cwd, record| {
            seen.lock().unwrap().push((cwd, record));
            Box::pin(async {})
        });
        let out = Arc::new(Fake::new(keep.clone(), BRIEF));
        let gate = Gated::passing(fx.host.clone());
        fx.cx.host = Arc::new(gate.clone());
        script_dreams(&fx.host, dreams);
        let mut table = Table::new();
        register(&mut table);
        pace::register_with(&mut table, keep.clone());
        dreamer::register_with(&mut table, out.clone());
        pace::install_keep(&fx.cx, &keep);
        let paths = Paths::new(home.clone());
        fx.cx.kept(|f: &mut Fleet| {
            f.paths = Some(paths);
            f.options = Some(Options {
                dreams,
                auto_compact: "auto",
            });
            f.dream = Some(dreamer::seam(out.clone()));
        });
        Self {
            fx,
            table,
            out,
            kept,
            gate,
            home,
            main_next: AtomicU64::new(0),
        }
    }

    /// Dreams and compaction as the person set them.
    fn options(&self, dreams: bool, auto_compact: &'static str) {
        script_dreams(&self.fx.host, dreams);
        self.fx.cx.kept(|f: &mut Fleet| {
            f.options = Some(Options {
                dreams,
                auto_compact,
            })
        });
    }

    /// The next fork waits until `release`.
    fn hold_forks(&self) {
        self.gate.hold();
    }

    fn release(&self) {
        self.gate.open();
    }

    fn set_brief(&self, brief: String) {
        self.out.set_brief(&brief);
    }

    fn cx(&self) -> &Ctx {
        &self.fx.cx
    }

    fn state(&self) -> AgentState {
        read_state(&self.fx.cx).unwrap()
    }

    fn rec(&self, id: &str) -> AgentRec {
        self.state()
            .rows
            .get(id)
            .unwrap_or_else(|| panic!("no row {id}"))
            .clone()
    }

    fn forks(&self) -> Vec<String> {
        self.fx.host.seen.lock().unwrap().forks.clone()
    }

    /// The looks and dreams the dreamer kept (a chapter's start is the pace's and not one).
    fn looks(&self) -> Vec<Value> {
        let kept = self.kept.lock().unwrap();
        (kept.iter())
            .map(|(_, record)| record.clone())
            .filter(|record| record["kind"] == "look" || record["kind"] == "dream")
            .collect()
    }

    fn looked_at(&self) -> bool {
        self.looks()
            .iter()
            .any(|r| r["kind"] == "look" || r["kind"] == "dream")
    }

    fn ledger(&self) -> BTreeMap<String, String> {
        match self.fx.cx.store.get(&self.fx.cx.sid, AGENT_LEDGER).unwrap() {
            Some(Value::Object(map)) => map
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect(),
            _ => BTreeMap::new(),
        }
    }

    fn word(&self, agent: &str, turn: &str) -> String {
        self.ledger()
            .get(&completion_key(&self.fx.cx.sid, agent, turn))
            .cloned()
            .unwrap_or_default()
    }

    fn file_text(&self) -> Option<String> {
        let path = self.home.join(format!(
            ".claude-self-reflect/csr-companion/agents/{}.json",
            self.fx.cx.sid
        ));
        std::fs::read_to_string(path).ok()
    }

    fn handoffs_file(&self) -> Vec<Value> {
        let path = self.home.join(format!(
            ".claude-self-reflect/csr-companion/handoffs/{}.json",
            self.fx.cx.sid
        ));
        let Ok(text) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        let file: Value = serde_json::from_str(&text).unwrap();
        file["handoffs"].as_array().cloned().unwrap_or_default()
    }

    fn writes(&self) -> u64 {
        self.fx.cx.kept(|f: &mut Fleet| f.writes)
    }

    async fn event(&self, key: &str, e: Value) -> Reply {
        run(&self.table, key, &self.fx.cx, e).await
    }

    /// `/after` of every entry of `key`: `arg` is the result (a gate) or `{e, result}` (a stream).
    async fn after(&self, key: &str, arg: Value, left: &Option<Value>) {
        for (at, entry) in self.table.entries(key).iter().enumerate() {
            let Some(after) = &entry.after else { continue };
            let cx = Ctx {
                left: unpack(left, at),
                ..self.fx.cx.clone()
            };
            let _ = after(cx, arg.clone()).await;
        }
    }

    /// A gate: `/event`, the call itself (its `result`), `/after`.
    async fn gate(&self, key: &str, e: Value, result: Value) -> Reply {
        let reply = self.event(key, e).await;
        self.after(key, result, &reply.left).await;
        reply
    }

    async fn advance(&self, ms: f64) {
        self.fx
            .timers
            .advance(ms, Arc::new(self.gate.clone()))
            .await;
    }

    async fn spawn(&self, id: &str, extra: Value, outcome: Value) -> Reply {
        self.gate("spawned", spawn_call(id, extra), outcome).await
    }

    async fn spawn_a1(&self, id: &str) -> Reply {
        let outcome = json!({ "model": "claude-opus-5", "agentId": "a1" });
        self.spawn(id, json!({}), outcome).await
    }

    async fn step_with(&self, agent: Option<&str>, turn: &str, index: u64, usage: Value) {
        let e = json!({ "turnId": turn, "index": index, "model": "opus", "messageCount": 4,
            "agentId": agent });
        let result = json!({ "turnId": turn, "index": index, "answer": "Looking at the files",
            "toolUses": [], "stopReason": "end_turn", "usage": usage });
        self.after("step", json!({ "e": e, "result": result }), &None)
            .await;
    }

    async fn step(&self, agent: Option<&str>, turn: &str, index: u64) {
        self.step_with(agent, turn, index, usage()).await;
    }

    /// n steps of Claude's main loop (or an agent's): the main session's continue the turn's
    /// index where the last call stopped; `from` replays the indexes from there.
    async fn steps(&self, n: u64, agent: Option<&str>, from: Option<u64>) {
        for k in 0..n {
            let index = match (from, agent) {
                (Some(from), _) => from + k,
                (None, None) => self.main_next.fetch_add(1, Ordering::SeqCst),
                (None, Some(_)) => k,
            };
            self.step_with(agent, "turn-1", index, look_usage()).await;
        }
    }

    async fn finish(&self, agent: &str, turn: &str, answer: &str, reason: &str) -> Reply {
        let e = json!({ "answer": answer, "durationMs": 1000, "isAborted": reason == "aborted",
            "turnId": turn, "reason": reason, "agentId": agent });
        self.event("turn_complete", e).await
    }

    async fn reported(&self, agent: &str, turn: &str, answer: &str) -> Reply {
        self.finish(agent, turn, answer, "answer").await
    }

    async fn main_turn_ends(&self, turn: &str) {
        let e = json!({ "answer": "", "durationMs": 1, "isAborted": false, "turnId": turn,
            "reason": "answer" });
        self.event("turn_complete", e).await;
    }

    async fn edit(&self, agent: Option<&str>, id: &str, file: &str) -> Reply {
        let e = json!({ "tool": "Edit", "file_path": file, "old_string": "a", "new_string": "b",
            "tool_use_id": id, "agentId": agent });
        self.gate("tool", e, json!({ "result": "ok", "text": "Saved" }))
            .await
    }

    async fn start(&self) {
        let e = json!({ "cwd": "/repo", "surface": "terminal", "isInteractive": true,
            "startedAt": 0 });
        self.event("start", e).await;
    }

    /// The person's Enter, as `/after` of the digit gate sees it.
    async fn submit(&self, text: &str) {
        self.after("digit", json!({ "text": text }), &None).await;
    }

    /// A session whose own schedule is far off and whose Claude has spent plenty: only a
    /// completion can start a look.
    fn roomy(&self, pace: Value) {
        let cx = self.cx();
        put(
            cx,
            "chapter",
            merge(
                fresh_chapter(&cx.sid, T0),
                json!({ "due": 10_000, "gen": 1 }),
            ),
        );
        let base = merge(
            fresh_pace(&cx.sid),
            json!({ "claude": 5_000_000, "step": LOOK }),
        );
        put(cx, "pace", merge(base, pace));
    }

    fn chapter_due(&self) -> f64 {
        num(&row(self.cx(), "chapter"), "due")
    }
}

fn spawn_call(id: &str, extra: Value) -> Value {
    merge(
        json!({ "tool_use_id": id, "prompt": "Do it.", "description": "Scan the repo",
            "subagentType": "Explore", "model": "haiku", "background": true, "fork": false }),
        extra,
    )
}

/// The brief `pane.py` would print once these agents had reported: CSR's lines, then the agents
/// as the machine envelope.
struct Said {
    id: &'static str,
    turn: &'static str,
    answer: Option<&'static str>,
    status: &'static str,
    kind: &'static str,
}

fn said(id: &'static str, answer: &'static str) -> Said {
    Said {
        id,
        turn: "turn-1",
        answer: Some(answer),
        status: "completed",
        kind: "Explore",
    }
}

fn working(id: &'static str, kind: &'static str) -> Said {
    Said {
        id,
        turn: "turn-1",
        answer: None,
        status: "running",
        kind,
    }
}

fn brief_with(agents: &[Said]) -> String {
    let rows: Vec<Value> = agents
        .iter()
        .map(|row| {
            let completion = row
                .answer
                .map(|answer| json!({ "turn_id": row.turn, "reason": "answer", "answer": answer }));
            json!({ "id": row.id, "parent": null, "type": row.kind, "model": null,
                "status": row.status, "edits": [], "overlaps": [], "completion": completion })
        })
        .collect();
    let payload = json!({ "kind": "context", "session": SID, "at": T0, "agents": rows });
    format!("{BRIEF}\n\n{}\n{payload}\n{}\n", pane::OPEN, pane::CLOSE)
}

fn red(mut state: AgentState, event: Event) -> AgentState {
    reduce(&mut state, event);
    state
}

fn touch(at: i64, agent: &str, tool: &str) -> Event {
    Event::Touch {
        at,
        agent_id: agent.into(),
        tool: tool.into(),
    }
}

fn complete(at: i64, agent: &str, turn: &str, reason: &str, answer: &str) -> Event {
    Event::Complete {
        at,
        agent_id: agent.into(),
        turn_id: turn.into(),
        reason: reason.into(),
        answer: answer.into(),
    }
}

fn listed(id: &str, kind: &str, description: &str, status: &str) -> Listed {
    Listed::of(&json!({ "id": id, "type": kind, "description": description, "status": status }))
        .unwrap()
}

fn recon(mut state: AgentState, list: &[Listed], at: i64) -> AgentState {
    reconcile(&mut state, list, at);
    state
}

fn ids(state: &AgentState) -> Vec<String> {
    let mut ids: Vec<String> = state.rows.keys().map(str::to_string).collect();
    ids.sort();
    ids
}

fn running_in_file(state: &AgentState, now: i64) -> usize {
    let file = project_agents_file(state, now);
    file.agents
        .iter()
        .filter(|row| row.status == Status::Running)
        .count()
}

// ---------------------------------------------------------------- agents.test.ts

#[tokio::test]
async fn spawn_result_supplies_id_and_model() {
    let rig = Rig::new();
    let reply = rig.spawn_a1("tu-1").await;
    assert!(
        reply.answer.is_none() && reply.next.is_none(),
        "an observer lets `next` answer"
    );
    let row = rig.rec("a1");
    assert_eq!(row.model.as_deref(), Some("claude-opus-5")); // the resolved model, not "haiku"
    assert_eq!(row.kind.as_deref(), Some("Explore"));
    assert_eq!(row.description, "Scan the repo");
    assert_eq!(row.parent, None);
    assert_eq!(row.started, Some(T0));
    assert_eq!(row.status, Status::Running);
    assert_eq!(
        rig.state().bound.get("tu-1").map(String::as_str),
        Some("a1")
    );
}

#[tokio::test]
async fn a_spawn_that_started_nothing_makes_no_row() {
    let rig = Rig::new();
    let reply = rig
        .spawn("tu-1", json!({}), json!({ "deny": "the person said no" }))
        .await;
    assert!(reply.answer.is_none() && reply.next.is_none());
    let state = rig.state();
    assert_eq!(ids(&state), Vec::<String>::new());
    assert!(state.spawns.is_empty());
}

#[tokio::test]
async fn events_before_spawn_merge_without_reset() {
    let rig = Rig::new();
    // The Agent call is under way: its `/event` ran, `next` has not answered.
    let begun = rig.event("spawned", spawn_call("tu-1", json!({}))).await;
    assert!(rig.state().spawns.contains("tu-1")); // recorded before `next` answered
    rig.edit(Some("a1"), "tool-1", "/repo/src/a.ts").await;
    rig.step(Some("a1"), "turn-1", 0).await;
    rig.finish("a1", "turn-1", "The report.", "answer").await;
    let pending = rig.rec("a1").completion.map(|c| c.disposition);
    assert_eq!(pending, Some(Disposition::Pending));
    let outcome = json!({ "model": "claude-opus-5", "agentId": "a1" });
    rig.after("spawned", outcome, &begun.left).await;
    let state = rig.state();
    let row = state.rows.get("a1").unwrap();
    assert_eq!(row.tools, 1);
    assert_eq!(row.tokens, 116);
    assert_eq!(
        row.paths.values().cloned().collect::<Vec<_>>(),
        ["/repo/src/a.ts"]
    );
    assert_eq!(row.status, Status::Completed); // the spawn result does not make it run again
    assert_eq!(
        row.completion.as_ref().map(|c| c.answer.as_str()),
        Some("The report.")
    );
    assert_eq!(row.model.as_deref(), Some("claude-opus-5"));
    assert_eq!(row.kind.as_deref(), Some("Explore"));
    assert_eq!(row.started, Some(T0));
    let pending: Vec<&str> = state.pending.keys().collect();
    assert_eq!(pending, [completion_key(SESSION, "a1", "turn-1")]);
}

#[tokio::test]
async fn nested_parent_tree_uses_parentid() {
    let rig = Rig::new();
    rig.spawn("tu-1", json!({}), json!({ "model": "m", "agentId": "a1" }))
        .await;
    let child = json!({ "parentAgentId": "a1", "description": "Child" });
    rig.spawn("tu-2", child, json!({ "model": "m", "agentId": "a2" }))
        .await;
    let state = rig.state();
    assert_eq!(state.rows.get("a1").unwrap().parent, None);
    assert_eq!(state.rows.get("a2").unwrap().parent.as_deref(), Some("a1"));
    assert_eq!(state.rows.get("a2").unwrap().description, "Child");
}

#[tokio::test]
async fn list_disappearance_is_not_completion() {
    let rig = Rig::new();
    let agent =
        json!({ "id": "a1", "type": "Explore", "description": "Scan", "status": "running" });
    rig.fx.host.world.lock().unwrap().agents = vec![agent];
    rig.spawn_a1("tu-1").await;
    rig.main_turn_ends("main-1").await; // a main turn ends: the list is read
    assert!(until(|| rig.state().rows.get("a1").is_some_and(|row| row.listed)).await);
    rig.fx.host.world.lock().unwrap().agents = vec![]; // the row is gone from the list
    rig.main_turn_ends("main-2").await;
    assert!(until(|| rig.state().rows.get("a1").is_some_and(|row| !row.listed)).await);
    let state = rig.state();
    let row = state.rows.get("a1").unwrap();
    assert!(!row.listed);
    assert_eq!(row.status, Status::Running); // not finished, failed or killed: nothing says so
    assert_eq!(row.completion, None);
    assert!(state.pending.is_empty());
    // and the pure rule: a row the list never had is made from the list, with no report
    let only = recon(
        AgentState::empty(SESSION),
        &[listed("z", "Plan", "x", "completed")],
        T0,
    );
    assert_eq!(only.rows.get("z").unwrap().status, Status::Completed);
    assert_eq!(only.rows.get("z").unwrap().completion, None);
    assert!(only.pending.is_empty());
}

#[test]
fn pending_waiting_idle_are_not_stalled() {
    let at = T0;
    let long = 3 * 60 * 60 * 1000;
    for status in [
        Status::Pending,
        Status::Waiting,
        Status::Idle,
        Status::Completed,
        Status::Failed,
        Status::Killed,
        Status::Unknown,
    ] {
        assert!(!is_stalled(status, at - long, at), "{status:?}");
    }
    assert!(!is_stalled(Status::Running, at - STALL_MS + 1, at));
    assert!(is_stalled(Status::Running, at - STALL_MS, at));
    // a list poll never refreshes last_at: a running row stays stalled however often it is listed
    let mut state = red(AgentState::empty(SESSION), touch(at, "a1", "Bash"));
    for n in 1..=12 {
        let list = [listed("a1", "Explore", "x", "running")];
        state = recon(state, &list, at + n * 60_000);
    }
    let row = state.rows.get("a1").unwrap();
    assert_eq!(row.last_at, at);
    assert!(is_stalled(row.status, row.last_at, at + 12 * 60_000));
    // the list says waiting or idle: not stalled, and not running either
    let list = [listed("a1", "Explore", "x", "waiting")];
    let waiting = recon(state, &list, at + 13 * 60_000);
    let row = waiting.rows.get("a1").unwrap();
    assert_eq!(row.status, Status::Waiting);
    assert!(!is_stalled(row.status, row.last_at, at + 60 * 60_000));
}

#[test]
fn a_never_listed_active_row_untouched_for_stall_ms_is_dropped_by_the_next_poll_and() {
    let running = |state: &AgentState, now| running_in_file(state, now);
    // the ghost: one touch of an in-process agent the list never reports
    let mut ghost = red(AgentState::empty(SESSION), touch(T0, "ghost", "Edit"));
    let row = ghost.rows.get("ghost").unwrap();
    assert_eq!(row.kind, None);
    assert_eq!(row.description, "");
    assert_eq!(row.started, None);
    assert!(!row.listed);
    assert_eq!(row.status, Status::Running);
    assert_eq!(running(&ghost, T0 + STALL_MS), 1);
    // within STALL_MS it stays
    assert_eq!(
        ids(&recon(ghost.clone(), &[], T0 + STALL_MS - 1)),
        ["ghost"]
    );
    // past it, one poll drops it; the agents file and its running count omit it
    let dropped = recon(ghost.clone(), &[], T0 + STALL_MS);
    assert_eq!(ids(&dropped), Vec::<String>::new());
    assert!(project_agents_file(&dropped, T0 + STALL_MS)
        .agents
        .is_empty());
    assert_eq!(running(&dropped, T0 + STALL_MS), 0);
    // a later event of that id makes it again, as before
    ghost = red(dropped, touch(T0 + STALL_MS + 5, "ghost", "Edit"));
    assert_eq!(ids(&ghost), ["ghost"]);

    // a listed agent that drops off the list keeps its last state, however long it has been quiet
    let mut on = red(AgentState::empty(SESSION), touch(T0, "a1", "Bash"));
    on = recon(on, &[listed("a1", "Explore", "x", "running")], T0 + 1000);
    let off = recon(on, &[], T0 + 3 * STALL_MS);
    let row = off.rows.get("a1").unwrap();
    assert_eq!(
        (row.status, row.listed, row.kind.as_deref()),
        (Status::Running, false, Some("Explore"))
    );
    let later = recon(off, &[], T0 + 6 * STALL_MS);
    let row = later.rows.get("a1").unwrap();
    assert_eq!((row.status, row.listed), (Status::Running, false));
    assert_eq!(running(&later, T0 + 6 * STALL_MS), 1);

    // a row a spawn event gave a type and a description keeps today's rule too
    let spawn = Event::Spawn {
        at: T0,
        tool_use_id: "u1".into(),
        description: "Scan".into(),
        kind: Some("Explore".into()),
        parent: None,
        model: None,
        requests: vec![],
    };
    let mut spawned = red(AgentState::empty(SESSION), spawn);
    let started = Event::Spawned {
        at: T0,
        tool_use_id: "u1".into(),
        agent_id: Some("s1".into()),
        model: Some("m".into()),
    };
    spawned = red(spawned, started);
    assert_eq!(ids(&recon(spawned, &[], T0 + 3 * STALL_MS)), ["s1"]);

    // a finished never-listed row is not a ghost
    let done = red(
        AgentState::empty(SESSION),
        complete(T0, "c1", "t1", "answer", "ok"),
    );
    assert_eq!(ids(&recon(done, &[], T0 + 3 * STALL_MS)), ["c1"]);

    // one dropped ghost among live rows leaves the others alone
    let mut mixed = red(AgentState::empty(SESSION), touch(T0, "ghost", "Edit"));
    mixed = red(mixed, touch(T0 + STALL_MS, "a1", "Bash"));
    let list = [listed("a1", "Explore", "x", "running")];
    mixed = recon(mixed, &list, T0 + STALL_MS + 10);
    assert_eq!(ids(&mixed), ["a1"]);
    assert_eq!(running(&mixed, T0 + STALL_MS + 10), 1);
}

#[tokio::test]
async fn resumed_agent_has_new_completion_identity() {
    let rig = Rig::new();
    rig.spawn_a1("tu-1").await;
    rig.step(Some("a1"), "turn-1", 0).await;
    rig.reported("a1", "turn-1", "First report.").await;
    let row = rig.rec("a1");
    assert_eq!(row.status, Status::Completed);
    let key = completion_key(SESSION, "a1", "turn-1");
    assert_eq!(row.completion.map(|c| c.key), Some(key.clone()));
    rig.step(Some("a1"), "turn-2", 0).await; // the same agent, run again
    let row = rig.rec("a1");
    assert_eq!(row.status, Status::Running);
    assert_eq!(row.completion, None); // no report yet for this run
    rig.reported("a1", "turn-2", "Second report.").await;
    let state = rig.state();
    let completion = state.rows.get("a1").unwrap().completion.clone().unwrap();
    assert_eq!(completion.key, completion_key(SESSION, "a1", "turn-2"));
    assert_eq!(completion.answer, "Second report.");
    let mut pending: Vec<String> = state.pending.keys().map(str::to_string).collect();
    pending.sort();
    let mut want = vec![key, completion_key(SESSION, "a1", "turn-2")];
    want.sort();
    assert_eq!(pending, want);
    rig.reported("a1", "turn-2", "Second report.").await; // the same run twice: one identity
    assert_eq!(rig.state().pending.len(), 2);
}

#[tokio::test]
async fn reload_preserves_claimed_completions() {
    let rig = Rig::new();
    rig.spawn_a1("tu-1").await;
    rig.reported("a1", "turn-1", "The report.").await;
    let key = completion_key(SESSION, "a1", "turn-1");
    let mut ledger = Ledger::new();
    assert!(admit_completion(&mut ledger, &key).fresh);
    let due: Vec<String> = pending_completions(&rig.state(), &ledger)
        .into_iter()
        .map(|entry| entry.key)
        .collect();
    assert_eq!(due, std::slice::from_ref(&key));
    assert_eq!(
        claim_completions(&mut ledger, std::slice::from_ref(&key)),
        std::slice::from_ref(&key)
    );
    let mut state = rig.state();
    mark_disposition(&mut state, std::slice::from_ref(&key), Disposition::Claimed);
    // a reload: the state and the ledger come back from storage, and the module sees the
    // completion again
    state = serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
    ledger = serde_json::from_value(serde_json::to_value(&ledger).unwrap()).unwrap();
    state = red(
        state,
        complete(T0 + 5, "a1", "turn-1", "answer", "The report."),
    );
    assert!(state.pending.is_empty()); // not pending again
    let disposition = state
        .rows
        .get("a1")
        .unwrap()
        .completion
        .as_ref()
        .map(|c| c.disposition);
    assert_eq!(disposition, Some(Disposition::Claimed));
    assert!(pending_completions(&state, &ledger).is_empty());
    assert!(claim_completions(&mut ledger, std::slice::from_ref(&key)).is_empty()); // never claimed twice
    let before = ledger.clone();
    let again = admit_completion(&mut ledger, &key);
    assert_eq!((again.fresh, again.full), (false, false)); // nor admitted twice
    assert_eq!(ledger, before);
    settle_completions(&mut ledger, std::slice::from_ref(&key), Disposition::Looked);
    assert_eq!(ledger[&key], Disposition::Looked);
    settle_completions(&mut ledger, std::slice::from_ref(&key), Disposition::Failed);
    // a final word stays final
    assert_eq!(ledger[&key], Disposition::Looked);
    // a full ledger refuses a new identity and evicts none
    let mut full: Ledger = (0..LEDGER_MAX)
        .map(|i| (format!("k{i}"), Disposition::Looked))
        .collect();
    assert!(admit_completion(&mut full, "new").full);
    assert_eq!(full.len(), LEDGER_MAX);
}

#[tokio::test]
async fn observer_failure_never_repeats_next() {
    let rig = Rig::new();
    // The store cannot be opened: every observer fails where it reads or writes the state.
    let blocker = rig.fx.dir.path().join("csr-companion");
    std::fs::write(&blocker, "not a folder").unwrap();
    let clean = |reply: &Reply| reply.answer.is_none() && reply.next.is_none();
    let reply = rig.edit(Some("a1"), "tool-1", "/repo/src/a.ts").await;
    assert!(clean(&reply)); // the observer answers nothing: the tool's own result stands
    assert!(clean(&rig.edit(None, "tool-2", "/repo/src/a.ts").await));
    assert!(clean(&rig.spawn_a1("tu-1").await));
    rig.step(Some("a1"), "turn-1", 0).await;
    assert!(clean(
        &rig.finish("a1", "turn-1", "Found it.", "answer").await
    ));
    std::fs::remove_file(&blocker).unwrap();
    assert_eq!(ids(&rig.state()), Vec::<String>::new()); // and nothing was recorded
}

#[tokio::test]
async fn tracking_works_with_dreams_and_compaction_off() {
    let rig = Rig::new();
    rig.options(false, "off");
    rig.spawn_a1("tu-1").await;
    rig.edit(Some("a1"), "tool-1", "/repo/src/b.ts").await;
    rig.edit(None, "tool-2", "/repo/src/b.ts").await; // the main loop wrote the same file
    rig.step(Some("a1"), "turn-1", 0).await;
    rig.reported("a1", "turn-1", "Done.").await;
    let state = rig.state();
    let row = state.rows.get("a1").unwrap();
    assert_eq!(row.tools, 1);
    assert_eq!(row.tokens, 116);
    assert_eq!(row.status, Status::Completed);
    let main = state
        .main
        .get(&fingerprint("/repo/src/b.ts"))
        .map(String::as_str);
    assert_eq!(main, Some("/repo/src/b.ts"));
    assert_eq!(state.pending.len(), 1);
}

#[test]
fn a_failed_or_refused_or_interrupted_turn_is_terminal_and_never_pending() {
    let mut state = AgentState::empty(SESSION);
    state = red(state, complete(T0, "a1", "t1", "error", "partial"));
    state = red(state, complete(T0, "a2", "t1", "refusal", "no"));
    state = red(state, complete(T0, "a3", "t1", "aborted", ""));
    state = red(state, complete(T0, "a4", "t1", "answer", "   "));
    let rows: Vec<&AgentRec> = (1..=4)
        .map(|n| state.rows.get(&format!("a{n}")).unwrap())
        .collect();
    let statuses: Vec<Status> = rows.iter().map(|row| row.status).collect();
    assert_eq!(
        statuses,
        [
            Status::Failed,
            Status::Failed,
            Status::Killed,
            Status::Completed
        ]
    );
    let words: Vec<Option<Disposition>> = rows
        .iter()
        .map(|row| row.completion.as_ref().map(|c| c.disposition))
        .collect();
    assert_eq!(words, [Some(Disposition::Skipped); 4]);
    // an answer with no text is "no report"
    assert_eq!(
        rows[3].completion.as_ref().map(|c| c.answer.as_str()),
        Some("")
    );
    assert!(state.pending.is_empty());
}

#[test]
fn tool_and_step_observations_are_counted_once() {
    let mut state = AgentState::empty(SESSION);
    let tool = || Event::Tool {
        at: T0,
        agent_id: Some("a1".into()),
        tool_use_id: "u1".into(),
        tool: "Edit".into(),
        ok: true,
        path: Some("/r/x.ts".into()),
    };
    state = red(red(state, tool()), tool());
    let stepped = || Event::Step {
        at: T0,
        agent_id: "a1".into(),
        turn_id: "t".into(),
        index: 0.into(),
        answer: "hi".into(),
        tokens: 50,
    };
    state = red(red(state, stepped()), stepped());
    assert_eq!(state.rows.get("a1").unwrap().tools, 1);
    assert_eq!(state.rows.get("a1").unwrap().tokens, 50);
    assert_eq!(
        step_key(SESSION, Some("a1"), "t", &0.into()),
        r#"["sess-a","a1","t",0]"#
    );
    assert_eq!(
        step_key(SESSION, None, "t", &3.into()),
        r#"["sess-a",null,"t",3]"#
    );
    // a failed or denied edit is not a write
    let denied = Event::Tool {
        at: T0,
        agent_id: Some("a1".into()),
        tool_use_id: "u2".into(),
        tool: "Write".into(),
        ok: false,
        path: Some("/r/y.ts".into()),
    };
    state = red(state, denied);
    let paths: Vec<String> = state
        .rows
        .get("a1")
        .unwrap()
        .paths
        .values()
        .cloned()
        .collect();
    assert_eq!(paths, ["/r/x.ts"]);
}

#[test]
fn paths_are_normalized_and_a_state_is_keyed_by_its_session() {
    assert_eq!(
        normalize_path("src/../lib/./a.ts", "/repo/"),
        "/repo/lib/a.ts"
    );
    assert_eq!(normalize_path("/a//b/../c", "/x"), "/a/c");
}

/// `tests/redaction_table.json`, the table `bin/private.py` is held to.
const TABLE: [(&str, &str); 35] = [
    (
        "use sk-ant-api03-AbCdEf0123456789_-xyz here",
        "use [redacted] here",
    ),
    (
        "OPENAI sk-proj-0123456789abcdefghij0123",
        "OPENAI [redacted]",
    ),
    ("xai-0123456789abcdefghijKLMN", "[redacted]"),
    (
        "token ghp_0123456789abcdefghijABCDEFGHIJ0123 ok",
        "token [redacted] ok",
    ),
    (
        "github_pat_11ABCDEFG0123456789_abcdefghijklmnop",
        "[redacted]",
    ),
    (
        "id AKIAIOSFODNN7EXAMPLE and ASIAIOSFODNN7EXAMPLE",
        "id [redacted] and [redacted]",
    ),
    ("xoxb-1234567890-abcdefghij", "[redacted]"),
    (
        "curl -H \"Authorization: Bearer abc123.def456-ghi789\" x",
        "curl -H \"Authorization: Bearer [redacted]\" x",
    ),
    (
        "authorization: basic dXNlcjpwYXNzd29yZA==",
        "authorization: basic [redacted]",
    ),
    ("password=hunter2 next", "password=[redacted] next"),
    (
        "DB_PASSWORD: \"correct horse battery\"",
        "DB_PASSWORD: \"[redacted]\"",
    ),
    ("API_KEY='abc 123'", "API_KEY='[redacted]'"),
    (
        "{\"token\": \"abc.def\", \"ok\": 1}",
        "{\"token\": \"[redacted]\", \"ok\": 1}",
    ),
    ("?access_token=abc123&x=1", "?access_token=[redacted]&x=1"),
    (
        "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nabc\n-----END RSA PRIVATE KEY----- after",
        "[redacted] after",
    ),
    (
        "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk",
        "[redacted]",
    ),
    (
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpM",
        "[redacted]",
    ),
    (
        "the token budget is 10% of Claude tokens: 120k",
        "the token budget is 10% of Claude tokens: 120k",
    ),
    (
        "sk-short and ask-me-anything and task-12345678901234567890",
        "sk-short and ask-me-anything and task-12345678901234567890",
    ),
    ("git push origin feat/plugin", "git push origin feat/plugin"),
    ("Basic authentication works", "Basic authentication works"),
    (
        "Bearer authentication is used here",
        "Bearer authentication is used here",
    ),
    (
        "a cancellation token: stop work",
        "a cancellation token: stop work",
    ),
    ("token: string", "token: string"),
    ("token: string | null;", "token: string | null;"),
    ("password: required", "password: required"),
    (
        "const secret: Secret<Key> = load()",
        "const secret: Secret<Key> = load()",
    ),
    ("password: s3cret-swordfish!", "password: [redacted]"),
    (
        "password=\"correct horse battery\" now",
        "password=\"[redacted]\" now",
    ),
    ("secret: 'plain words in quotes'", "secret: '[redacted]'"),
    (
        "send Bearer abc123def456 here",
        "send Bearer [redacted] here",
    ),
    (
        "Authorization: Bearer abcdefghij",
        "Authorization: Bearer [redacted]",
    ),
    (
        "Authorization: Basic dXNlcjpwYXNzd29yZA== ok",
        "Authorization: Basic [redacted] ok",
    ),
    ("export TOKEN=abc123", "export TOKEN=[redacted]"),
    (
        "wrapped status token=abc123 done",
        "wrapped status token=[redacted] done",
    ),
];

#[test]
fn redaction_by_agents_ts_is_the_helper_s_table_and_then_some() {
    let say = |text: &str| redact_agent_value(&json!(text));
    for (input, expected) in TABLE {
        assert_eq!(say(input), json!(expected), "{input}");
    }
    let hash = say("hash 0123456789abcdef0123456789abcdef01234567 end");
    assert_eq!(hash, json!("hash [redacted] end"));
    // under 32 digits: a commit, not a secret
    assert_eq!(
        say("a 0123456789abcdef short"),
        json!("a 0123456789abcdef short")
    );
    assert_eq!(
        say("STRIPE_KEY=live_abc\nnext line"),
        json!("STRIPE_KEY=[redacted]\nnext line")
    );
    assert_eq!(
        say("export SIGNING_SECRET: plain\nok"),
        json!("export SIGNING_SECRET: [redacted]\nok")
    );
    assert_eq!(
        say("\x1b[31mred\x1b[0m \x1b]0;title\x07 bell\x07 ok"),
        json!("red  bell ok")
    );
    let nested = redact_agent_value(&json!({ "a": ["password=hunter2"], "n": 3 }));
    assert_eq!(nested, json!({ "a": ["password=[redacted]"], "n": 3 }));
    let twice = redact_agent_value(&say("password=hunter2 and sk-abcdefghijklmnopqrstuv"));
    assert_eq!(twice, json!("password=[redacted] and [redacted]"));
}

#[tokio::test]
async fn the_agents_file_is_written_at_once_then_at_most_every_five_seconds_terminal() {
    let rig = Rig::new();
    rig.options(false, "auto");
    rig.spawn_a1("tu-1").await;
    assert_eq!(rig.writes(), 1);
    let path = rig
        .home
        .join(".claude-self-reflect/csr-companion/agents/sess-a.json");
    assert!(path.exists());
    // through the helper's writer: its strings redacted, then serialized
    for n in 0..10 {
        rig.edit(Some("a1"), &format!("tool-{n}"), &format!("/repo/f{n}.ts"))
            .await;
    }
    rig.step(Some("a1"), "turn-1", 0).await;
    rig.reported("a1", "turn-1", "The report.").await;
    rig.advance(FILE_EVERY_MS as f64 - 1.).await;
    assert_eq!(rig.writes(), 1); // the burst is held back
    rig.advance(1.).await;
    assert!(until(|| rig.writes() == 2).await);
    rig.advance(FILE_EVERY_MS as f64 * 3.).await;
    assert_eq!(rig.writes(), 2); // and it was one write: nothing changed since
    let text = rig.file_text().unwrap();
    let file: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(file["session"], SESSION);
    assert_eq!(file["agents"][0]["tools"], 10);
    // the terminal event reached the file within the interval
    assert_eq!(file["agents"][0]["status"], "completed");
    assert_eq!(file["agents"][0]["completion"]["answer"], "The report.");
    assert!(utf8_length(&text) <= FILE_BYTES);
}

#[tokio::test]
async fn a_session_id_that_could_name_another_file_is_never_written() {
    let rig = Rig::build(World::default(), "../../etc/x", true);
    rig.spawn_a1("tu-1").await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(rig.writes(), 0);
    assert!(!rig.home.join(".claude-self-reflect").exists());
    assert!(!rig.home.join("etc").exists());
}

#[test]
fn planwrite_the_first_write_at_once_the_rest_one_trailing_write_a_lost_timer() {
    let mut state = red(AgentState::empty(SESSION), touch(T0, "a1", "Bash"));
    assert_eq!(plan_write(&mut state, T0).0, Act::Now);
    assert_eq!(
        plan_write(&mut state, T0 + 1000),
        (Act::Later, FILE_EVERY_MS - 1000)
    );
    // a timer is due: this change is covered by its write
    assert_eq!(plan_write(&mut state.clone(), T0 + 2000).0, Act::None);
    // the timer never fired (a reload): due is in the past
    assert_eq!(
        plan_write(&mut state.clone(), T0 + FILE_EVERY_MS + 1).0,
        Act::Now
    );
    // no agents, no file
    assert_eq!(plan_write(&mut AgentState::empty(SESSION), T0).0, Act::None);
}

#[test]
fn one_parent_and_seven_children_and_multibyte_reports_stay_within_16_kb_and_12() {
    let mut state = AgentState::empty(SESSION);
    let parent = red(state.clone(), touch(T0, "p", "Bash"))
        .rows
        .get("p")
        .unwrap()
        .clone();
    state.rows.insert(
        "p",
        AgentRec {
            kind: Some("Plan".into()),
            model: Some("opus".into()),
            ..parent
        },
    );
    for n in 0..7 {
        let spawn = Event::Spawn {
            at: T0,
            tool_use_id: format!("t{n}"),
            description: format!("Child {n}"),
            kind: Some("Explore".into()),
            parent: Some("p".into()),
            model: None,
            requests: vec![],
        };
        state = red(state, spawn);
        let spawned = Event::Spawned {
            at: T0,
            tool_use_id: format!("t{n}"),
            agent_id: Some(format!("c{n}")),
            model: Some("opus".into()),
        };
        state = red(state, spawned);
        let report = "界".repeat(700);
        state = red(
            state,
            complete(T0 + n, &format!("c{n}"), "turn-1", "answer", &report),
        );
    }
    let file = project_agents_file(&state, T0 + 1000);
    let mut named: Vec<&str> = file.agents.iter().map(|row| row.id.as_str()).collect();
    named.sort();
    assert_eq!(named, ["c0", "c1", "c2", "c3", "c4", "c5", "c6", "p"]);
    assert_eq!(file.agents[0].id, "p"); // the parent first
    assert_eq!(file.agents.len(), 8);
    assert!(file.agents[1..]
        .iter()
        .all(|row| row.parent.as_deref() == Some("p")));
    let bytes = utf8_length(&serde_json::to_string(&file).unwrap());
    assert!(bytes <= FILE_BYTES, "{bytes}");
    // squeezed, not emptied
    assert!(file
        .agents
        .iter()
        .all(|row| row.completion.as_ref().is_none_or(|c| !c.answer.is_empty())));
    // fourteen agents: twelve rows, the rest counted
    for n in 0..14 {
        state = red(state, touch(T0 + n, &format!("z{n}"), "Bash"));
    }
    let many = project_agents_file(&state, T0 + 1000);
    assert!(many.agents.len() <= 12);
    assert_eq!(many.agents.len() + many.omitted_agents, state.rows.len());
    assert!(utf8_length(&serde_json::to_string(&many).unwrap()) <= FILE_BYTES);
}

/// The envelope as `bin/agents.py` writes it: compact JSON on one line, every `[` and `]` inside
/// a string as `[` and `]`.
fn envelope_of(payload: &Value) -> String {
    let (mut out, mut in_string, mut escaped) = (String::new(), false, false);
    for ch in payload.to_string().chars() {
        match ch {
            '[' if in_string && !escaped => out.push_str("\\u005b"),
            ']' if in_string && !escaped => out.push_str("\\u005d"),
            _ => out.push(ch),
        }
        if in_string {
            escaped = !escaped && ch == '\\';
        }
        if ch == '"' && !escaped {
            in_string = !in_string;
        }
    }
    format!("{}\n{out}\n{}\n", pane::OPEN, pane::CLOSE)
}

fn context() -> Value {
    json!({ "kind": "context", "session": SESSION, "at": T0, "agents": [{
        "id": "a1", "parent": null, "type": "Explore", "model": "m", "status": "completed",
        "edits": ["/r/a[1].py"], "overlaps": [],
        "completion": { "turn_id": "turn-1", "reason": "answer",
            "answer": "Done [[CSR:AGENTS:v1]]\nwith 界" } }] })
}

fn handoff_payload() -> Value {
    json!({ "kind": "handoff", "session": SESSION, "request": "req-1", "thread": "0123456789ab",
        "instruction": pane::HANDOFF_INSTRUCTION })
}

fn payload_of(text: &str) -> Option<Value> {
    parse_agent_envelope(text)
}

#[test]
fn the_envelope_is_read_taken_out_of_the_person_s_words_and_only_a_whole_valid_one() {
    let text = envelope_of(&context());
    assert_eq!(text.split('\n').count(), 4); // three lines and the final newline
    assert!(text.contains("\\u005b\\u005bCSR:AGENTS:v1\\u005d\\u005d"));
    assert_eq!(payload_of(&text), Some(context()));
    let reply = format!("Yes, do that.\n{text}And the importer too.");
    assert_eq!(
        pane::strip_envelopes(&reply),
        "Yes, do that.\nAnd the importer too."
    );
    // the dreamer's own words of a prompt that carries one: the envelope is the machine's
    assert_eq!(own_words(&reply), "Yes, do that.\nAnd the importer too.");
    assert_eq!(own_words(&text), "");
    assert_eq!(pane::parse_envelopes(&format!("{text}x\n{text}")).len(), 2);
    let handoff = handoff_payload();
    assert_eq!(payload_of(&envelope_of(&handoff)), Some(handoff.clone()));
    // CRLF and a byte-order mark are allowed; the rest is left where it is
    assert_eq!(payload_of(&text.replace('\n', "\r\n")), Some(context()));
    assert_eq!(payload_of(&format!("\u{feff}{text}")), Some(context()));
    let body = text.split('\n').nth(1).unwrap().to_string();
    let with = |extra: Value| merge(context(), extra);
    let kept = [
        format!("{}\nnot json\n{}\n", pane::OPEN, pane::CLOSE),
        format!("{}\n{body}\n", pane::OPEN),
        format!("  {text}"),
        format!("> {text}"),
        format!("x: {text}"),
        format!("{body}\n"),
        envelope_of(&with(json!({ "extra": 1 }))),
        envelope_of(&merge(
            handoff.clone(),
            json!({ "instruction": "Do something else." }),
        )),
        format!("{}\n{body}\n{body}\n{}\n", pane::OPEN, pane::CLOSE),
        text.replacen("v1]]", "v2]]", 1),
        envelope_of(&with(json!({ "pad": "x".repeat(20_000) }))),
    ];
    for text in kept {
        assert_eq!(pane::strip_envelopes(&text), text);
        assert_eq!(payload_of(&text), None);
    }
    let prose = "Mind [[CSR:AGENTS:v1]] in prose";
    assert_eq!(own_words(prose), prose);
}

#[test]
fn the_size_limit_is_on_the_bytes_the_envelope_takes_in_the_text_line_endings() {
    let handoff = handoff_payload();
    let json = handoff.to_string();
    let exactly = |ends: &str| {
        let plain = format!("{}\n{json}\n{}\n", pane::OPEN, pane::CLOSE);
        // JSON ignores the padding: the LF envelope is exactly at the limit
        let body = format!(
            "{json}{}",
            " ".repeat(pane::MAX_BYTES - utf8_length(&plain))
        );
        format!("{}\n{body}\n{}\n", pane::OPEN, pane::CLOSE).replace('\n', ends)
    };
    let lf = exactly("\n");
    assert_eq!(utf8_length(&lf), pane::MAX_BYTES);
    assert_eq!(payload_of(&lf), Some(handoff)); // at the limit with LF: read
    let crlf = lf.replace('\n', "\r\n");
    assert_eq!(utf8_length(&crlf), pane::MAX_BYTES + 3);
    assert_eq!(payload_of(&crlf), None); // the same envelope with CRLF takes 3 more bytes: over
    assert_eq!(pane::strip_envelopes(&crlf), crlf);
}

#[test]
fn completion_news_is_the_claimed_runs_the_envelope_reports_by_identity() {
    let envelope = payload_of(&envelope_of(&context()));
    let key = completion_key(SESSION, "a1", "turn-1");
    let ledger = |word| Ledger::from([(key.clone(), word)]);
    let news = |envelope: Option<&Value>, ledger: &Ledger, wanted: &[String]| {
        completion_news(envelope, ledger, wanted)
    };
    assert_eq!(
        news(
            envelope.as_ref(),
            &ledger(Disposition::Claimed),
            std::slice::from_ref(&key)
        ),
        std::slice::from_ref(&key)
    );
    // not claimed: not this look's
    assert!(news(
        envelope.as_ref(),
        &ledger(Disposition::Pending),
        std::slice::from_ref(&key)
    )
    .is_empty());
    // already looked at
    assert!(news(
        envelope.as_ref(),
        &ledger(Disposition::Looked),
        std::slice::from_ref(&key)
    )
    .is_empty());
    let other = [completion_key(SESSION, "a9", "turn-1")];
    assert!(news(envelope.as_ref(), &ledger(Disposition::Claimed), &other).is_empty());
    assert!(news(
        None,
        &ledger(Disposition::Claimed),
        std::slice::from_ref(&key)
    )
    .is_empty());
    let mut failed = context();
    failed["agents"][0]["status"] = json!("failed");
    failed["agents"][0]["completion"] = Value::Null;
    let failed = payload_of(&envelope_of(&failed));
    assert!(news(
        failed.as_ref(),
        &ledger(Disposition::Claimed),
        std::slice::from_ref(&key)
    )
    .is_empty());
}

// ---------------------------------------------------------------- handing a thread to an agent

const REQUEST: &str = "req-0123456789abcdef";
const THREAD: &str = "0123456789ab";

fn handed(request: &str) -> String {
    let payload = json!({ "kind": "handoff", "session": SESSION, "request": request,
        "thread": THREAD, "instruction": pane::HANDOFF_INSTRUCTION });
    envelope_of(&payload)
}

/// What the pane's `handoffs/<session>.drafts.jsonl` holds: this request, drafted 5 s before.
fn draft(rig: &Rig) {
    let folder = rig.home.join(".claude-self-reflect/csr-companion/handoffs");
    std::fs::create_dir_all(&folder).unwrap();
    let line = json!({ "request": REQUEST, "thread": THREAD, "drafted_at": T0 - 5000 });
    std::fs::write(folder.join("sess-a.drafts.jsonl"), format!("{line}\n")).unwrap();
}

#[tokio::test]
async fn a_handed_thread_sent_by_the_person_bound_by_the_agent_call_that_carries_its() {
    let rig = Rig::new();
    draft(&rig);
    rig.submit(&format!("Take this one.\n{}", handed(REQUEST)))
        .await; // the person's Enter
    assert!(until(|| rig.handoffs_file().len() == 1).await);
    let first = rig.handoffs_file()[0].clone();
    assert_eq!(first["request"], REQUEST);
    assert_eq!(first["thread"], THREAD);
    assert_eq!(first["outcome"], "submitted");
    assert_eq!(first["submitted_at"], T0);
    assert_eq!(first["drafted_at"], T0 - 5000);
    assert_eq!(first["agent_id"], Value::Null);
    // the Agent call, with the envelope in its prompt, and the agent's first step before the
    // call's result
    let call = spawn_call(
        "tu-9",
        json!({ "prompt": format!("Do the task.\n{}", handed(REQUEST)),
            "description": "Handoff", "subagentType": "general-purpose" }),
    );
    let begun = rig.event("spawned", call).await;
    assert!(rig.state().spawns.contains("tu-9"));
    rig.step(Some("a1"), "turn-1", 0).await;
    rig.after(
        "spawned",
        json!({ "model": "m", "agentId": "a1" }),
        &begun.left,
    )
    .await;
    assert!(until(|| rig.handoffs_file()[0]["outcome"] == "bound").await);
    let bound = rig.handoffs_file()[0].clone();
    assert_eq!(bound["agent_id"], "a1");
    assert_eq!(bound["turn_id"], "turn-1");
    assert_eq!(bound["spawn_tool_use_id"], "tu-9");
    rig.reported("a1", "turn-1", "It is done.").await;
    assert!(until(|| rig.handoffs_file()[0]["outcome"] == "succeeded").await);
    let done = rig.handoffs_file()[0].clone();
    assert_eq!(done["ended_at"], T0);
    assert_eq!(done["reason"], Value::Null);
    // ids and words: no report, no prompt
    let text = std::fs::read_to_string(
        rig.home
            .join(".claude-self-reflect/csr-companion/handoffs/sess-a.json"),
    )
    .unwrap();
    assert!(!text.contains("It is done."));
}

#[tokio::test]
async fn a_prompt_that_did_not_enter_or_an_envelope_the_pane_never_drafted_is_no_handoff() {
    let rig = Rig::new();
    draft(&rig);
    rig.submit(&handed("req-ffffffffffffffff")).await;
    let foreign = json!({ "kind": "handoff", "session": "sess-b", "request": REQUEST,
        "thread": THREAD, "instruction": pane::HANDOFF_INSTRUCTION });
    rig.submit(&envelope_of(&foreign)).await;
    rig.submit("a bare [[CSR:AGENTS:v1]] marker").await;
    // and a prompt a hook dropped did not go
    let dropped = json!({ "text": handed(REQUEST), "drop": "not sent" });
    rig.after("digit", dropped, &None).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(rig.handoffs_file(), Vec::<Value>::new());
    assert!(rig.state().handoffs.is_empty());
}

fn submitted(at: i64, request: &str, thread: &str) -> HandoffEvent {
    HandoffEvent::Submitted {
        at,
        session: SESSION.into(),
        request: request.into(),
        thread: thread.into(),
        drafted_at: 0,
    }
}

/// The handoffs with `event` applied, and whether they changed (a new array in the TypeScript).
fn assoc(handoffs: &[AgentHandoff], event: HandoffEvent) -> (Vec<AgentHandoff>, bool) {
    let mut next = handoffs.to_vec();
    let changed = associate_handoff(&mut next, event);
    (next, changed)
}

#[test]
fn associatehandoff_exact_request_first_turn_the_run_that_ends_nothing_by_likeness() {
    let spawn = |requests: &[&str], tool_use_id: &str, nested| HandoffEvent::Spawn {
        requests: requests.iter().map(|r| r.to_string()).collect(),
        tool_use_id: tool_use_id.into(),
        nested,
    };
    let spawned = |tool_use_id: &str, agent_id: Option<&str>| HandoffEvent::Spawned {
        tool_use_id: tool_use_id.into(),
        agent_id: agent_id.map(str::to_string),
        turn: None,
    };
    let (sent, _) = assoc(&[], submitted(1, "r1", THREAD));
    // sent once
    assert!(!assoc(&sent, submitted(2, "r1", THREAD)).1);
    // a nested call's envelope binds nothing; a request nobody sent binds nothing
    assert!(!assoc(&sent, spawn(&["r1"], "u1", true)).1);
    assert!(!assoc(&sent, spawn(&["r9"], "u1", false)).1);
    let (carried, _) = assoc(&sent, spawn(&["r1"], "u1", false));
    assert_eq!(carried[0].spawn_tool_use_id.as_deref(), Some("u1"));
    assert_eq!(carried[0].outcome, "submitted");
    // a second call is not it
    assert!(!assoc(&carried, spawn(&["r1"], "u2", false)).1);
    // another call's result
    assert!(!assoc(&carried, spawned("u2", Some("a2"))).1);
    let (denied, _) = assoc(&carried, spawned("u1", None));
    assert_eq!(denied[0].outcome, "submitted"); // not handed off
    assert_eq!(denied[0].agent_id, None);
    assert_eq!(denied[0].reason.as_deref(), Some("spawn denied"));
    let (bound, _) = assoc(&carried, spawned("u1", Some("a1")));
    assert_eq!(bound[0].outcome, "bound");
    assert_eq!(bound[0].agent_id.as_deref(), Some("a1"));
    assert_eq!(bound[0].turn_id, None);
    let turn_of = |agent: &str, turn: &str| HandoffEvent::Turn {
        agent_id: agent.into(),
        turn_id: turn.into(),
    };
    let (turn, _) = assoc(&bound, turn_of("a1", "t1"));
    assert_eq!(turn[0].turn_id.as_deref(), Some("t1"));
    assert!(!assoc(&turn, turn_of("a1", "t2")).1); // the first turn only
    let ended = |at, agent: &str, turn: &str, reason: &str, empty| HandoffEvent::Ended {
        at,
        agent_id: agent.into(),
        turn_id: Some(turn.into()),
        reason: reason.into(),
        empty,
    };
    assert!(!assoc(&turn, ended(8, "a1", "t2", "error", false)).1); // another run's end
    assert!(!assoc(&turn, ended(8, "a2", "t1", "error", false)).1); // another agent's
    let table = [
        ("answer", false, "succeeded", None),
        ("answer", true, "failed", Some("no report")),
        ("error", false, "failed", Some("error")),
        ("refusal", false, "failed", Some("refusal")),
        ("aborted", false, "failed", Some("aborted")),
        ("killed", false, "failed", Some("killed")),
    ];
    for (reason, empty, outcome, why) in table {
        let (end, _) = assoc(&turn, ended(8, "a1", "t1", reason, empty));
        assert_eq!(end[0].outcome, outcome, "{reason}");
        assert_eq!(end[0].reason.as_deref(), why, "{reason}");
        assert_eq!(end[0].ended_at, Some(8), "{reason}");
    }
    // two requests in one prompt: which agent is whose cannot be told
    let (one, _) = assoc(&[], submitted(1, "r1", THREAD));
    let (two, _) = assoc(&one, submitted(1, "r2", "ba9876543210"));
    let (both, _) = assoc(&two, spawn(&["r1", "r2"], "u1", false));
    let seen: Vec<(&str, Option<&str>, Option<&str>)> = both
        .iter()
        .map(|h| {
            (
                h.outcome.as_str(),
                h.reason.as_deref(),
                h.spawn_tool_use_id.as_deref(),
            )
        })
        .collect();
    assert_eq!(seen, [("submitted", Some("ambiguous"), None); 2]);
}

#[test]
fn the_list_saying_a_bound_agent_failed_or_was_killed_settles_its_handoff_its() {
    let mut state = AgentState::empty(SESSION);
    state = red(
        state,
        Event::Handoff {
            at: 1,
            request: "r1".into(),
            thread: THREAD.into(),
            drafted_at: 0,
        },
    );
    state = red(
        state,
        Event::Spawn {
            at: 2,
            tool_use_id: "u1".into(),
            description: "h".into(),
            kind: Some("general-purpose".into()),
            parent: None,
            model: None,
            requests: vec!["r1".into()],
        },
    );
    state = red(
        state,
        Event::Spawned {
            at: 3,
            tool_use_id: "u1".into(),
            agent_id: Some("a1".into()),
            model: Some("m".into()),
        },
    );
    assert_eq!(state.handoffs[0].outcome, "bound");
    assert_eq!(state.handoffs[0].agent_id.as_deref(), Some("a1"));
    let mut gone = state.clone();
    assert!(!reconcile(&mut gone, &[], 10)); // gone from the list, or stalled: not an outcome
    assert_eq!(gone.handoffs, state.handoffs);
    let list = |status| [listed("a1", "general-purpose", "h", status)];
    let idle = recon(state.clone(), &list("idle"), 10);
    assert_eq!(idle.handoffs[0].outcome, "bound");
    let killed = recon(state.clone(), &list("killed"), 10);
    assert_eq!(killed.handoffs[0].outcome, "failed");
    assert_eq!(killed.handoffs[0].reason.as_deref(), Some("killed"));
    assert_eq!(killed.handoffs[0].ended_at, Some(10));
}

// ---------------------------------------------------------------- companion.test.ts

/// A dreaming session whose brief is `brief`.
fn dreaming_rig(brief: String) -> Rig {
    let rig = Rig::dreaming(World::default());
    rig.set_brief(brief);
    rig
}

#[tokio::test]
async fn completion_usage_never_counts_towards_the_share() {
    let rig = Rig::dreaming(World::default());
    rig.steps(3, Some("sub-1"), None).await;
    // the turn's total: not Claude's spend either
    let usage = json!({ "input_tokens": 4500, "output_tokens": 500,
        "cache_creation_input_tokens": 0, "cache_read_input_tokens": 200_000, "model": "opus" });
    for _ in 0..2 {
        let e = json!({ "answer": REPORT, "durationMs": 1000, "isAborted": false,
            "turnId": "turn-1", "reason": "answer", "agentId": "sub-1", "usage": usage });
        rig.event("turn_complete", e).await;
    }
    rig.steps(5, Some("sub-1"), None).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let pace = row(rig.cx(), "pace");
    assert_eq!(num(&pace, "claude"), 0.);
    assert_eq!(pace["turns"], Value::Null);
}

/// A report, the main session's next step and the 20 s timer all reach for it: one claim, one
/// look, whatever the order.
async fn one_claim(order: &str) {
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT)]));
    rig.roomy(json!({}));
    rig.reported("a1", "turn-1", REPORT).await;
    assert_eq!(rig.word("a1", "turn-1"), "pending"); // waiting for the next step, or 20 s
    match order {
        "step first" => {
            rig.steps(1, None, None).await;
            assert!(until(|| !rig.forks().is_empty()).await);
            rig.advance(20_000.).await;
        }
        "timer first" => {
            rig.advance(20_000.).await;
            assert!(until(|| !rig.forks().is_empty()).await);
            rig.steps(1, None, None).await;
        }
        _ => {
            tokio::join!(rig.steps(1, None, None), rig.advance(20_000.));
        }
    }
    assert!(until(|| rig.word("a1", "turn-1") == "looked").await);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(rig.forks().len(), 1);
    assert_eq!(rig.word("a1", "turn-1"), "looked");
    assert!(rig.forks()[0].contains(REPORT)); // the agent's own words, quoted
    let looks: Vec<(Value, Value)> = rig
        .looks()
        .iter()
        .filter(|r| r["kind"] == "look" || r["kind"] == "dream")
        .map(|r| (r["trigger"].clone(), r["completions"].clone()))
        .collect();
    assert_eq!(looks, [(json!("agent"), json!(1))]);
    // the chapter's own schedule is not moved by it
    assert_eq!(rig.chapter_due(), 10_000.);
}

#[tokio::test]
async fn completion_step_and_timer_share_one_claim() {
    one_claim("step first").await;
}

#[tokio::test]
async fn completion_step_and_timer_share_one_claim_the_timer_first() {
    one_claim("timer first").await;
}

#[tokio::test]
async fn completion_step_and_timer_share_one_claim_both_at_once() {
    one_claim("together").await;
}

#[tokio::test]
async fn a_timer_finding_the_main_session_at_work_leaves_the_report_for_its_next_step() {
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT)]));
    rig.roomy(json!({}));
    rig.event("turn_start", json!({ "text": "go", "turnId": "main-1" }))
        .await;
    rig.reported("a1", "turn-1", REPORT).await;
    rig.advance(25_000.).await; // 20 s passed with the main turn still running
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(rig.forks().is_empty());
    assert_eq!(rig.word("a1", "turn-1"), "pending"); // not consumed
    rig.steps(1, None, None).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    assert_eq!(rig.forks().len(), 1);
}

fn two_replies() -> World {
    World {
        replies: vec!["dream: none".into(), DREAM_REPLY.into()],
        ..World::default()
    }
}

/// The chapter's look is due at the next step.
fn due_now(rig: &Rig) {
    let cx = rig.cx();
    put(
        cx,
        "chapter",
        merge(fresh_chapter(&cx.sid, T0), json!({ "due": 1, "gen": 1 })),
    );
}

#[tokio::test]
async fn a_timer_that_finds_the_main_session_just_stepped_tries_again_once_the_idle_time() {
    let rig = Rig::dreaming(two_replies());
    rig.set_brief(brief_with(&[said("a1", REPORT)]));
    rig.hold_forks();
    rig.roomy(json!({}));
    due_now(&rig);
    rig.reported("a1", "turn-1", REPORT).await; // the 20 s timer starts now
    rig.advance(10_000.).await;
    // the main session steps at 10 s: its own look starts (the fork waits on the gate), so the
    // report is not taken
    rig.steps(1, None, None).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    // the timer fires at 20 s: the last step was 10 s ago, a look is running
    rig.advance(10_000.).await;
    rig.release();
    assert!(until(|| rig.looked_at()).await);
    assert_eq!(rig.word("a1", "turn-1"), "pending");
    // the idle time it still lacked is up: it looks without waiting for a prompt
    rig.advance(10_000.).await;
    assert!(until(|| rig.forks().len() >= 2).await);
    assert!(until(|| rig.word("a1", "turn-1") == "looked").await);
    assert_eq!(rig.forks().len(), 2);
    assert_eq!(rig.word("a1", "turn-1"), "looked");
}

#[tokio::test]
async fn a_timer_that_finds_a_look_already_running_tries_once_more_after_it() {
    let rig = Rig::dreaming(two_replies());
    rig.set_brief(brief_with(&[said("a1", REPORT)]));
    rig.hold_forks();
    rig.roomy(json!({}));
    due_now(&rig);
    rig.steps(1, None, None).await; // the main session's own look starts; its fork waits
    assert!(until(|| !rig.forks().is_empty()).await);
    rig.reported("a1", "turn-1", REPORT).await; // the report comes in while that look runs
    rig.advance(25_000.).await; // its timer finds the look still running
    assert_eq!(rig.forks().len(), 1);
    rig.release();
    assert!(until(|| rig.looked_at()).await);
    rig.advance(25_000.).await; // and tries again, once, after
    assert!(until(|| rig.forks().len() >= 2).await);
    assert!(until(|| rig.word("a1", "turn-1") == "looked").await);
    assert_eq!(rig.forks().len(), 2);
    assert_eq!(rig.word("a1", "turn-1"), "looked");
}

#[tokio::test]
async fn a_report_stranded_behind_a_long_look_is_offered_once_the_look_ends_without() {
    let rig = Rig::dreaming(two_replies());
    rig.set_brief(brief_with(&[said("a1", REPORT)]));
    rig.hold_forks();
    rig.roomy(json!({}));
    due_now(&rig);
    rig.steps(1, None, None).await; // the main session's own look starts; its fork waits
    assert!(until(|| !rig.forks().is_empty()).await);
    rig.reported("a1", "turn-1", REPORT).await; // the report comes in while that look runs
    rig.advance(25_000.).await; // its timer fires with the look still running, and the retry
    rig.advance(25_000.).await;
    assert_eq!(rig.forks().len(), 1);
    assert_eq!(rig.word("a1", "turn-1"), "pending");
    rig.release(); // the look ends: no step, no turn, no new report follows
    assert!(until(|| rig.looked_at()).await);
    rig.advance(25_000.).await;
    assert!(until(|| rig.forks().len() >= 2).await);
    assert!(until(|| rig.word("a1", "turn-1") == "looked").await);
    assert_eq!(rig.forks().len(), 2);
    assert_eq!(rig.word("a1", "turn-1"), "looked");
}

/// Not in the TypeScript tests: the timer's own check. A timer armed while the loop was idle,
/// and fired after a turn began, leaves the report for the turn's next step.
#[tokio::test]
async fn a_timer_armed_while_idle_leaves_the_report_when_a_turn_began_before_it_fired() {
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT)]));
    rig.roomy(json!({}));
    rig.reported("a1", "turn-1", REPORT).await; // armed: the loop is idle
    rig.event("turn_start", json!({ "text": "go", "turnId": "main-1" }))
        .await;
    rig.advance(25_000.).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(rig.forks().is_empty());
    assert_eq!(rig.word("a1", "turn-1"), "pending");
}

/// Not in the TypeScript tests: "once more" is once. A timer that finds a look running tries
/// again after the wait, and that one gives up.
#[tokio::test]
async fn a_timer_that_finds_a_look_running_tries_once_more_and_then_leaves_it() {
    let rig = Rig::dreaming(two_replies());
    rig.set_brief(brief_with(&[said("a1", REPORT)]));
    rig.hold_forks();
    rig.roomy(json!({}));
    due_now(&rig);
    rig.steps(1, None, None).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    // The running look keeps its lease fresh with a timer of its own.
    let renewing = rig.fx.timers.pending();
    assert_eq!(renewing, 1);
    rig.reported("a1", "turn-1", REPORT).await;
    rig.advance(25_000.).await;
    assert_eq!(rig.fx.timers.pending(), renewing + 1); // the one more try
    rig.advance(25_000.).await;
    assert_eq!(rig.fx.timers.pending(), renewing); // and no third
}

#[tokio::test]
async fn completion_claim_survives_reload_and_fork_failure() {
    let world = World {
        fork_fails: true,
        ..World::default()
    };
    let rig = Rig::dreaming(world);
    rig.set_brief(brief_with(&[said("a1", REPORT)]));
    rig.roomy(json!({}));
    rig.reported("a1", "turn-1", REPORT).await;
    rig.steps(1, None, None).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    assert!(until(|| rig.word("a1", "turn-1") == "failed").await);
    // the fork threw: the chance is spent, and was charged as a guess, as any look's is
    assert!(num(&row(rig.cx(), "pace"), "dreamer") > 0.);
    // a reload: the module starts again over the same state and store
    rig.start().await;
    rig.advance(60_000.).await;
    rig.steps(3, None, None).await;
    rig.advance(60_000.).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(rig.forks().len(), 1); // a claimed completion is never retried
    assert_eq!(rig.word("a1", "turn-1"), "failed");
}

#[tokio::test]
async fn a_pending_completion_a_reload_finds_is_offered_once() {
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT)]));
    rig.roomy(json!({}));
    rig.reported("a1", "turn-1", REPORT).await;
    rig.start().await; // the module was reloaded before the 20 s were up
    rig.start().await;
    rig.advance(20_000.).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    rig.advance(120_000.).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(rig.forks().len(), 1);
    assert_eq!(rig.word("a1", "turn-1"), "looked");
}

#[tokio::test]
async fn budget_blocked_completion_is_not_rearmed() {
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT)]));
    // 10 tokens of Claude's for each of the dreamer's: 41,000 does not fit in 10,000
    rig.roomy(json!({ "claude": 100_000 }));
    rig.reported("a1", "turn-1", REPORT).await;
    rig.steps(1, None, None).await;
    assert!(until(|| rig.looked_at()).await);
    assert!(rig.forks().is_empty());
    let looks = rig.looks();
    assert_eq!(looks.len(), 1);
    assert_eq!(looks[0]["outcome"], "budget");
    assert_eq!(looks[0]["trigger"], "agent");
    assert_eq!(looks[0]["completions"], 1);
    assert_eq!(rig.word("a1", "turn-1"), "skipped"); // the opportunity was consumed
    rig.roomy(json!({})); // Claude spends plenty more
    rig.steps(4, None, None).await;
    rig.advance(120_000.).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(rig.forks().is_empty()); // and the finished agent is not looked at for it
    assert_eq!(
        rig.looks().iter().filter(|r| r["kind"] == "look").count(),
        1
    );
    assert_eq!(rig.chapter_due(), 10_000.);
}

#[tokio::test]
async fn overshoot_debt_blocks_completion_look() {
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT)]));
    let owing = json!({ "claude": 1_000_000, "dreamer": 300_000, "looks": 3, "lastLook": 41_000 });
    rig.roomy(owing); // over its share, and owed
    rig.reported("a1", "turn-1", REPORT).await;
    rig.steps(1, None, None).await;
    assert!(until(|| rig.looked_at()).await);
    assert!(rig.forks().is_empty());
    let looks = rig.looks();
    assert_eq!(looks.len(), 1);
    assert_eq!(
        (looks[0]["outcome"].clone(), looks[0]["trigger"].clone()),
        (json!("budget"), json!("agent"))
    );
    assert!(looks[0]["owed"].as_f64().unwrap() > 0.);
    assert_eq!(rig.word("a1", "turn-1"), "skipped");
    // the debt paid by Claude's spending, a later report gets its look
    let paid = json!({ "claude": 20_000_000, "dreamer": 300_000, "looks": 3, "lastLook": 41_000 });
    rig.roomy(paid);
    // the agents file now holds both
    rig.set_brief(brief_with(&[
        said("a1", REPORT),
        said("a2", "A second report."),
    ]));
    rig.reported("a2", "turn-1", "A second report.").await;
    rig.steps(1, None, None).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    assert_eq!(rig.forks().len(), 1);
    assert!(rig.forks()[0].contains("A second report."));
    assert!(!rig.forks()[0].contains(REPORT));
}

#[tokio::test]
async fn simultaneous_completions_coalesce() {
    let brief = brief_with(&[
        said("a1", "Report one."),
        said("a2", "Report two."),
        Said {
            turn: "turn-4",
            ..said("a3", "Report three.")
        },
    ]);
    let rig = dreaming_rig(brief);
    rig.roomy(json!({}));
    tokio::join!(
        rig.reported("a1", "turn-1", "Report one."),
        rig.reported("a2", "turn-1", "Report two."),
        rig.reported("a3", "turn-4", "Report three."),
    );
    rig.steps(1, None, None).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    let all_looked = || {
        let words = rig.ledger();
        words.len() == 3 && words.values().all(|word| word == "looked")
    };
    assert!(until(all_looked).await);
    rig.advance(60_000.).await;
    assert_eq!(rig.forks().len(), 1); // one look for the three
    for text in ["Report one.", "Report two.", "Report three."] {
        assert!(rig.forks()[0].contains(text), "{text}");
    }
    assert_eq!(
        rig.ledger().values().cloned().collect::<Vec<_>>(),
        ["looked"; 3]
    );
    let counts: Vec<Value> = rig
        .looks()
        .iter()
        .filter(|r| r["kind"] == "look" || r["kind"] == "dream")
        .map(|r| r["completions"].clone())
        .collect();
    assert_eq!(counts, [json!(3)]);
}

#[tokio::test]
async fn running_failed_and_empty_agents_never_trigger_looks() {
    let rig = Rig::dreaming(World::default());
    rig.roomy(json!({}));
    rig.steps(4, Some("run-1"), None).await; // still running: no report
    rig.finish("bad-1", "t", "half a report", "error").await;
    rig.finish("bad-2", "t", "no", "refusal").await;
    rig.finish("bad-3", "t", "", "aborted").await;
    rig.reported("empty-1", "t", "").await;
    rig.reported("empty-2", "t", "  \n ").await;
    rig.advance(25_000.).await;
    rig.steps(3, None, None).await;
    rig.advance(120_000.).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(rig.forks().is_empty());
    assert!(rig.ledger().is_empty()); // none was ever offered a look
    assert!(rig.looks().iter().all(|r| r["kind"] != "look"));
}

/// The sweep of the per-session keys of old sessions is the pace's (`sweepSessions`, at the
/// start): it names `agentLedger` among them. This runs against the whole table.
#[tokio::test]
async fn the_ledger_of_an_old_session_is_swept_with_its_other_per_session_keys() {
    let rig = Rig::dreaming(World::default());
    let full = crate::companion::app::table();
    let cx = rig.cx();
    for n in 1..=22 {
        let old = format!("old-{n}");
        cx.store
            .set(&old, "chapter", &fresh_chapter(&old, n))
            .unwrap();
        cx.store
            .set(&old, AGENT_LEDGER, &json!({ r#"["x","y","z"]"#: "looked" }))
            .unwrap();
    }
    cx.store
        .set(SID, AGENT_LEDGER, &json!({ "k": "pending" }))
        .unwrap();
    let e = json!({ "cwd": "/repo", "surface": "terminal", "isInteractive": true, "startedAt": 0 });
    run(&full, "start", cx, e).await;
    let has = |sid: &str| cx.store.get(sid, AGENT_LEDGER).unwrap().is_some();
    assert!(until(|| !has("old-1")).await);
    assert!(!has("old-1"));
    assert!(!has("old-2"));
    assert!(has("old-3")); // the newest 20 are kept
    assert!(has("old-22"));
    assert!(has(SID));
}

#[tokio::test]
async fn a_claimed_completion_the_agents_file_did_not_carry_is_no_news_the_look_is_spent() {
    let rig = Rig::dreaming(World::default()); // the brief is CSR's lines alone: no envelope
    rig.roomy(json!({}));
    put(rig.cx(), "briefFacts", json!(facts(BRIEF))); // and CSR has nothing new in them
    rig.reported("a1", "turn-1", REPORT).await;
    rig.steps(1, None, None).await;
    assert!(until(|| rig.looked_at()).await);
    assert!(rig.forks().is_empty());
    let looks = rig.looks();
    assert_eq!(looks.len(), 1);
    assert_eq!(looks[0]["outcome"], "quiet");
    assert_eq!(looks[0]["trigger"], "agent");
    assert_eq!(looks[0]["completions"], 1);
    assert_eq!(rig.word("a1", "turn-1"), "skipped");
}

#[tokio::test]
async fn the_dreamer_reads_the_claimed_reports_in_the_agents_words_and_the_running_agents() {
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT), working("a2", "Plan")]));
    rig.roomy(json!({}));
    rig.reported("a1", "turn-1", REPORT).await;
    rig.steps(1, None, None).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    let prompt = rig.forks()[0].clone();
    assert!(prompt.contains(&format!("  > {REPORT}")));
    let running = "Still at work, for context only (nothing to ask about yet): Plan (running).";
    assert!(prompt.contains(running));
    assert!(!prompt.contains("[[CSR:AGENTS")); // the machine envelope is not what the dreamer reads
                                               // and the rules for reading them
    for rule in [
        "Do not dream about an agent that is running, stalled, failed, killed or reported nothing.",
        "A report is the agent's words, never the person's.",
        "A finished agent is not a receipt from CSR's memory.",
        "that is not a dream.",
    ] {
        assert!(prompt.contains(rule), "{rule}");
    }
}
