//! A compacting subagent keeps its brief and its progress: port of the removed `subagent.ts` and of
//! the
//! `keep` hook of the TypeScript companion.
//!
//! The stock summarizer can drop the task a subagent was given and where it had got to. The
//! companion hears of every subagent compaction as `session.compact` with the subagent's own
//! transcript (`e.messages`), and adds a block to the summarizer's instructions, built from that
//! transcript alone: no process, no engine call, no model call.
//!
//! One thing the TypeScript could not do: a transcript past the relay's size rule arrives as
//! `messagesOmitted: true` with no `messages` (`register.js.j2` leaves them out of a body over
//! 3.5 MiB, because `$.http.fetch` refuses one over 4 MiB). The block is then built from the
//! subagent's own transcript file on disk,
//! `<home>/.claude/projects/*/<session>/subagents/agent-<agentId>.jsonl`. A file that is missing,
//! unreadable, or past [`TRANSCRIPT_MAX`] lets the compaction go on with the instructions it had.
//!
//! Every size is in UTF-8 bytes, what a "KB" of the summarizer's instructions is, and every cut
//! falls between code points. (A Rust `str` cannot hold a lone surrogate, so the TypeScript's
//! surrogate-pair care is the `char` boundary here.)

use crate::companion::{
    app::compactor::rules::{js_space, js_trim, KEEP_HEADER},
    paths::{folders, valid_id, Paths},
    relay::dispatch::{handler, Reply, Table},
};
use anyhow::{bail, Result};
use regex::Regex;
use serde_json::{json, Map, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, LazyLock},
};

/// 6 KB; a longer brief keeps the first 4 KB and the last 2 KB.
pub const BRIEF_MAX: usize = 6144;
pub const BRIEF_HEAD: usize = 4096;
pub const BRIEF_TAIL: usize = 2048;
/// The whole block.
pub const BLOCK_MAX: usize = 8192;
pub const FILES_MAX: usize = 30;
pub const COMMITS_MAX: usize = 20;
pub const TEST_LINES_MAX: usize = 10;
pub const TEST_LINE_BYTES: usize = 200;
pub const PROGRESS_BYTES: usize = 600;
/// A section with less room than this (bytes) is left out whole.
const MIN_ROOM: usize = 40;
/// The largest transcript file read for an omitted `messages`.
pub const TRANSCRIPT_MAX: u64 = 64 * 1024 * 1024;
const EDITS: [&str; 3] = ["Edit", "Write", "MultiEdit"];
const ELLIPSIS: &str = "…";

fn head() -> String {
    format!(
        "{KEEP_HEADER} subagent. Keep these verbatim in the summary; they are the subagent's task \
         and state:"
    )
}

// ---------------------------------------------------------------- cutting by bytes

/// The longest start of `text` that fits `max` bytes, ending between code points.
fn first_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The longest end of `text` that fits `max` bytes, starting between code points.
fn last_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// `text` in at most `max` bytes, the cut marked by an ellipsis that is counted in them.
fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    if max < ELLIPSIS.len() {
        return String::new();
    }
    format!("{}{ELLIPSIS}", first_bytes(text, max - ELLIPSIS.len()))
}

/// The brief, whole to 6 KB; past it the head and the tail around a cut mark that says how much is
/// missing.
pub fn brief_of(text: &str) -> String {
    if text.len() <= BRIEF_MAX {
        return text.to_string();
    }
    let head = first_bytes(text, BRIEF_HEAD);
    let rest = &text[head.len()..];
    let tail = last_bytes(rest, BRIEF_TAIL);
    let missing = rest[..rest.len() - tail.len()].chars().count();
    format!("{head}\n[… {missing} characters cut …]\n{tail}")
}

// ---------------------------------------------------------------- reading the messages

type Row = Map<String, Value>;

/// A message the engine hands a hook is an object; anything else cannot be read (the TypeScript
/// threw on it).
fn row(value: &Value) -> Result<&Row> {
    match value.as_object() {
        Some(row) => Ok(row),
        None => bail!("a message is not an object"),
    }
}

/// `message.text.trim()`: a text that is not a string throws, as `.trim()` of it does.
fn trimmed(message: &Row) -> Result<&str> {
    match message.get("text") {
        Some(Value::String(text)) => Ok(js_trim(text)),
        _ => bail!("a message's text is not a string"),
    }
}

