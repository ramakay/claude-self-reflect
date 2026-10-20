//! The agents of a session, as the typed events of Claude Code tell them: port of
//! the removed `agents.ts`. A lifecycle reducer, a reconciler for `agent.list`, the completion
//! ledger, the bounded file the pane reads, the machine envelope, and the handoff of a thread to a
//! background agent. Pure: the handlers in `fleet.rs` hand it events and keep what it changes.
//!
//! The TypeScript never changes a state in place and returns a new one; here a state is changed
//! in place and a function returns whether the handoffs it keeps changed (the TypeScript tests
//! that with `!==` on the array), which is all its callers ask.

use super::text::{
    cap, fingerprint, js_trim, one_line, redact_str, safe_id, squash_space, utf8_length,
};
use crate::companion::agents;
use serde::{Deserialize, Serialize};
use serde_json::{json, Number, Value};
use std::collections::{BTreeMap, HashSet};

/// A running agent with no event for this long is shown as stalled (milliseconds).
pub const STALL_MS: i64 = 10 * 60 * 1000;
/// `agent.list` while any agent is pending, running or waiting.
pub const LIST_EVERY_MS: f64 = 60_000.;
/// Code points of an agent's report that are kept.
pub const ANSWER_MAX: usize = 700;
pub const LAST_MAX: usize = 240;
pub const DESC_MAX: usize = 160;
/// Agents the session state keeps; the oldest finished ones go first.
pub const ROWS_KEPT: usize = 60;
/// Files an agent wrote whose names are kept; past it only the fingerprint is.
pub const PATHS_KEPT: usize = 400;
/// Fingerprints of an agent's files, the membership an overlap is found by.
pub const PATHS_MEMBERS: usize = 4000;
pub const MAIN_KEPT: usize = 4000;
const SEEN_TOOLS: usize = 400;
const SEEN_STEPS: usize = 400;
const SEEN_COMPLETIONS: usize = 400;
const SPAWNS_KEPT: usize = 100;
pub const HANDOFFS_KEPT: usize = 40;
pub const LEDGER_MAX: usize = 1500;
pub const FILE_BYTES: usize = 16384;
pub const FILE_ROWS: usize = 12;
pub const FILE_EVERY_MS: i64 = 5000;
pub const EDITS_SHOWN: usize = 8;

const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

pub fn is_edit_tool(tool: &str) -> bool {
    EDIT_TOOLS.contains(&tool)
}

pub fn completion_key(session: &str, agent: &str, turn: &str) -> String {
    json!([session, agent, turn]).to_string()
}

pub fn step_key(session: &str, agent: Option<&str>, turn: &str, index: &Number) -> String {
    json!([session, agent, turn, index]).to_string()
}

// ---------------------------------------------------------------- an object that keeps its order

/// Whether JavaScript orders `key` among an object's integer keys: a canonical array index.
fn index_of(key: &str) -> Option<u64> {
    let bytes = key.as_bytes();
    let canonical = match bytes {
        [] => false,
        [b'0'] => true,
        [first, rest @ ..] => (b'1'..=b'9').contains(first) && rest.iter().all(u8::is_ascii_digit),
    };
    let n: u64 = key.parse().ok().filter(|_| canonical)?;
    (n <= 4_294_967_294).then_some(n)
}

/// A JavaScript object used as a map: keys keep the order they came in, but integer-like keys
/// come first, ascending. Serialized as a list of `[key, value]` pairs, which keeps that order
/// through a store that sorts object keys.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ordered<V>(Vec<(String, V)>);

impl<V> Default for Ordered<V> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<V> Ordered<V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.0.iter().any(|(k, _)| k == key)
    }

    /// `{...obj, [key]: value}`: an existing key keeps its place.
    pub fn insert(&mut self, key: &str, value: V) {
        if let Some(slot) = self.0.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value;
            return;
        }
        self.0.push((key.to_string(), value));
        if index_of(key).is_some() {
            self.0
                .sort_by_key(|(k, _)| index_of(k).map_or((1, 0), |n| (0, n)));
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<V> {
        let at = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(at).1)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(k, _)| k.as_str())
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.0.iter().map(|(_, v)| v)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// `Object.entries(obj).slice(-most)`.
    fn keep_last(&mut self, most: usize) {
        let from = self.0.len().saturating_sub(most);
        self.0.drain(..from);
    }

    fn retain(&mut self, keep: impl Fn(&str, &V) -> bool) {
        self.0.retain(|(k, v)| keep(k, v));
    }
}

// ---------------------------------------------------------------- the state

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    Running,
    Waiting,
    Idle,
    Completed,
    Failed,
    Killed,
    Unknown,
}

