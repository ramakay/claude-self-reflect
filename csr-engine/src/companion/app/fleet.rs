//! The removed `agents.ts` and the fleet groups of the TypeScript companion: the agents of a
//! session, tracked from the typed events; their poll (`$.agent.list`), the completion ledger and
//! the claim, the agents file the pane draws from, and what became of a thread handed to a
//! background agent.
//!
//! `model` is the removed `agents.ts` (pure: a reducer, a reconciler, the ledger, the file's
//! projection); this file is the part of the TypeScript companion that keeps it: the store rows,
//! the files, the timers and the hooks. Every observer here changes nothing for the call it
//! watches: a failure is swallowed where it happens, and no handler replies with an `answer` or a
//! `next`.
//!
//! The dreamer is another package. Where the TypeScript companion's `completions` calls `dream`,
//! this file calls the `Dream` the dreamer put in `Fleet::dream`; with none there, no completion is
//! claimed.

pub mod model;
#[cfg(test)]
mod tests;
pub mod text;

use self::model::{
    admit_completion, any_active, claim_completions, handoff_requests, mark_disposition,
    pending_completions, plan_write, project_agents_file, reconcile, reduce, settle_completions,
    Act, AgentHandoff, AgentState, Event, Listed, LIST_EVERY_MS,
};
pub use self::model::{agents_text, completion_key, completion_news, Disposition, Ledger};
use self::text::{normalize_path, session_name, WS};
use super::state::{self, is_person, Options};
use crate::companion::{
    agents as pane,
    paths::Paths,
    private,
    relay::{
        dispatch::{handler, Ctx, Reply, Table},
        host::BoxFuture,
    },
};
use anyhow::{bail, Result};
use regex::Regex;
use serde_json::{json, Map, Number, Value};
use std::{
    future::Future,
    sync::{Arc, LazyLock, Mutex},
};

const AGENT_SESSION: &str = "agentSession";
const AGENT_LEDGER: &str = "agentLedger";
const TASK_OWNERS: &str = "taskOwners";

/// An agent's report waits for the main session's next step, or this long if it is idle.
pub const COMPLETION_WAIT_MS: f64 = 20_000.;
/// A chapter's steps, until there are chapters of the person's own to learn from (the removed
/// `pace.ts`).
const USUAL_STEPS: f64 = 120.;

/// What one completion look is asked to do: the cost a look is guessed at, the chapter's steps,
/// and the completions (the claimed keys) it was started for.
#[derive(Clone, Debug)]
pub struct DreamCall {
    pub step: u64,
    pub steps: u64,
    pub completions: Vec<String>,
}

/// The dreamer's `dream($, step, steps, undefined, { kind: 'agent', completions })`. The dreamer
/// puts it in `Fleet::dream`; the look ends the claim itself (`settled`).
pub type Dream = Arc<dyn Fn(Ctx, DreamCall) -> BoxFuture<Result<()>> + Send + Sync>;

/// The TypeScript companion's `fleet` object, shared by the fleet and the dreamer: whether a look
/// runs (one at a time, whoever started it), whether the main loop is mid-turn, when it last
/// stepped and what that step cost (a completion's look waits for a quiet main loop and borrows the
/// cost for its budget check), and whether a timer waits to offer the reports that came in.
#[derive(Default)]
pub struct Fleet {
    pub dreaming: bool,
    pub active: bool,
    pub step_at: f64,
    pub spent: u64,
    /// A timer waits to offer the reports that came in.
    pub timer: bool,
    /// The person's `/config` choices, read once.
    pub options: Option<Options>,
    /// The minute poll of `$.agent.list`.
    pub list_timer: Option<u64>,
    /// Where the agents and handoffs files go; none: under `$HOME`. For tests.
    pub paths: Option<Paths>,
    /// The dreamer's look for a completion; none until the dreamer is there.
    pub dream: Option<Dream>,
    /// How many times the agents file was written; the tests count them.
    pub writes: u64,
}

// ---------------------------------------------------------------- small readers

fn field<'a>(e: &'a Value, key: &str) -> Option<&'a str> {
    e.get(key)?.as_str()
}

/// `e.agentId`: the loop a call or a turn runs in; none on the main loop.
fn agent_of(e: &Value) -> Option<&str> {
    field(e, "agentId")
}