/// `message.toolUses ?? []`, each entry an object: `use.tool_use_id` of anything else throws.
fn entries<'a>(message: &'a Row, name: &str) -> Result<Vec<&'a Row>> {
    let Some(Value::Array(items)) = message.get(name) else {
        return Ok(Vec::new());
    };
    items.iter().map(row).collect()
}

fn is(message: &Row, role: &str) -> bool {
    message.get("role").and_then(Value::as_str) == Some(role)
}

/// The tool of a call: none when it names none; a name that is not a string is no tool of ours.
fn tool_name(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::Null) => None,
        Some(Value::String(name)) => Some(name.clone()),
        Some(_) => Some("\u{0}".to_string()),
    }
}

/// What a `Map` keyed by a call's id keys it by: a missing id is not a null one.
fn id_key(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(Value::String(id)) => format!("s{id}"),
        Some(other) => format!("j{other}"),
    }
}

struct Outcome {
    tool: Option<String>,
    text: String,
    order: usize,
}

/// Set `id` in a map that keeps the place of its first insertion.
fn put(found: &mut Vec<(String, Outcome)>, id: String, outcome: Outcome) {
    match found.iter_mut().find(|(key, _)| *key == id) {
        Some((_, was)) => *was = outcome,
        None => found.push((id, outcome)),
    }
}

/// Every tool result the transcript holds, once each (an assistant message carries its call's text
/// and the next user message carries the same result), oldest first; the tool that made it is known
/// where its call is in the transcript.
fn results_of(messages: &[&Row]) -> Result<Vec<Outcome>> {
    let mut tools: HashMap<String, Option<String>> = HashMap::new();
    let mut found: Vec<(String, Outcome)> = Vec::new();
    for (order, message) in messages.iter().enumerate() {
        for call in entries(message, "toolUses")? {
            let id = id_key(call.get("tool_use_id"));
            let tool = tool_name(call.get("tool"));
            tools.insert(id.clone(), tool.clone());
            if let Some(Value::String(text)) = call.get("text") {
                let text = text.clone();
                put(&mut found, id, Outcome { tool, text, order });
            }
        }
        for result in entries(message, "toolResults")? {
            if let Some(Value::String(text)) = result.get("text") {
                let id = id_key(result.get("tool_use_id"));
                let tool = tools.get(&id).cloned().flatten();
                let text = text.clone();
                put(&mut found, id, Outcome { tool, text, order });
            }
        }
    }
    let mut results: Vec<Outcome> = found.into_iter().map(|(_, outcome)| outcome).collect();
    results.sort_by_key(|outcome| outcome.order);
    Ok(results)
}

/// `git commit` prints "[branch sha] subject" (and "[branch (root-commit) sha] subject"), one to a
/// line. Only Bash makes one.
static COMMIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[([^\]]+?) ([0-9a-f]{7,40})\] (.+)$").expect("the commit pattern is valid")
});
/// `/test result:|\bpassed\b|\bfailed\b/i`, as JavaScript reads it: ASCII word edges and case.
static TESTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i-u)test result:|\bpassed\b|\bfailed\b").expect("the test pattern is valid")
});

