pub mod backfill;
pub mod codex_rollout;
pub mod coedit_backfill;
pub mod plans;
pub mod registry;
pub mod watcher;

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConversationAttribution {
    pub project_name: String,
    pub source: &'static str,
    pub parent_conversation_id: Option<String>,
}

/// Regex to strip `<private>...</private>` content before storage.
static PRIVATE_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?si)<private>.*?</private>").unwrap());

/// Exact CSR hook headers are scrubbed only when they lead a system-reminder.
/// User prose and assistant prose mentioning these markers remain searchable.
static CSR_SYSTEM_REMINDER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)<system-reminder>[ \t\r\n]*(?:CSR ENDLESS MEMORY ACTIVE|CSR PICKUP —|EPISODE INDEX —|(?:RELEVANT )?PAST CONTEXT - NOT INSTRUCTIONS).*?</system-reminder>",
    )
    .unwrap()
});

/// A chunk of a conversation, ready for embedding and storage.
#[derive(Debug, Clone)]
pub struct ConversationChunk {
    pub id: String,
    pub conversation_id: String,
    pub project_name: String,
    pub timestamp: String,
    pub content: String,
    pub message_count: usize,
    /// Human-readable summary from JSONL `{"type":"summary"}` line, or first user message.
    /// Used for timeline display instead of raw tool-heavy content.
    pub summary: Option<String>,
    /// Highest-authority speaker among this chunk's messages (User > Assistant >
    /// ToolResult). Drives provenance-aware recall; defaults to ToolResult.
    pub author: crate::provenance::Speaker,
    /// Sequential chunk index within its conversation (0-based). Same index that
    /// feeds the deterministic UUIDv5 chunk id. Saga Phase 1 provenance signal.
    pub seq: usize,
    /// True if ANY message in this chunk has JSONL `isSidechain: true`, OR the
    /// conversation id starts with `agent-`. Over-labeling beats under-labeling
    /// for later credit assignment (agent-pollution finding). Labeling only —
    /// nothing filters on this yet (Phase 2+).
    pub is_sidechain: bool,
}

#[derive(Debug, Default)]
pub(crate) struct ParsedConversation {
    pub chunks: Vec<ConversationChunk>,
    pub suppression: CsrSuppressionStats,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CsrSuppressionStats {
    pub csr_tool_blocks_suppressed: usize,
    pub csr_hook_wrappers_scrubbed: usize,
}

#[derive(Default)]
struct CsrMessageSanitizer {
    suppressed_tool_use_ids: HashSet<String>,
    stats: CsrSuppressionStats,
}

/// Namespace UUID for deterministic chunk IDs (UUIDv5).
const CSR_NAMESPACE: Uuid = Uuid::from_bytes([
    0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
]);

/// Normalize project name from Claude's dash-separated directory format.
///
/// Port of `shared/normalization.py:normalize_project_name`.
///
/// Examples:
///   "-Users-name-projects-claude-self-reflect" -> "claude-self-reflect"
///   "-Users-name-projects-my-project"          -> "my-project"
///   "my-project"                                -> "my-project"
pub fn normalize_project_name(dir_name: &str) -> String {
    if dir_name.is_empty() {
        return String::new();
    }

    // Strip trailing slashes
    let trimmed = dir_name.trim_end_matches('/');

    // Extract the final path component
    let final_component = trimmed.rsplit('/').next().unwrap_or(trimmed);

    // If it's Claude's dash-separated format, extract after "projects-"
    if final_component.starts_with('-') && final_component.contains("projects") {
        if let Some(idx) = final_component.rfind("projects-") {
            let start = idx + "projects-".len();
            if start < final_component.len() {
                return final_component[start..].to_string();
            }
        }
    }

    // For regular paths, return the directory name
    if final_component.is_empty() {
        // Fallback: parent name
        let parent = trimmed.trim_end_matches('/');
        parent.rsplit('/').nth(1).unwrap_or(parent).to_string()
    } else {
        final_component.to_string()
    }
}

/// Discover all project directories under the Claude projects base path.
pub fn discover_projects(base_dir: &Path) -> Result<Vec<(PathBuf, String)>> {
    let mut projects = Vec::new();

    if !base_dir.exists() {
        return Ok(projects);
    }

    for entry in fs::read_dir(base_dir).context("reading projects directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let dir_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let project_name = normalize_project_name(&dir_name);
            if !project_name.is_empty() {
                projects.push((path, project_name));
            }
        }
    }

    Ok(projects)
}

/// List all JSONL files in a project directory.
pub fn list_jsonl_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "jsonl") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// List main transcripts and nested sidechain transcripts in stable path order.
pub fn list_conversation_jsonl_files(dir: &Path) -> Result<Vec<PathBuf>> {
    fn visit(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        let mut entries = fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                visit(&path, files)?;
            } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") {
                files.push(path);
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    visit(dir, &mut files)?;
    Ok(files)
}

/// Derive storage attribution from a transcript's position beneath the Claude
/// projects root. Sidechains are `<project>/<session>/subagents/agent-*.jsonl`;
/// their immediate parent is never the project.
pub(crate) fn derive_conversation_attribution(
    projects_dir: &Path,
    file_path: &Path,
) -> ConversationAttribution {
    let canonical_base = projects_dir
        .canonicalize()
        .unwrap_or_else(|_| projects_dir.to_path_buf());
    let canonical_file = file_path
        .canonicalize()
        .unwrap_or_else(|_| file_path.to_path_buf());
    derive_conversation_attribution_canonical(&canonical_base, &canonical_file)
}

