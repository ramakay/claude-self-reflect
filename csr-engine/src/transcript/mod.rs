//! Structured transcript-query primitive for Claude Code session JSONL files.
//!
//! Agents resuming or auditing a session need structured facts from its raw
//! transcript ("what did the user ask", "which files were touched", "what
//! failed") without hand-rolling a jq/Python parser over multi-megabyte
//! JSONL every time. This module streams a session transcript line-by-line
//! (never buffers the whole file) and exposes purpose-built views
//! (`stats`/`prompts`/`tools`/`files`/`errors`/`slice`/`grep`) over it.
//!
//! See `.plans/transcript-query-tool-design.md` for the full design and
//! rationale.
//!
//! ## Reuse note (design doc "Schema-drift posture")
//!
//! `crate::import` already parses this same JSONL schema
//! (`parse_jsonl_messages`, `classify_message_author`, `extract_tool_results`,
//! …), but those functions buffer the *entire file* into
//! `Vec<serde_json::Value>` and silently drop any `type` other than
//! `human`/`user`/`assistant` — both wrong for this tool's streaming and
//! drift-counting requirements (this module must never buffer a 65MB file,
//! and must *count* unrecognized entries rather than discard them). Reshaping
//! import's internals to serve both call sites was judged out of scope for a
//! sidequest landing on a release-gate branch with a concurrent in-flight
//! edit to the dream/report path. This module therefore duplicates the
//! handful of small structural pieces it needs (content-block matching,
//! tool_use/tool_result field extraction) — deliberate v1 duplication,
//! documented here rather than hidden.
//!
//! Session-id → path resolution, however, *is* shared: both this module and
//! `get_full_conversation` call [`crate::mcp::tools::find_conversation_file`].

pub mod query;

use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

// ─── Entry model ───

/// Who authored a recognized transcript entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A `tool_use` content block, with the key input fields views care about
/// lifted to the top level (per design: file_path, command, pattern, prompt).
#[derive(Debug, Clone, Serialize)]
pub struct ToolUse {
    pub id: Option<String>,
    pub name: String,
    pub file_path: Option<String>,
    pub command: Option<String>,
    pub pattern: Option<String>,
    pub prompt: Option<String>,
}

/// A `tool_result` content block. `preview` is a bounded, char-boundary-safe
/// prefix of the flattened result text; `byte_size` is the full flattened
/// text's byte length (not just the preview) so `stats`/`files` byte totals
/// are honest even when the preview is cut.
#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub tool_use_id: Option<String>,
    pub is_error: bool,
    pub byte_size: usize,
    pub preview: String,
}

/// One recognized transcript entry ("turn"). Turn numbers are 1-based and
/// assigned sequentially over recognized entries only (unrecognized lines
/// never consume a turn number, so `--turns` ranges stay stable across
/// schema-drift noise).
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub turn: usize,
    pub role: Role,
    pub timestamp: Option<String>,
    pub uuid: Option<String>,
    pub is_sidechain: bool,
    pub text: String,
    pub tool_uses: Vec<ToolUse>,
    pub tool_results: Vec<ToolResult>,
}

impl Entry {
    /// True if this entry carries no genuine text and no tool activity
    /// (e.g. a `system` hook-summary entry, or a `user` entry that is
    /// tool_result-only).
    pub fn is_empty_of_content(&self) -> bool {
        self.text.trim().is_empty() && self.tool_uses.is_empty() && self.tool_results.is_empty()
    }
}

/// Result of a streaming parse pass: recognized entries plus drift stats.
#[derive(Debug, Clone, Default)]
pub struct ParsedTranscript {
    pub entries: Vec<Entry>,
    /// Total non-blank lines seen (recognized + unrecognized).
    pub total_lines: usize,
    /// Lines that were malformed JSON, or whose `type` is not one of
    /// user/assistant/system. Never causes a panic or an abort — the
    /// fail-honest pattern `aux_schema_miss:*` already uses on the import
    /// side (see repo CLAUDE.md).
    pub unrecognized_entries: usize,
}