impl Status {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Running | Self::Waiting)
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Killed)
    }

    /// A word of `$.agent.list()`; any other is `unknown`.
    pub fn of_word(word: Option<&str>) -> Self {
        match word {
            Some("pending") => Self::Pending,
            Some("running") => Self::Running,
            Some("waiting") => Self::Waiting,
            Some("idle") => Self::Idle,
            Some("completed") => Self::Completed,
            Some("failed") => Self::Failed,
            Some("killed") => Self::Killed,
            _ => Self::Unknown,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Idle => "idle",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Killed => "killed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Disposition {
    Pending,
    Claimed,
    Looked,
    Skipped,
    Failed,
}

impl Disposition {
    pub fn of_word(word: &str) -> Option<Self> {
        Some(match word {
            "pending" => Self::Pending,
            "claimed" => Self::Claimed,
            "looked" => Self::Looked,
            "skipped" => Self::Skipped,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    pub key: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    pub at: i64,
    /// `answer`, `aborted`, `refusal` or `error`, as the turn ended.
    pub reason: String,
    pub answer: String,
    pub disposition: Disposition,
}

/// One agent as the state keeps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentRec {
    pub id: String,
    pub parent: Option<String>,
    pub description: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub model: Option<String>,
    pub status: Status,
    pub started: Option<i64>,
    pub first_seen: i64,
    pub last_at: i64,
    pub tools: u64,
    pub tokens: u64,
    pub last: String,
    pub last_tool: Option<String>,
    pub current_turn: Option<String>,
    pub completion: Option<Completion>,
    pub listed: bool,
    /// Fingerprint -> path of a file this agent wrote (`""` past `PATHS_KEPT`).
    pub paths: Ordered<String>,
    #[serde(rename = "seenTools")]
    pub seen_tools: Vec<String>,
    #[serde(rename = "seenSteps")]
    pub seen_steps: Vec<String>,
    pub first_turn: Option<String>,
}

/// What an Agent call asked for, until its result says what started.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingSpawn {
    pub at: i64,
    pub description: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub parent: Option<String>,
    pub model: Option<String>,
}

/// A report that waits for a look.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingCompletion {
    pub agent: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    pub at: i64,
    pub answer: String,
}

/// What became of a request to hand a thread to a background agent. The names are the file the
/// pane reads (`agents::Handoff`), field for field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentHandoff {
    pub request: String,
    pub session: String,
    pub thread: String,
    pub drafted_at: i64,
    pub submitted_at: Option<i64>,
    pub spawn_tool_use_id: Option<String>,
    pub agent_id: Option<String>,
    pub turn_id: Option<String>,
    /// `draft`, `submitted`, `bound`, `succeeded` or `failed`.
    pub outcome: String,
    pub ended_at: Option<i64>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentState {
    pub version: u8,
    pub session: String,
    pub rows: Ordered<AgentRec>,
    pub spawns: Ordered<PendingSpawn>,
    /// Agent call id -> the agent it started.
    pub bound: Ordered<String>,
    /// Fingerprint -> path of a file the main loop wrote.
    pub main: Ordered<String>,
    #[serde(rename = "mainOmitted")]
    pub main_omitted: u64,
    pub seen: Vec<String>,
    pub pending: Ordered<PendingCompletion>,
    #[serde(rename = "listedAt")]
    pub listed_at: Option<i64>,
    #[serde(rename = "fileAt")]
    pub file_at: Option<i64>,
    #[serde(rename = "fileDue")]
    pub file_due: Option<i64>,
    pub handoffs: Vec<AgentHandoff>,
}

impl AgentState {
    pub fn empty(session: &str) -> Self {
        Self {
            version: 1,
            session: session.to_string(),
            rows: Ordered::new(),
            spawns: Ordered::new(),
            bound: Ordered::new(),
            main: Ordered::new(),
            main_omitted: 0,
            seen: Vec::new(),
            pending: Ordered::new(),
            listed_at: None,
            file_at: None,
            file_due: None,
            handoffs: Vec::new(),
        }
    }
}

/// A stalled agent is a display flag, never a status: a running agent no event has touched for
/// `STALL_MS`. Waiting and idle are not stalled, and a list poll never refreshes `last_at`.
pub fn is_stalled(status: Status, last_at: i64, now: i64) -> bool {
    status == Status::Running && now - last_at >= STALL_MS
}

fn provisional(id: &str, at: i64, status: Status) -> AgentRec {
    AgentRec {
        id: id.to_string(),
        parent: None,
        description: String::new(),
        kind: None,
        model: None,
        status,
        started: None,
        first_seen: at,
        last_at: at,
        tools: 0,
        tokens: 0,
        last: String::new(),
        last_tool: None,
        current_turn: None,
        completion: None,
        listed: false,
        paths: Ordered::new(),
        seen_tools: Vec::new(),
        seen_steps: Vec::new(),
        first_turn: None,
    }
}

/// `[...list, item].slice(-most)`.
fn push_kept(list: &mut Vec<String>, item: String, most: usize) {
    list.push(item);
    let from = list.len().saturating_sub(most);
    list.drain(..from);
}

/// An event's agent runs: unknown and pending become running.
fn running_now(status: Status) -> Status {
    match status {
        Status::Unknown | Status::Pending => Status::Running,
        other => other,
    }
}

/// The rows the state keeps: the newest by last activity; a row with a completion waiting is
/// never the one to go.
fn trim_rows(state: &mut AgentState) {
    let total = state.rows.len();
    if total <= ROWS_KEPT {
        return;
    }
    let waiting: HashSet<&str> = state.pending.values().map(|p| p.agent.as_str()).collect();
    let mut order: Vec<(&str, i64)> = state
        .rows
        .iter()
        .filter(|(id, _)| !waiting.contains(id))
        .map(|(id, row)| (id, row.last_at))
        .collect();
    order.sort_by_key(|(_, last_at)| *last_at);
    let drop: HashSet<String> = order
        .into_iter()
        .take(total - ROWS_KEPT)
        .map(|(id, _)| id.to_string())
        .collect();
    state.rows.retain(|id, _| !drop.contains(id));
}

fn with_row(state: &mut AgentState, row: AgentRec) {
    let id = row.id.clone();
    state.rows.insert(&id, row);
    trim_rows(state);
}

// ---------------------------------------------------------------- events

#[derive(Clone, Debug)]
pub enum Event {
    /// The Agent call, before `next` answers: what it asked for.
    Spawn {
        at: i64,
        tool_use_id: String,
        description: String,
        kind: Option<String>,
        parent: Option<String>,
        model: Option<String>,
        /// The handoff requests (envelopes) the call's prompt carries.
        requests: Vec<String>,
    },
    /// What `next` answered: no `agent_id` means it started none (denied).
    Spawned {
        at: i64,
        tool_use_id: String,
        agent_id: Option<String>,
        model: Option<String>,
    },
    /// A tool began in an agent's loop: it is alive, whatever it comes to.
    Touch {
        at: i64,
        agent_id: String,
        tool: String,
    },
    /// A tool finished. No `agent_id`: the main loop's (only its writes are kept).
    Tool {
        at: i64,
        agent_id: Option<String>,
        tool_use_id: String,
        tool: String,
        ok: bool,
        path: Option<String>,
    },
    Step {
        at: i64,
        agent_id: String,
        turn_id: String,
        index: Number,
        answer: String,
        tokens: u64,
    },
    Complete {
        at: i64,
        agent_id: String,
        turn_id: String,
        reason: String,
        answer: String,
    },
    /// A prompt the person's Enter sent carried the envelope of a request the pane drafted.
    Handoff {
        at: i64,
        request: String,
        thread: String,
        drafted_at: i64,
    },
}

/// `cap(redact(text).replace(/[ \t]+\n/g, '\n').trim(), 700)`.
fn report(text: &str) -> String {
    static TRAILING: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"[ \t]+\n").expect("a fixed pattern"));
    let redacted = redact_str(text);
    let lines = TRAILING.replace_all(&redacted, "\n");
    cap(js_trim(&lines), ANSWER_MAX)
}

