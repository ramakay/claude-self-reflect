//! View implementations for `csr-engine transcript` / `csr_transcript`.
//!
//! Every view is a pure function over an already-parsed [`ParsedTranscript`]
//! (see `crate::transcript::parse_transcript`) — no I/O here. Each view
//! renders to either compact text or `--json` (serde) and honors a
//! char-budget with an honest, never-silent truncation marker (design doc
//! "Every view honors a char budget … and, when cut, ends with an explicit
//! continuation line").

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use regex::Regex;
use serde_json::{json, Value};

use super::{
    index_tool_results, truncate_chars, Entry, ParsedTranscript, Role, TranscriptRequest, ViewKind,
    DEFAULT_LAST_N,
};

/// Dispatch to the requested view. Called by [`crate::transcript::run`]
/// after the transcript has already been parsed.
pub fn render_view(
    parsed: &ParsedTranscript,
    path: &Path,
    project: &str,
    req: &TranscriptRequest,
) -> String {
    match req.view {
        ViewKind::Stats => render_stats(parsed, path, project, req.json),
        ViewKind::Prompts => render_prompts(parsed, req).render(req.budget_chars, req.json),
        ViewKind::Tools => render_tools(parsed, req).render(req.budget_chars, req.json),
        ViewKind::Files => render_files(parsed, req).render(req.budget_chars, req.json),
        ViewKind::Errors => render_errors(parsed, req).render(req.budget_chars, req.json),
        ViewKind::Slice => render_slice(parsed, req).render(req.budget_chars, req.json),
        ViewKind::Grep => match render_grep(parsed, req) {
            Ok(view) => view.render(req.budget_chars, req.json),
            Err(e) => {
                if req.json {
                    json!({ "error": e }).to_string()
                } else {
                    format!("<transcript_error>\n<error>{e}</error>\n</transcript_error>")
                }
            }
        },
    }
}

// ─── Generic list-view renderer (prompts/tools/files/errors/slice/grep) ───

/// One renderable unit: a compact text line plus its full-fidelity JSON
/// twin. `turn` drives both budget-cut bookkeeping and the resume hint.
struct RenderItem {
    turn: usize,
    text: String,
    json: Value,
}

/// A budget-aware, turn-numbered list view. Text rendering never splits an
/// item mid-line; JSON rendering never splits an item mid-object. Both cut
/// at the same item boundary and report the same resume turn, so a
/// `--turns <resume>..` follow-up call lands exactly where either surface
/// left off.
struct ViewData {
    view: &'static str,
    extra_json: Vec<(String, Value)>,
    header_lines: Vec<String>,
    items: Vec<RenderItem>,
    empty_message: String,
}

impl ViewData {
    fn render(&self, budget_chars: usize, json: bool) -> String {
        if json {
            self.render_json(budget_chars)
        } else {
            self.render_text(budget_chars)
        }
    }

    fn render_text(&self, budget_chars: usize) -> String {
        let mut out = String::new();
        for h in &self.header_lines {
            out.push_str(h);
            out.push('\n');
        }
        if self.items.is_empty() {
            out.push_str(&self.empty_message);
            out.push('\n');
            return out;
        }
        let mut shown = 0usize;
        for item in &self.items {
            let projected = out.len() + item.text.len() + 1;
            if shown > 0 && projected > budget_chars {
                break;
            }
            out.push_str(&item.text);
            out.push('\n');
            shown += 1;
        }
        if shown < self.items.len() {
            let remaining = self.items.len() - shown;
            let resume_turn = self.items[shown].turn;
            out.push_str(&format!(
                "... {remaining} more turns; --turns {resume_turn}.. to continue\n"
            ));
        }
        out
    }

    fn render_json(&self, budget_chars: usize) -> String {
        let header_cost: usize = self.header_lines.iter().map(|h| h.len()).sum();
        let mut acc = header_cost;
        let mut shown = 0usize;
        for item in &self.items {
            let item_len = item.json.to_string().len();
            if shown > 0 && acc + item_len > budget_chars {
                break;
            }
            acc += item_len;
            shown += 1;
        }
        let truncated = shown < self.items.len();

        let mut obj = serde_json::Map::new();
        obj.insert("view".into(), json!(self.view));
        for (k, v) in &self.extra_json {
            obj.insert(k.clone(), v.clone());
        }
        if self.items.is_empty() {
            obj.insert("message".into(), json!(self.empty_message));
        }
        obj.insert(
            "items".into(),
            Value::Array(self.items[..shown].iter().map(|i| i.json.clone()).collect()),
        );
        obj.insert("truncated".into(), json!(truncated));
        if truncated {
            let remaining = self.items.len() - shown;
            let resume_turn = self.items[shown].turn;
            obj.insert("remaining".into(), json!(remaining));
            obj.insert("resume_turns".into(), json!(format!("{resume_turn}..")));
        }
        serde_json::to_string_pretty(&Value::Object(obj)).unwrap_or_default()
    }
}

