use super::{array, num, paths::Paths, render::text::control, string};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::{collections::HashSet, fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt};

pub const MAX_BYTES: usize = 16384;
pub const OPEN: &str = "[[CSR:AGENTS:v1]]";
pub const CLOSE: &str = "[[/CSR:AGENTS:v1]]";
pub const HANDOFF_INSTRUCTION: &str =
    "Copy this envelope unchanged into the background agent's prompt.";
const STATUSES: &[&str] = &[
    "pending",
    "running",
    "waiting",
    "idle",
    "completed",
    "failed",
    "killed",
    "unknown",
];
const REASONS: &[&str] = &["answer", "aborted", "refusal", "error"];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Completion {
    pub key: String,
    #[serde(
        rename(deserialize = "turnId", serialize = "turn_id"),
        alias = "turn_id"
    )]
    pub turn_id: String,
    pub at: f64,
    pub reason: String,
    pub answer: String,
    pub disposition: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    pub id: String,
    pub parent: Option<String>,
    pub description: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub model: Option<String>,
    pub status: String,
    pub started: Option<f64>,
    pub first_seen: f64,
    pub last_at: f64,
    pub tools: u64,
    pub tokens: u64,
    pub edits: Vec<String>,
    pub edits_omitted: u64,
    pub overlaps: Vec<String>,
    pub overlaps_omitted: u64,
    pub last: String,
    pub last_tool: Option<String>,
    pub current_turn: Option<String>,
    pub completion: Option<Completion>,
    pub listed: bool,
}
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct Projection {
    pub agents: Vec<Agent>,
    pub omitted_agents: u64,
    pub at: Option<f64>,
}
fn id(s: &str, extras: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || extras.as_bytes().contains(&c))
}
pub fn session_id(s: &str) -> bool {
    id(s, "_-")
}
fn agent_id(s: &str) -> bool {
    id(s, "_.:@-")
}
fn number(n: f64) -> bool {
    n.is_finite() && n.abs() < 1e18
}
fn clean(s: &str, most: usize) -> String {
    s.chars().filter(|c| !control(*c)).take(most).collect()
}
fn keys(v: &Value, expected: &[&str]) -> bool {
    v.as_object()
        .is_some_and(|v| v.len() == expected.len() && expected.iter().all(|k| v.contains_key(*k)))
}

pub fn row_of(raw: &Value) -> Option<Agent> {
    if !keys(
        raw,
        &[
            "id",
            "parent",
            "description",
            "type",
            "model",
            "status",
            "started",
            "first_seen",
            "last_at",
            "tools",
            "tokens",
            "edits",
            "edits_omitted",
            "overlaps",
            "overlaps_omitted",
            "last",
            "last_tool",
            "current_turn",
            "completion",
            "listed",
        ],
    ) {
        return None;
    }
    let mut row: Agent = serde_json::from_value(raw.clone()).ok()?;
    let done_ok = row.completion.as_ref().is_none_or(|c| {
        keys(
            &raw["completion"],
            &["key", "turnId", "at", "reason", "answer", "disposition"],
        ) && c.key.chars().count() <= 600
            && agent_id(&c.turn_id)
            && number(c.at)
            && REASONS.contains(&c.reason.as_str())
            && ["pending", "claimed", "looked", "skipped", "failed"]
                .contains(&c.disposition.as_str())
    });
    if !agent_id(&row.id)
        || row.parent.as_deref().is_some_and(|s| !agent_id(s))
        || !STATUSES.contains(&row.status.as_str())
        || !number(row.first_seen)
        || !number(row.last_at)
        || row.started.is_some_and(|n| !number(n))
        || [
            row.tools,
            row.tokens,
            row.edits_omitted,
            row.overlaps_omitted,
        ]
        .iter()
        .any(|n| *n >= 1_000_000_000)
        || row.edits.len() > 8
        || row.overlaps.len() > 8
        || !done_ok
        || row.current_turn.as_deref().is_some_and(|s| !agent_id(s))
    {
        return None;
    }
    row.description = clean(&row.description, 300);
    row.last = clean(&row.last, 300);
    for value in [&mut row.kind, &mut row.model, &mut row.last_tool]
        .into_iter()
        .flatten()
    {
        *value = clean(value, 300);
    }
    for path in row.edits.iter_mut().chain(&mut row.overlaps) {
        *path = clean(path, 300);
    }
    if let Some(done) = &mut row.completion {
        done.answer = clean(&done.answer, 700);
    }
    Some(row)
}