/// One event into the state. Whether the handoffs it keeps changed.
pub fn reduce(state: &mut AgentState, event: Event) -> bool {
    match event {
        Event::Spawn {
            at,
            tool_use_id,
            description,
            kind,
            parent,
            model,
            requests,
        } => {
            let nested = parent.is_some();
            let spawn = PendingSpawn {
                at,
                description: one_line(&description, DESC_MAX),
                kind,
                parent,
                model,
            };
            state.spawns.insert(&tool_use_id, spawn);
            state.spawns.keep_last(SPAWNS_KEPT);
            if requests.is_empty() {
                return false;
            }
            let ev = HandoffEvent::Spawn {
                requests,
                tool_use_id,
                nested,
            };
            associate_handoff(&mut state.handoffs, ev)
        }
        Event::Spawned {
            at,
            tool_use_id,
            agent_id,
            model,
        } => {
            let asked = state.spawns.remove(&tool_use_id);
            let Some(agent_id) = agent_id.filter(|id| !id.is_empty()) else {
                // Denied or none started: nothing is made up.
                let ev = HandoffEvent::Spawned {
                    tool_use_id,
                    agent_id: None,
                    turn: None,
                };
                return associate_handoff(&mut state.handoffs, ev);
            };
            let row = (state.rows.get(&agent_id).cloned()).unwrap_or_else(|| {
                provisional(
                    &agent_id,
                    asked.as_ref().map_or(at, |a| a.at),
                    Status::Running,
                )
            });
            let mut merged = row.clone();
            if let Some(text) = asked
                .as_ref()
                .map(|a| &a.description)
                .filter(|d| !d.is_empty())
            {
                merged.description = text.clone();
            }
            merged.kind = asked
                .as_ref()
                .and_then(|a| a.kind.clone())
                .or(row.kind.clone());
            merged.parent = row
                .parent
                .clone()
                .or_else(|| asked.as_ref().and_then(|a| a.parent.clone()));
            merged.model = model
                .or_else(|| asked.as_ref().and_then(|a| a.model.clone()))
                .or(row.model.clone());
            merged.started = Some(row.started.unwrap_or(asked.as_ref().map_or(at, |a| a.at)));
            merged.first_seen = row
                .first_seen
                .min(asked.as_ref().map_or(row.first_seen, |a| a.at));
            merged.status = running_now(row.status);
            // A handoff this Agent call carried is bound to the agent it started and its first
            // turn (events that came before this result included); a report that already came
            // is its outcome.
            let ev = HandoffEvent::Spawned {
                tool_use_id: tool_use_id.clone(),
                agent_id: Some(agent_id.clone()),
                turn: merged.first_turn.clone(),
            };
            let mut moved = associate_handoff(&mut state.handoffs, ev);
            if let Some(done) = merged
                .completion
                .clone()
                .filter(|c| Some(&c.turn_id) == merged.first_turn.as_ref())
            {
                let ev = HandoffEvent::Ended {
                    at: done.at,
                    agent_id: merged.id.clone(),
                    turn_id: Some(done.turn_id.clone()),
                    reason: done.reason.clone(),
                    empty: done.disposition == Disposition::Skipped && done.reason == "answer",
                };
                moved |= associate_handoff(&mut state.handoffs, ev);
            }
            state.bound.insert(&tool_use_id, agent_id);
            with_row(state, merged);
            moved
        }
        Event::Touch { at, agent_id, tool } => {
            let mut row = (state.rows.get(&agent_id).cloned())
                .unwrap_or_else(|| provisional(&agent_id, at, Status::Running));
            row.status = running_now(row.status);
            row.last_at = row.last_at.max(at);
            row.last_tool = Some(cap(&tool, 60));
            with_row(state, row);
            false
        }
        Event::Tool {
            at,
            agent_id,
            tool_use_id,
            tool,
            ok,
            path,
        } => {
            let wrote = path.filter(|p| ok && is_edit_tool(&tool) && !p.is_empty());
            let Some(agent_id) = agent_id else {
                // The main loop: only what it wrote matters here, for the files an agent also
                // wrote.
                let Some(wrote) = wrote else { return false };
                let print = fingerprint(&wrote);
                if state.main.contains(&print) {
                    return false;
                }
                if state.main.len() >= MAIN_KEPT {
                    state.main_omitted += 1;
                } else {
                    state.main.insert(&print, wrote);
                }
                return false;
            };
            let mut row = (state.rows.get(&agent_id).cloned())
                .unwrap_or_else(|| provisional(&agent_id, at, Status::Running));
            if row.seen_tools.contains(&tool_use_id) {
                return false;
            }
            if let Some(wrote) = wrote {
                let print = fingerprint(&wrote);
                if !row.paths.contains(&print) && row.paths.len() < PATHS_MEMBERS {
                    let named = row.paths.len() < PATHS_KEPT;
                    row.paths
                        .insert(&print, if named { wrote } else { String::new() });
                }
            }
            row.tools += 1;
            row.last_tool = Some(cap(&tool, 60));
            row.last_at = row.last_at.max(at);
            push_kept(&mut row.seen_tools, tool_use_id, SEEN_TOOLS);
            row.status = running_now(row.status);
            with_row(state, row);
            false
        }
        Event::Step {
            at,
            agent_id,
            turn_id,
            index,
            answer,
            tokens,
        } => {
            let key = step_key(&state.session, Some(&agent_id), &turn_id, &index);
            let mut row = (state.rows.get(&agent_id).cloned())
                .unwrap_or_else(|| provisional(&agent_id, at, Status::Running));
            if row.seen_steps.contains(&key) {
                return false;
            }
            let new_run = row.current_turn.as_ref().is_some_and(|t| *t != turn_id);
            let text = one_line(&answer, LAST_MAX);
            let first = row.first_turn.clone().unwrap_or_else(|| turn_id.clone());
            let mut moved = false;
            if row.first_turn.is_none() {
                let ev = HandoffEvent::Turn {
                    agent_id: agent_id.clone(),
                    turn_id: turn_id.clone(),
                };
                moved = associate_handoff(&mut state.handoffs, ev);
            }
            row.tokens += tokens;
            if !text.is_empty() {
                row.last = text;
            }
            row.last_at = row.last_at.max(at);
            // A step is running work: an agent that was idle, waiting or ended before this turn
            // runs again, with no report yet.
            let same_run = row.current_turn.as_ref() == Some(&turn_id);
            row.status = if row.status == Status::Running
                || !(row.status.is_terminal() && !new_run && same_run)
            {
                Status::Running
            } else {
                row.status
            };
            if new_run {
                row.completion = None;
            }
            row.current_turn = Some(turn_id);
            row.first_turn = Some(first);
            push_kept(&mut row.seen_steps, key, SEEN_STEPS);
            with_row(state, row);
            moved
        }
        Event::Complete {
            at,
            agent_id,
            turn_id,
            reason,
            answer,
        } => {
            let key = completion_key(&state.session, &agent_id, &turn_id);
            if state.seen.contains(&key) {
                return false;
            }
            let mut row = (state.rows.get(&agent_id).cloned())
                .unwrap_or_else(|| provisional(&agent_id, at, Status::Running));
            let kept = if reason == "answer" {
                report(&answer)
            } else {
                String::new()
            };
            let eligible = reason == "answer" && !js_trim(&kept).is_empty();
            row.completion = Some(Completion {
                key: key.clone(),
                turn_id: turn_id.clone(),
                at,
                reason: reason.clone(),
                answer: kept.clone(),
                disposition: if eligible {
                    Disposition::Pending
                } else {
                    Disposition::Skipped
                },
            });
            row.status = match reason.as_str() {
                "answer" => Status::Completed,
                "aborted" => Status::Killed,
                _ => Status::Failed,
            };
            if eligible {
                let pending = PendingCompletion {
                    agent: agent_id.clone(),
                    turn_id: turn_id.clone(),
                    at,
                    answer: kept,
                };
                state.pending.insert(&key, pending);
            }
            // The run's end settles a handoff bound to this agent and turn: a report is
            // success; an error, a refusal, an interrupt or no words are not.
            let ev = HandoffEvent::Ended {
                at,
                agent_id: agent_id.clone(),
                turn_id: Some(turn_id.clone()),
                empty: !eligible && reason == "answer",
                reason,
            };
            let moved = associate_handoff(&mut state.handoffs, ev);
            push_kept(&mut state.seen, key, SEEN_COMPLETIONS);
            row.first_turn = row.first_turn.or(Some(turn_id.clone()));
            row.current_turn = Some(turn_id);
            row.last_at = row.last_at.max(at);
            with_row(state, row);
            moved
        }
        Event::Handoff {
            at,
            request,
            thread,
            drafted_at,
        } => {
            let session = state.session.clone();
            let ev = HandoffEvent::Submitted {
                at,
                session,
                request,
                thread,
                drafted_at,
            };
            associate_handoff(&mut state.handoffs, ev)
        }
    }
}

