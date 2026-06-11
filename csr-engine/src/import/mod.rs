pub mod watcher;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use uuid::Uuid;

/// Regex to strip `<private>...</private>` content before storage.
static PRIVATE_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?si)<private>.*?</private>").unwrap());

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
    let trimmed = first_user_message.trim_start();
    crate::extraction::provenance::AGENT_PROMPT_SIGNATURES
        .iter()
        .any(|sig| trimmed.starts_with(sig))
}

pub fn parse_jsonl_file(path: &Path, project_name: &str) -> Result<Vec<ConversationChunk>> {
    let conversation_id = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let file = fs::File::open(path).context("opening JSONL file")?;
    let reader = BufReader::new(file);
    let mut messages: Vec<String> = Vec::new();
    let mut authors: Vec<crate::provenance::Speaker> = Vec::new();
    let mut first_timestamp: Option<String> = None;
    let mut last_timestamp: Option<String> = None;
    let mut summary: Option<String> = None;
    let mut first_user_message: Option<String> = None;

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        // sonic-rs: serde-compatible drop-in, ~2.8x faster on aarch64
        let parsed: serde_json::Value = match sonic_rs::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Extract message type
        let msg_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");

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

        // Extract text content + tool context (both stripped of private tags)
        let text = extract_message_text(&parsed);
        let tool_context = strip_private_tags(&extract_tool_context(&parsed));
        let combined_text = if !text.is_empty() && !tool_context.is_empty() {
            format!("{}\n{}", text, tool_context)
        } else if !tool_context.is_empty() {
            tool_context
        } else {
            text
        };
        if !combined_text.is_empty() {
            messages.push(combined_text);
            authors.push(classify_message_author(&parsed));
        }
    }

    if messages.is_empty() {
        return Ok(Vec::new());
    }

    // Skip CSR's own agent-subprocess transcripts (the session-briefing analyst,
    // the compaction summarizer). Their FIRST user message IS the agent prompt, so
    // importing them pollutes the search index with CSR talking to itself — which
    // then feeds back into the next briefing. A normal user session never opens with
    // these strings; quoting them later in a session (e.g. via hook-injected context)
    // is fine because we only test the first user message.
    if let Some(ref fm) = first_user_message {
        if is_csr_agent_prompt(fm) {
            return Ok(Vec::new());
        }
    }

    // Use last_timestamp for ordering (shows "last active" not "started at")
    // Fall back to first_timestamp if somehow no last, then to now()
    let timestamp = last_timestamp
        .or(first_timestamp)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    // Priority: JSONL summary > first user message > None
    let chunk_summary = summary.or(first_user_message);

    // Chunk into groups of 50 messages
    let chunk_size = 50;
    let mut chunks = Vec::new();

    for (i, (chunk_msgs, chunk_authors)) in messages
        .chunks(chunk_size)
        .zip(authors.chunks(chunk_size))
        .enumerate()
    {
        let combined = chunk_msgs.join("\n\n");
        let chunk_id = generate_chunk_id(&conversation_id, i);

        chunks.push(ConversationChunk {
            id: chunk_id,
            conversation_id: conversation_id.clone(),
            project_name: project_name.to_string(),
            timestamp: timestamp.clone(),
            content: combined,
            message_count: chunk_msgs.len(),
            summary: chunk_summary.clone(),
            author: chunk_author(chunk_authors),
        });
    }

    Ok(chunks)
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

fn extract_message_text(msg: &serde_json::Value) -> String {
    let raw = extract_message_text_raw(msg);
    // Strip privacy-tagged content before storage/embedding
    strip_private_tags(&raw)
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

/// Generate a deterministic chunk ID using UUIDv5.
fn generate_chunk_id(conversation_id: &str, chunk_index: usize) -> String {
    let input = format!("{}-chunk-{}", conversation_id, chunk_index);
    Uuid::new_v5(&CSR_NAMESPACE, input.as_bytes()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
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