pub(crate) fn derive_conversation_attribution_canonical(
    canonical_projects_dir: &Path,
    canonical_file_path: &Path,
) -> ConversationAttribution {
    let components: Vec<String> = canonical_file_path
        .strip_prefix(canonical_projects_dir)
        .ok()
        .and_then(|relative| {
            relative
                .components()
                .map(|component| match component {
                    std::path::Component::Normal(value) => value.to_str().map(str::to_string),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
        })
        .unwrap_or_default();
    let project_name = components
        .first()
        .map(|name| normalize_project_name(name))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let is_sidechain = components.len() >= 4
        && components
            .get(2)
            .is_some_and(|component| component == "subagents")
        && canonical_file_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("agent-") && name.ends_with(".jsonl"));

    ConversationAttribution {
        project_name,
        source: if is_sidechain {
            "sidechain"
        } else {
            "conversation"
        },
        parent_conversation_id: is_sidechain.then(|| components[1].clone()),
    }
}

/// Parse a JSONL conversation file into chunks of ~50 messages each.
/// Uses BufReader streaming + sonic-rs for ~2.8x faster parsing.
///
/// Extracts conversation summary from `{"type":"summary"}` lines when available.
/// Falls back to the first user message for timeline display.
/// True if the first user message is a CSR agent prompt (briefing analyst or
/// compaction summarizer) — exclude the whole transcript from import so CSR
/// talking to itself never enters the search index. Signatures live in
/// `extraction::provenance` (single registry). Deliberately starts_with, not
/// contains: import-skip drops a whole conversation, so only transcripts that
/// ARE an agent prompt qualify — not real sessions that merely quote one.
fn is_csr_agent_prompt(first_user_message: &str) -> bool {
    let mut trimmed = first_user_message.trim_start();
    // `claude -p -` (prompt via stdin) records the literal "-" argument as the
    // first line of the user message, with the piped prompt following it.
    if let Some(rest) = trimmed.strip_prefix('-') {
        if rest.starts_with('\n') || rest.starts_with("\r\n") {
            trimmed = rest.trim_start();
        }
    }
    crate::extraction::provenance::AGENT_PROMPT_SIGNATURES
        .iter()
        .any(|sig| trimmed.starts_with(sig))
}

pub fn parse_jsonl_file(path: &Path, project_name: &str) -> Result<Vec<ConversationChunk>> {
    Ok(parse_jsonl_file_with_stats(path, project_name)?.chunks)
}

pub(crate) fn parse_jsonl_file_with_stats(
    path: &Path,
    project_name: &str,
) -> Result<ParsedConversation> {
    let conversation_id = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let file = fs::File::open(path).context("opening JSONL file")?;
    let reader = BufReader::new(file);
    let mut messages: Vec<String> = Vec::new();
    let mut authors: Vec<crate::provenance::Speaker> = Vec::new();
    let mut sidechains: Vec<bool> = Vec::new();
    let mut first_timestamp: Option<String> = None;
    let mut last_timestamp: Option<String> = None;
    let mut summary: Option<String> = None;
    let mut first_user_message: Option<String> = None;
    let mut csr_sanitizer = CsrMessageSanitizer::default();

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        // sonic-rs: serde-compatible drop-in, ~2.8x faster on aarch64
        let mut parsed: serde_json::Value = match sonic_rs::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Extract message type
        let msg_type = parsed
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Capture summary line (Claude Code writes {"type":"summary","summary":"..."})
        if msg_type == "summary" {
            if let Some(s) = parsed.get("summary").and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    summary = Some(s.to_string());
                }
            }
            continue;
        }

        // Include human/user/assistant messages (Claude Code uses "user", not "human")
        if msg_type != "human" && msg_type != "user" && msg_type != "assistant" {
            continue;
        }

        sanitize_message_for_search(&mut parsed, &mut csr_sanitizer);

        // Capture first and last timestamps
        if let Some(ts) = parsed.get("timestamp").and_then(|v| v.as_str()) {
            if first_timestamp.is_none() {
                first_timestamp = Some(ts.to_string());
            }
            last_timestamp = Some(ts.to_string());
        }

        // Capture first user message as fallback summary (what the user was trying to do)
        if first_user_message.is_none() && (msg_type == "user" || msg_type == "human") {
            let text = extract_message_text(&parsed);
            if !text.is_empty() && text.len() > 5 {
                // Truncate to 200 chars for timeline preview
                let preview = if text.len() > 200 {
                    let boundary = text.floor_char_boundary(200);
                    format!("{}...", &text[..boundary])
                } else {
                    text.clone()
                };
                first_user_message = Some(preview);
            }
        }

        // Extract text content + tool context + tool RESULTS (all stripped of private tags).
        // tool_result blocks carry the substance of research conversations — fetched docs,
        // file contents, and subagent reports. Without them, ~90% of a session's content is
        // invisible to search and recall collapses to the opening user prompt.
        let text = extract_message_text(&parsed);
        let tool_context = strip_private_tags(&extract_tool_context(&parsed));
        let tool_results = strip_private_tags(&extract_tool_results(&parsed));
        let combined_text = [text, tool_context, tool_results]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let is_sidechain_msg = parsed
            .get("isSidechain")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !combined_text.is_empty() {
            messages.push(combined_text);
            authors.push(classify_message_author(&parsed));
            sidechains.push(is_sidechain_msg);
        }
    }

    if messages.is_empty() {
        return Ok(ParsedConversation {
            chunks: Vec::new(),
            suppression: csr_sanitizer.stats,
        });
    }

    // Skip CSR's own agent-subprocess transcripts (the session-briefing analyst,
    // the compaction summarizer). Their FIRST user message IS the agent prompt, so
    // importing them pollutes the search index with CSR talking to itself — which
    // then feeds back into the next briefing. A normal user session never opens with
    // these strings; quoting them later in a session (e.g. via hook-injected context)
    // is fine because we only test the first user message.
    if let Some(ref fm) = first_user_message {
        if is_csr_agent_prompt(fm) {
            return Ok(ParsedConversation {
                chunks: Vec::new(),
                suppression: csr_sanitizer.stats,
            });
        }
    }

    // Use last_timestamp for ordering (shows "last active" not "started at")
    // Fall back to first_timestamp if somehow no last, then to now()
    let timestamp = last_timestamp
        .or(first_timestamp)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    // Priority: JSONL summary > first user message > None
    let chunk_summary = summary.or(first_user_message);

    // Chunk by character budget, NOT a fixed message count. The embedding model
    // (all-MiniLM-L6-v2) truncates input at ~256 tokens, so a chunk larger than
    // ~900 chars only embeds its head — the rest is unsearchable. Sizing each chunk
    // under that window means the whole conversation actually lands in vector space.
    let mut chunks: Vec<ConversationChunk> = Vec::new();
    let mut buf = String::new();
    let mut buf_authors: Vec<crate::provenance::Speaker> = Vec::new();
    let mut buf_sidechains: Vec<bool> = Vec::new();
    let mut buf_msgs = 0usize;

    for ((msg, author), sidechain) in messages.iter().zip(authors.iter()).zip(sidechains.iter()) {
        // A single message larger than the budget is hard-split into multiple chunks
        // so its tail (e.g. the end of a long report) is embedded too.
        if msg.len() > CHUNK_CHAR_BUDGET {
            if !buf.is_empty() {
                push_chunk(
                    &mut chunks,
                    &conversation_id,
                    project_name,
                    &timestamp,
                    std::mem::take(&mut buf),
                    buf_msgs,
                    &chunk_summary,
                    chunk_author(&buf_authors),
                    chunk_is_sidechain(&buf_sidechains, &conversation_id),
                );
                buf_authors.clear();
                buf_sidechains.clear();
                buf_msgs = 0;
            }
            let mut start = 0;
            while start < msg.len() {
                let mut end = (start + CHUNK_CHAR_BUDGET).min(msg.len());
                end = msg.floor_char_boundary(end);
                if end <= start {
                    end = msg.len();
                }
                push_chunk(
                    &mut chunks,
                    &conversation_id,
                    project_name,
                    &timestamp,
                    msg[start..end].to_string(),
                    1,
                    &chunk_summary,
                    *author,
                    chunk_is_sidechain(std::slice::from_ref(sidechain), &conversation_id),
                );
                start = end;
            }
            continue;
        }

        // Flush before exceeding the budget, then start a fresh chunk.
        if !buf.is_empty() && buf.len() + msg.len() + 2 > CHUNK_CHAR_BUDGET {
            push_chunk(
                &mut chunks,
                &conversation_id,
                project_name,
                &timestamp,
                std::mem::take(&mut buf),
                buf_msgs,
                &chunk_summary,
                chunk_author(&buf_authors),
                chunk_is_sidechain(&buf_sidechains, &conversation_id),
            );
            buf_authors.clear();
            buf_sidechains.clear();
            buf_msgs = 0;
        }
        if !buf.is_empty() {
            buf.push_str("\n\n");
        }
        buf.push_str(msg);
        buf_authors.push(*author);
        buf_sidechains.push(*sidechain);
        buf_msgs += 1;
    }
    if !buf.is_empty() {
        push_chunk(
            &mut chunks,
            &conversation_id,
            project_name,
            &timestamp,
            buf,
            buf_msgs,
            &chunk_summary,
            chunk_author(&buf_authors),
            chunk_is_sidechain(&buf_sidechains, &conversation_id),
        );
    }

    Ok(ParsedConversation {
        chunks,
        suppression: csr_sanitizer.stats,
    })
}