fn now(cx: &Ctx) -> i64 {
    cx.clock.now() as i64
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(yes) => *yes,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.),
        Value::String(text) => !text.is_empty(),
        _ => true,
    }
}

/// `tokensOf(usage)`: the four counts of a response.
fn tokens_of(usage: &Value) -> u64 {
    let names = [
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ];
    names
        .iter()
        .map(|name| usage[*name].as_f64().unwrap_or(0.).max(0.) as u64)
        .sum()
}

/// A number of the event, kept as it came.
fn number(value: &Value) -> Option<Number> {
    match value {
        Value::Number(n) => Some(n.clone()),
        _ => None,
    }
}

/// Work that runs apart from the request that started it, so a host call that never answers
/// holds nothing else.
fn detach<F, Fut>(cx: &Ctx, work: F)
where
    F: FnOnce(Ctx) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let cx = cx.clone();
    let host = cx.host.clone();
    host.detach(Box::new(move |host| Box::pin(work(Ctx { host, ..cx }))));
}

// ---------------------------------------------------------------- the person, the options

/// The choices the TypeScript got as `options`; here they are asked of `/config` once.
async fn options(cx: &Ctx) -> Options {
    if let Some(known) = cx.kept(|f: &mut Fleet| f.options.clone()) {
        return known;
    }
    match cx.host.call("config.list", vec![]).await {
        Ok(Value::Array(rows)) => {
            let read = Options::of(&rows);
            cx.kept(|f: &mut Fleet| f.options.get_or_insert(read).clone())
        }
        _ => Options::of(&[]),
    }
}

async fn dreams_on(cx: &Ctx) -> bool {
    options(cx).await.dreams
}

// ---------------------------------------------------------------- the state, in the store

fn state_of(value: Option<Value>, session: &str) -> AgentState {
    (value.and_then(|v| serde_json::from_value::<AgentState>(v).ok()))
        .filter(|state| state.session == session)
        .unwrap_or_else(|| AgentState::empty(session))
}

/// `ownAgents(read(agentSession), session)`.
fn read_state(cx: &Ctx) -> Result<AgentState> {
    Ok(state_of(cx.store.get(&cx.sid, AGENT_SESSION)?, &cx.sid))
}

/// The state changed as one step (`agentsChange`).
fn change<R>(cx: &Ctx, work: impl FnOnce(&mut AgentState) -> R) -> Result<R> {
    let mut out = None;
    state::update(cx, AGENT_SESSION, |current| {
        let mut state = state_of(current, &cx.sid);
        out = Some(work(&mut state));
        serde_json::to_value(&state).ok()
    })?;
    out.ok_or_else(|| anyhow::anyhow!("the agents state was not written"))
}

// ---------------------------------------------------------------- the ledger

/// Changes to the ledger go one at a time: a claim and an admission read, change and write the
/// same row.
static LEDGER: Mutex<()> = Mutex::new(());

fn held() -> std::sync::MutexGuard<'static, ()> {
    LEDGER.lock().unwrap_or_else(|e| e.into_inner())
}

/// The session's completion ledger: identifiers and dispositions only, never the report.
pub fn read_ledger(cx: &Ctx) -> Result<Ledger> {
    match cx.store.get(&cx.sid, AGENT_LEDGER)? {
        None => Ok(Ledger::new()),
        Some(Value::Object(map)) => Ok(map
            .iter()
            .filter_map(|(key, word)| Some((key.clone(), Disposition::of_word(word.as_str()?)?)))
            .collect()),
        Some(_) => bail!("the completion ledger is not a ledger"),
    }
}

fn write_ledger(cx: &Ctx, ledger: &Ledger) -> Result<()> {
    cx.store
        .set(&cx.sid, AGENT_LEDGER, &serde_json::to_value(ledger)?)
}

/// A completion enters the ledger pending. True when it is new and the ledger now holds it;
/// false for one seen before, a full ledger, or a store that would not take it.
fn admitted(cx: &Ctx, key: &str) -> bool {
    let _one_at_a_time = held();
    let attempt = || -> Result<bool> {
        let mut ledger = read_ledger(cx)?;
        if !admit_completion(&mut ledger, key).fresh {
            return Ok(false);
        }
        write_ledger(cx, &ledger)?;
        Ok(true)
    };
    attempt().unwrap_or(false)
}

