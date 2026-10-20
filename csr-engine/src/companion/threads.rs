//! The summary page (`s`) and the threads page (`f`), and the lost-and-found writers a click on a
//! thread's answers uses. pane.py's `newest_summary`, `summary_lines`, `draw_summary`,
//! `thread_group`, `draw_threads`, `lost_asks`, `lost_rows`, `keep_lost` and `new_handoff`.
use super::{
    agents, array, num,
    pages::Lines,
    paths::{epoch, Paths},
    private,
    render::text::*,
    session::Session,
    string, truthy,
};
use chrono::{Local, TimeZone};
use serde_json::{json, Value};
use std::path::Path;

const ITEM: usize = 6;
const PHONE_GUTTER: usize = 2;
pub const LOST_ANSWERS: [&str; 2] = ["drop", "later"];
/// (key, title, short name) of each group on the threads page, in the order they are drawn.
const THREAD_GROUPS: [(&str, &str, &str); 9] = [
    ("waiting", "Waiting on you", "waiting"),
    ("lost", "Went quiet", "quiet"),
    ("carried", "From the session before", "before"),
    ("open", "Still open", "open"),
    ("faded", "Showed once", "once"),
    ("answered", "Guessed done", "guessed"),
    ("later", "Set aside for later", "later"),
    ("handed", "Handed off", "handed"),
    ("drop", "Dropped", "dropped"),
];
const LOST_ASKS: [(&str, &str); 4] = [
    ("side", "→ side task"),
    ("sub", "→ subagent"),
    ("drop", "✗ drop"),
    ("later", "later"),
];

fn thousands(n: i64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn stamped(at: f64, format: &str) -> String {
    Local
        .timestamp_opt(at as i64, 0)
        .single()
        .map(|t| t.format(format).to_string())
        .unwrap_or_default()
}

fn row_text(entry: &Value) -> String {
    let content = &entry["message"]["content"];
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p["type"] == "text")
            .map(|p| string(&p["text"]))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The newest compaction summary in a session's transcript (the isCompactSummary row):
/// `{text, at, count}`, or null when there is none. Only whole lines count.
pub fn newest_summary(path: Option<&Path>) -> Value {
    let Some(bytes) = path.and_then(|p| std::fs::read(p).ok()) else {
        return Value::Null;
    };
    let (mut found, mut count) = (Value::Null, 0);
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        if !line.ends_with(b"\n") {
            break;
        }
        if !line.windows(16).any(|w| w == b"isCompactSummary") {
            continue;
        }
        let Ok(entry) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let text = row_text(&entry);
        if truthy(&entry["isCompactSummary"]) && !text.trim().is_empty() {
            count += 1;
            let at = entry["timestamp"].as_str().and_then(epoch);
            found = json!({"text": text, "at": at, "count": count});
        }
    }
    found
}

/// The summary wrapped to the width, its blank lines and indents kept: (line, marked) pairs, marked
/// where the summary says how it read you (Interpreted as: / Read as: / (unclear) / typo for).
pub fn summary_lines(text: &str, width: usize) -> Vec<(String, bool)> {
    static READ_AS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)interpreted as:|read as:|\(unclear\)|typo for").unwrap()
    });
    let mut rows: Vec<(String, bool)> = vec![];
    for raw in text.replace("**", "").lines() {
        if raw.trim().is_empty() {
            if rows.last().is_some_and(|r| !r.0.is_empty()) {
                rows.push((String::new(), false));
            }
            continue;
        }
        let indent = " ".repeat((len(raw) - len(raw.trim_start())).min(8));
        let marked = READ_AS.is_match(raw);
        let room = width.saturating_sub(len(&indent)).max(8);
        rows.extend(
            wrap(raw.trim(), room, usize::MAX / 2)
                .into_iter()
                .map(|piece| (format!("{indent}{piece}"), marked)),
        );
    }
    rows
}

/// The s page: the newest compaction summary, wrapped, the read-as lines in the accent ink. The
/// returned top is the scroll kept within the text.
pub fn draw_summary(summary: &Value, cols: usize, rows: usize, top: usize) -> (Lines, usize) {
    let width = cols.saturating_sub(1).max(24);
    let room = rows.saturating_sub(3).max(1);
    let mut body = vec![];
    let mut facts = "No compaction summary yet.".to_owned();
    let mut top = top;
    if summary.is_object() {
        body = summary_lines(string(&summary["text"]), width - 1);
        top = top.min(body.len().saturating_sub(room));
        let when = summary["at"]
            .as_f64()
            .filter(|n| *n != 0.)
            .map(|at| stamped(at, "%m-%d %H:%M"))
            .unwrap_or_default();
        let count = num(&summary["count"]) as i64;
        let marks = body.iter().filter(|(_, m)| *m).count();
        facts = [
            when,
            if count > 1 {
                format!("summary {count}")
            } else {
                String::new()
            },
            format!(
                "lines {}-{} of {}",
                top + 1,
                body.len().min(top + room),
                body.len()
            ),
            if marks > 0 {
                format!("{marks} read-as")
            } else {
                String::new()
            },
            format!("{} chars", thousands(len(string(&summary["text"])) as i64)),
        ]
        .into_iter()
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    }
    let head = [
        format!(" {BOLD}Compaction summary{OFF}"),
        format!(" {}", fit(&facts, width - 1)),
    ];
    let mut out = Lines {
        rows: head
            .iter()
            .zip(dark_steps(head.len()))
            .map(|(line, ground)| on_dark(line, ground))
            .collect(),
        ..Lines::default()
    };
    let start = if summary.is_object() { top } else { 0 };
    for (line, marked) in body.iter().skip(start).take(room) {
        out.rows.push(format!(
            " {}{line}{OFF}",
            ink(if *marked { "turn" } else { "you" })
        ));
    }
    (out, top)
}