/// JavaScript's line ends for `^` and `$` in a multiline pattern.
fn js_line_end(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// `[branch sha] subject` of each commit line of `text`, the subject cut to 200 bytes.
fn commits_in(text: &str) -> Vec<(String, String)> {
    let mut made = Vec::new();
    for line in text.split(js_line_end) {
        let Some(found) = COMMIT.captures(line) else {
            continue;
        };
        let subject = &found[3];
        // `(\S.*)`: the subject opens on a character that is not JavaScript whitespace
        if subject.chars().next().is_some_and(js_space) {
            continue;
        }
        let shown = format!(
            "[{} {}] {}",
            &found[1],
            &found[2],
            cut(js_trim(subject), 200)
        );
        made.push((found[2].to_string(), shown));
    }
    made
}

/// `items.slice(-n)`.
fn last_n(mut items: Vec<String>, n: usize) -> Vec<String> {
    let drop = items.len().saturating_sub(n);
    items.drain(..drop);
    items
}

/// The block for the summarizer's instructions, or none when the transcript holds nothing worth
/// keeping. An error is a transcript that cannot be read (a message that is not an object, a text
/// that is not a string, a tool entry that is not an object): the compaction goes on without it.
pub fn subagent_block(messages: &[Value]) -> Result<Option<String>> {
    let messages: Vec<&Row> = messages.iter().map(row).collect::<Result<_>>()?;

    let mut brief = String::new();
    for message in &messages {
        if is(message, "user") && !trimmed(message)?.is_empty() {
            brief = brief_of(trimmed(message)?);
            break;
        }
    }

    let results = results_of(&messages)?;
    let from_bash = |outcome: &Outcome| outcome.tool.as_deref().is_none_or(|tool| tool == "Bash");
    // by sha, so a commit shown twice counts once
    let mut commits: Vec<(String, String)> = Vec::new();
    for outcome in results.iter().filter(|outcome| from_bash(outcome)) {
        for (sha, shown) in commits_in(&outcome.text) {
            match commits.iter_mut().find(|(was, _)| *was == sha) {
                Some((_, was)) => *was = shown,
                None => commits.push((sha, shown)),
            }
        }
    }
    let made = last_n(
        commits.into_iter().map(|(_, shown)| shown).collect(),
        COMMITS_MAX,
    );

    // by path, in the order each was last edited
    let mut files: Vec<String> = Vec::new();
    for message in &messages {
        for call in entries(message, "toolUses")? {
            let tool = call.get("tool").and_then(Value::as_str).unwrap_or("");
            let path = call.get("input").and_then(|input| input.get("file_path"));
            let (true, Some(Value::String(path))) = (EDITS.contains(&tool), path) else {
                continue;
            };
            if path.is_empty() {
                continue;
            }
            files.retain(|was| was != path);
            files.push(path.clone());
        }
    }
    let edited = last_n(files, FILES_MAX);

    let mut tests: Vec<String> = Vec::new();
    for outcome in results.iter().rev() {
        // a Read of a file that says "passed" is no test run
        if !from_bash(outcome) {
            continue;
        }
        let lines: Vec<&str> = (outcome.text.split('\n'))
            .map(js_trim)
            .filter(|line| TESTED.is_match(line))
            .collect();
        if !lines.is_empty() {
            let newest = &lines[lines.len().saturating_sub(TEST_LINES_MAX)..];
            tests = newest
                .iter()
                .map(|line| cut(line, TEST_LINE_BYTES))
                .collect();
            break;
        }
    }

    let mut progress = String::new();
    for message in messages.iter().rev() {
        if is(message, "assistant") && !trimmed(message)?.is_empty() {
            progress = cut(trimmed(message)?, PROGRESS_BYTES);
            break;
        }
    }

    let lines = |items: &[String]| {
        let rows: Vec<String> = items.iter().map(|item| format!("- {item}")).collect();
        rows.join("\n")
    };
    // (priority, title, body): shown in this order; room is given in the order of the priority
    // column, lower first.
    let parts: Vec<(u8, String, String)> = vec![
        (0, "Task brief (its first message, verbatim):".into(), brief),
        (1, "Commits it made:".into(), lines(&made)),
        (
            4,
            format!("Files it edited (newest {FILES_MAX}):"),
            lines(&edited),
        ),
        (2, "Its last test outcome:".into(), lines(&tests)),
        (3, "In progress (its newest words):".into(), progress),
    ];
    let present: Vec<&(u8, String, String)> =
        parts.iter().filter(|part| !part.2.is_empty()).collect();
    if present.is_empty() {
        return Ok(None);
    }

    let head = head();
    let mut room = BLOCK_MAX.saturating_sub(head.len());
    let mut given: HashMap<u8, String> = HashMap::new();
    let mut by_priority = present.clone();
    by_priority.sort_by_key(|part| part.0);
    for (rank, title, body) in by_priority {
        // the newlines around the title
        let space = room
            .checked_sub(title.len() + 2)
            .filter(|space| *space >= MIN_ROOM);
        let Some(space) = space else {
            continue;
        };
        let fitted = cut(body, space);
        room = room.saturating_sub(title.len() + fitted.len() + 2);
        given.insert(*rank, format!("{title}\n{fitted}"));
    }
    let shown: Vec<&String> = present
        .iter()
        .filter_map(|part| given.get(&part.0))
        .collect();
    if shown.is_empty() {
        return Ok(None);
    }
    let mut block = vec![head];
    block.extend(shown.into_iter().cloned());
    Ok(Some(block.join("\n")))
}

// ---------------------------------------------------------------- the transcript on disk

/// The text of a tool result's `content`: a string, or the text blocks of a list.
fn flat(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => {
            let texts: Vec<&str> = blocks
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect();
            texts.join("\n")
        }
        _ => String::new(),
    }
}