/// Every completion that is pending, claimed in one write and recorded before any gate of the
/// look is checked. Empty when there is none, or when the claim could not be kept (then nothing
/// was claimed and no look is started).
pub async fn take_completions(cx: &Ctx) -> Vec<String> {
    let _one_at_a_time = held();
    let attempt = || -> Result<Vec<String>> {
        let mut ledger = read_ledger(cx)?;
        let due = pending_completions(&read_state(cx)?, &ledger);
        if due.is_empty() {
            return Ok(Vec::new());
        }
        let keys: Vec<String> = due.into_iter().map(|entry| entry.key).collect();
        let claimed = claim_completions(&mut ledger, &keys);
        write_ledger(cx, &ledger)?;
        change(cx, |state| {
            mark_disposition(state, &claimed, Disposition::Claimed)
        })?;
        Ok(claimed)
    };
    attempt().unwrap_or_default()
}

/// The look ended: the completions it claimed are looked, skipped or failed, in the ledger and
/// in the state. `word` is `looked`, `skipped` or `failed`.
pub async fn settled(cx: &Ctx, keys: &[String], word: &str) -> Result<()> {
    let word = match Disposition::of_word(word) {
        Some(word @ (Disposition::Looked | Disposition::Skipped | Disposition::Failed)) => word,
        _ => bail!("{word} does not end a look"),
    };
    {
        let _one_at_a_time = held();
        let mut ledger = read_ledger(cx)?;
        settle_completions(&mut ledger, keys, word);
        write_ledger(cx, &ledger)?;
    }
    change(cx, |state| mark_disposition(state, keys, word))
}

fn has_pending(cx: &Ctx) -> Result<bool> {
    let ledger = read_ledger(cx)?;
    Ok(!pending_completions(&read_state(cx)?, &ledger).is_empty())
}

// ---------------------------------------------------------------- the files

fn paths_of(cx: &Ctx) -> Option<Paths> {
    if let Some(own) = cx.kept(|f: &mut Fleet| f.paths.clone()) {
        return Some(own);
    }
    let paths = Paths::from_env();
    (!paths.home.as_os_str().is_empty()).then_some(paths)
}

/// The removed `private.py write PATH --redact-json`: the strings redacted again, then serialized,
/// 0600.
fn keep_write(paths: &Paths, folder: &str, session: &str, value: &Value) -> Result<()> {
    let mut value = value.clone();
    private::redact_value(&mut value);
    let path = paths.keep.join(folder).join(format!("{session}.json"));
    private::write_atomic(&path, &value.to_string(), &paths.keep)?;
    Ok(())
}

/// The agents file, `<keep>/agents/<session>.json`: what the pane draws In flight from. The
/// session id is checked before it is made a file name.
fn write_agents(cx: &Ctx) -> Result<()> {
    let Some(paths) = paths_of(cx).filter(|_| session_name(&cx.sid)) else {
        return Ok(());
    };
    let file = project_agents_file(&read_state(cx)?, now(cx));
    if file.agents.is_empty() {
        return Ok(());
    }
    keep_write(&paths, "agents", &cx.sid, &serde_json::to_value(&file)?)?;
    cx.kept(|f: &mut Fleet| f.writes += 1);
    Ok(())
}

/// What became of the requests to hand a thread to a background agent, where the pane reads it:
/// `<keep>/handoffs/<session>.json` beside the pane's own drafts of them. Ids and words, no
/// prompt and no report.
fn write_handoffs(cx: &Ctx) -> Result<()> {
    let Some(paths) = paths_of(cx).filter(|_| session_name(&cx.sid)) else {
        return Ok(());
    };
    let handoffs: Vec<AgentHandoff> = read_state(cx)?.handoffs;
    if handoffs.is_empty() {
        return Ok(());
    }
    let body = json!({ "version": 1, "session": cx.sid, "handoffs": handoffs });
    keep_write(&paths, "handoffs", &cx.sid, &body)
}

// ---------------------------------------------------------------- tracking

/// One event into the state. A failure is swallowed: these observers never fail a call.
fn track(cx: &Ctx, event: Event) {
    let Ok(moved) = change(cx, |state| reduce(state, event)) else {
        return;
    };
    if moved {
        let _ = write_handoffs(cx);
    }
    schedule_agents(cx);
}