pub fn check(raw: &Value, session: &str) -> Projection {
    let empty = Projection::default;
    if !keys(
        raw,
        &["version", "session", "at", "agents", "omitted_agents"],
    ) || raw["version"] != 1
        || raw["session"] != session
        || raw["at"].as_f64().is_none_or(|n| !number(n))
        || raw["omitted_agents"]
            .as_u64()
            .is_none_or(|n| n >= 1_000_000_000)
        || !raw["agents"].is_array()
        || array(&raw["agents"]).len() > 12
    {
        return empty();
    }
    let Some(agents) = array(&raw["agents"])
        .iter()
        .map(row_of)
        .collect::<Option<Vec<_>>>()
    else {
        return empty();
    };
    if agents.iter().map(|a| &a.id).collect::<HashSet<_>>().len() != agents.len() {
        return empty();
    }
    Projection {
        agents,
        omitted_agents: raw["omitted_agents"].as_u64().unwrap(),
        at: raw["at"].as_f64(),
    }
}
fn bounded(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = vec![];
    file.take((limit + 1) as u64).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= limit).then_some(bytes)
}
pub fn read_agents(path: &Path, session: &str) -> Projection {
    if !session_id(session) {
        return Projection::default();
    }
    bounded(path, MAX_BYTES)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .map(|v| check(&v, session))
        .unwrap_or_default()
}
pub fn agents_of(paths: &Paths, session: &str) -> Projection {
    paths
        .kept_file("agents", session)
        .map(|p| read_agents(&p, session))
        .unwrap_or_default()
}
pub fn is_active(row: &Agent) -> bool {
    ["pending", "running", "waiting"].contains(&row.status.as_str())
}
pub fn stalled(row: &Agent, now: f64) -> bool {
    row.status == "running" && now - row.last_at / 1000. >= 600.
}
pub fn no_report(row: &Agent) -> bool {
    row.completion
        .as_ref()
        .is_some_and(|c| c.reason == "answer" && c.answer.trim().is_empty())
}
pub fn reported(row: &Agent) -> &str {
    row.completion
        .as_ref()
        .filter(|c| c.reason == "answer")
        .map(|c| c.answer.trim())
        .unwrap_or("")
}
pub fn recent_completion(agents: &[Agent], now: f64) -> Option<f64> {
    agents
        .iter()
        .filter_map(|a| a.completion.as_ref().map(|c| c.at / 1000.))
        .filter(|t| now - t <= 1800.)
        .max_by(f64::total_cmp)
}
pub fn tree(agents: &[Agent]) -> Vec<(&Agent, usize, bool)> {
    fn visit<'a>(
        row: &'a Agent,
        depth: usize,
        order: &[&'a Agent],
        seen: &mut HashSet<&'a str>,
        out: &mut Vec<(&'a Agent, usize, bool)>,
    ) {
        if !seen.insert(&row.id) {
            return;
        }
        let unknown = row
            .parent
            .as_ref()
            .is_some_and(|p| !order.iter().any(|a| a.id == *p));
        out.push((row, depth, unknown));
        for child in order
            .iter()
            .filter(|a| a.parent.as_deref() == Some(&row.id))
        {
            visit(child, depth + 1, order, seen, out);
        }
    }
    let mut order: Vec<_> = agents.iter().collect();
    order.sort_by(|a, b| {
        (!is_active(a))
            .cmp(&(!is_active(b)))
            .then(b.last_at.total_cmp(&a.last_at))
            .then(a.id.cmp(&b.id))
    });
    let mut out = vec![];
    let mut seen = HashSet::new();
    for row in &order {
        if row
            .parent
            .as_ref()
            .is_none_or(|p| *p == row.id || !order.iter().any(|a| a.id == *p))
        {
            visit(row, 0, &order, &mut seen, &mut out);
        }
    }
    for row in &order {
        visit(row, 0, &order, &mut seen, &mut out);
    }
    out
}

pub fn valid_payload(value: &Value) -> bool {
    super::envelopes::valid(value)
}

#[derive(Serialize, Deserialize)]
struct EnvelopeContext {
    kind: String,
    session: String,
    at: serde_json::Number,
    agents: Vec<EnvelopeAgent>,
}
#[derive(Serialize, Deserialize)]
struct EnvelopeAgent {
    id: String,
    parent: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    model: Option<String>,
    status: String,
    edits: Vec<String>,
    overlaps: Vec<String>,
    completion: Option<EnvelopeCompletion>,
}
#[derive(Serialize, Deserialize)]
struct EnvelopeCompletion {
    turn_id: String,
    reason: String,
    answer: String,
}
#[derive(Serialize, Deserialize)]
struct HandoffEnvelope {
    kind: String,
    session: String,
    request: String,
    thread: String,
    instruction: String,
}
pub fn format_agents_context(payload: &Value) -> Result<String, &'static str> {
    if !valid_payload(payload) {
        return Err("not an agents context or handoff payload");
    }
    let dumped = if payload["kind"] == "context" {
        let context: EnvelopeContext =
            serde_json::from_value(payload.clone()).map_err(|_| "invalid payload")?;
        serde_json::to_string(&context)
    } else {
        let handoff: HandoffEnvelope =
            serde_json::from_value(payload.clone()).map_err(|_| "invalid payload")?;
        serde_json::to_string(&handoff)
    }
    .map_err(|_| "invalid payload")?;
    format_dumped(&dumped)
}
fn format_dumped(dumped: &str) -> Result<String, &'static str> {
    let (mut inside, mut escaped) = (false, false);
    let mut body = String::new();
    for c in dumped.chars() {
        if inside {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                inside = false;
            } else if c == '[' {
                body.push_str("\\u005b");
                continue;
            } else if c == ']' {
                body.push_str("\\u005d");
                continue;
            }
        } else if c == '"' {
            inside = true;
        }
        body.push(c);
    }
    let text = format!("{OPEN}\n{body}\n{CLOSE}\n");
    if text.len() > MAX_BYTES {
        Err("the envelope is over 16,384 bytes")
    } else {
        Ok(text)
    }
}
#[derive(Serialize)]
struct Context<'a> {
    kind: &'static str,
    session: &'a str,
    at: i64,
    agents: Vec<ContextAgent<'a>>,
}
#[derive(Serialize)]
struct ContextAgent<'a> {
    id: &'a str,
    parent: Option<&'a str>,
    #[serde(rename = "type")]
    kind: Option<&'a str>,
    model: Option<&'a str>,
    status: &'a str,
    edits: &'a [String],
    overlaps: &'a [String],
    completion: Option<Report<'a>>,
}
#[derive(Serialize)]
struct Report<'a> {
    turn_id: &'a str,
    reason: &'static str,
    answer: String,
    #[serde(skip)]
    _row: std::marker::PhantomData<&'a ()>,
}
fn context<'a>(projection: &'a Projection, session: &'a str) -> Context<'a> {
    Context {
        kind: "context",
        session,
        at: projection.at.unwrap_or(0.) as i64,
        agents: projection
            .agents
            .iter()
            .take(12)
            .map(|row| ContextAgent {
                id: &row.id,
                parent: row.parent.as_deref(),
                kind: row.kind.as_deref(),
                model: row.model.as_deref(),
                status: &row.status,
                edits: &row.edits,
                overlaps: &row.overlaps,
                completion: row
                    .completion
                    .as_ref()
                    .filter(|c| c.reason == "answer" && !c.answer.trim().is_empty())
                    .map(|c| Report {
                        turn_id: &c.turn_id,
                        reason: "answer",
                        answer: c.answer.clone(),
                        _row: std::marker::PhantomData,
                    }),
            })
            .collect(),
    }
}
pub fn context_payload(projection: &Projection, session: &str) -> Value {
    serde_json::to_value(context(projection, session)).unwrap()
}
pub fn context_envelope(projection: &Projection, session: &str) -> String {
    let mut payload = context(projection, session);
    if payload.agents.is_empty() {
        return String::new();
    }
    for most in [700, 400, 160, 40, 0] {
        for a in &mut payload.agents {
            if let Some(report) = &mut a.completion {
                report.answer = report.answer.chars().take(most).collect();
                if most == 0 {
                    a.completion = None;
                }
            }
        }
        if let Ok(text) = format_dumped(&serde_json::to_string(&payload).unwrap()) {
            return text;
        }
    }
    while payload.agents.pop().is_some() && !payload.agents.is_empty() {
        if let Ok(text) = format_dumped(&serde_json::to_string(&payload).unwrap()) {
            return text;
        }
    }
    String::new()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub request: String,
    pub session: String,
    pub thread: String,
    pub drafted_at: f64,
    pub submitted_at: Option<f64>,
    pub spawn_tool_use_id: Option<String>,
    pub agent_id: Option<String>,
    pub turn_id: Option<String>,
    pub outcome: String,
    pub ended_at: Option<f64>,
    pub reason: Option<String>,
}
pub fn handoff_of(raw: &Value, session: &str) -> Option<Handoff> {
    if !keys(
        raw,
        &[
            "request",
            "session",
            "thread",
            "drafted_at",
            "submitted_at",
            "spawn_tool_use_id",
            "agent_id",
            "turn_id",
            "outcome",
            "ended_at",
            "reason",
        ],
    ) {
        return None;
    }
    let mut h: Handoff = serde_json::from_value(raw.clone()).ok()?;
    if h.session != session
        || !id(&h.request, "_.:-")
        || !thread_id(&h.thread)
        || !number(h.drafted_at)
        || h.submitted_at.is_some_and(|n| !number(n))
        || h.ended_at.is_some_and(|n| !number(n))
        || !["draft", "submitted", "bound", "succeeded", "failed"].contains(&h.outcome.as_str())
        || [&h.spawn_tool_use_id, &h.agent_id, &h.turn_id, &h.reason]
            .iter()
            .any(|s| {
                s.as_ref()
                    .is_some_and(|s| s.chars().count() > 200 || s.chars().any(control))
            })
    {
        return None;
    }
    h.submitted_at = h.submitted_at.map(|n| n / 1000.);
    h.ended_at = h.ended_at.map(|n| n / 1000.);
    Some(h)
}
fn thread_id(s: &str) -> bool {
    s.len() == 12
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn read_handoffs(state: &Path, drafts: &Path, session: &str) -> Vec<Handoff> {
    if !session_id(session) {
        return vec![];
    }
    let raw: Value = bounded(state, 4 * MAX_BYTES)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut out = if keys(&raw, &["version", "session", "handoffs"])
        && raw["version"] == 1
        && raw["session"] == session
        && raw["handoffs"].is_array()
        && array(&raw["handoffs"]).len() <= 40
    {
        array(&raw["handoffs"])
            .iter()
            .filter_map(|v| handoff_of(v, session))
            .collect()
    } else {
        vec![]
    };
    let regular = std::fs::symlink_metadata(drafts).is_ok_and(|m| m.is_file());
    let bytes = if regular {
        super::tail::head(drafts, (4 * MAX_BYTES) as u64).unwrap_or_default()
    } else {
        vec![]
    };
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<_> = text.lines().collect();
    for line in lines.iter().skip(lines.len().saturating_sub(160)) {
        let Ok(item) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let request = string(&item["request"]);
        let thread = string(&item["thread"]);
        if !id(request, "_.:-")
            || !thread_id(thread)
            || item["drafted_at"].as_f64().is_none_or(|n| !number(n))
            || out.iter().any(|h: &Handoff| h.request == request)
        {
            continue;
        }
        out.push(Handoff {
            request: request.into(),
            session: session.into(),
            thread: thread.into(),
            drafted_at: num(&item["drafted_at"]),
            submitted_at: None,
            spawn_tool_use_id: None,
            agent_id: None,
            turn_id: None,
            outcome: "draft".into(),
            ended_at: None,
            reason: None,
        });
    }
    out.sort_by(|a, b| a.drafted_at.total_cmp(&b.drafted_at));
    out.drain(..out.len().saturating_sub(40));
    out
}
pub fn handoffs_of(paths: &Paths, session: &str) -> Vec<Handoff> {
    let Some(state) = paths.kept_file("handoffs", session) else {
        return vec![];
    };
    let drafts = paths
        .keep
        .join("handoffs")
        .join(format!("{session}.drafts.jsonl"));
    read_handoffs(&state, &drafts, session)
}

pub fn handoff_envelope(
    session: &str,
    request: &str,
    thread: &str,
) -> Result<String, &'static str> {
    if !session_id(session) || !id(request, "_.:-") || !thread_id(thread) {
        return Err("not an agents context or handoff payload");
    }
    format_dumped(
        &serde_json::to_string(&HandoffEnvelope {
            kind: "handoff".into(),
            session: session.into(),
            request: request.into(),
            thread: thread.into(),
            instruction: HANDOFF_INSTRUCTION.into(),
        })
        .unwrap(),
    )
}