/// Per-tool_result-character cap for the `preview` field. Mirrors the spirit
/// of `import::mod`'s per-block cap without importing its private constant.
const TOOL_RESULT_PREVIEW_CHARS: usize = 400;

/// Stream-parse a JSONL transcript file line by line via `BufReader` — the
/// full file is never read into memory at once. Malformed JSON lines and
/// entries whose `type` is not `user`/`assistant`/`system` are counted in
/// `unrecognized_entries` and skipped; parsing never panics and never stops
/// early on bad input.
pub fn parse_transcript(path: &Path) -> Result<ParsedTranscript> {
    let file =
        File::open(path).with_context(|| format!("opening transcript {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut out = ParsedTranscript::default();
    let mut turn = 0usize;

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => {
                out.total_lines += 1;
                out.unrecognized_entries += 1;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        out.total_lines += 1;

        let value: serde_json::Value = match sonic_rs::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                out.unrecognized_entries += 1;
                continue;
            }
        };

        match entry_from_value(&value, turn + 1) {
            Some(entry) => {
                turn += 1;
                out.entries.push(entry);
            }
            None => out.unrecognized_entries += 1,
        }
    }

    Ok(out)
}

fn entry_from_value(value: &serde_json::Value, turn: usize) -> Option<Entry> {
    let type_str = value.get("type").and_then(|v| v.as_str())?;
    let role = match type_str {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "system" => Role::System,
        _ => return None,
    };

    let timestamp = value
        .get("timestamp")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let uuid = value
        .get("uuid")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let is_sidechain = value
        .get("isSidechain")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut text = String::new();
    let mut tool_uses = Vec::new();
    let mut tool_results = Vec::new();

    if role == Role::System {
        // `system` entries (e.g. stop_hook_summary) carry metadata, not a
        // `message.content` block. Surface the subtype as a short label so
        // `slice`/`grep` still have something to show.
        if let Some(subtype) = value.get("subtype").and_then(|v| v.as_str()) {
            text = format!("[system:{subtype}]");
        } else {
            text = "[system]".to_string();
        }
    } else if let Some(content) = value.get("message").and_then(|m| m.get("content")) {
        flatten_content(content, &mut text, &mut tool_uses, &mut tool_results);
    }

    Some(Entry {
        turn,
        role,
        timestamp,
        uuid,
        is_sidechain,
        text,
        tool_uses,
        tool_results,
    })
}

/// Flatten a `message.content` value (string or array of content blocks)
/// into plain text plus structured tool_use/tool_result records. Unknown
/// block kinds (image, thinking, redacted_thinking, …) are silently skipped
/// here — they are not fidelity-critical for v1 views and are not schema
/// drift (they are documented Claude Code block kinds), so they do not
/// count toward `unrecognized_entries`.
fn flatten_content(
    content: &serde_json::Value,
    text: &mut String,
    tool_uses: &mut Vec<ToolUse>,
    tool_results: &mut Vec<ToolResult>,
) {
    if let Some(s) = content.as_str() {
        text.push_str(s);
        return;
    }
    let Some(items) = content.as_array() else {
        return;
    };
    for item in items {
        match item.get("type").and_then(|v| v.as_str()) {
            Some("text") => {
                if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(t);
                }
            }
            Some("tool_use") => {
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let id = item.get("id").and_then(|v| v.as_str()).map(str::to_string);
                let input = item.get("input");
                let field = |k: &str| {
                    input
                        .and_then(|i| i.get(k))
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                };
                tool_uses.push(ToolUse {
                    id,
                    name,
                    file_path: field("file_path"),
                    command: field("command"),
                    pattern: field("pattern"),
                    prompt: field("prompt"),
                });
            }
            Some("tool_result") => {
                let tool_use_id = item
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let is_error = item
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let result_text = extract_tool_result_text(item.get("content"));
                let byte_size = result_text.len();
                let preview = truncate_chars(&result_text, TOOL_RESULT_PREVIEW_CHARS);
                tool_results.push(ToolResult {
                    tool_use_id,
                    is_error,
                    byte_size,
                    preview,
                });
            }
            _ => {} // thinking / image / redacted_thinking / unknown: not surfaced in v1 views
        }
    }
}