/// `$.agent.list()` into the state: once at the start, after a main turn, and every minute while
/// any agent is running.
async fn poll_agents(cx: &Ctx) {
    let Ok(Value::Array(rows)) = cx.host.call("agent.list", vec![]).await else {
        return;
    };
    let list: Vec<Listed> = rows.iter().filter_map(Listed::of).collect();
    let at = now(cx);
    let Ok(moved) = change(cx, |state| reconcile(state, &list, at)) else {
        return;
    };
    if moved {
        let _ = write_handoffs(cx);
    }
    schedule_agents(cx);
}

/// At most one write in five seconds. The first goes at once; a change inside the interval sets
/// one timer for its end and the writes it covers are one (the state says when it is due, so a
/// reload that lost the timer sets it again).
fn schedule_agents(cx: &Ctx) {
    let at = now(cx);
    match change(cx, |state| plan_write(state, at)) {
        Ok((Act::Now, _)) => {
            let _ = write_agents(cx);
        }
        Ok((Act::Later, wait)) => {
            cx.after(wait as f64, |cx| async move { schedule_agents(&cx) });
        }
        _ => {}
    }
}

/// The minute poll, from the session start: it asks `$.agent.list()` only while an agent is
/// pending, running or waiting. A start again (the relay posts it after a respawn too) replaces
/// the timer rather than adding one.
fn watch_agents(cx: &Ctx) {
    let id = cx.every(LIST_EVERY_MS, |cx| async move {
        if read_state(&cx).is_ok_and(|state| any_active(&state)) {
            poll_agents(&cx).await;
        }
    });
    if let Some(old) = cx.kept(|f: &mut Fleet| f.list_timer.replace(id)) {
        cx.timers.cancel(old);
    }
}

/// A prompt that entered, and carried the envelope of a request the pane drafted: that request
/// was sent. Only the text that entered counts (a prompt a hook dropped did not go), and only a
/// request the pane drafted in this session.
fn handoff_sent(cx: &Ctx, entered: &Value) {
    let Some(sent) = field(entered, "text").filter(|text| text.contains(pane::OPEN)) else {
        return;
    };
    if entered.get("drop").is_some() || !session_name(&cx.sid) {
        return;
    }
    let asked = handoff_requests(sent, &cx.sid);
    let Some(paths) = paths_of(cx).filter(|_| !asked.is_empty()) else {
        return;
    };
    let drafts = paths
        .keep
        .join("handoffs")
        .join(format!("{}.drafts.jsonl", cx.sid));
    let drafts = std::fs::read_to_string(drafts).unwrap_or_default();
    for (request, thread) in asked {
        for line in drafts.split('\n') {
            let Ok(draft) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(drafted) = draft["drafted_at"].as_f64() else {
                continue;
            };
            if draft["request"] == request.as_str() && draft["thread"] == thread.as_str() {
                let at = now(cx);
                let drafted_at = drafted as i64;
                track(
                    cx,
                    Event::Handoff {
                        at,
                        request,
                        thread,
                        drafted_at,
                    },
                );
                break;
            }
        }
    }
}

// ---------------------------------------------------------------- completions

/// Offer the reports that wait, once the loop is idle: one look takes all that waits (agents
/// that finish together share it), after claiming them, so no other step or timer can take them.
/// A look that ends re-arms the timer for the reports that came in during it.
pub async fn completions(cx: &Ctx) {
    if !dreams_on(cx).await || cx.kept(|f: &mut Fleet| f.dreaming) {
        return;
    }
    if !is_person(cx).await || !has_pending(cx).unwrap_or(false) {
        return;
    }
    // One look at a time, whoever started it: the test and the set are one step.
    let go = cx.kept(|f: &mut Fleet| match (f.dreaming, f.dream.clone()) {
        (false, Some(dream)) => {
            f.dreaming = true;
            Some(dream)
        }
        _ => None,
    });
    let Some(dream) = go else {
        return;
    };
    // A look that panics is a look that failed; the claim it held is the dreamer's to settle.
    let _ = tokio::spawn(take_and_look(cx.clone(), dream)).await;
    cx.kept(|f: &mut Fleet| f.dreaming = false);
    arm_completions(cx, None, false).await;
}

