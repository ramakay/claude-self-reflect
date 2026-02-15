pub mod watcher;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use uuid::Uuid;

/// A chunk of a conversation, ready for embedding and storage.
#[derive(Debug, Clone)]
pub struct ConversationChunk {
    pub id: String,
    pub conversation_id: String,
    pub project_name: String,
    pub timestamp: String,
    pub content: String,
    pub message_count: usize,
}

/// Namespace UUID for deterministic chunk IDs (UUIDv5).
const CSR_NAMESPACE: Uuid = Uuid::from_bytes([
    0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30,
    0xc8,
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
        parent
            .rsplit('/')
            .nth(1)
            .unwrap_or(parent)
            .to_string()
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
        if path.extension().map_or(false, |ext| ext == "jsonl") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Parse a JSONL conversation file into chunks of ~50 messages each.
/// Uses BufReader streaming + sonic-rs for ~2.8x faster parsing.
pub fn parse_jsonl_file(path: &Path, project_name: &str) -> Result<Vec<ConversationChunk>> {
    let conversation_id = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let file = fs::File::open(path).context("opening JSONL file")?;
    let reader = BufReader::new(file);
    let mut messages: Vec<String> = Vec::new();
    let mut first_timestamp: Option<String> = None;

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
        let msg_type = parsed
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Include human/user/assistant messages (Claude Code uses "user", not "human")
        if msg_type != "human" && msg_type != "user" && msg_type != "assistant" {
            continue;
        }

        // Capture first timestamp
        if first_timestamp.is_none() {
            if let Some(ts) = parsed.get("timestamp").and_then(|v| v.as_str()) {
                first_timestamp = Some(ts.to_string());
            }
        }

        // Extract text content + tool context
        let text = extract_message_text(&parsed);
        let tool_context = extract_tool_context(&parsed);
        let combined_text = if !text.is_empty() && !tool_context.is_empty() {
            format!("{}\n{}", text, tool_context)
        } else if !tool_context.is_empty() {
            tool_context
        } else {
            text
        };
        if !combined_text.is_empty() {
            messages.push(combined_text);
        }
    }

    if messages.is_empty() {
        return Ok(Vec::new());
    }

    let timestamp = first_timestamp.unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    // Chunk into groups of 50 messages
    let chunk_size = 50;
    let mut chunks = Vec::new();

    for (i, chunk_msgs) in messages.chunks(chunk_size).enumerate() {
        let combined = chunk_msgs.join("\n\n");
        let chunk_id = generate_chunk_id(&conversation_id, i);

        chunks.push(ConversationChunk {
            id: chunk_id,
            conversation_id: conversation_id.clone(),
            project_name: project_name.to_string(),
            timestamp: timestamp.clone(),
            content: combined,
            message_count: chunk_msgs.len(),
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

        let msg_type = parsed
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if msg_type == "human" || msg_type == "user" || msg_type == "assistant" {
            messages.push(parsed);
        }
    }

    Ok(messages)
}

/// Extract text content from a JSONL message entry.
fn extract_message_text(msg: &serde_json::Value) -> String {
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

        let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
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
}