/// Character budget per chunk. Kept under the embedding model's ~256-token window
/// (all-MiniLM-L6-v2) so each chunk embeds in full rather than head-truncated.
const CHUNK_CHAR_BUDGET: usize = 900;

/// Per-tool_result character cap. Bounds giant logs while preserving enough of a
/// fetched doc / subagent report for the size-based chunker to slice and embed.
const MAX_TOOL_RESULT_CHARS: usize = 4000;

/// Push one finished chunk, assigning a deterministic sequential id.
#[allow(clippy::too_many_arguments)]
fn push_chunk(
    chunks: &mut Vec<ConversationChunk>,
    conversation_id: &str,
    project_name: &str,
    timestamp: &str,
    content: String,
    message_count: usize,
    summary: &Option<String>,
    author: crate::provenance::Speaker,
    is_sidechain: bool,
) {
    let i = chunks.len();
    chunks.push(ConversationChunk {
        id: generate_chunk_id(conversation_id, i),
        conversation_id: conversation_id.to_string(),
        project_name: project_name.to_string(),
        timestamp: timestamp.to_string(),
        content,
        message_count,
        summary: summary.clone(),
        author,
        seq: i,
        is_sidechain,
    });
}

/// Extract searchable text from tool_result blocks (WebFetch/Read/Bash/Task/Agent
/// outputs). This is where research substance and subagent reports live; without it
/// recall collapses to the opening user prompt. Capped per result to bound logs.
fn extract_tool_results(msg: &serde_json::Value) -> String {
    let content = match msg
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        Some(c) => c,
        None => return String::new(),
    };
    let mut out: Vec<String> = Vec::new();
    for item in content {
        if item.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
            continue;
        }
        let body = match item.get("content") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|b| {
                    if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                        b.get("text").and_then(|t| t.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        let capped = if body.len() > MAX_TOOL_RESULT_CHARS {
            let end = body.floor_char_boundary(MAX_TOOL_RESULT_CHARS);
            &body[..end]
        } else {
            body
        };
        out.push(capped.to_string());
    }
    out.join("\n")
}