// ─── stats ───

fn render_stats(parsed: &ParsedTranscript, path: &Path, project: &str, json: bool) -> String {
    let mut user_turns = 0usize;
    let mut assistant_turns = 0usize;
    let mut system_turns = 0usize;
    let mut tool_calls: BTreeMap<String, usize> = BTreeMap::new();
    let mut tool_errors = 0usize;
    let mut tool_result_bytes: u64 = 0;
    let mut first_ts: Option<&str> = None;
    let mut last_ts: Option<&str> = None;

    for entry in &parsed.entries {
        match entry.role {
            Role::User => user_turns += 1,
            Role::Assistant => assistant_turns += 1,
            Role::System => system_turns += 1,
        }
        for tu in &entry.tool_uses {
            *tool_calls.entry(tu.name.clone()).or_insert(0) += 1;
        }
        for tr in &entry.tool_results {
            tool_result_bytes += tr.byte_size as u64;
            if tr.is_error {
                tool_errors += 1;
            }
        }
        if let Some(ts) = entry.timestamp.as_deref() {
            if first_ts.is_none() {
                first_ts = Some(ts);
            }
            last_ts = Some(ts);
        }
    }

    let duration_secs = match (first_ts, last_ts) {
        (Some(a), Some(b)) => {
            match (
                chrono::DateTime::parse_from_rfc3339(a),
                chrono::DateTime::parse_from_rfc3339(b),
            ) {
                (Ok(da), Ok(db)) => Some((db - da).num_milliseconds() as f64 / 1000.0),
                _ => None,
            }
        }
        _ => None,
    };

    if json {
        json!({
            "view": "stats",
            "session_path": path.display().to_string(),
            "project": project,
            "total_lines": parsed.total_lines,
            "turn_count": parsed.entries.len(),
            "unrecognized_entries": parsed.unrecognized_entries,
            "user_turns": user_turns,
            "assistant_turns": assistant_turns,
            "system_turns": system_turns,
            "first_ts": first_ts,
            "last_ts": last_ts,
            "duration_secs": duration_secs,
            "tool_calls": tool_calls,
            "tool_errors": tool_errors,
            "tool_result_bytes": tool_result_bytes,
        })
        .to_string()
    } else {
        let mut out = String::new();
        out.push_str("<transcript_stats>\n");
        out.push_str(&format!("  <path>{}</path>\n", path.display()));
        out.push_str(&format!("  <project>{project}</project>\n"));
        out.push_str(&format!(
            "  <lines total=\"{}\" unrecognized=\"{}\"/>\n",
            parsed.total_lines, parsed.unrecognized_entries
        ));
        out.push_str(&format!(
            "  <turns total=\"{}\" user=\"{}\" assistant=\"{}\" system=\"{}\"/>\n",
            parsed.entries.len(),
            user_turns,
            assistant_turns,
            system_turns
        ));
        out.push_str(&format!(
            "  <span first=\"{}\" last=\"{}\" duration_secs=\"{}\"/>\n",
            first_ts.unwrap_or("-"),
            last_ts.unwrap_or("-"),
            duration_secs
                .map(|d| format!("{d:.1}"))
                .unwrap_or_else(|| "-".to_string())
        ));
        if tool_calls.is_empty() {
            out.push_str("  <tool_calls/>\n");
        } else {
            let joined = tool_calls
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("  <tool_calls>{joined}</tool_calls>\n"));
        }
        out.push_str(&format!(
            "  <tool_errors count=\"{tool_errors}\"/>\n  <tool_result_bytes total=\"{tool_result_bytes}\"/>\n"
        ));
        if parsed.unrecognized_entries > 0 {
            out.push_str(&format!(
                "  <note>unrecognized_entries: {} (schema drift? check aux_schema_miss)</note>\n",
                parsed.unrecognized_entries
            ));
        }
        out.push_str("</transcript_stats>\n");
        out
    }
}

// ─── prompts ───

fn render_prompts(parsed: &ParsedTranscript, req: &TranscriptRequest) -> ViewData {
    let items: Vec<RenderItem> = parsed
        .entries
        .iter()
        .filter(|e| e.role == Role::User && req.role.matches_role(Role::User))
        .filter(|e| !e.text.trim().is_empty())
        .map(|e| {
            let ts = e.timestamp.as_deref().unwrap_or("-");
            let preview = truncate_chars(e.text.trim(), 300);
            RenderItem {
                turn: e.turn,
                text: format!("[turn {}] {ts}: {preview}", e.turn),
                json: json!({
                    "turn": e.turn,
                    "timestamp": e.timestamp,
                    "text": e.text,
                }),
            }
        })
        .collect();

    ViewData {
        view: "prompts",
        extra_json: vec![],
        header_lines: vec!["<transcript_prompts>".to_string()],
        items,
        empty_message: "(no user prompts found)".to_string(),
    }
}

