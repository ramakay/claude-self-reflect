//! Record filter and episode splitter. Pure functions, no model, no I/O.
//!
//! An episode is one user prompt, the assistant reply, and one line per tool
//! call, capped near 1.5k tokens (chars / 4). Oversized text splits at line
//! boundaries and prefers a diff boundary (`diff --git`, `@@`) when one falls
//! in the second half of a piece.

use serde::Serialize;
use serde_json::Value;

/// About 1.5k tokens at chars / 4.
pub const EPISODE_CAP_CHARS: usize = 6000;
const TOOL_ARG_CHARS: usize = 160;
const PROMPT_HEAD_CHARS: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropKind {
    HookSuccess,
    HookAdditionalContext,
    RelevantMemories,
    AwaySummary,
    IsCompactSummary,
    ApiRequest,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct Drops {
    pub hook_success: u64,
    pub hook_additional_context: u64,
    pub relevant_memories: u64,
    pub away_summary: u64,
    pub is_compact_summary: u64,
    pub api_request: u64,
}

impl Drops {
    pub fn count(&mut self, kind: DropKind) {
        match kind {
            DropKind::HookSuccess => self.hook_success += 1,
            DropKind::HookAdditionalContext => self.hook_additional_context += 1,
            DropKind::RelevantMemories => self.relevant_memories += 1,
            DropKind::AwaySummary => self.away_summary += 1,
            DropKind::IsCompactSummary => self.is_compact_summary += 1,
            DropKind::ApiRequest => self.api_request += 1,
        }
    }

    pub fn add(&mut self, other: &Drops) {
        self.hook_success += other.hook_success;
        self.hook_additional_context += other.hook_additional_context;
        self.relevant_memories += other.relevant_memories;
        self.away_summary += other.away_summary;
        self.is_compact_summary += other.is_compact_summary;
        self.api_request += other.api_request;
    }
}

fn str_at<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Which drop class a raw record falls in, if any. Runs before the shared
/// sanitizer and before splitting.
pub fn classify_drop(row: &Value) -> Option<DropKind> {
    let att = row
        .get("attachment")
        .and_then(|a| a.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if str_at(row, "type").starts_with("api-request")
        || str_at(row, "subtype").starts_with("api-request")
        || att.starts_with("api-request")
    {
        return Some(DropKind::ApiRequest);
    }
    if row.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
        return Some(DropKind::IsCompactSummary);
    }
    match att {
        "hook_success" => return Some(DropKind::HookSuccess),
        "hook_additional_context" => return Some(DropKind::HookAdditionalContext),
        "relevant_memories" => return Some(DropKind::RelevantMemories),
        _ => {}
    }
    if str_at(row, "type") == "system" && str_at(row, "subtype") == "away_summary" {
        return Some(DropKind::AwaySummary);
    }
    None
}

/// `(uuid, kind, timestamp)` for a compaction boundary or summary row.
pub fn compact_record(row: &Value) -> Option<(String, &'static str, String)> {
    let uuid = str_at(row, "uuid");
    if uuid.is_empty() {
        return None;
    }
    let kind = if row.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
        "summary"
    } else if str_at(row, "type") == "system" && str_at(row, "subtype") == "compact_boundary" {
        "boundary"
    } else {
        return None;
    };
    Some((uuid.to_string(), kind, str_at(row, "timestamp").to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Episode {
    pub ordinal: i64,
    pub first_uuid: String,
    pub last_uuid: String,
    pub timestamp: String,
    pub text: String,
    pub token_est: i64,
    pub echo_class: i64,
}

/// Plain text of a (sanitized) user or assistant row: text blocks only.
pub fn row_text(row: &Value) -> String {
    let content = row
        .get("message")
        .and_then(|m| m.get("content"))
        .or_else(|| row.get("content"));
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Harness-written user rows that are not something the person typed.
pub fn is_harness_prompt(text: &str) -> bool {
    let t = text.trim_start();
    t.is_empty()
        || t.starts_with("<command-")
        || t.starts_with("<local-command")
        || t.starts_with("<system-reminder")
        || t.starts_with("<task-notification")
        || t.starts_with("Caveat: The messages below")
        || t.starts_with("[Request interrupted")
        || crate::import::is_csr_agent_prompt(t)
}

fn flatten(s: &str, max: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    one.chars().take(max).collect()
}

/// One-line summary of a tool call.
pub fn tool_line(name: &str, input: &Value) -> String {
    let arg = match name {
        "TodoWrite" => {
            let items = input
                .get("todos")
                .and_then(Value::as_array)
                .map(|todos| {
                    todos
                        .iter()
                        .map(|t| {
                            format!(
                                "{}:{}",
                                str_at(t, "status"),
                                flatten(str_at(t, "content"), 100)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .unwrap_or_default();
            flatten(&items, 600)
        }
        "TaskCreate" => flatten(str_at(input, "subject"), TOOL_ARG_CHARS),
        "TaskUpdate" => {
            let id = input
                .get("taskId")
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string())
                })
                .unwrap_or_default();
            format!("#{id} {}", str_at(input, "status"))
        }
        _ => [
            "file_path",
            "path",
            "command",
            "pattern",
            "query",
            "url",
            "description",
            "prompt",
            "subject",
            "skill",
        ]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .map(|s| flatten(s, TOOL_ARG_CHARS))
        .unwrap_or_default(),
    };
    if arg.is_empty() {
        format!("[{name}]")
    } else {
        format!("[{name}: {arg}]")
    }
}

/// Split `text` into pieces of at most `cap` chars at line boundaries,
/// preferring a diff boundary once a piece is half full.
pub fn split_text(text: &str, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_chars = 0usize;
    for line in text.split_inclusive('\n') {
        let mut line = line;
        let mut n = line.chars().count();
        // A single line over the cap is cut on a char boundary.
        while n > cap {
            if cur_chars > 0 {
                out.push(std::mem::take(&mut cur));
                cur_chars = 0;
            }
            let cut = line.char_indices().nth(cap).map_or(line.len(), |(i, _)| i);
            out.push(line[..cut].to_string());
            line = &line[cut..];
            n = line.chars().count();
        }
        let diff_edge =
            (line.starts_with("diff --git ") || line.starts_with("@@")) && cur_chars >= cap / 2;
        if (cur_chars + n > cap || diff_edge) && cur_chars > 0 {
            out.push(std::mem::take(&mut cur));
            cur_chars = 0;
        }
        cur.push_str(line);
        cur_chars += n;
    }
    if cur_chars > 0 {
        out.push(cur);
    }
    out
}

struct Cur {
    first_uuid: String,
    last_uuid: String,
    ts: String,
    text: String,
    chars: usize,
    /// Prompt line repeated at the head of a continuation episode.
    head: String,
    has_body: bool,
}

pub struct Splitter {
    eps: Vec<Episode>,
    /// Ordinal the next flushed episode takes. Right after `push` returns true
    /// it is the first ordinal of the new prompt group.
    pub next_ordinal: i64,
    cur: Option<Cur>,
}

impl Splitter {
    pub fn new(start_ordinal: i64) -> Self {
        Self {
            eps: Vec::new(),
            next_ordinal: start_ordinal,
            cur: None,
        }
    }

    fn flush(&mut self) {
        if let Some(cur) = self.cur.take() {
            if cur.has_body {
                let text = cur.text.trim_end().to_string();
                let echo = {
                    let prompt = text
                        .strip_prefix("U: ")
                        .and_then(|t| t.split("\nA: ").next())
                        .unwrap_or("");
                    i64::from(crate::search::echo::is_question_echo(&text, prompt))
                };
                self.eps.push(Episode {
                    ordinal: self.next_ordinal,
                    first_uuid: cur.first_uuid,
                    last_uuid: cur.last_uuid,
                    timestamp: cur.ts,
                    token_est: (text.chars().count() / 4) as i64,
                    echo_class: echo,
                    text,
                });
                self.next_ordinal += 1;
            }
        }
    }

    fn start(&mut self, uuid: &str, ts: &str, head: String, first_piece: &str) {
        let text = format!("{first_piece}\n");
        self.cur = Some(Cur {
            first_uuid: uuid.to_string(),
            last_uuid: uuid.to_string(),
            ts: ts.to_string(),
            chars: text.chars().count(),
            text,
            head,
            has_body: true,
        });
    }

    fn append(&mut self, uuid: &str, ts: &str, piece: &str) {
        let Some(cur) = self.cur.as_ref() else { return };
        let n = piece.chars().count() + 1;
        if cur.chars + n > EPISODE_CAP_CHARS {
            let head = cur.head.clone();
            self.flush();
            let budget = EPISODE_CAP_CHARS.saturating_sub(head.chars().count() + 2);
            let pieces = if n > budget {
                split_text(piece, budget.max(200))
            } else {
                vec![piece.to_string()]
            };
            let last = pieces.len() - 1;
            for (i, p) in pieces.iter().enumerate() {
                self.start(uuid, ts, head.clone(), &format!("{head}\n{p}"));
                if let Some(c) = self.cur.as_mut() {
                    c.has_body = true;
                }
                if i != last {
                    self.flush();
                }
            }
            return;
        }
        if let Some(cur) = self.cur.as_mut() {
            cur.text.push_str(piece);
            cur.text.push('\n');
            cur.chars += n;
            cur.last_uuid = uuid.to_string();
        }
    }

    /// Feed one sanitized, non-dropped row. Returns the first ordinal of the new
    /// prompt group when the row opened one.
    pub fn push(&mut self, row: &Value) -> Option<i64> {
        let kind = str_at(row, "type");
        let uuid = str_at(row, "uuid");
        let ts = str_at(row, "timestamp");
        match kind {
            "user" | "human" => {
                if row.get("isMeta").and_then(Value::as_bool) == Some(true) {
                    return None;
                }
                let text = row_text(row);
                if is_harness_prompt(&text) {
                    return None;
                }
                self.flush();
                let group_ord = self.next_ordinal;
                let head: String = format!("U: {} (cont.)", flatten(&text, PROMPT_HEAD_CHARS));
                let pieces = split_text(text.trim(), EPISODE_CAP_CHARS - 8);
                let last = pieces.len() - 1;
                for (i, p) in pieces.iter().enumerate() {
                    let body = if i == 0 {
                        format!("U: {p}")
                    } else {
                        format!("{head}\n{p}")
                    };
                    self.start(uuid, ts, head.clone(), body.trim_end());
                    if i != last {
                        self.flush();
                    }
                }
                Some(group_ord)
            }
            "assistant" => {
                self.cur.as_ref()?;
                let content = row
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .or_else(|| row.get("content"));
                match content {
                    Some(Value::String(s)) if !s.trim().is_empty() => {
                        self.append(uuid, ts, &format!("A: {}", s.trim()));
                    }
                    Some(Value::Array(blocks)) => {
                        for b in blocks {
                            match b.get("type").and_then(Value::as_str) {
                                Some("text") => {
                                    let t = str_at(b, "text").trim();
                                    if !t.is_empty() {
                                        self.append(uuid, ts, &format!("A: {t}"));
                                    }
                                }
                                Some("tool_use") => {
                                    let input = b.get("input").unwrap_or(&Value::Null);
                                    self.append(uuid, ts, &tool_line(str_at(b, "name"), input));
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                None
            }
            _ => None,
        }
    }

    pub fn finish(mut self) -> Vec<Episode> {
        self.flush();
        self.eps
    }
}