async fn take_and_look(cx: Ctx, dream: Dream) {
    let taken = take_completions(&cx).await;
    if taken.is_empty() {
        return;
    }
    // The brief reads the agents file: these reports must be in it.
    let _ = write_agents(&cx);
    let row = |name: &str| {
        let kept = cx.store.get(&cx.sid, name);
        kept.ok().flatten().unwrap_or(Value::Null)
    };
    let (chapter, pace) = (row("chapter"), row("pace"));
    let count = |value: &Value| value.as_f64().unwrap_or(0.);
    // What a look would cost, guessed from the main session's last step (a fork re-reads the same
    // context); with none known, from an average step of a usual chapter.
    let spent = cx.kept(|f: &mut Fleet| f.spent) as f64;
    let usual = (count(&pace["claude"]) / USUAL_STEPS).round();
    let step = [spent, count(&pace["step"]), usual]
        .into_iter()
        .find(|guess| *guess != 0.)
        .unwrap_or(0.)
        .max(1.);
    let call = DreamCall {
        step: step as u64,
        steps: count(&chapter["steps"]) as u64,
        completions: taken,
    };
    let _ = dream(cx, call).await;
}

/// Wait for the main loop to be idle, then look: `wait` is how long the timer runs. A timer
/// that fires with the main session mid-turn leaves the reports to its next step (the turn's
/// end arms the timer again). One that fires just after a step waits out the idle time it
/// lacked, and one that finds a look running tries once more after the wait: otherwise the
/// reports would sit until the next prompt.
pub async fn arm_completions(cx: &Ctx, wait: Option<f64>, retried: bool) {
    // Nothing waits (the usual case): no host call is made to find that out.
    if !has_pending(cx).unwrap_or(false) || !dreams_on(cx).await {
        return;
    }
    // One timer at a time: the test and the set are one step.
    let armed = cx.kept(|f: &mut Fleet| {
        let go = !f.timer && !f.active;
        f.timer |= go;
        go
    });
    if armed {
        arm(cx, wait.unwrap_or(COMPLETION_WAIT_MS), retried);
    }
}

fn arm(cx: &Ctx, wait: f64, retried: bool) {
    cx.after(
        wait,
        move |cx| async move { timer_fired(cx, retried).await },
    );
}

async fn timer_fired(cx: Ctx, retried: bool) {
    cx.kept(|f: &mut Fleet| f.timer = false);
    let (active, stepped) = cx.kept(|f: &mut Fleet| (f.active, f.step_at));
    if active {
        return; // mid-work: its next step takes them
    }
    let idle = cx.clock.now() - stepped;
    if idle < COMPLETION_WAIT_MS {
        return arm_completions(&cx, Some(COMPLETION_WAIT_MS - idle), retried).await;
    }
    if cx.kept(|f: &mut Fleet| f.dreaming) {
        if !retried {
            arm_completions(&cx, Some(COMPLETION_WAIT_MS), true).await;
        }
        return;
    }
    completions(&cx).await;
}

// ---------------------------------------------------------------- hooks

/// `TASK_ID` from a tool's result text: the id a background task is known by.
fn task_id(source: &str) -> Option<String> {
    static ID: LazyLock<Regex> = LazyLock::new(|| {
        let pattern =
            format!(r"(?-u:\b)(?:agentId|ID|task_id|taskId):[{WS}]*([A-Za-z0-9_-]{{6,}})");
        Regex::new(&pattern).expect("a fixed pattern")
    });
    Some(ID.captures(source)?[1].to_string())
}

/// What a subagent started in the background is its own (`ownTask`).
fn own_task(cx: &Ctx, id: &str, agent: &str) -> Result<()> {
    state::update(cx, TASK_OWNERS, |current| {
        let mut owners = match current {
            Some(Value::Object(owners)) => owners,
            _ => Map::new(),
        };
        owners.insert(id.to_string(), json!(agent));
        Some(Value::Object(owners))
    })?;
    Ok(())
}

/// A subagent's call is noted before `next` runs, for `/after`.
async fn watched_event(cx: Ctx, e: Value) -> Result<Reply> {
    // The TypeScript registers this hook only while compaction is on.
    if options(&cx).await.auto_compact == "off" {
        return Ok(Reply::none());
    }
    let Some(agent) = agent_of(&e) else {
        return Ok(Reply::none());
    };
    let left = json!({ "agent": agent, "tool": e["tool"] });
    Ok(Reply {
        left: Some(left),
        ..Reply::none()
    })
}