// ─── tools ───

fn describe_tool_use_fields(t: &super::ToolUse) -> String {
    let mut parts = Vec::new();
    if let Some(v) = &t.file_path {
        parts.push(format!("file_path={v}"));
    }
    if let Some(v) = &t.command {
        parts.push(format!("command={v}"));
    }
    if let Some(v) = &t.pattern {
        parts.push(format!("pattern={v}"));
    }
    if let Some(v) = &t.prompt {
        parts.push(format!("prompt={v}"));
    }
    parts.join(" ")
}

fn render_tools(parsed: &ParsedTranscript, req: &TranscriptRequest) -> ViewData {
    let result_index = index_tool_results(&parsed.entries);
    let tool_filter = req.tool.as_deref().map(str::to_lowercase);

    let mut items = Vec::new();
    for entry in &parsed.entries {
        if !req.role.matches_role(entry.role) {
            continue;
        }
        for tu in &entry.tool_uses {
            if let Some(f) = &tool_filter {
                if tu.name.to_lowercase() != *f {
                    continue;
                }
            }
            let fields = describe_tool_use_fields(tu);
            let (outcome_text, is_error, result_bytes, result_turn) =
                match tu.id.as_deref().and_then(|id| result_index.get(id)) {
                    Some((rturn, r)) => {
                        let tag = if r.is_error { "error" } else { "ok" };
                        (
                            format!("{tag} ({} bytes)", r.byte_size),
                            r.is_error,
                            Some(r.byte_size),
                            Some(*rturn),
                        )
                    }
                    None => (
                        "pending (no matching tool_result)".to_string(),
                        false,
                        None,
                        None,
                    ),
                };
            let text = if fields.is_empty() {
                format!("[turn {}] {} -> {outcome_text}", entry.turn, tu.name)
            } else {
                format!(
                    "[turn {}] {} {fields} -> {outcome_text}",
                    entry.turn, tu.name
                )
            };
            items.push(RenderItem {
                turn: entry.turn,
                text,
                json: json!({
                    "turn": entry.turn,
                    "tool_use_id": tu.id,
                    "name": tu.name,
                    "file_path": tu.file_path,
                    "command": tu.command,
                    "pattern": tu.pattern,
                    "prompt": tu.prompt,
                    "is_error": is_error,
                    "result_bytes": result_bytes,
                    "result_turn": result_turn,
                }),
            });
        }
    }

    ViewData {
        view: "tools",
        extra_json: if let Some(f) = &req.tool {
            vec![("tool_filter".to_string(), json!(f))]
        } else {
            vec![]
        },
        header_lines: vec!["<transcript_tools>".to_string()],
        items,
        empty_message: "(no matching tool calls found)".to_string(),
    }
}

// ─── files ───

const FILE_TOUCHING_TOOLS: &[&str] = &["Edit", "Write", "Read", "NotebookEdit", "MultiEdit"];

struct FileStat {
    count: usize,
    first_turn: usize,
    last_turn: usize,
}

fn render_files(parsed: &ParsedTranscript, req: &TranscriptRequest) -> ViewData {
    let mut stats: BTreeMap<String, FileStat> = BTreeMap::new();
    for entry in &parsed.entries {
        if !req.role.matches_role(entry.role) {
            continue;
        }
        for tu in &entry.tool_uses {
            if !FILE_TOUCHING_TOOLS.contains(&tu.name.as_str()) {
                continue;
            }
            let Some(fp) = &tu.file_path else { continue };
            let stat = stats.entry(fp.clone()).or_insert(FileStat {
                count: 0,
                first_turn: entry.turn,
                last_turn: entry.turn,
            });
            stat.count += 1;
            stat.first_turn = stat.first_turn.min(entry.turn);
            stat.last_turn = stat.last_turn.max(entry.turn);
        }
    }

    let items: Vec<RenderItem> = stats
        .into_iter()
        .map(|(path, stat)| RenderItem {
            turn: stat.first_turn,
            text: format!(
                "{path}: {} touches (turns {}-{})",
                stat.count, stat.first_turn, stat.last_turn
            ),
            json: json!({
                "path": path,
                "touches": stat.count,
                "first_turn": stat.first_turn,
                "last_turn": stat.last_turn,
            }),
        })
        .collect();

    ViewData {
        view: "files",
        extra_json: vec![],
        header_lines: vec!["<transcript_files>".to_string()],
        items,
        empty_message: "(no files touched via Edit/Write/Read/NotebookEdit/MultiEdit)".to_string(),
    }
}