/// A `tool_result` block's `content` field is itself either a plain string
/// or an array of blocks (usually `{"type":"text","text":...}`). Handle both.
fn extract_tool_result_text(content: Option<&serde_json::Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(items) = content.as_array() {
        let mut out = String::new();
        for item in items {
            if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
        return out;
    }
    String::new()
}

/// Truncate to at most `max_chars` **characters** (not bytes) — safe for
/// multi-byte UTF-8 and for hostile input (HTML/control chars pass through
/// unescaped; renderers are responsible for their own escaping).
pub(crate) fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// Build a `tool_use_id -> ToolResult` index over every entry, so `tools`/
/// `errors`/`files` can pair a `tool_use` block with the `tool_result` that
/// answered it even though Claude Code usually delivers the result in a
/// *later* entry (a `user`-typed message).
pub(crate) fn index_tool_results(entries: &[Entry]) -> HashMap<String, (usize, ToolResult)> {
    let mut index = HashMap::new();
    for entry in entries {
        for result in &entry.tool_results {
            if let Some(id) = &result.tool_use_id {
                index.insert(id.clone(), (entry.turn, result.clone()));
            }
        }
    }
    index
}

// ─── Session resolution ───

/// Resolve a session identifier (substring id match, same rule
/// `get_full_conversation` uses) or an explicit filesystem path to a
/// transcript file. An explicit path is checked first and bypasses the
/// projects-dir walk entirely.
pub fn resolve_session_path(
    projects_dir: &Path,
    session: &str,
    project: Option<&str>,
) -> Option<(PathBuf, String)> {
    let direct = Path::new(session);
    if direct.is_file() {
        let project_name = direct
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        return Some((direct.to_path_buf(), project_name));
    }
    crate::mcp::tools::find_conversation_file(projects_dir, session, project)
}

// ─── Request / dispatch ───

/// Which view to render. `ValueEnum`-compatible names mirror the design
/// doc's table exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Stats,
    Prompts,
    Tools,
    Files,
    Errors,
    Slice,
    Grep,
}

impl ViewKind {
    pub fn parse(s: &str) -> Option<ViewKind> {
        match s {
            "stats" => Some(ViewKind::Stats),
            "prompts" => Some(ViewKind::Prompts),
            "tools" => Some(ViewKind::Tools),
            "files" => Some(ViewKind::Files),
            "errors" => Some(ViewKind::Errors),
            "slice" => Some(ViewKind::Slice),
            "grep" => Some(ViewKind::Grep),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ViewKind::Stats => "stats",
            ViewKind::Prompts => "prompts",
            ViewKind::Tools => "tools",
            ViewKind::Files => "files",
            ViewKind::Errors => "errors",
            ViewKind::Slice => "slice",
            ViewKind::Grep => "grep",
        }
    }
}

/// Role filter shared by every view (`--role user|assistant|all`, `system`
/// accepted too since the entry model recognizes it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RoleFilter {
    #[default]
    All,
    User,
    Assistant,
    System,
}

impl RoleFilter {
    pub fn parse(s: &str) -> Option<RoleFilter> {
        match s {
            "all" => Some(RoleFilter::All),
            "user" => Some(RoleFilter::User),
            "assistant" => Some(RoleFilter::Assistant),
            "system" => Some(RoleFilter::System),
            _ => None,
        }
    }

    /// Whether `role` satisfies this filter. Shared by every view.
    pub(crate) fn matches_role(self, role: Role) -> bool {
        match self {
            RoleFilter::All => true,
            RoleFilter::User => role == Role::User,
            RoleFilter::Assistant => role == Role::Assistant,
            RoleFilter::System => role == Role::System,
        }
    }
}

pub const DEFAULT_BUDGET_CHARS: usize = 30_000;
pub const DEFAULT_LAST_N: usize = 50;