/// Parse a JSONL file into raw serde_json::Value messages (for extraction module).
/// Returns all messages with their original structure intact.
pub fn parse_jsonl_messages(path: &Path) -> Result<Vec<serde_json::Value>> {
    let file = fs::File::open(path).context("opening JSONL file for extraction")?;
    let reader = BufReader::new(file);
    let mut messages = Vec::new();

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let parsed: serde_json::Value = match sonic_rs::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if msg_type == "human" || msg_type == "user" || msg_type == "assistant" {
            messages.push(parsed);
        }
    }

    Ok(messages)
}

/// Parse messages for any pipeline that creates searchable reflections.
/// Provenance and codegraph callers intentionally use [`parse_jsonl_messages`]
/// so their structural analysis retains the original transcript.
pub(crate) fn parse_jsonl_messages_for_search(path: &Path) -> Result<Vec<serde_json::Value>> {
    let messages = parse_jsonl_messages(path)?;
    Ok(sanitize_messages_for_search(&messages).0)
}

/// Remove CSR's own tool payloads and exact hook-wrapper blocks while preserving
/// unrelated sibling blocks and surrounding prose. This is the single sanitizer
/// shared by chunk import, V3, heuristic, narrative, and story inputs.
pub(crate) fn sanitize_messages_for_search(
    messages: &[serde_json::Value],
) -> (Vec<serde_json::Value>, CsrSuppressionStats) {
    let mut sanitizer = CsrMessageSanitizer::default();
    let mut sanitized = messages.to_vec();
    for message in &mut sanitized {
        sanitize_message_for_search(message, &mut sanitizer);
    }
    (sanitized, sanitizer.stats)
}

fn sanitize_message_for_search(
    message: &mut serde_json::Value,
    sanitizer: &mut CsrMessageSanitizer,
) {
    let msg_type = message
        .get("type")
        .and_then(|value| value.as_str())
        .map(str::to_owned);
    let scrub_wrappers = matches!(msg_type.as_deref(), Some("user" | "human"));

    if let Some(content) = message
        .get_mut("message")
        .and_then(|value| value.get_mut("content"))
    {
        sanitize_content(content, scrub_wrappers, sanitizer);
    } else if let Some(content) = message.get_mut("content") {
        sanitize_content(content, scrub_wrappers, sanitizer);
    }
}

fn sanitize_content(
    content: &mut serde_json::Value,
    scrub_wrappers: bool,
    sanitizer: &mut CsrMessageSanitizer,
) {
    if let Some(text) = content.as_str() {
        if scrub_wrappers {
            *content = serde_json::Value::String(scrub_csr_system_reminders(
                text,
                &mut sanitizer.stats.csr_hook_wrappers_scrubbed,
            ));
        }
        return;
    }

    let Some(items) = content.as_array_mut() else {
        return;
    };
    let mut kept = Vec::with_capacity(items.len());
    for mut item in std::mem::take(items) {
        match item.get("type").and_then(|value| value.as_str()) {
            Some("tool_use") => {
                let name = item
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown");
                if is_csr_tool_use(&item, name) {
                    if let Some(id) = item.get("id").and_then(|value| value.as_str()) {
                        sanitizer.suppressed_tool_use_ids.insert(id.to_string());
                    }
                    sanitizer.stats.csr_tool_blocks_suppressed += 1;
                    continue;
                }
            }
            Some("tool_result") => {
                let is_csr_result = item
                    .get("tool_use_id")
                    .and_then(|value| value.as_str())
                    .is_some_and(|id| sanitizer.suppressed_tool_use_ids.contains(id));
                if is_csr_result {
                    sanitizer.stats.csr_tool_blocks_suppressed += 1;
                    continue;
                }
            }
            Some("text") if scrub_wrappers => {
                if let Some(text) = item.get_mut("text") {
                    if let Some(raw) = text.as_str() {
                        *text = serde_json::Value::String(scrub_csr_system_reminders(
                            raw,
                            &mut sanitizer.stats.csr_hook_wrappers_scrubbed,
                        ));
                    }
                }
            }
            _ => {}
        }
        kept.push(item);
    }
    *items = kept;
}

fn scrub_csr_system_reminders(text: &str, count: &mut usize) -> String {
    *count += CSR_SYSTEM_REMINDER_RE.find_iter(text).count();
    CSR_SYSTEM_REMINDER_RE.replace_all(text, "").to_string()
}

/// Extract text content from a JSONL message entry.
/// Strips `<private>...</private>` tagged content before returning.
/// Classify who authored a JSONL message. Critical for poisoning defense
/// (§Q6.2): Claude Code delivers `tool_result` blocks inside `type:"user"`
/// messages, so role alone is not enough — only genuine user prose counts as
/// authoritative `User`.
pub(crate) fn classify_message_author(msg: &serde_json::Value) -> crate::provenance::Speaker {
    use crate::provenance::Speaker;
    let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if msg_type == "assistant" {
        return Speaker::Assistant;
    }
    // user / human: genuine text → User; otherwise (tool_result-only / empty) →
    // non-authoritative ToolResult.
    if extract_message_text_raw(msg).trim().is_empty() {
        Speaker::ToolResult
    } else {
        Speaker::User
    }
}

/// Aggregate the author of a multi-message chunk: highest authority present,
/// User > Assistant > ToolResult. Empty → ToolResult (non-authoritative).
pub(crate) fn chunk_author(authors: &[crate::provenance::Speaker]) -> crate::provenance::Speaker {
    use crate::provenance::Speaker;
    if authors.contains(&Speaker::User) {
        Speaker::User
    } else if authors.contains(&Speaker::Assistant) {
        Speaker::Assistant
    } else {
        Speaker::ToolResult
    }
}