// ─── errors ───

fn index_tool_use_names(entries: &[Entry]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for entry in entries {
        for tu in &entry.tool_uses {
            if let Some(id) = &tu.id {
                map.insert(id.clone(), tu.name.clone());
            }
        }
    }
    map
}

fn render_errors(parsed: &ParsedTranscript, req: &TranscriptRequest) -> ViewData {
    let names = index_tool_use_names(&parsed.entries);

    let mut items = Vec::new();
    for entry in &parsed.entries {
        if !req.role.matches_role(entry.role) {
            continue;
        }
        for tr in &entry.tool_results {
            if !tr.is_error {
                continue;
            }
            let name = tr
                .tool_use_id
                .as_deref()
                .and_then(|id| names.get(id))
                .map(String::as_str)
                .unwrap_or("unknown");
            items.push(RenderItem {
                turn: entry.turn,
                text: format!("[turn {}] {name} failed: {}", entry.turn, tr.preview),
                json: json!({
                    "turn": entry.turn,
                    "tool": name,
                    "tool_use_id": tr.tool_use_id,
                    "byte_size": tr.byte_size,
                    "preview": tr.preview,
                }),
            });
        }
    }

    ViewData {
        view: "errors",
        extra_json: vec![],
        header_lines: vec!["<transcript_errors>".to_string()],
        items,
        empty_message: "(no tool errors found)".to_string(),
    }
}

// ─── slice ───

fn render_slice(parsed: &ParsedTranscript, req: &TranscriptRequest) -> ViewData {
    let filtered: Vec<&Entry> = parsed
        .entries
        .iter()
        .filter(|e| req.role.matches_role(e.role))
        .collect();

    let selected: Vec<&&Entry> = if let Some((start, end)) = req.turns {
        filtered
            .iter()
            .filter(|e| e.turn >= start && end.is_none_or(|end| e.turn <= end))
            .collect()
    } else {
        let n = req.last.unwrap_or(DEFAULT_LAST_N);
        let skip = filtered.len().saturating_sub(n);
        filtered.iter().skip(skip).collect()
    };

    let items: Vec<RenderItem> = selected
        .into_iter()
        .map(|entry| {
            let ts = entry.timestamp.as_deref().unwrap_or("-");
            let preview = truncate_chars(entry.text.trim(), 500);
            let mut text = format!("[turn {}] {} {ts}: {preview}", entry.turn, entry.role);
            for tu in &entry.tool_uses {
                let fields = describe_tool_use_fields(tu);
                if fields.is_empty() {
                    text.push_str(&format!("\n  tool_use: {}", tu.name));
                } else {
                    text.push_str(&format!("\n  tool_use: {} {fields}", tu.name));
                }
            }
            for tr in &entry.tool_results {
                let tag = if tr.is_error { "error" } else { "ok" };
                text.push_str(&format!(
                    "\n  tool_result: {tag} ({} bytes): {}",
                    tr.byte_size,
                    truncate_chars(&tr.preview, 200)
                ));
            }
            RenderItem {
                turn: entry.turn,
                json: json!({
                    "turn": entry.turn,
                    "role": entry.role.to_string(),
                    "timestamp": entry.timestamp,
                    "text": entry.text,
                    "tool_uses": entry.tool_uses,
                    "tool_results": entry.tool_results,
                }),
                text,
            }
        })
        .collect();

    ViewData {
        view: "slice",
        extra_json: vec![],
        header_lines: vec!["<transcript_slice>".to_string()],
        items,
        empty_message: "(no turns in the requested range)".to_string(),
    }
}

// ─── grep ───