async fn watched_after(cx: Ctx, result: Value) -> Result<Reply> {
    let Some(left) = cx.left.clone() else {
        return Ok(Reply::none());
    };
    let agent = left["agent"].as_str().unwrap_or("");
    let started = matches!(
        left["tool"].as_str(),
        Some("Agent" | "Workflow" | "Bash" | "Monitor")
    );
    let failed = truthy(&result["isError"]) || result.get("deny").is_some();
    let id = task_id(field(&result, "text").unwrap_or(""));
    if let Some(id) = id.filter(|_| started && !failed) {
        own_task(&cx, &id, agent)?;
    }
    Ok(Reply::none())
}

async fn turn_start(cx: Ctx, _e: Value) -> Result<Reply> {
    cx.kept(|f: &mut Fleet| f.active = true);
    Ok(Reply::none())
}

async fn turn_complete(cx: Ctx, e: Value) -> Result<Reply> {
    if let Some(agent) = agent_of(&e) {
        // An agent's turn ends: a run of that agent. `e.answer` is its report.
        if !is_person(&cx).await {
            return Ok(Reply::none());
        }
        let turn = field(&e, "turnId").unwrap_or("");
        let reason = field(&e, "reason").unwrap_or("");
        let answer = field(&e, "answer").unwrap_or("");
        track(
            &cx,
            Event::Complete {
                at: now(&cx),
                agent_id: agent.to_string(),
                turn_id: turn.to_string(),
                reason: reason.to_string(),
                answer: answer.to_string(),
            },
        );
        let reported = reason == "answer" && !text::js_trim(answer).is_empty();
        let key = completion_key(&cx.sid, agent, turn);
        if reported && dreams_on(&cx).await && admitted(&cx, &key) {
            arm_completions(&cx, None, false).await;
        }
        return Ok(Reply::none());
    }
    cx.kept(|f: &mut Fleet| f.active = false);
    if is_person(&cx).await {
        // Not held by the host: a list that never answers must not stop the turn's other
        // observers.
        detach(&cx, |cx| async move { poll_agents(&cx).await });
        // Reports that came in mid-turn are looked at once the person has the floor.
        arm_completions(&cx, None, false).await;
    }
    Ok(Reply::none())
}

async fn spawn_event(cx: Ctx, e: Value) -> Result<Reply> {
    let person = is_person(&cx).await;
    if person {
        let requests = handoff_requests(field(&e, "prompt").unwrap_or(""), &cx.sid);
        let kept = |name: &str| field(&e, name).map(str::to_string);
        track(
            &cx,
            Event::Spawn {
                at: now(&cx),
                tool_use_id: kept("tool_use_id").unwrap_or_default(),
                description: kept("description").unwrap_or_default(),
                kind: kept("subagentType").filter(|kind| !kind.is_empty()),
                parent: kept("parentAgentId"),
                model: kept("model"),
                requests: requests.into_iter().map(|(request, _)| request).collect(),
            },
        );
    }
    if !person {
        return Ok(Reply::none()); // no one is drawn: nothing for `/after` to do
    }
    let left = json!({ "person": person, "toolUseId": e["tool_use_id"] });
    Ok(Reply {
        left: Some(left),
        ..Reply::none()
    })
}

async fn spawn_after(cx: Ctx, result: Value) -> Result<Reply> {
    let Some(left) = cx.left.clone().filter(|left| left["person"] == true) else {
        return Ok(Reply::none());
    };
    let started = result.get("deny").is_none();
    let agent = field(&result, "agentId").filter(|id| started && !id.is_empty());
    track(
        &cx,
        Event::Spawned {
            at: now(&cx),
            tool_use_id: left["toolUseId"].as_str().unwrap_or("").to_string(),
            agent_id: agent.map(str::to_string),
            model: field(&result, "model")
                .filter(|_| agent.is_some())
                .map(str::to_string),
        },
    );
    Ok(Reply::none())
}