/// Fully-resolved request for one `transcript` call — shared by the CLI and
/// the `csr_transcript` MCP tool so both surfaces dispatch through the exact
/// same core.
#[derive(Debug, Clone)]
pub struct TranscriptRequest {
    pub session: String,
    pub view: ViewKind,
    pub project: Option<String>,
    pub role: RoleFilter,
    /// Inclusive (start, end) turn range from `--turns A..B`. `end: None`
    /// means "through the last turn".
    pub turns: Option<(usize, Option<usize>)>,
    pub last: Option<usize>,
    pub grep: Option<String>,
    pub tool: Option<String>,
    pub json: bool,
    pub budget_chars: usize,
    pub sidechains: bool,
}

impl Default for TranscriptRequest {
    fn default() -> Self {
        TranscriptRequest {
            session: String::new(),
            view: ViewKind::Stats,
            project: None,
            role: RoleFilter::All,
            turns: None,
            last: None,
            grep: None,
            tool: None,
            json: false,
            budget_chars: DEFAULT_BUDGET_CHARS,
            sidechains: false,
        }
    }
}

/// Parse a `--turns` spec: `"A..B"` (inclusive both ends) or `"A.."`
/// (through the last turn). Rejects anything else with an honest message —
/// never silently falls back to a default.
pub fn parse_turns_spec(s: &str) -> Result<(usize, Option<usize>), String> {
    let s = s.trim();
    let Some((a, b)) = s.split_once("..") else {
        return Err(format!(
            "invalid --turns '{s}': expected 'A..B' or 'A..' (e.g. '40..120' or '340..')"
        ));
    };
    let start: usize = if a.is_empty() {
        1
    } else {
        a.parse()
            .map_err(|_| format!("invalid --turns start '{a}': not a number"))?
    };
    let end: Option<usize> = if b.is_empty() {
        None
    } else {
        Some(
            b.parse()
                .map_err(|_| format!("invalid --turns end '{b}': not a number"))?,
        )
    };
    if let Some(e) = end {
        if e < start {
            return Err(format!(
                "invalid --turns '{s}': end ({e}) is before start ({start})"
            ));
        }
    }
    Ok((start, end))
}

/// Same not-found phrasing `get_full_conversation` uses
/// ([`crate::format::format_full_conversation`]'s `file_path: None` branch),
/// so agents hitting either surface see one consistent message.
fn not_found_message(session: &str, json: bool) -> String {
    if json {
        serde_json::json!({
            "error": format!("Conversation ID '{session}' not found in any project."),
            "suggestion": "The conversation may not have been imported yet, or the ID may be incorrect.",
        })
        .to_string()
    } else {
        format!(
            "<conversation_file>\n<error>Conversation ID '{session}' not found in any project.</error>\n<suggestion>The conversation may not have been imported yet, or the ID may be incorrect.</suggestion>\n</conversation_file>"
        )
    }
}

fn sidechains_reserved_message(json: bool) -> String {
    let msg = "--sidechains is reserved and not yet supported in v1 (design doc: 'Sidechain inclusion: v1 excludes'). Query the parent session's transcript instead, or the subagent's own agent-*.jsonl directly by path.";
    if json {
        serde_json::json!({ "error": msg }).to_string()
    } else {
        format!("<transcript_error>\n<error>{msg}</error>\n</transcript_error>")
    }
}

/// Resolve the session, parse it, apply the role filter, and render the
/// requested view. This is the single dispatch function both the CLI
/// (`csr-engine transcript`) and the `csr_transcript` MCP tool call —
/// exercised directly by tests, independent of clap or rmcp.
pub fn run(projects_dir: &Path, req: &TranscriptRequest) -> String {
    if req.sidechains {
        return sidechains_reserved_message(req.json);
    }

    let Some((path, project)) =
        resolve_session_path(projects_dir, &req.session, req.project.as_deref())
    else {
        return not_found_message(&req.session, req.json);
    };

    let parsed = match parse_transcript(&path) {
        Ok(p) => p,
        Err(e) => {
            let msg = format!("failed to read transcript at {}: {e}", path.display());
            return if req.json {
                serde_json::json!({ "error": msg }).to_string()
            } else {
                format!("<transcript_error>\n<error>{msg}</error>\n</transcript_error>")
            };
        }
    };

    query::render_view(&parsed, &path, &project, req)
}