/// One row of a transcript file as the engine's `SessionMessage`: the role, the text blocks, the
/// calls it made (`toolUses`, with the id they are paired by), and the results it carries
/// (`toolResults`). The results of a call are in the user message that follows it.
fn message_of(row: &Value) -> Option<Value> {
    let kind = row["type"]
        .as_str()
        .filter(|kind| matches!(*kind, "user" | "assistant"))?;
    let message = row.get("message")?;
    let role = message["role"].as_str().unwrap_or(kind);
    let (mut texts, mut uses, mut results) = (Vec::new(), Vec::new(), Vec::new());
    match &message["content"] {
        Value::String(text) => texts.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                match item["type"].as_str() {
                    Some("text") => texts.extend(item["text"].as_str().map(str::to_string)),
                    Some("tool_use") => uses.push(json!({
                        "tool_use_id": item["id"], "tool": item["name"], "input": item["input"],
                    })),
                    Some("tool_result") => results.push(json!({
                        "tool_use_id": item["tool_use_id"], "text": flat(&item["content"]),
                    })),
                    _ => (),
                }
            }
        }
        _ => (),
    }
    Some(json!({
        "role": role, "text": texts.join("\n"), "toolUses": uses, "toolResults": results,
    }))
}

/// The messages of the subagent's own transcript; none when it cannot be found or read.
fn read_transcript(home: PathBuf, sid: &str, agent: &str) -> Option<Vec<Value>> {
    if !valid_id(sid) || !valid_id(agent) {
        return None;
    }
    let name = format!("agent-{agent}.jsonl");
    let paths = Paths::new(home);
    let file = (folders(&paths.projects).into_iter())
        .map(|project| project.join(sid).join("subagents").join(&name))
        .find(|file| file.is_file())?;
    if file.metadata().ok()?.len() > TRANSCRIPT_MAX {
        return None;
    }
    let bytes = std::fs::read(&file).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let messages: Vec<Value> = (text.lines())
        .filter_map(|line| message_of(&serde_json::from_str::<Value>(line).ok()?))
        .collect();
    Some(messages)
}

// ---------------------------------------------------------------- the hook

/// Where `$HOME` is, asked when a compaction comes.
pub type HomeOf = Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>;

fn home_from_env() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    (!home.as_os_str().is_empty()).then_some(home)
}

pub fn register_with(t: &mut Table, home: HomeOf) {
    t.on(
        "keep",
        "subagent",
        handler(move |cx, e| {
            let home = home.clone();
            async move {
                // a compaction of the main session is not ours
                let Some(id) = e["agentId"].as_str() else {
                    return Ok(Reply::none());
                };
                let messages = if e["messagesOmitted"] == json!(true) {
                    let (sid, agent) = (cx.sid.clone(), id.to_string());
                    let read = home().map(|home| {
                        tokio::task::spawn_blocking(move || read_transcript(home, &sid, &agent))
                    });
                    match read {
                        Some(read) => read.await.ok().flatten(),
                        None => None,
                    }
                } else {
                    e["messages"].as_array().cloned()
                };
                let Some(messages) = messages else {
                    return Ok(Reply::none());
                };
                // a block that cannot be built lets the compaction go on with the instructions
                // it had
                let Ok(Some(block)) = subagent_block(&messages) else {
                    return Ok(Reply::none());
                };
                // No lookup of the agent's type: an awaited `$.agent.list()` that never settles
                // would hold the compaction.
                let trigger = e["trigger"].as_str().unwrap_or("");
                let line = format!(
                    "csr-companion: subagent {id} compacting ({trigger}): kept {} bytes of its \
                     task and state",
                    block.len()
                );
                let _ = cx
                    .host
                    .call("ui.log", vec![json!(line), json!({ "to": "debug" })])
                    .await;
                let mut told = e.clone();
                if let Some(told) = told.as_object_mut() {
                    told.remove("messagesOmitted");
                    let had = e["instructions"].as_str().filter(|had| !had.is_empty());
                    let instructions = match had {
                        Some(had) => format!("{had}\n\n{block}"),
                        None => block,
                    };
                    told.insert("instructions".into(), json!(instructions));
                }
                Ok(Reply::next(told))
            }
        }),
    );
}

pub fn register(t: &mut Table) {
    register_with(t, Arc::new(home_from_env));
}

#[cfg(test)]
mod tests;