/// Aggregate sidechain status for a chunk: true if any of its messages is
/// sidechain, OR the whole conversation is an agent-subprocess transcript
/// (conversation_id starts with "agent-"). Over-labels on purpose.
pub(crate) fn chunk_is_sidechain(sidechain_flags: &[bool], conversation_id: &str) -> bool {
    sidechain_flags.iter().any(|&s| s) || conversation_id.starts_with("agent-")
}

fn extract_message_text(msg: &serde_json::Value) -> String {
    let raw = extract_message_text_raw(msg);
    // Strip privacy-tagged content before storage/embedding
    let without_private = strip_private_tags(&raw);
    let msg_type = msg.get("type").and_then(|value| value.as_str());
    if matches!(msg_type, Some("user" | "human")) {
        let mut ignored_count = 0;
        scrub_csr_system_reminders(&without_private, &mut ignored_count)
    } else {
        without_private
    }
}

fn extract_message_text_raw(msg: &serde_json::Value) -> String {
    // Try "message.content" array format (Claude's format)
    if let Some(content) = msg
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        let texts: Vec<&str> = content
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    item.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect();
        return texts.join("\n");
    }

    // Try simple "content" string
    if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
        return text.to_string();
    }

    // Try "message.content" as string
    if let Some(text) = msg
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
    {
        return text.to_string();
    }

    String::new()
}

/// Strip `<private>...</private>` tagged content from text (case-insensitive).
/// S-3 fix: also handles unclosed `<private>` tags by redacting to end of input.
fn strip_private_tags(text: &str) -> String {
    // Case-insensitive fast-path check to avoid regex overhead
    let lower = text.to_ascii_lowercase();
    if !lower.contains("<private>") {
        return text.to_string();
    }
    let result = PRIVATE_TAG_RE.replace_all(text, "[private]").to_string();
    // Handle unclosed <private> tag: redact from opening tag to end of input
    let result_lower = result.to_ascii_lowercase();
    if let Some(pos) = result_lower.find("<private>") {
        let mut truncated = result[..pos].to_string();
        truncated.push_str("[private]");
        return truncated;
    }
    result
}

/// Extract searchable context from tool_use blocks in a message.
///
/// Coding sessions are dominated by tool calls (Read, Edit, Bash, Grep, etc.).
/// Without this, 70%+ of a session's activity is invisible to search.
/// Extracts tool name + key parameters (file_path, command, pattern, query).
fn extract_tool_context(msg: &serde_json::Value) -> String {
    let content = msg
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array());

    let content = match content {
        Some(c) => c,
        None => return String::new(),
    };

    let mut tool_lines: Vec<String> = Vec::new();

    for item in content {
        if item.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
            continue;
        }

        let name = item
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("unknown");
        let input = match item.get("input") {
            Some(i) => i,
            None => {
                tool_lines.push(format!("[{}]", name));
                continue;
            }
        };

        // Extract the most searchable parameter for each tool type
        let detail = if let Some(fp) = input.get("file_path").and_then(|v| v.as_str()) {
            // Shorten to last 2 path components for searchability
            let parts: Vec<&str> = fp.rsplit('/').take(2).collect();
            parts.into_iter().rev().collect::<Vec<_>>().join("/")
        } else if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
            // Truncate long commands
            let truncated: String = cmd.chars().take(120).collect();
            truncated
        } else if let Some(pat) = input.get("pattern").and_then(|v| v.as_str()) {
            pat.to_string()
        } else if let Some(q) = input.get("query").and_then(|v| v.as_str()) {
            q.to_string()
        } else {
            String::new()
        };

        if detail.is_empty() {
            tool_lines.push(format!("[{}]", name));
        } else {
            tool_lines.push(format!("[{}: {}]", name, detail));
        }
    }

    tool_lines.join(" ")
}

pub(crate) fn is_csr_tool_use(item: &serde_json::Value, name: &str) -> bool {
    if name.starts_with("csr_")
        || name.starts_with("mcp__claude-self-reflect__")
        || name.starts_with("mcp__claude_self_reflect__")
    {
        return true;
    }

    const BARE_CSR_TOOLS: &[&str] = &[
        "reflect_on_past",
        "store_reflection",
        "quick_check",
        "search_by_recency",
        "get_recent_work",
        "get_timeline",
        "search_by_file",
        "search_by_concept",
        "search_insights",
        "get_more",
        "get_full_conversation",
        "get_session_learnings",
        "code_graph",
        "why",
        "resolve",
    ];

    BARE_CSR_TOOLS.contains(&name) && has_csr_server_identity(item)
}

fn has_csr_server_identity(item: &serde_json::Value) -> bool {
    const IDENTITY_KEYS: &[&str] = &[
        "server",
        "server_name",
        "serverName",
        "mcp_server",
        "mcpServer",
        "namespace",
    ];
    IDENTITY_KEYS.iter().any(|key| {
        item.get(*key).is_some_and(|value| match value {
            serde_json::Value::String(identity) => is_csr_server_name(identity),
            serde_json::Value::Object(object) => ["name", "id", "server"]
                .iter()
                .filter_map(|field| object.get(*field).and_then(|v| v.as_str()))
                .any(is_csr_server_name),
            _ => false,
        })
    })
}

fn is_csr_server_name(identity: &str) -> bool {
    matches!(identity, "claude-self-reflect" | "claude_self_reflect")
}

