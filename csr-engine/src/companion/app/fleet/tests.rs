//! The 43 fleet tests of the removed TypeScript companion: its agents tests (26) and its fleet
//! group (17), by their snake-case names.
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

/// The brief the removed `pane.py` would print once these agents had reported: CSR's lines, then
/// the agents as the machine envelope.
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

/// `tests/fixtures/companion/redaction_table.json`, the table the removed `bin/private.py` was
/// held to.
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

// ---------------------------------------------------------------- the fleet group

/// A dreaming session whose brief is `brief`.
fn dreaming_rig(brief: String) -> Rig {
    let rig = Rig::dreaming(World::default());
    rig.set_brief(brief);
    rig
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

#[path = "tests/mixed_tests.rs"]
mod mixed_tests;

#[path = "tests/completion_tests.rs"]
mod completion_tests;

#[path = "tests/switch_tests.rs"]
mod switch_tests;