pub fn decide(handoffs: &[Handoff]) -> Option<(&str, &Handoff)> {
    let (_, last) = handoffs
        .iter()
        .enumerate()
        .filter(|(_, h)| h.outcome != "draft")
        .max_by(|(i, a), (j, b)| {
            a.submitted_at
                .unwrap_or(a.drafted_at)
                .total_cmp(&b.submitted_at.unwrap_or(b.drafted_at))
                .then(a.drafted_at.total_cmp(&b.drafted_at))
                .then(j.cmp(i))
        })?;
    let word = match last.outcome.as_str() {
        "submitted" => "unlinked",
        "failed" => "failed",
        _ => "handed",
    };
    Some((word, last))
}

pub fn parse_envelopes(text: &str) -> Vec<(usize, usize, Value)> {
    super::envelopes::parse(text)
        .into_iter()
        .map(|e| {
            (
                text[..e.start].chars().count(),
                text[..e.end].chars().count(),
                e.payload,
            )
        })
        .collect()
}
pub fn strip_envelopes(text: &str) -> String {
    super::envelopes::strip(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    fn row(id: &str) -> Value {
        json!({
            "id":id,"parent":null,"description":"Scan the repo","type":"Explore",
            "model":"claude-opus-5","status":"running","started":1799999700000i64,
            "first_seen":1799999700000i64,"last_at":1799999990000i64,"tools":3,"tokens":1200,
            "edits":[],"edits_omitted":0,"overlaps":[],"overlaps_omitted":0,
            "last":"Reading the files","last_tool":"Read","current_turn":"turn-1",
            "completion":null,"listed":true,
        })
    }
    fn doc(rows: Vec<Value>) -> Value {
        json!({"version":1,"session":"sess-a","at":1800000000000i64,
            "agents":rows,"omitted_agents":0})
    }
    fn fixture() -> Value {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/contamination/agents-v1.json"
        ))
        .unwrap()
    }
    fn payload() -> Value {
        fixture()["fixtures"]["context"]["payload"].clone()
    }

    #[test]
    fn test_corrupt_or_wrong_session_agents_file_fails_open() {
        let good = doc(vec![row("a1")]);
        assert_eq!(check(&good, "sess-a").agents.len(), 1);
        let mut bad = vec![];
        for (key, value) in [
            ("version", json!(2)),
            ("session", json!("other")),
            ("extra", json!(1)),
            ("at", json!(true)),
            ("omitted_agents", json!(-1)),
        ] {
            let mut raw = good.clone();
            raw[key] = value;
            bad.push(raw);
        }
        for (key, value) in [
            ("status", json!("done")),
            ("tools", json!("3")),
            ("tools", json!(true)),
            ("id", json!("../x")),
            ("completion", json!({"key":"k"})),
            ("edits", json!(vec!["x"; 9])),
        ] {
            let mut raw = good.clone();
            raw["agents"][0][key] = value;
            bad.push(raw);
        }
        bad.push(doc(vec![row("a1"), row("a1")]));
        bad.push(doc((0..13).map(|i| row(&format!("a{i}"))).collect()));
        for raw in bad {
            assert!(check(&raw, "sess-a").agents.is_empty(), "{raw}");
        }
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("s.json");
        for raw in [
            b"{\"version\":1,".as_slice(),
            b"\xff\xfe",
            b"[]",
            b"null",
            b"",
        ] {
            fs::write(&path, raw).unwrap();
            assert!(read_agents(&path, "sess-a").agents.is_empty());
        }
        let mut large = good.clone();
        large["agents"][0]["last"] = "x".repeat(20000).into();
        fs::write(&path, large.to_string()).unwrap();
        assert!(read_agents(&path, "sess-a").agents.is_empty());
        assert!(read_agents(&path, "../../etc/passwd").agents.is_empty());
    }
    #[test]
    fn test_a_link_a_directory_and_an_unreadable_file_read_as_no_agents() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("real.json");
        let path = d.path().join("s.json");
        fs::write(&target, doc(vec![row("a1")]).to_string()).unwrap();
        symlink(&target, &path).unwrap();
        assert!(read_agents(&path, "sess-a").agents.is_empty());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(read_agents(&path, "sess-a").agents.is_empty());
        fs::set_permissions(&target, fs::Permissions::from_mode(0o0)).unwrap();
        if unsafe { libc::geteuid() } != 0 {
            assert!(read_agents(&target, "sess-a").agents.is_empty());
        }
    }
    #[test]
    fn test_a_row_with_an_unknown_parent_is_a_root_marked_so_and_a_cycle_is_cut() {
        let mut a = row("a");
        let mut b = row("b");
        let mut c = row("c");
        a["parent"] = "b".into();
        b["parent"] = "a".into();
        c["parent"] = "absent".into();
        let projection = check(&doc(vec![a, b, c]), "sess-a");
        let tree = tree(&projection.agents);
        assert_eq!(tree.len(), 3);
        assert!(tree.iter().any(|(a, d, u)| a.id == "c" && *d == 0 && *u));
        assert_eq!(tree.iter().filter(|(_, d, _)| *d == 1).count(), 1);
    }
    #[test]
    fn test_failed_killed_stalled_and_empty_agents_are_told_apart() {
        let mut raw = row("a1");
        raw["last_at"] = (1800000000000i64 - 600000).into();
        let mut a = row_of(&raw).unwrap();
        assert!(stalled(&a, 1800000000.));
        for status in [
            "pending",
            "waiting",
            "idle",
            "completed",
            "failed",
            "killed",
        ] {
            a.status = status.into();
            assert!(!stalled(&a, 1800000000.));
        }
        a.completion = Some(Completion {
            key: "k".into(),
            turn_id: "t".into(),
            at: 1800000000000.,
            reason: "answer".into(),
            answer: "  ".into(),
            disposition: "skipped".into(),
        });
        assert!(no_report(&a));
        assert_eq!(reported(&a), "");
        a.completion.as_mut().unwrap().answer = " Report. ".into();
        assert_eq!(reported(&a), "Report.");
        assert_eq!(recent_completion(&[a], 1800000001.), Some(1800000000.));
    }
    #[test]
    fn test_the_context_envelope_is_exactly_three_lines_with_its_brackets_escaped() {
        let context = payload();
        let text = format_agents_context(&context).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(text.ends_with('\n'));
        assert!(text.contains(r"\u005b\u005bCSR:AGENTS:v1\u005d\u005d"));
        assert!(text.contains(r"notes\u005b1\u005d.md"));
        assert_eq!(
            serde_json::from_str::<Value>(text.lines().nth(1).unwrap()).unwrap(),
            context
        );
    }
    #[test]
    fn test_only_valid_payloads_are_formatted_and_the_size_is_bounded() {
        let good = payload();
        for (key, value) in [
            ("extra", json!(1)),
            ("kind", json!("other")),
            ("session", json!("../x")),
            ("agents", json!("x")),
            ("agents", json!([{}])),
        ] {
            let mut bad = good.clone();
            bad[key] = value;
            assert!(format_agents_context(&bad).is_err());
        }
        let mut wide = good.clone();
        let mut row = wide["agents"][0].clone();
        row["edits"] = json!(vec!["é".repeat(300); 8]);
        row["overlaps"] = json!(vec!["é".repeat(300); 8]);
        wide["agents"] = json!(vec![row; 12]);
        assert!(format_agents_context(&wide).is_err());
    }
    #[test]
    fn test_the_size_limit_is_on_the_bytes_the_text_takes_line_endings_included() {
        let text = handoff_envelope("sess-a", "req-1", "0123456789ab").unwrap();
        let pad = MAX_BYTES - text.len();
        let lines: Vec<_> = text.lines().collect();
        let lf = format!(
            "{}\n{}{}\n{}\n",
            lines[0],
            lines[1],
            " ".repeat(pad),
            lines[2]
        );
        assert_eq!(lf.len(), MAX_BYTES);
        assert_eq!(parse_envelopes(&lf).len(), 1);
        let crlf = lf.replace('\n', "\r\n");
        assert!(parse_envelopes(&crlf).is_empty());
        assert_eq!(strip_envelopes(&crlf), crlf);
    }
    #[test]
    fn test_a_handoff_envelope_carries_its_instruction_word_for_word() {
        let text = handoff_envelope("sess-a", "req-1", "0123456789ab").unwrap();
        let found = parse_envelopes(&text);
        assert_eq!((found[0].0, found[0].1), (0, text.chars().count()));
        assert_eq!(found[0].2["instruction"], HANDOFF_INSTRUCTION);
    }
    #[test]
    fn test_brief_agent_envelope_round_trip() {
        let text = format_agents_context(&payload()).unwrap();
        let around = format!(
            "Facts about the work.\n* a line\n\n{}",
            text.trim_end_matches('\n')
        );
        assert_eq!(parse_envelopes(&around)[0].2, payload());
        assert_eq!(
            strip_envelopes(&around),
            "Facts about the work.\n* a line\n\n"
        );
    }
    #[test]
    fn test_agent_envelope_preserves_surrounding_user_reply() {
        let text = format!(
            "Yes, please do that.\n{}And check the importer too.",
            format_agents_context(&payload()).unwrap()
        );
        let want = "Yes, please do that.\nAnd check the importer too.";
        assert_eq!(strip_envelopes(&text), want);
        assert_eq!(super::super::send::own_words(&text), want);
    }
    #[test]
    fn test_only_complete_valid_envelopes_at_the_start_of_a_line_are_removed() {
        let good = format_agents_context(&payload()).unwrap();
        let body = good.lines().nth(1).unwrap();
        for bad in [
            format!("  {good}"),
            format!("> {good}"),
            good.replace("v1]]", "v2]]"),
            format!("{OPEN}\nnot json\n{CLOSE}\n"),
            format!("{OPEN}\n{body}\n"),
            format!("{OPEN}\n{body}\n{body}\n{CLOSE}\n"),
            format!("{body}\n"),
        ] {
            assert!(parse_envelopes(&bad).is_empty());
            assert_eq!(strip_envelopes(&bad), bad);
        }
        for good in [
            good.replace('\n', "\r\n"),
            format!("\u{feff}{good}"),
            good.trim_end_matches('\n').to_owned(),
        ] {
            assert_eq!(parse_envelopes(&good).len(), 1);
            assert_eq!(strip_envelopes(&good), "");
        }
    }
    #[test]
    fn test_the_fixture_is_what_the_formatter_writes_and_never_rewritten_by_check() {
        for entry in fixture()["fixtures"].as_object().unwrap().values() {
            assert_eq!(
                format_agents_context(&entry["payload"]).unwrap(),
                entry["text"]
            );
            assert_eq!(
                parse_envelopes(entry["text"].as_str().unwrap())[0].2,
                entry["payload"]
            );
        }
    }
    #[test]
    fn test_one_parent_and_seven_children_fit_with_their_reports() {
        let mut rows = vec![row("p1")];
        for n in 0..7 {
            let mut a = row(&format!("c{n}"));
            a["parent"] = "p1".into();
            a["completion"] = json!({"key":"key","turnId":"turn-1","at":1800000000000i64,
                "reason":"answer","answer":"Report. ".repeat(80),"disposition":"pending"});
            rows.push(a);
        }
        let projection = check(&doc(rows), "sess-a");
        let text = context_envelope(&projection, "sess-a");
        assert!(text.len() <= MAX_BYTES);
        assert_eq!(
            parse_envelopes(&text)[0].2["agents"]
                .as_array()
                .unwrap()
                .len(),
            8
        );
        assert_eq!(tree(&projection.agents)[0].0.id, "p1");
    }
    #[test]
    fn test_handoff_times_the_hooks_write_in_milliseconds_read_as_seconds() {
        let raw = json!({"request":"req-1","session":"sess-a","thread":"0123456789ab",
            "drafted_at":1800000000.,"submitted_at":1800000010000i64,"ended_at":1800000020000i64,
            "spawn_tool_use_id":"u1","agent_id":"a1","turn_id":"t1",
            "outcome":"failed","reason":"error"});
        let read = handoff_of(&raw, "sess-a").unwrap();
        assert_eq!(
            (read.submitted_at, read.ended_at),
            (Some(1800000010.), Some(1800000020.))
        );
    }
}