/// Generate a deterministic chunk ID using UUIDv5.
fn generate_chunk_id(conversation_id: &str, chunk_index: usize) -> String {
    let input = format!("{}-chunk-{}", conversation_id, chunk_index);
    Uuid::new_v5(&CSR_NAMESPACE, input.as_bytes()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidechain_attribution_uses_project_ancestor_and_parent_session() {
        let projects = Path::new("/Users/test/.claude/projects");
        let path = projects
            .join("-Users-test-projects-real-project")
            .join("parent-session")
            .join("subagents")
            .join("agent-child.jsonl");

        let attribution = derive_conversation_attribution(projects, &path);
        assert_eq!(attribution.project_name, "real-project");
        assert_eq!(attribution.source, "sidechain");
        assert_eq!(
            attribution.parent_conversation_id.as_deref(),
            Some("parent-session")
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_projects_root_attributes_canonical_sidechain_path() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let real_projects = temp.path().join("real-projects");
        let sidechain = real_projects
            .join("-Users-test-projects-real-project")
            .join("parent-session")
            .join("subagents");
        std::fs::create_dir_all(&sidechain).unwrap();
        let file = sidechain.join("agent-child.jsonl");
        std::fs::write(&file, "{}\n").unwrap();
        let linked_projects = temp.path().join("linked-projects");
        symlink(&real_projects, &linked_projects).unwrap();

        let attribution =
            derive_conversation_attribution(&linked_projects, &file.canonicalize().unwrap());
        assert_eq!(attribution.project_name, "real-project");
        assert_eq!(attribution.source, "sidechain");
        assert_eq!(
            attribution.parent_conversation_id.as_deref(),
            Some("parent-session")
        );
    }

    #[test]
    fn recursive_conversation_discovery_includes_sidechains_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        let sidechains = root.join("session-a/subagents");
        std::fs::create_dir_all(&sidechains).unwrap();
        std::fs::write(root.join("main.jsonl"), "{}\n").unwrap();
        std::fs::write(sidechains.join("agent-z.jsonl"), "{}\n").unwrap();
        std::fs::write(sidechains.join("ignore.txt"), "ignored").unwrap();

        let files = list_conversation_jsonl_files(&root).unwrap();
        assert_eq!(
            files,
            vec![root.join("main.jsonl"), sidechains.join("agent-z.jsonl")]
        );
    }

    #[test]
    fn sidechain_transcript_suppresses_csr_tool_blocks_through_shared_sanitizer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-child.jsonl");
        let lines = [
            serde_json::json!({
                "type": "assistant",
                "isSidechain": true,
                "timestamp": "2026-08-06T12:00:00Z",
                "message": {"content": [
                    {"type":"tool_use","id":"csr-1","name":"csr_reflect_on_past","input":{"query":"SIDECHAIN CSR SECRET"}},
                    {"type":"text","text":"SIDECHAIN ASSISTANT TEXT KEPT"}
                ]}
            }),
            serde_json::json!({
                "type": "user",
                "isSidechain": true,
                "timestamp": "2026-08-06T12:00:01Z",
                "message": {"content": [
                    {"type":"tool_result","tool_use_id":"csr-1","content":"SIDECHAIN CSR RESULT SECRET"},
                    {"type":"text","text":"SIDECHAIN USER TEXT KEPT"}
                ]}
            }),
        ];
        std::fs::write(
            &path,
            lines
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();

        let parsed = parse_jsonl_file_with_stats(&path, "real-project").unwrap();
        let content = parsed
            .chunks
            .iter()
            .map(|chunk| chunk.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(content.contains("SIDECHAIN ASSISTANT TEXT KEPT"));
        assert!(content.contains("SIDECHAIN USER TEXT KEPT"));
        assert!(!content.contains("SIDECHAIN CSR SECRET"));
        assert!(!content.contains("SIDECHAIN CSR RESULT SECRET"));
        assert_eq!(parsed.suppression.csr_tool_blocks_suppressed, 2);
    }
    use crate::provenance::Speaker;

    #[test]
    fn classify_assistant_message() {
        let msg = serde_json::json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": "I'll fix it."}]}
        });
        assert_eq!(classify_message_author(&msg), Speaker::Assistant);
    }

    #[test]
    fn classify_genuine_user_prose() {
        let msg = serde_json::json!({
            "type": "user",
            "message": {"content": [{"type": "text", "text": "adopt epistemic continuity"}]}
        });
        assert_eq!(classify_message_author(&msg), Speaker::User);
    }

    #[test]
    fn classify_tool_result_in_user_message_is_not_user() {
        // Claude Code delivers tool_result as a user-type message — it must NOT
        // be treated as authoritative user content (poisoning defense §Q6.2).
        let msg = serde_json::json!({
            "type": "user",
            "message": {"content": [{"type": "tool_result", "content": "exit 0\n332 passed"}]}
        });
        assert_eq!(classify_message_author(&msg), Speaker::ToolResult);
    }

    #[test]
    fn chunk_author_prefers_user_then_assistant() {
        use Speaker::*;
        assert_eq!(chunk_author(&[ToolResult, Assistant, User]), User);
        assert_eq!(chunk_author(&[ToolResult, Assistant]), Assistant);
        assert_eq!(chunk_author(&[ToolResult, ToolResult]), ToolResult);
        assert_eq!(chunk_author(&[]), ToolResult);
    }

    #[test]
    fn chunk_is_sidechain_any_message_or_agent_prefix() {
        assert!(chunk_is_sidechain(&[false, true], "conv-abc"));
        assert!(chunk_is_sidechain(&[false, false], "agent-123"));
        assert!(!chunk_is_sidechain(&[false], "conv-abc"));
        assert!(!chunk_is_sidechain(&[], "conv-abc"));
        assert!(chunk_is_sidechain(&[], "agent-xyz"));
    }

    #[test]
    fn test_normalize_project_name() {
        assert_eq!(
            normalize_project_name("-Users-ramakrishnanannaswamy-projects-claude-self-reflect"),
            "claude-self-reflect"
        );
        assert_eq!(
            normalize_project_name("-Users-name-projects-my-project"),
            "my-project"
        );
        assert_eq!(normalize_project_name("my-project"), "my-project");
        assert_eq!(normalize_project_name(""), "");
        assert_eq!(
            normalize_project_name("/Users/name/.claude/projects/-Users-name-projects-foo"),
            "foo"
        );
    }

    #[test]
    fn test_is_csr_agent_prompt_skips_self_transcripts() {
        assert!(is_csr_agent_prompt(
            "You are CSR Episode Analyst. Generate a brief, actionable session briefing."
        ));
        assert!(is_csr_agent_prompt(
            "You are summarizing a coding session for future context restoration"
        ));
        // Real user work is never misclassified.
        assert!(!is_csr_agent_prompt(
            "Fix the V3 retry storm in csr-engine/src/daemon/mod.rs"
        ));
        // Merely quoting the prompt far into the message is not a meta-transcript
        // (only the leading window is tested).
        let quoted = format!("{}You are CSR Episode Analyst", "x".repeat(300));
        assert!(!is_csr_agent_prompt(&quoted));
        // Ratification extractor transcripts: `claude -p -` records the literal
        // "-" stdin marker as the first line before the piped prompt.
        assert!(is_csr_agent_prompt(
            "-\n# Ratification Dialog-Act Extraction\n\nYou are extracting OBSERVABLE dialog-acts"
        ));
        assert!(is_csr_agent_prompt(
            "# Ratification Dialog-Act Extraction\n\nYou are extracting"
        ));
        // A real message that merely starts with a dash stays importable.
        assert!(!is_csr_agent_prompt("- fix the daemon\n- ship release"));
    }

    #[test]
    fn test_generate_chunk_id_deterministic() {
        let id1 = generate_chunk_id("conv-abc", 0);
        let id2 = generate_chunk_id("conv-abc", 0);
        let id3 = generate_chunk_id("conv-abc", 1);
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_extract_tool_context() {
        // Message with tool_use blocks
        let msg: serde_json::Value = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "name": "Read",
                        "input": {"file_path": "/Users/me/projects/foo/src/engine.rs"}
                    },
                    {
                        "type": "tool_use",
                        "name": "Bash",
                        "input": {"command": "cargo test --release"}
                    },
                    {
                        "type": "tool_use",
                        "name": "Grep",
                        "input": {"pattern": "dump_to_disk"}
                    },
                    {
                        "type": "text",
                        "text": "Let me check the files."
                    }
                ]
            }
        });

        let ctx = extract_tool_context(&msg);
        assert!(ctx.contains("[Read: src/engine.rs]"));
        assert!(ctx.contains("[Bash: cargo test --release]"));
        assert!(ctx.contains("[Grep: dump_to_disk]"));
        // text blocks should not appear in tool context
        assert!(!ctx.contains("Let me check"));
    }

    #[test]
    fn test_extract_tool_context_empty() {
        // Message with only text, no tools
        let msg: serde_json::Value = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": "Hello world"}
                ]
            }
        });
        assert!(extract_tool_context(&msg).is_empty());
    }

    #[test]
    fn csr_tool_call_and_bound_result_are_suppressed_but_siblings_remain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("csr-filter.jsonl");
        let lines = [
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-08-06T12:00:00Z",
                "message": {"content": [
                    {
                        "type": "tool_use",
                        "id": "csr-call",
                        "name": "mcp__claude-self-reflect__csr_reflect_on_past",
                        "input": {"query": "SECRET RETRIEVAL QUERY"}
                    },
                    {
                        "type": "tool_use",
                        "id": "read-call",
                        "name": "Read",
                        "input": {"file_path": "/repo/src/kept.rs"}
                    }
                ]}
            }),
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-08-06T12:00:01Z",
                "message": {"content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "csr-call",
                        "content": "SECRET RETRIEVAL RESULT"
                    },
                    {
                        "type": "tool_result",
                        "tool_use_id": "read-call",
                        "content": "KEPT FILE RESULT"
                    }
                ]}
            }),
        ];
        std::fs::write(
            &path,
            lines
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();

        let parsed = parse_jsonl_file_with_stats(&path, "test").unwrap();
        assert_eq!(parsed.suppression.csr_tool_blocks_suppressed, 2);
        let content = parsed
            .chunks
            .into_iter()
            .map(|chunk| chunk.content)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(!content.contains("SECRET RETRIEVAL QUERY"));
        assert!(!content.contains("SECRET RETRIEVAL RESULT"));
        assert!(content.contains("[Read: src/kept.rs]"));
        assert!(content.contains("KEPT FILE RESULT"));
    }

    #[test]
    fn unresolved_tool_result_is_kept() {
        let msg = serde_json::json!({
            "type": "user",
            "message": {"content": [{
                "type": "tool_result",
                "tool_use_id": "missing-call",
                "content": "UNKNOWN RESULT MUST STAY"
            }]}
        });

        assert_eq!(extract_tool_results(&msg), "UNKNOWN RESULT MUST STAY");
    }

    #[test]
    fn bare_csr_name_requires_matching_server_identity() {
        let csr = serde_json::json!({
            "type": "assistant",
            "message": {"content": [{
                "type": "tool_use",
                "id": "csr-bare",
                "name": "reflect_on_past",
                "server_name": "claude-self-reflect",
                "input": {"query": "SUPPRESSED"}
            }]}
        });
        let unrelated = serde_json::json!({
            "type": "assistant",
            "message": {"content": [{
                "type": "tool_use",
                "id": "other-bare",
                "name": "reflect_on_past",
                "server_name": "another-server",
                "input": {"query": "KEPT"}
            }]}
        });
        let (sanitized, stats) = sanitize_messages_for_search(&[csr, unrelated]);

        assert!(extract_tool_context(&sanitized[0]).is_empty());
        assert_eq!(
            extract_tool_context(&sanitized[1]),
            "[reflect_on_past: KEPT]"
        );
        assert_eq!(stats.csr_tool_blocks_suppressed, 1);
    }

    #[test]
    fn memory_manifest_system_reminder_is_scrubbed_without_user_prose() {
        let msg = serde_json::json!({
            "type": "user",
            "message": {"content": [{
                "type": "text",
                "text": "Please fix the importer.\n<system-reminder>CSR ENDLESS MEMORY ACTIVE — every past session in this project is indexed and searchable.\nRetrieved memory payload.</system-reminder>\nKeep this sentence."
            }]}
        });

        assert_eq!(
            extract_message_text(&msg),
            "Please fix the importer.\n\nKeep this sentence."
        );
    }

    #[test]
    fn csr_discussion_outside_user_reminder_is_kept() {
        let user = serde_json::json!({
            "type": "user",
            "message": {"content": [{
                "type": "text",
                "text": "Discuss CSR PICKUP — without treating this prose as injected."
            }]}
        });
        let assistant = serde_json::json!({
            "type": "assistant",
            "message": {"content": [{
                "type": "text",
                "text": "<system-reminder>CSR PICKUP — quoted by the assistant</system-reminder>"
            }]}
        });

        assert!(extract_message_text(&user).contains("CSR PICKUP"));
        assert!(extract_message_text(&assistant).contains("CSR PICKUP"));
    }

    #[test]
    fn sanitized_messages_keep_user_prose_and_sibling_blocks_out_of_v3_contamination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secondary-pipelines.jsonl");
        let lines = [
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-08-06T12:00:00Z",
                "message": {"content": [
                    {
                        "type": "tool_use",
                        "id": "csr-call",
                        "name": "mcp__claude_self_reflect__csr_reflect_on_past",
                        "input": {"query": "CSR QUERY MUST DISAPPEAR"}
                    },
                    {
                        "type": "tool_use",
                        "id": "read-call",
                        "name": "Read",
                        "input": {"file_path": "/repo/src/kept.rs"}
                    }
                ]}
            }),
            serde_json::json!({
                "type": "human",
                "timestamp": "2026-08-06T12:00:01Z",
                "message": {"content": [
                    {
                        "type": "text",
                        "text": "USER PROSE BEFORE\n<system-reminder>CSR PICKUP — CSR WRAPPER MUST DISAPPEAR</system-reminder>\nUSER PROSE AFTER"
                    },
                    {
                        "type": "tool_result",
                        "tool_use_id": "csr-call",
                        "content": "CSR RESULT MUST DISAPPEAR"
                    },
                    {
                        "type": "tool_result",
                        "tool_use_id": "read-call",
                        "content": "SIBLING RESULT MUST STAY"
                    }
                ]}
            }),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-08-06T12:00:02Z",
                "message": {"content": [{"type": "text", "text": "Implemented kept.rs successfully"}]}
            }),
        ];
        std::fs::write(
            &path,
            lines
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();

        let messages = parse_jsonl_messages_for_search(&path).unwrap();
        let serialized = serde_json::to_string(&messages).unwrap();
        let result = crate::extraction::extract_v3(&messages);
        let v3 = format!("{}\n{}", result.search_index, result.context_cache);

        for forbidden in [
            "CSR QUERY MUST DISAPPEAR",
            "CSR RESULT MUST DISAPPEAR",
            "CSR WRAPPER MUST DISAPPEAR",
        ] {
            assert!(!serialized.contains(forbidden));
            assert!(!v3.contains(forbidden));
        }
        for retained in [
            "USER PROSE BEFORE",
            "USER PROSE AFTER",
            "kept.rs",
            "SIBLING RESULT MUST STAY",
        ] {
            assert!(
                serialized.contains(retained),
                "sanitized input lost {retained}"
            );
        }
        assert!(v3.contains("USER PROSE BEFORE"));
        assert!(v3.contains("USER PROSE AFTER"));
    }

    #[test]
    fn test_extract_message_text_user_type() {
        // "user" type messages should work the same as "human"
        let msg: serde_json::Value = serde_json::json!({
            "type": "user",
            "message": {
                "content": [
                    {"type": "text", "text": "Fix the chunking bug"}
                ]
            }
        });
        assert_eq!(extract_message_text(&msg), "Fix the chunking bug");
    }

    #[test]
    fn test_strip_private_tags() {
        assert_eq!(strip_private_tags("hello world"), "hello world");
        assert_eq!(
            strip_private_tags("before <private>secret key ABC123</private> after"),
            "before [private] after"
        );
        assert_eq!(
            strip_private_tags("no <private>one</private> and <private>two</private> tags"),
            "no [private] and [private] tags"
        );
    }

    #[test]
    fn test_strip_private_unclosed_tag() {
        // S-3 fix: unclosed <private> tag should redact to end of input
        assert_eq!(
            strip_private_tags("before <private>secret leaked here"),
            "before [private]"
        );
    }

    #[test]
    fn test_extract_message_text_strips_private() {
        let msg: serde_json::Value = serde_json::json!({
            "type": "user",
            "message": {
                "content": [
                    {"type": "text", "text": "My API key is <private>sk-abc123</private> please use it"}
                ]
            }
        });
        let text = extract_message_text(&msg);
        assert!(!text.contains("sk-abc123"));
        assert!(text.contains("[private]"));
    }
}