/// Which group of the threads page a thread sits in.
pub fn thread_group(t: &Value) -> &str {
    if let Some(hidden) = t["hidden"].as_str().filter(|h| !h.is_empty()) {
        return hidden;
    }
    if matches!(string(&t["state"]), "lost" | "open") && truthy(&t["waiting"]) {
        return "waiting";
    }
    string(&t["state"])
}

/// The two things a thread can be asked to become, as the words typed into Claude's prompt
/// (unsent).
pub fn lost_asks(thread: &Value) -> Value {
    let quiet = format!(
        "Lost and found: “{}” went quiet after {}.",
        fit(string(&thread["text"]), 240),
        stamped(num(&thread["last_seen"]), "%H:%M")
    );
    json!({
        "side": one_line(&format!(
            "{quiet} Revisit it as a side task: finish or close it in a few steps without \
             derailing the current work, then tell me in one line."
        ), 700),
        "sub": one_line(&format!(
            "{quiet} Hand it to a background subagent with a five-part spec (objective, files, \
             interfaces, constraints, verification), keep working on the current task, and report \
             its result when it lands."
        ), 700),
    })
}

type Block = (String, Option<Value>);

/// A thread's answers under its row, two to a row; each row is its own block, so a click knows
/// the thread and, by its column, the answer.
pub fn lost_rows(item: &Value, width: usize, at: usize, sent: &Value) -> Vec<Block> {
    let half = width.saturating_sub(at) / 2;
    let mut rows: Vec<Block> = vec![];
    let mut pairs: Vec<&[(&str, &str)]> = vec![&LOST_ASKS[..2], &LOST_ASKS[2..]];
    if sent[0] == item["key"] && !sent[0].is_null() {
        let said = if truthy(&sent[1]) {
            "✓ now in Claude's prompt"
        } else {
            "✗ couldn't reach Claude's pane"
        };
        rows.push((
            format!(
                "{}{}{}{OFF}",
                " ".repeat(at),
                ink("faint"),
                fit(said, width.saturating_sub(at))
            ),
            None,
        ));
        pairs.remove(0);
    }
    for pair in pairs {
        let mut text = " ".repeat(at);
        let mut spans = vec![];
        for (k, (answer, label)) in pair.iter().enumerate() {
            spans.push(json!([at + k * half, at + (k + 1) * half, answer]));
            let first: String = label.chars().take(1).collect();
            let rest: String = label.chars().skip(1).collect();
            let shown = fit(&rest, half.saturating_sub(2));
            let pad = half.saturating_sub(1).saturating_sub(len(&shown));
            text.push_str(&format!(
                "{}{BOLD}{first}{OFF}{}{shown}{}{OFF}",
                ink("turn"),
                ink("quiet"),
                " ".repeat(pad)
            ));
        }
        rows.push((
            text,
            Some(json!({
                "key": format!("{}:{}", string(&item["key"]), pair[0].0),
                "lost": item["lost"],
                "item": item["key"],
                "asks": item["asks"],
                "spans": spans,
            })),
        ));
    }
    rows
}