fn render_grep(parsed: &ParsedTranscript, req: &TranscriptRequest) -> Result<ViewData, String> {
    let Some(pattern) = req.grep.as_deref() else {
        return Err("grep view requires --grep <regex>".to_string());
    };
    let re = Regex::new(pattern).map_err(|e| format!("invalid --grep regex '{pattern}': {e}"))?;

    let items: Vec<RenderItem> = parsed
        .entries
        .iter()
        .filter(|e| req.role.matches_role(e.role))
        .filter(|e| re.is_match(&e.text))
        .map(|e| {
            let ts = e.timestamp.as_deref().unwrap_or("-");
            let preview = truncate_chars(e.text.trim(), 300);
            RenderItem {
                turn: e.turn,
                text: format!("[turn {}] {} {ts}: {preview}", e.turn, e.role),
                json: json!({
                    "turn": e.turn,
                    "role": e.role.to_string(),
                    "timestamp": e.timestamp,
                    "text": e.text,
                }),
            }
        })
        .collect();

    Ok(ViewData {
        view: "grep",
        extra_json: vec![("pattern".to_string(), json!(pattern))],
        header_lines: vec!["<transcript_grep>".to_string()],
        items,
        empty_message: "(no matches)".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{self, RoleFilter, TranscriptRequest, ViewKind};
    use std::io::Write;

    /// Golden fixture transcript: every entry kind the design doc's test
    /// plan calls for — user msg, assistant msg with nested content,
    /// tool_use + ok tool_result, tool_use + error tool_result, an
    /// unknown-kind line, hostile text (HTML/injection chars).
    fn write_golden_fixture(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("golden.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();

        // turn 1: user prompt (plain string content)
        writeln!(
            f,
            r#"{{"type":"user","timestamp":"2026-08-10T10:00:00Z","uuid":"u1","message":{{"role":"user","content":"Fix the <script>alert('x')</script> injection bug in auth.rs"}}}}"#
        )
        .unwrap();

        // unrecognized-kind line (schema drift / vendor entry) — must be counted, never break parsing
        writeln!(
            f,
            r#"{{"type":"queue-operation","operation":"enqueue","timestamp":"2026-08-10T10:00:01Z"}}"#
        )
        .unwrap();

        // turn 2: assistant msg with nested content (text + tool_use)
        writeln!(
            f,
            r#"{{"type":"assistant","timestamp":"2026-08-10T10:00:02Z","uuid":"a1","message":{{"role":"assistant","content":[{{"type":"text","text":"Reading the file first."}},{{"type":"tool_use","id":"tu_read1","name":"Read","input":{{"file_path":"/repo/src/auth.rs"}}}}]}}}}"#
        )
        .unwrap();

        // turn 3: user entry carrying the ok tool_result for tu_read1
        writeln!(
            f,
            r#"{{"type":"user","timestamp":"2026-08-10T10:00:03Z","uuid":"u2","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"tu_read1","is_error":false,"content":"fn login() {{}}"}}]}}}}"#
        )
        .unwrap();

        // turn 4: assistant issues an Edit that will fail
        writeln!(
            f,
            r#"{{"type":"assistant","timestamp":"2026-08-10T10:00:04Z","uuid":"a2","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"tu_edit1","name":"Edit","input":{{"file_path":"/repo/src/auth.rs","command":"n/a"}}}}]}}}}"#
        )
        .unwrap();

        // turn 5: user entry carrying the error tool_result for tu_edit1
        writeln!(
            f,
            r#"{{"type":"user","timestamp":"2026-08-10T10:00:05Z","uuid":"u3","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"tu_edit1","is_error":true,"content":"old_string not found in file"}}]}}}}"#
        )
        .unwrap();

        // turn 6: assistant retries with Bash + Grep tool_use (command/pattern fields)
        writeln!(
            f,
            r#"{{"type":"assistant","timestamp":"2026-08-10T10:00:06Z","uuid":"a3","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"tu_bash1","name":"Bash","input":{{"command":"cargo test"}}}},{{"type":"tool_use","id":"tu_grep1","name":"Grep","input":{{"pattern":"fn login"}}}}]}}}}"#
        )
        .unwrap();

        // turn 7: user gives ok result for bash, none for grep (pending) + hostile text
        writeln!(
            f,
            r#"{{"type":"user","timestamp":"2026-08-10T10:00:07Z","uuid":"u4","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"tu_bash1","is_error":false,"content":"test result: ok. 12 passed"}},{{"type":"text","text":"Also: `; DROP TABLE users; --` should be treated as inert data, not a command."}}]}}}}"#
        )
        .unwrap();

        // another unrecognized line
        writeln!(f, r#"{{"type":"last-prompt","lastPrompt":"..."}}"#).unwrap();

        // turn 8: system entry
        writeln!(
            f,
            r#"{{"type":"system","subtype":"stop_hook_summary","timestamp":"2026-08-10T10:00:08Z","uuid":"s1"}}"#
        )
        .unwrap();

        // malformed JSON line (must count as unrecognized, never panic)
        writeln!(f, r#"{{not valid json"#).unwrap();

        // turn 9: final assistant summary
        writeln!(
            f,
            r#"{{"type":"assistant","timestamp":"2026-08-10T10:00:09Z","uuid":"a4","message":{{"role":"assistant","content":[{{"type":"text","text":"Done: tests pass, auth.rs untouched by the failed edit."}}]}}}}"#
        )
        .unwrap();

        path
    }

    fn base_req(view: ViewKind, session: &str) -> TranscriptRequest {
        TranscriptRequest {
            session: session.to_string(),
            view,
            ..Default::default()
        }
    }

    // ─── parse_transcript ───

    #[test]
    fn golden_fixture_parses_expected_turns_and_drift_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let parsed = transcript::parse_transcript(&path).unwrap();

        assert_eq!(parsed.entries.len(), 9, "9 recognized entries (turns)");
        // queue-operation, last-prompt, and the malformed line = 3 unrecognized
        assert_eq!(parsed.unrecognized_entries, 3);
        assert_eq!(parsed.entries[0].role, Role::User);
        assert_eq!(parsed.entries[0].turn, 1);
        assert_eq!(parsed.entries.last().unwrap().turn, 9);
    }

    #[test]
    fn hostile_text_is_preserved_verbatim_not_executed_or_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let parsed = transcript::parse_transcript(&path).unwrap();
        assert!(parsed.entries[0]
            .text
            .contains("<script>alert('x')</script>"));
        let turn7 = parsed.entries.iter().find(|e| e.turn == 7).unwrap();
        assert!(turn7.text.contains("DROP TABLE users"));
    }

    // ─── stats view ───

    #[test]
    fn stats_view_text_reports_exact_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let out = transcript::run(
            dir.path(),
            &base_req(ViewKind::Stats, &path.to_string_lossy()),
        );
        assert!(out.contains("<lines total=\"12\" unrecognized=\"3\"/>"));
        assert!(out.contains("<turns total=\"9\" user=\"4\" assistant=\"4\" system=\"1\"/>"));
        assert!(out.contains("<tool_errors count=\"1\"/>"));
        assert!(out.contains(
            "<note>unrecognized_entries: 3 (schema drift? check aux_schema_miss)</note>"
        ));
    }

    #[test]
    fn stats_view_json_round_trips_and_matches_fixture_facts() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let mut req = base_req(ViewKind::Stats, &path.to_string_lossy());
        req.json = true;
        let out = transcript::run(dir.path(), &req);
        let v: Value = serde_json::from_str(&out).expect("stats --json must be valid JSON");
        assert_eq!(v["turn_count"], json!(9));
        assert_eq!(v["unrecognized_entries"], json!(3));
        assert_eq!(v["tool_errors"], json!(1));
        assert_eq!(v["user_turns"], json!(4));
        assert_eq!(v["assistant_turns"], json!(4));
        assert_eq!(v["system_turns"], json!(1));
        assert_eq!(v["tool_calls"]["Read"], json!(1));
        assert_eq!(v["tool_calls"]["Edit"], json!(1));
        assert_eq!(v["tool_calls"]["Bash"], json!(1));
        assert_eq!(v["tool_calls"]["Grep"], json!(1));
    }

    // ─── prompts view ───

    #[test]
    fn prompts_view_returns_only_user_text_with_turn_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let out = transcript::run(
            dir.path(),
            &base_req(ViewKind::Prompts, &path.to_string_lossy()),
        );
        assert!(out.contains("[turn 1]"));
        assert!(out.contains("Fix the"));
        // turn 3, 5 are tool_result-only user entries — must NOT appear as prompts
        assert!(!out.contains("[turn 3]"));
        assert!(!out.contains("[turn 5]"));
        // turn 7 has genuine user text alongside a tool_result — must appear
        assert!(out.contains("[turn 7]"));
    }

    // ─── tools view ───

    #[test]
    fn tools_view_pairs_tool_use_with_its_later_tool_result() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let out = transcript::run(
            dir.path(),
            &base_req(ViewKind::Tools, &path.to_string_lossy()),
        );
        assert!(out.contains("[turn 2] Read file_path=/repo/src/auth.rs -> ok (13 bytes)"));
        assert!(out
            .contains("[turn 4] Edit file_path=/repo/src/auth.rs command=n/a -> error (28 bytes)"));
        assert!(out.contains("[turn 6] Grep pattern=fn login -> pending (no matching tool_result)"));
    }

    #[test]
    fn tools_view_tool_filter_narrows_to_named_tool_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let mut req = base_req(ViewKind::Tools, &path.to_string_lossy());
        req.tool = Some("Edit".to_string());
        let out = transcript::run(dir.path(), &req);
        assert!(out.contains("Edit"));
        assert!(!out.contains("Read file_path"));
        assert!(!out.contains("Bash"));
    }

    // ─── files view ───

    #[test]
    fn files_view_aggregates_distinct_paths_with_turn_span() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let out = transcript::run(
            dir.path(),
            &base_req(ViewKind::Files, &path.to_string_lossy()),
        );
        assert!(out.contains("/repo/src/auth.rs: 2 touches (turns 2-4)"));
    }

    // ─── errors view ───

    #[test]
    fn errors_view_lists_only_error_tool_results_with_owning_tool_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let out = transcript::run(
            dir.path(),
            &base_req(ViewKind::Errors, &path.to_string_lossy()),
        );
        assert!(out.contains("[turn 5] Edit failed: old_string not found in file"));
        assert!(!out.contains("Read failed"));
        assert!(!out.contains("Bash failed"));
    }

    // ─── slice + grep composition ───

    #[test]
    fn grep_returns_turn_then_slice_of_that_turn_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());

        let mut grep_req = base_req(ViewKind::Grep, &path.to_string_lossy());
        grep_req.grep = Some("DROP TABLE".to_string());
        let grep_out = transcript::run(dir.path(), &grep_req);
        assert!(grep_out.contains("[turn 7]"));

        let mut slice_req = base_req(ViewKind::Slice, &path.to_string_lossy());
        slice_req.turns = Some((7, Some(7)));
        let slice_out = transcript::run(dir.path(), &slice_req);
        assert!(slice_out.contains("[turn 7]"));
        assert!(slice_out.contains("DROP TABLE"));
        assert!(!slice_out.contains("[turn 6]"));
        assert!(!slice_out.contains("[turn 8]"));
    }

    #[test]
    fn grep_invalid_regex_reports_honest_error_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let mut req = base_req(ViewKind::Grep, &path.to_string_lossy());
        req.grep = Some("(unclosed".to_string());
        let out = transcript::run(dir.path(), &req);
        assert!(out.contains("invalid --grep regex"));
    }

    #[test]
    fn slice_last_n_returns_final_n_recognized_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let mut req = base_req(ViewKind::Slice, &path.to_string_lossy());
        req.last = Some(2);
        let out = transcript::run(dir.path(), &req);
        assert!(out.contains("[turn 8]"));
        assert!(out.contains("[turn 9]"));
        assert!(!out.contains("[turn 7]"));
    }

    #[test]
    fn role_filter_narrows_slice_to_assistant_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let mut req = base_req(ViewKind::Slice, &path.to_string_lossy());
        req.role = RoleFilter::Assistant;
        req.turns = Some((1, None));
        let out = transcript::run(dir.path(), &req);
        assert!(out.contains("[turn 2]"));
        assert!(!out.contains("[turn 1]")); // turn 1 is a user entry
        assert!(!out.contains("[turn 3]")); // turn 3 is a user entry
    }

    // ─── budget / truncation ───

    #[test]
    fn budget_cut_reports_remaining_count_and_exact_resume_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let mut req = base_req(ViewKind::Slice, &path.to_string_lossy());
        req.turns = Some((1, Some(9)));
        req.role = RoleFilter::All;
        // Small enough that only the first couple of turns fit.
        req.budget_chars = 120;
        let out = transcript::run(dir.path(), &req);

        assert!(
            out.contains("[turn 1]"),
            "content before the cut must be intact"
        );
        assert!(
            out.contains("more turns; --turns"),
            "must end with an explicit continuation line, got: {out}"
        );
        // Resume turn must be a turn that was NOT rendered.
        let resume_marker = out.lines().last().unwrap().to_string();
        assert!(resume_marker.starts_with("... "));
        assert!(resume_marker.ends_with(".. to continue"));
    }

    #[test]
    fn budget_cut_json_carries_truncated_remaining_and_resume_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let mut req = base_req(ViewKind::Slice, &path.to_string_lossy());
        req.turns = Some((1, Some(9)));
        req.json = true;
        req.budget_chars = 120;
        let out = transcript::run(dir.path(), &req);
        let v: Value = serde_json::from_str(&out).expect("must still be valid JSON when truncated");
        assert_eq!(v["truncated"], json!(true));
        assert!(v["remaining"].as_u64().unwrap() > 0);
        assert!(v["resume_turns"].as_str().unwrap().ends_with(".."));
        // Items array itself must be non-empty and short (proves the cut, not a crash).
        assert!(!v["items"].as_array().unwrap().is_empty());
        assert!(v["items"].as_array().unwrap().len() < 9);
    }

    #[test]
    fn generous_budget_never_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let mut req = base_req(ViewKind::Slice, &path.to_string_lossy());
        req.turns = Some((1, Some(9)));
        req.budget_chars = super::super::DEFAULT_BUDGET_CHARS;
        let out = transcript::run(dir.path(), &req);
        assert!(!out.contains("more turns"));
        for t in 1..=9 {
            assert!(out.contains(&format!("[turn {t}]")), "missing turn {t}");
        }
    }

    // ─── drift: renamed/unknown fields never break views ───

    #[test]
    fn drift_fixture_unknown_fields_never_panics_and_stats_reports_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drift.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        // Renamed `type` (vendor drift) — must be counted, not crash.
        writeln!(
            f,
            r#"{{"kind":"user","message":{{"role":"user","content":"hi"}}}}"#
        )
        .unwrap();
        // Recognized type but content is an unexpected shape (object, not string/array).
        writeln!(
            f,
            r#"{{"type":"user","timestamp":"2026-08-10T00:00:00Z","message":{{"role":"user","content":{{"weird":"shape"}}}}}}"#
        )
        .unwrap();
        // A tool_use block with a completely new/renamed field alongside known ones.
        writeln!(
            f,
            r#"{{"type":"assistant","timestamp":"2026-08-10T00:00:01Z","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"x1","name":"FutureTool","input":{{"file_path":"/a","brand_new_field":"???"}}}}]}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"totally":"unrecognized shape with no type field"}}"#
        )
        .unwrap();

        let parsed = transcript::parse_transcript(&path).unwrap();
        // line 1 (renamed `type`) + line 4 (no type) = 2 unrecognized;
        // lines 2 and 3 ARE recognized (type present, content shape tolerated).
        assert_eq!(parsed.unrecognized_entries, 2);
        assert_eq!(parsed.entries.len(), 2);

        let mut req = base_req(ViewKind::Stats, &path.to_string_lossy());
        req.json = true;
        let out = transcript::run(dir.path(), &req);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["unrecognized_entries"], json!(2));

        // tools view must not panic on the object-shaped content entry either.
        let tools_out = transcript::run(
            dir.path(),
            &base_req(ViewKind::Tools, &path.to_string_lossy()),
        );
        assert!(tools_out.contains("FutureTool"));
    }

    // ─── not-found / sidechains / arg dispatch ───

    #[test]
    fn missing_session_reports_same_honest_message_as_get_full_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let out = transcript::run(
            dir.path(),
            &base_req(ViewKind::Stats, "nonexistent-session-id"),
        );
        assert!(out.contains("not found in any project"));
    }

    #[test]
    fn sidechains_flag_rejected_with_clear_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        let mut req = base_req(ViewKind::Stats, &path.to_string_lossy());
        req.sidechains = true;
        let out = transcript::run(dir.path(), &req);
        assert!(out.contains("reserved") && out.contains("not yet supported"));
    }

    #[test]
    fn parse_turns_spec_accepts_bounded_and_open_ranges_rejects_garbage() {
        assert_eq!(transcript::parse_turns_spec("40..120"), Ok((40, Some(120))));
        assert_eq!(transcript::parse_turns_spec("340..").unwrap(), (340, None));
        assert!(transcript::parse_turns_spec("nope").is_err());
        assert!(transcript::parse_turns_spec("120..40").is_err());
    }

    #[test]
    fn explicit_path_bypasses_projects_dir_walk() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_golden_fixture(dir.path());
        // Pass a projects_dir that does NOT contain the file to prove the
        // explicit-path branch is what resolved it.
        let unrelated_dir = tempfile::tempdir().unwrap();
        let out = transcript::run(
            unrelated_dir.path(),
            &base_req(ViewKind::Stats, &path.to_string_lossy()),
        );
        assert!(out.contains("<turns total=\"9\""));
    }

    // ─── perf smoke (ignored by default: run with `cargo test -- --ignored`) ───

    #[test]
    #[ignore]
    fn perf_smoke_100k_lines_stats_completes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.jsonl");
        let mut f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        for i in 0..100_000usize {
            if i % 2 == 0 {
                writeln!(
                    f,
                    r#"{{"type":"user","timestamp":"2026-08-10T00:00:00Z","message":{{"role":"user","content":"message number {i}"}}}}"#
                )
                .unwrap();
            } else {
                writeln!(
                    f,
                    r#"{{"type":"assistant","timestamp":"2026-08-10T00:00:00Z","message":{{"role":"assistant","content":[{{"type":"text","text":"reply {i}"}},{{"type":"tool_use","id":"t{i}","name":"Bash","input":{{"command":"echo {i}"}}}}]}}}}"#
                )
                .unwrap();
            }
        }
        drop(f);

        let parsed = transcript::parse_transcript(&path).unwrap();
        assert_eq!(parsed.entries.len(), 100_000);
        assert_eq!(parsed.unrecognized_entries, 0);

        let dummy_dir = tempfile::tempdir().unwrap();
        let out = transcript::run(
            dummy_dir.path(),
            &base_req(ViewKind::Stats, &path.to_string_lossy()),
        );
        assert!(out.contains("<turns total=\"100000\""));
    }
}