/// A step of the main loop is the dreamer's and the pace's; a subagent's is a row's activity
/// (its tokens show on its row), and that is all. The dreamer reads the main conversation, so a
/// subagent's tokens are never Claude's spend and its step never counts as a step of the chapter.
async fn step(cx: Ctx, told: Value) -> Result<Reply> {
    let (e, result) = (&told["e"], &told["result"]);
    let person = is_person(&cx).await;
    let spent = tokens_of(&result["usage"]);
    if let Some(agent) = agent_of(e) {
        if person {
            let pick = |name: &str| field(result, name).or(field(e, name));
            let index = number(&result["index"]).or_else(|| number(&e["index"]));
            if let (Some(turn_id), Some(index)) = (pick("turnId"), index) {
                track(
                    &cx,
                    Event::Step {
                        at: now(&cx),
                        agent_id: agent.to_string(),
                        turn_id: turn_id.to_string(),
                        index,
                        answer: field(result, "answer").unwrap_or("").to_string(),
                        tokens: spent,
                    },
                );
            }
        }
        return Ok(Reply::none());
    }
    let stepped = cx.clock.now();
    let dreams = dreams_on(&cx).await;
    cx.kept(|f: &mut Fleet| {
        f.step_at = stepped;
        if dreams && person && spent > 0 {
            f.spent = spent;
        }
    });
    Ok(Reply::none())
}

/// A tool of an agent's loop is alive when it begins and counted when it ends; the main loop's
/// writes are kept for the files an agent also wrote. `next` answers once, whatever these
/// observations come to.
async fn tool_event(cx: Ctx, e: Value) -> Result<Reply> {
    let tool = field(&e, "tool").unwrap_or("");
    let agent = agent_of(&e);
    let tracked = agent.is_some() || model::is_edit_tool(tool);
    let person = tracked && is_person(&cx).await;
    if let (true, Some(agent)) = (person, agent) {
        let (agent_id, tool) = (agent.to_string(), tool.to_string());
        track(
            &cx,
            Event::Touch {
                at: now(&cx),
                agent_id,
                tool,
            },
        );
    }
    if !person {
        return Ok(Reply::none()); // no one is drawn: nothing for `/after` to do
    }
    let file = field(&e, "file_path").or_else(|| field(&e, "notebook_path"));
    let left = json!({
        "person": person, "agentId": agent, "toolUseId": e["tool_use_id"], "tool": tool,
        "file": file,
    });
    Ok(Reply {
        left: Some(left),
        ..Reply::none()
    })
}

async fn tool_after(cx: Ctx, result: Value) -> Result<Reply> {
    let Some(left) = cx.left.clone().filter(|left| left["person"] == true) else {
        return Ok(Reply::none());
    };
    let ok = result.get("deny").is_none() && !truthy(&result["isError"]);
    let path = left["file"]
        .as_str()
        .map(|file| normalize_path(file, &cx.cwd));
    let text = |name: &str| left[name].as_str().map(str::to_string);
    track(
        &cx,
        Event::Tool {
            at: now(&cx),
            agent_id: text("agentId"),
            tool_use_id: text("toolUseId").unwrap_or_default(),
            tool: text("tool").unwrap_or_default(),
            ok,
            path,
        },
    );
    Ok(Reply::none())
}

async fn digit(cx: Ctx, entered: Value) -> Result<Reply> {
    handoff_sent(&cx, &entered);
    Ok(Reply::none())
}

/// The session's start: the agents of this session, the minute poll, one look at the list now,
/// and the reports a reload left pending.
async fn start(cx: Ctx, e: Value) -> Result<Reply> {
    if e["isInteractive"] != true {
        return Ok(Reply::none());
    }
    watch_agents(&cx);
    // A timer a reload cancelled is due no more.
    let _ = change(&cx, |state| state.file_due = None);
    detach(&cx, |cx| async move { poll_agents(&cx).await });
    arm_completions(&cx, None, false).await;
    Ok(Reply::none())
}

pub fn register(t: &mut Table) {
    t.gate(
        "watched",
        "fleet",
        handler(watched_event),
        handler(watched_after),
    );
    t.on("turn_start", "fleet", handler(turn_start));
    t.on("turn_complete", "fleet", handler(turn_complete));
    t.gate(
        "spawned",
        "fleet",
        handler(spawn_event),
        handler(spawn_after),
    );
    t.after("step", "fleet", handler(step));
    t.gate("tool", "fleet", handler(tool_event), handler(tool_after));
    t.after("digit", "fleet", handler(digit));
    t.on("start", "fleet", handler(start));
}