// ---------------------------------------------------------------- agent.list

/// One row of `$.agent.list()`.
#[derive(Clone, Debug, Default)]
pub struct Listed {
    pub id: String,
    pub parent_id: Option<String>,
    pub kind: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
}

impl Listed {
    /// A row as the host gives it; one with no string `id` is not a row.
    pub fn of(value: &Value) -> Option<Self> {
        let text = |name: &str| value.get(name).and_then(Value::as_str).map(str::to_string);
        Some(Self {
            id: text("id")?,
            parent_id: text("parentId"),
            kind: text("type"),
            description: text("description"),
            status: text("status"),
        })
    }
}

/// One no list entry or spawn event ever gave a type, a description or a start, still active and
/// untouched for `STALL_MS`: one tool event of an in-process agent the list never reports.
fn is_ghost(row: &AgentRec, at: i64) -> bool {
    row.status.is_active()
        && !row.listed
        && row.kind.is_none()
        && row.description.is_empty()
        && row.started.is_none()
        && at - row.last_at >= STALL_MS
}

/// The list brought into the state. A row the list no longer has is not finished, failed or
/// killed: it keeps its last state, marked not listed. A row that ended in an event keeps that
/// unless the list says it runs again (it resumed under the same id). The list gives no model,
/// no start and no activity: `last_at` is never moved by it. A ghost is dropped; a later event of
/// that id makes it again.
pub fn reconcile(state: &mut AgentState, list: &[Listed], at: i64) -> bool {
    let ids: HashSet<&str> = list.iter().map(|entry| entry.id.as_str()).collect();
    let mut rows: Ordered<AgentRec> = Ordered::new();
    for (id, row) in state.rows.iter() {
        if ids.contains(id) {
            rows.insert(id, row.clone());
        } else if !is_ghost(row, at) {
            let mut row = row.clone();
            row.listed = false;
            rows.insert(id, row);
        }
    }
    for entry in list {
        let listed = Status::of_word(entry.status.as_deref());
        let row =
            (rows.get(&entry.id).cloned()).unwrap_or_else(|| provisional(&entry.id, at, listed));
        let keep = row.status.is_terminal() && !listed.is_active();
        let mut next = row.clone();
        next.listed = true;
        next.parent = row.parent.clone().or_else(|| entry.parent_id.clone());
        next.kind = row
            .kind
            .clone()
            .or_else(|| entry.kind.clone().filter(|k| !k.is_empty()));
        if row.description.is_empty() {
            next.description = one_line(entry.description.as_deref().unwrap_or(""), DESC_MAX);
        }
        next.status = if keep { row.status } else { listed };
        // A resumed agent runs again with no report of its own yet; the earlier one stays in
        // the ledger.
        if !keep && listed.is_active() && row.status.is_terminal() {
            next.completion = None;
        }
        rows.insert(&entry.id, next);
    }
    state.rows = rows;
    state.listed_at = Some(at);
    // An agent the list says failed or was killed, bound to a handoff that has no outcome yet:
    // that is its outcome. A row that is merely gone from the list, or quiet, is not.
    let mut moved = false;
    for entry in list {
        let word = Status::of_word(entry.status.as_deref());
        if matches!(word, Status::Failed | Status::Killed) {
            let ev = HandoffEvent::Ended {
                at,
                agent_id: entry.id.clone(),
                turn_id: None,
                reason: word.word().to_string(),
                empty: false,
            };
            moved |= associate_handoff(&mut state.handoffs, ev);
        }
    }
    trim_rows(state);
    moved
}