/// The f page: every thread Lost and found knows, grouped, nothing hidden; each says why it sits
/// where it does. A click opens one and shows its answers. The returned top is the scroll kept
/// within the page.
pub fn draw_threads(
    every: &[Value],
    cols: usize,
    rows: usize,
    open: &Value,
    sent: &Value,
    top: usize,
    phone: bool,
) -> (Lines, usize) {
    let width = cols.saturating_sub(1).max(24);
    let at = if phone { PHONE_GUTTER } else { ITEM };
    let groups: Vec<Vec<&Value>> = THREAD_GROUPS
        .iter()
        .map(|(name, _, _)| every.iter().filter(|t| thread_group(t) == *name).collect())
        .collect();
    let facts = if every.is_empty() {
        "No compaction summary yet.".to_owned()
    } else {
        format!(
            "{} threads: {}",
            every.len(),
            THREAD_GROUPS
                .iter()
                .zip(&groups)
                .filter(|(_, g)| !g.is_empty())
                .map(|((_, _, short), g)| format!("{} {short}", g.len()))
                .collect::<Vec<_>>()
                .join(" · ")
        )
    };
    let mut head = vec![format!(
        " {BOLD}{}{OFF}",
        fit("Lost and found · every thread", width - 1)
    )];
    head.extend(
        wrap(&facts, width - 1, 3)
            .into_iter()
            .map(|line| format!(" {line}")),
    );
    let mut out = Lines {
        rows: head
            .iter()
            .zip(dark_steps(head.len()))
            .map(|(line, ground)| on_dark(line, ground))
            .collect(),
        ..Lines::default()
    };
    let mut body: Vec<Block> = vec![];
    for ((_, title, _), group) in THREAD_GROUPS.iter().zip(&groups) {
        if group.is_empty() {
            continue;
        }
        body.push((String::new(), None));
        body.push((
            format!(
                " {}{BOLD}{title}{OFF}{}  {}{OFF}",
                ink("turn"),
                ink("faint"),
                group.len()
            ),
            None,
        ));
        for t in group {
            let asks = lost_asks(t);
            let key = format!("lost:{}", string(&t["key"]));
            let item = json!({"key": key, "lost": t["key"], "top": false, "asks": asks});
            let act = json!({"key": key, "send": asks["side"], "lost": t["key"]});
            for line in wrap(string(&t["text"]), width.saturating_sub(at), 3) {
                body.push((
                    format!("{}{}{line}{OFF}", " ".repeat(at), ink("you")),
                    Some(act.clone()),
                ));
            }
            body.push((
                format!(
                    "{}{}{}{OFF}",
                    " ".repeat(at),
                    ink("faint"),
                    fit(string(&t["why"]), width.saturating_sub(at))
                ),
                Some(act.clone()),
            ));
            if open.as_str() == Some(&key) {
                for line in array(&t["detail"]).iter().take(3) {
                    body.push((
                        format!(
                            "{}{}{}{OFF}",
                            " ".repeat(at),
                            ink("quiet"),
                            fit(string(line), width.saturating_sub(at))
                        ),
                        Some(act.clone()),
                    ));
                }
                body.extend(lost_rows(&item, width, at, sent));
            }
        }
    }
    let room = rows.saturating_sub(1).saturating_sub(out.rows.len()).max(1);
    let top = top.min(body.len().saturating_sub(room));
    for (line, act) in body.into_iter().skip(top).take(room) {
        if let Some(act) = act {
            out.acts.insert(out.rows.len(), act);
        }
        out.rows.push(line);
    }
    (out, top)
}

/// Every thread of a session, as the f page draws them: the transcript's summaries and typed
/// prompts, the session before's carried threads, what you answered and what was handed off.
pub fn lost_value(paths: &Paths, session: &Session) -> Vec<Value> {
    let pairs = |v: &Value| -> Vec<(f64, String)> {
        array(v)
            .iter()
            .map(|r| (num(&r[0]), string(&r[1]).into()))
            .collect()
    };
    let carried = paths
        .kept_file("carried", &session.id)
        .map(|p| super::paths::read_json(&p))
        .and_then(|v| serde_json::from_value::<Vec<super::lost::Thread>>(v).ok())
        .unwrap_or_default();
    let project = session
        .path
        .as_deref()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|s| s.to_str());
    super::lost::lost_found(
        paths,
        &pairs(&session.data["summaries"]),
        &pairs(&session.data["typed"]),
        &carried,
        project,
        &agents::handoffs_of(paths, &session.id),
    )
    .iter()
    .filter_map(|t| serde_json::to_value(t).ok())
    .collect()
}

/// The project a transcript belongs to: the folder Claude Code keeps it in.
pub fn project_of(session: Option<&Session>) -> Option<String> {
    session?
        .path
        .as_deref()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .map(str::to_owned)
}

/// One line in the lost-and-found log: what was done with a thread, and any more the click knows.
pub fn keep_lost(
    paths: &Paths,
    key: &str,
    answer: &str,
    at: f64,
    project: Option<&str>,
    more: Value,
) {
    let mut record = json!({"key": key, "answer": answer, "at": at, "project": project});
    if let Some(more) = more.as_object() {
        for (k, v) in more {
            record[k] = v.clone();
        }
    }
    let _ = private::append(
        &paths.keep.join("lost-found.jsonl"),
        &format!("{}\n", private::dumps(&record)),
        &paths.keep,
    );
}

/// A click on "→ subagent": a new request id (a retry is always a new one), kept as a draft, and
/// the envelope that carries it into the unsent prompt. (request, envelope), or none where there
/// is no session to tie it to.
pub fn new_handoff(paths: &Paths, sid: &str, thread: &str) -> Option<(String, String)> {
    let thread_ok = thread.len() == 12
        && thread
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !agents::session_id(sid) || !thread_ok {
        return None;
    }
    let request = format!("req-{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
    let record = json!({"request": request, "thread": thread, "drafted_at": super::paths::now()});
    private::append(
        &paths
            .keep
            .join("handoffs")
            .join(format!("{sid}.drafts.jsonl")),
        &format!("{}\n", private::dumps(&record)),
        &paths.keep,
    )
    .ok()?;
    let envelope = agents::handoff_envelope(sid, &request, thread).ok()?;
    Some((request, envelope))
}