/// Whether any agent is one the list should be asked about again.
pub fn any_active(state: &AgentState) -> bool {
    state.rows.values().any(|row| row.status.is_active())
}

// ---------------------------------------------------------------- completions

/// The ledger of completion identities, kept in the store under the session (identifiers and
/// dispositions only: no report, no path, no prompt). It, not the 12-row file and not the state's
/// own list, says what was taken, and a completion a look claimed is never offered again.
pub type Ledger = BTreeMap<String, Disposition>;

#[derive(Debug, PartialEq, Eq)]
pub struct Admission {
    pub fresh: bool,
    pub full: bool,
}

/// A completion enters the ledger once. At the limit nothing is evicted to make room (a consumed
/// identity forgotten is a completion looked at twice): the entry is refused, and the caller
/// suppresses completion looks.
pub fn admit_completion(ledger: &mut Ledger, key: &str) -> Admission {
    if ledger.contains_key(key) {
        return Admission {
            fresh: false,
            full: false,
        };
    }
    if ledger.len() >= LEDGER_MAX {
        return Admission {
            fresh: false,
            full: true,
        };
    }
    ledger.insert(key.to_string(), Disposition::Pending);
    Admission {
        fresh: true,
        full: false,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingEntry {
    pub key: String,
    pub agent: String,
    pub turn_id: String,
    pub at: i64,
    pub answer: String,
}

/// The completions a look may still take, oldest first: pending in the state, and pending in the
/// ledger too.
pub fn pending_completions(state: &AgentState, ledger: &Ledger) -> Vec<PendingEntry> {
    let mut due: Vec<PendingEntry> = state
        .pending
        .iter()
        .filter(|(key, _)| ledger.get(*key) == Some(&Disposition::Pending))
        .map(|(key, entry)| PendingEntry {
            key: key.to_string(),
            agent: entry.agent.clone(),
            turn_id: entry.turn_id.clone(),
            at: entry.at,
            answer: entry.answer.clone(),
        })
        .collect();
    due.sort_by_key(|entry| entry.at);
    due
}

/// The pending ones among `keys` become claimed, all at once; the others (taken already,
/// unknown) are not.
pub fn claim_completions(ledger: &mut Ledger, keys: &[String]) -> Vec<String> {
    let claimed: Vec<String> = keys
        .iter()
        .filter(|key| ledger.get(*key) == Some(&Disposition::Pending))
        .cloned()
        .collect();
    for key in &claimed {
        ledger.insert(key.clone(), Disposition::Claimed);
    }
    claimed
}

/// A claimed look ended: its completions looked, skipped or failed. Only a claimed one moves.
pub fn settle_completions(ledger: &mut Ledger, keys: &[String], word: Disposition) {
    for key in keys {
        if ledger.get(key) == Some(&Disposition::Claimed) {
            ledger.insert(key.clone(), word);
        }
    }
}

/// The state told what became of those completions: they are no longer pending, and their rows
/// say so.
pub fn mark_disposition(state: &mut AgentState, keys: &[String], word: Disposition) {
    let gone: HashSet<&str> = keys.iter().map(String::as_str).collect();
    if word != Disposition::Pending {
        state.pending.retain(|key, _| !gone.contains(key));
    }
    let ids: Vec<String> = state.rows.keys().map(str::to_string).collect();
    for id in ids {
        let hit = (state.rows.get(&id))
            .and_then(|row| row.completion.as_ref())
            .is_some_and(|done| gone.contains(done.key.as_str()));
        if hit {
            let mut row = state.rows.get(&id).cloned().expect("listed just above");
            if let Some(done) = row.completion.as_mut() {
                done.disposition = word;
            }
            state.rows.insert(&id, row);
        }
    }
}

// ---------------------------------------------------------------- the agents file

/// One agent as the file shows it: the pane's `agents::Agent`, field for field.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AgentRow {
    pub id: String,
    pub parent: Option<String>,
    pub description: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub model: Option<String>,
    pub status: Status,
    pub started: Option<i64>,
    pub first_seen: i64,
    pub last_at: i64,
    pub tools: u64,
    pub tokens: u64,
    pub edits: Vec<String>,
    pub edits_omitted: usize,
    pub overlaps: Vec<String>,
    pub overlaps_omitted: usize,
    pub last: String,
    pub last_tool: Option<String>,
    pub current_turn: Option<String>,
    pub completion: Option<Completion>,
    pub listed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AgentsFile {
    pub version: u8,
    pub session: String,
    pub at: i64,
    pub agents: Vec<AgentRow>,
    pub omitted_agents: usize,
}

/// `clean(text, most)`: redacted first, one line, cut after.
fn clean(text: &str, most: usize) -> String {
    cap(js_trim(&squash_space(&redact_str(text))), most)
}

fn clean_or_null(text: Option<&str>, most: usize) -> Option<String> {
    let text = text.filter(|t| !t.is_empty())?;
    Some(clean(text, most)).filter(|t| !t.is_empty())
}

/// JavaScript's default sort of strings: by UTF-16 code unit.
fn js_sorted(mut list: Vec<String>) -> Vec<String> {
    list.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    list
}

/// A record as the file's row: its lists cut from what it wrote (the files the main loop also
/// wrote first), every free text redacted first and cut after.
fn row_of(rec: &AgentRec, main: &Ordered<String>) -> AgentRow {
    let prints: Vec<&str> = rec.paths.keys().collect();
    let named = |list: Vec<&str>| {
        let paths = (list.into_iter())
            .filter_map(|print| rec.paths.get(print))
            .filter(|path| !path.is_empty())
            .map(|path| clean(path, 300))
            .collect();
        js_sorted(paths)
    };
    let overlapping: Vec<&str> = prints
        .iter()
        .copied()
        .filter(|p| main.contains(p))
        .collect();
    let overlaps = named(overlapping.clone());
    let rest = named(
        prints
            .iter()
            .copied()
            .filter(|p| !main.contains(p))
            .collect(),
    );
    let edits: Vec<String> = overlaps
        .iter()
        .chain(&rest)
        .take(EDITS_SHOWN)
        .cloned()
        .collect();
    let shown: Vec<String> = overlaps.iter().take(EDITS_SHOWN).cloned().collect();
    AgentRow {
        id: rec.id.clone(),
        parent: rec.parent.clone().filter(|p| safe_id(p)),
        description: clean(&rec.description, DESC_MAX),
        kind: clean_or_null(rec.kind.as_deref(), 60),
        model: clean_or_null(rec.model.as_deref(), 80),
        status: rec.status,
        started: rec.started,
        first_seen: rec.first_seen,
        last_at: rec.last_at,
        tools: rec.tools,
        tokens: rec.tokens,
        edits_omitted: prints.len() - edits.len(),
        edits,
        overlaps_omitted: overlapping.len() - shown.len(),
        overlaps: shown,
        last: clean(&rec.last, LAST_MAX),
        last_tool: clean_or_null(rec.last_tool.as_deref(), 60),
        current_turn: rec.current_turn.clone().filter(|t| safe_id(t)),
        completion: rec.completion.clone().map(|done| Completion {
            answer: cap(&redact_str(&done.answer), ANSWER_MAX),
            ..done
        }),
        listed: rec.listed,
    }
}

/// Which agents the file shows: the running ones first, then the newest finished, parents with
/// their children. A parent the file cannot spare a place for is not left out when a child shown
/// needs it (a minimal row stands in for it).
fn selection(state: &AgentState) -> (Vec<&AgentRec>, usize, HashSet<String>) {
    let all: Vec<&AgentRec> = state.rows.values().filter(|rec| safe_id(&rec.id)).collect();
    let by_id: BTreeMap<&str, &AgentRec> = all.iter().map(|rec| (rec.id.as_str(), *rec)).collect();
    let mut ordered = all.clone();
    ordered.sort_by(|a, b| {
        (!a.status.is_active())
            .cmp(&!b.status.is_active())
            .then(b.last_at.cmp(&a.last_at))
            .then(a.id.cmp(&b.id))
    });
    let mut taken: HashSet<&str> = HashSet::new();
    for rec in &ordered {
        if taken.contains(rec.id.as_str()) {
            continue;
        }
        let mut need: Vec<&str> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut at = Some(*rec);
        while let Some(current) = at.filter(|c| !seen.contains(c.id.as_str())) {
            seen.insert(&current.id);
            if !taken.contains(current.id.as_str()) {
                need.push(&current.id);
            }
            at = current
                .parent
                .as_deref()
                .and_then(|p| by_id.get(p).copied());
        }
        if taken.len() + need.len() > FILE_ROWS {
            continue;
        }
        taken.extend(need);
    }
    // Those the order itself would show are full rows; an ancestor taken only for the child
    // below it is a stand-in.
    let own: HashSet<&str> = ordered
        .iter()
        .take(FILE_ROWS)
        .map(|r| r.id.as_str())
        .collect();
    let minimal: HashSet<String> = (taken.iter())
        .filter(|id| !own.contains(*id))
        .map(|id| id.to_string())
        .collect();
    let chosen: Vec<&AgentRec> = ordered
        .into_iter()
        .filter(|rec| taken.contains(rec.id.as_str()))
        .collect();
    let omitted = all.len() - chosen.len();
    (chosen, omitted, minimal)
}

/// Parents before children, each agent once; the order within a level is the selection's.
fn parents_first(rows: Vec<AgentRow>) -> Vec<AgentRow> {
    fn visit(at: usize, rows: &[AgentRow], path: &mut Vec<usize>, done: &mut Vec<usize>) {
        if done.contains(&at) || path.contains(&at) {
            return;
        }
        let parent = rows[at]
            .parent
            .as_ref()
            .and_then(|p| rows.iter().position(|other| other.id == *p));
        if let Some(parent) = parent {
            path.push(at);
            visit(parent, rows, path, done);
            path.pop();
        }
        if !done.contains(&at) {
            done.push(at);
        }
    }
    let mut done = Vec::new();
    for at in 0..rows.len() {
        visit(at, &rows, &mut Vec::new(), &mut done);
    }
    done.into_iter().map(|at| rows[at].clone()).collect()
}

fn cut_lists(row: &mut AgentRow, most: usize) {
    row.edits_omitted += row.edits.len().saturating_sub(most);
    row.edits.truncate(most);
    row.overlaps_omitted += row.overlaps.len().saturating_sub(most);
    row.overlaps.truncate(most);
}

fn cut_answer(row: &mut AgentRow, most: usize) {
    if let Some(done) = row.completion.as_mut() {
        done.answer = cap(&done.answer, most);
    }
}

/// What may go from a row to make the file fit, a step at a time, the least useful first. Each
/// step cuts a field and says how many paths it took out; none cuts serialized JSON.
const SQUEEZES: usize = 10;
fn squeeze(step: usize, row: &mut AgentRow) {
    match step {
        0 => row.last = cap(&row.last, 120),
        1 => cut_answer(row, 400),
        2 => {
            row.description = cap(&row.description, 80);
            row.last = cap(&row.last, 60);
        }
        3 => cut_lists(row, 4),
        4 => cut_answer(row, 160),
        5 => {
            row.last = String::new();
            row.description = cap(&row.description, 40);
        }
        6 => cut_lists(row, 1),
        7 => cut_answer(row, 40),
        8 => cut_lists(row, 0),
        _ => cut_answer(row, 0),
    }
}

pub fn projected_size(file: &AgentsFile) -> usize {
    utf8_length(&serde_json::to_string(file).unwrap_or_default())
}

pub fn project_agents_file(state: &AgentState, now: i64) -> AgentsFile {
    let (chosen, omitted, minimal) = selection(state);
    let mut rows = parents_first(chosen.iter().map(|rec| row_of(rec, &state.main)).collect());
    // A stand-in ancestor carries its place in the tree and nothing more.
    for row in rows.iter_mut().filter(|row| minimal.contains(&row.id)) {
        row.last = String::new();
        cut_answer(row, 0);
        cut_lists(row, 0);
    }
    let mut file = AgentsFile {
        version: 1,
        session: state.session.clone(),
        at: now,
        agents: rows,
        omitted_agents: omitted,
    };
    for step in 0..SQUEEZES {
        if projected_size(&file) <= FILE_BYTES {
            break;
        }
        file.agents.iter_mut().for_each(|row| squeeze(step, row));
    }
    // Still too big: the rows farthest down the order go, finished ones first, a parent only
    // after its children.
    while file.agents.len() > 1 && projected_size(&file) > FILE_BYTES {
        let parents: HashSet<&str> = file
            .agents
            .iter()
            .filter_map(|r| r.parent.as_deref())
            .collect();
        let victim = (file.agents.iter().enumerate())
            .filter(|(_, row)| !parents.contains(row.id.as_str()))
            .min_by_key(|(_, row)| (row.status.is_active(), row.last_at))
            .map(|(at, _)| at)
            // Every row is some row's parent (a cycle): the last goes, as the loop must end.
            .unwrap_or(file.agents.len() - 1);
        file.agents.remove(victim);
        file.omitted_agents += 1;
    }
    file
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Now,
    Later,
    None,
}

/// Whether the file is to be written now, later or not at all, and the state with that decided:
/// at most one write in `FILE_EVERY_MS`, the updates in between coalesced into one write when the
/// interval is up. `file_due` in the past is a timer that never fired (a reload cancelled it): it
/// counts for nothing. The wait is how long a `Later` timer runs.
pub fn plan_write(state: &mut AgentState, now: i64) -> (Act, i64) {
    if state.rows.is_empty() {
        return (Act::None, 0);
    }
    if state.file_due.is_some_and(|due| due > now) {
        return (Act::None, 0);
    }
    let since = state.file_at.map(|at| now - at);
    if since.is_none_or(|since| !(0..FILE_EVERY_MS).contains(&since)) {
        state.file_at = Some(now);
        state.file_due = None;
        return (Act::Now, 0);
    }
    let due = state.file_at.unwrap_or(now) + FILE_EVERY_MS;
    state.file_due = Some(due);
    (Act::Later, due - now)
}

// ---------------------------------------------------------------- the machine envelope

/// The first complete valid envelope of a text (its payload), or none. The reader is the pane's
/// (`companion::agents`), which the fixtures hold to the formatter.
pub fn parse_agent_envelope(source: &str) -> Option<Value> {
    agents::parse_envelopes(source)
        .into_iter()
        .next()
        .map(|(_, _, payload)| payload)
}

/// The finished runs a context envelope reports, by identity: each key is `[session, agent,
/// turn]` as `completion_key` makes it, here only the ones the ledger has claimed. These, and no
/// elapsed-time text, are what a completion's look is news for.
pub fn completion_news(
    envelope: Option<&Value>,
    ledger: &Ledger,
    wanted: &[String],
) -> Vec<String> {
    let Some(payload) = envelope.filter(|p| p["kind"] == "context") else {
        return Vec::new();
    };
    let session = payload["session"].as_str().unwrap_or("");
    let rows = payload["agents"].as_array().map_or(&[][..], Vec::as_slice);
    rows.iter()
        .filter_map(|row| {
            let done = row.get("completion").filter(|c| !c.is_null())?;
            let answer = done["answer"].as_str().unwrap_or("");
            (done["reason"] == "answer" && !js_trim(answer).is_empty()).then(|| {
                completion_key(
                    session,
                    row["id"].as_str().unwrap_or(""),
                    done["turn_id"].as_str().unwrap_or(""),
                )
            })
        })
        .filter(|key| wanted.contains(key) && ledger.get(key) == Some(&Disposition::Claimed))
        .collect()
}

/// What the prompt says of the agents (the dreamer's `AGENT_RULES`) from the brief's envelope:
/// the agents still running, as context only, and the reports of the runs this look was started
/// for, each in the agent's own words under its name. Nothing else of the agents is read: a
/// finished run no look claimed, a failed one, an empty one say nothing here.
pub fn agents_text(envelope: Option<&Value>, reported: &[String]) -> String {
    let Some(payload) = envelope.filter(|p| p["kind"] == "context") else {
        return String::new();
    };
    let session = payload["session"].as_str().unwrap_or("");
    let rows = payload["agents"].as_array().map_or(&[][..], Vec::as_slice);
    let running: Vec<String> = rows
        .iter()
        .filter(|row| {
            matches!(
                row["status"].as_str(),
                Some("running" | "waiting" | "pending")
            )
        })
        .map(|row| {
            let kind = row["type"].as_str().unwrap_or("an agent");
            format!("{kind} ({})", row["status"].as_str().unwrap_or(""))
        })
        .collect();
    let mut lines = Vec::new();
    if !running.is_empty() {
        lines.push(format!(
            "Still at work, for context only (nothing to ask about yet): {}.",
            running.join(", ")
        ));
    }
    for row in rows {
        let Some(done) = row.get("completion").filter(|c| !c.is_null()) else {
            continue;
        };
        let key = completion_key(
            session,
            row["id"].as_str().unwrap_or(""),
            done["turn_id"].as_str().unwrap_or(""),
        );
        if !reported.contains(&key) {
            continue;
        }
        let quoted: Vec<String> = (done["answer"].as_str().unwrap_or("").split('\n'))
            .map(|line| format!("  > {line}"))
            .collect();
        let kind = row["type"].as_str().unwrap_or("An agent");
        lines.push(format!(
            "- {kind} finished, and reported:\n{}",
            quoted.join("\n")
        ));
    }
    lines.join("\n")
}

/// The requests (handoff envelopes) a text carries for this session, in order, as `(request,
/// thread)`.
pub fn handoff_requests(source: &str, session: &str) -> Vec<(String, String)> {
    agents::parse_envelopes(source)
        .into_iter()
        .filter(|(_, _, payload)| payload["kind"] == "handoff" && payload["session"] == session)
        .map(|(_, _, payload)| {
            let text = |name: &str| payload[name].as_str().unwrap_or("").to_string();
            (text("request"), text("thread"))
        })
        .collect()
}

// ---------------------------------------------------------------- handing a thread to an agent

/// The pane drafts a request (a click on "to subagent"): an id, and the thread, in an envelope
/// appended to the unsent prompt. What follows is only ever this, by identity and never by
/// likeness of words or "the next agent spawned":
///  1. the person sends a prompt carrying the envelope: submitted;
///  2. Claude's own Agent call carries the envelope in its prompt: the call is that request's;
///  3. the call started an agent: bound to its id and first turn; denied, or the envelope in two
///     requests' company: not linked;
///  4. that turn ends: succeeded with a report, else failed (an error, a refusal, an interrupt,
///     no words, killed).
///
/// A nested call (an agent's own, `parentAgentId`) carrying the same envelope binds nothing. A
/// retry is a new request: an earlier failure never reaches it.
#[derive(Clone, Debug)]
pub enum HandoffEvent {
    Submitted {
        at: i64,
        session: String,
        request: String,
        thread: String,
        drafted_at: i64,
    },
    Spawn {
        requests: Vec<String>,
        tool_use_id: String,
        nested: bool,
    },
    Spawned {
        tool_use_id: String,
        agent_id: Option<String>,
        turn: Option<String>,
    },
    Turn {
        agent_id: String,
        turn_id: String,
    },
    Ended {
        at: i64,
        agent_id: String,
        turn_id: Option<String>,
        reason: String,
        empty: bool,
    },
}

/// The handoffs with one event applied. True when the TypeScript would hand back a new array
/// (any change at all, even one that writes what was already there); false when it hands back
/// the very array.
pub fn associate_handoff(handoffs: &mut Vec<AgentHandoff>, event: HandoffEvent) -> bool {
    match event {
        HandoffEvent::Submitted {
            at,
            session,
            request,
            thread,
            drafted_at,
        } => {
            if let Some(have) = handoffs.iter_mut().find(|h| h.request == request) {
                if have.submitted_at.is_some() {
                    return false;
                }
                have.submitted_at = Some(at);
                if have.outcome == "draft" {
                    have.outcome = "submitted".into();
                }
                return true;
            }
            handoffs.push(AgentHandoff {
                request,
                session,
                thread,
                drafted_at,
                submitted_at: Some(at),
                spawn_tool_use_id: None,
                agent_id: None,
                turn_id: None,
                outcome: "submitted".into(),
                ended_at: None,
                reason: None,
            });
            let from = handoffs.len().saturating_sub(HANDOFFS_KEPT);
            handoffs.drain(..from);
            true
        }
        HandoffEvent::Spawn {
            requests,
            tool_use_id,
            nested,
        } => {
            if nested {
                return false;
            }
            let mut wanted: Vec<&String> = Vec::new();
            for request in &requests {
                let open = handoffs.iter().any(|h| {
                    h.request == *request
                        && h.outcome == "submitted"
                        && h.spawn_tool_use_id.is_none()
                });
                if open && !wanted.contains(&request) {
                    wanted.push(request);
                }
            }
            // Two requests in one prompt: which agent is whose cannot be told. Neither is
            // linked.
            match wanted.len() {
                0 => false,
                1 => {
                    let at = handoffs.iter().position(|h| h.request == *wanted[0]);
                    let have = &mut handoffs[at.expect("one of them is kept")];
                    have.spawn_tool_use_id = Some(tool_use_id);
                    have.reason = None;
                    true
                }
                _ => {
                    for have in handoffs.iter_mut() {
                        if wanted.contains(&&have.request) && have.outcome == "submitted" {
                            have.reason = Some("ambiguous".into());
                        }
                    }
                    true
                }
            }
        }
        HandoffEvent::Spawned {
            tool_use_id,
            agent_id,
            turn,
        } => {
            let Some(have) = handoffs.iter_mut().find(|h| {
                h.spawn_tool_use_id.as_deref() == Some(tool_use_id.as_str())
                    && h.outcome == "submitted"
            }) else {
                return false;
            };
            match agent_id.filter(|id| !id.is_empty()) {
                None => have.reason = Some("spawn denied".into()),
                Some(agent_id) => {
                    have.outcome = "bound".into();
                    have.agent_id = Some(agent_id);
                    have.turn_id = turn;
                    have.reason = None;
                }
            }
            true
        }
        HandoffEvent::Turn { agent_id, turn_id } => {
            let Some(have) = handoffs.iter_mut().find(|h| {
                h.outcome == "bound"
                    && h.agent_id.as_deref() == Some(agent_id.as_str())
                    && h.turn_id.is_none()
            }) else {
                return false;
            };
            have.turn_id = Some(turn_id);
            true
        }
        HandoffEvent::Ended {
            at,
            agent_id,
            turn_id,
            reason,
            empty,
        } => {
            // Only the run the handoff was bound to: a later run of the same agent is not its
            // outcome, nor an earlier one.
            let Some(have) = handoffs.iter_mut().find(|h| {
                h.outcome == "bound"
                    && h.agent_id.as_deref() == Some(agent_id.as_str())
                    && (h.turn_id.is_none()
                        || turn_id.is_none()
                        || h.turn_id.as_deref() == turn_id.as_deref())
            }) else {
                return false;
            };
            let ok = reason == "answer" && !empty;
            have.turn_id = have.turn_id.take().or(turn_id);
            have.ended_at = Some(at);
            have.outcome = if ok { "succeeded" } else { "failed" }.into();
            have.reason = match (ok, reason.as_str()) {
                (true, _) => None,
                (false, "answer") => Some("no report".into()),
                (false, other) => Some(other.to_string()),
            };
            true
        }
    }
}
