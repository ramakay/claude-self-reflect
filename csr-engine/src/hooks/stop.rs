//! Stop hook — imports transcript and maintains rolling session summary.
//!
//! For all sessions, imports the current transcript so content is searchable.
//! Also writes a rolling "session_latest" reflection so SessionStart has
//! a summary even if session-end (Haiku story generation) never fires
//! (e.g. Ctrl+C kills the session).
//!
//! v9.2: Extracts structured "episodes" — JSON objects capturing what happened
//! in a session (request, files investigated/modified, tools, outcome, errors).
//! Always returns Ok(()) — never blocks Claude Code.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::HookInput;
use crate::engine::Engine;
use crate::search::cross_project::resolve_project_from_cwd;

/// A single todo item captured from the last TodoWrite call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TodoItem {
    pub content: String,
    pub status: String,
}

/// A structured episode capturing what happened in a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub schema: String,
    pub session_id: String,
    pub project: String,
    pub timestamp: String,
    pub request: String,
    pub investigated: Vec<String>,
    pub completed: String,
    pub next_steps: Option<String>,
    pub blockers: Option<String>,
    pub outcome: String,
    pub error_signatures: Vec<String>,
    pub tools_used: Vec<String>,
    pub files_modified: Vec<String>,
    pub message_count: usize,
    pub duration_minutes: u32,

    // v2 working-state fields. `#[serde(default)]` keeps v1 episodes deserializable.
    #[serde(default)]
    pub todos: Vec<TodoItem>,
    #[serde(default)]
    pub approved_plan: Option<String>,
    #[serde(default)]
    pub prev_episode_id: Option<String>,
    #[serde(default)]
    pub anchors: Vec<crate::extraction::anchors::FunctionAnchor>,
}

/// Extract a structured episode from JSONL transcript lines.
///
/// Pure function — no I/O, no engine access. Parses each line as JSON and
/// extracts fields from the Claude Code transcript format.
pub fn extract_episode(lines: &[&str], session_id: &str, project: &str) -> Episode {
    let mut request = String::new();
    let mut investigated = HashSet::new();
    let mut files_modified = HashSet::new();
    let mut tools_used = HashSet::new();
    let mut completed = String::new();
    let mut error_signatures = Vec::new();
    let mut next_steps: Option<String> = None;
    let mut message_count: usize = 0;
    let mut todos: Vec<TodoItem> = Vec::new();
    let mut approved_plan: Option<String> = None;

    // Tool names whose file_path inputs count as "investigated"
    let read_tools: HashSet<&str> = ["Read", "Glob", "Grep"].into_iter().collect();
    // Tool names whose file_path inputs count as "modified"
    let write_tools: HashSet<&str> = ["Edit", "Write", "MultiEdit"].into_iter().collect();

    // Error signature patterns
    let error_patterns: &[&str] = &["error[", "Error:", "panic", "FAIL", "FAILED", "Exception"];

    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        let msg_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
        message_count += 1;

        // Extract request from first user message
        if (msg_type == "user" || msg_type == "human") && request.is_empty() {
            if let Some(content) = val.get("message").and_then(|m| m.get("content")) {
                let text = extract_text_from_content(content);
                if !text.is_empty() {
                    request = truncate_str(&text, 200).to_string();
                }
            }
        }

        // Extract tool_use information
        if msg_type == "assistant" {
            if let Some(content) = val.get("message").and_then(|m| m.get("content")) {
                if let Some(arr) = content.as_array() {
                    for block in arr {
                        let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");

                        if block_type == "tool_use" {
                            if let Some(name) = block.get("name").and_then(|v| v.as_str()) {
                                tools_used.insert(name.to_string());

                                if name == "TodoWrite" {
                                    if let Some(items) = block
                                        .get("input")
                                        .and_then(|i| i.get("todos"))
                                        .and_then(|t| t.as_array())
                                    {
                                        todos = items
                                            .iter()
                                            .filter_map(|t| {
                                                Some(TodoItem {
                                                    content: t
                                                        .get("content")?
                                                        .as_str()?
                                                        .to_string(),
                                                    status: t
                                                        .get("status")
                                                        .and_then(|s| s.as_str())
                                                        .unwrap_or("pending")
                                                        .to_string(),
                                                })
                                            })
                                            .take(10)
                                            .collect();
                                    }
                                }
                                if name == "ExitPlanMode" {
                                    if let Some(plan) = block
                                        .get("input")
                                        .and_then(|i| i.get("plan"))
                                        .and_then(|p| p.as_str())
                                    {
                                        approved_plan = Some(truncate_str(plan, 1500).to_string());
                                    }
                                }

                                if let Some(input) = block.get("input") {
                                    if let Some(fp) =
                                        input.get("file_path").and_then(|v| v.as_str())
                                    {
                                        if read_tools.contains(name) {
                                            investigated.insert(fp.to_string());
                                        }
                                        if write_tools.contains(name) {
                                            files_modified.insert(fp.to_string());
                                        }
                                    }
                                }
                            }
                        }

                        // Track last assistant text for `completed`
                        if block_type == "text" {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                let trimmed = text.trim();
                                if !trimmed.is_empty() {
                                    completed = truncate_str(trimmed, 300).to_string();
                                }
                            }
                        }
                    }
                }
                // Also handle plain string content
                if let Some(text) = content.as_str() {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        completed = truncate_str(trimmed, 300).to_string();
                    }
                }
            }
        }

        // Extract error signatures from tool_result blocks
        if msg_type == "tool_result" || msg_type == "user" {
            if let Some(content) = val.get("message").and_then(|m| m.get("content")) {
                let text = extract_text_from_content(content);
                for pattern in error_patterns {
                    if text.contains(pattern) {
                        // Extract a short context around the error
                        if let Some(pos) = text.find(pattern) {
                            let start = pos.saturating_sub(20);
                            let end = (pos + pattern.len() + 60).min(text.len());
                            // Safe UTF-8 boundary handling
                            let start = text.floor_char_boundary(start);
                            let end = text.floor_char_boundary(end);
                            let sig = text[start..end].trim().to_string();
                            if !error_signatures.contains(&sig) {
                                error_signatures.push(sig);
                            }
                        }
                    }
                }
            }
        }

        // Extract next_steps from any message
        if let Some(content) = val.get("message").and_then(|m| m.get("content")) {
            let text = extract_text_from_content(content);
            let lower = text.to_lowercase();
            for keyword in &["next step", "next:", "todo:", "remaining:"] {
                if let Some(pos) = lower.find(keyword) {
                    let start = pos;
                    let end = (pos + 200).min(text.len());
                    let end = text.floor_char_boundary(end);
                    let snippet = text[start..end].trim().to_string();
                    if !snippet.is_empty() {
                        next_steps = Some(snippet);
                    }
                }
            }
        }
    }

    // Determine outcome
    let outcome = if message_count < 3 {
        "interrupted".to_string()
    } else if !error_signatures.is_empty() {
        "failed".to_string()
    } else {
        let lower = completed.to_lowercase();
        if lower.contains("complete")
            || lower.contains("fixed")
            || lower.contains("done")
            || lower.contains("success")
        {
            "success".to_string()
        } else {
            "partial".to_string()
        }
    };

    let mut investigated_vec: Vec<String> = investigated.into_iter().collect();
    investigated_vec.sort();
    let mut files_modified_vec: Vec<String> = files_modified.into_iter().collect();
    files_modified_vec.sort();
    let mut tools_used_vec: Vec<String> = tools_used.into_iter().collect();
    tools_used_vec.sort();

    Episode {
        schema: "v2".to_string(),
        session_id: session_id.to_string(),
        project: project.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        request,
        investigated: investigated_vec,
        completed,
        next_steps,
        blockers: None,
        outcome,
        error_signatures,
        tools_used: tools_used_vec,
        files_modified: files_modified_vec,
        message_count,
        duration_minutes: 0, // Cannot reliably determine from transcript alone
        todos,
        approved_plan,
        prev_episode_id: None,
        anchors: Vec::new(),
    }
}

/// Choose the most recent episode session that is not the current one.
pub fn pick_prev_episode(candidates: &[(String, String)], current_session: &str) -> Option<String> {
    candidates
        .iter()
        .filter(|(sid, _)| sid != current_session)
        .max_by(|a, b| a.1.cmp(&b.1))
        .map(|(sid, _)| sid.clone())
}

/// Generate tags for an episode reflection.
pub fn episode_tags(episode: &Episode) -> Vec<String> {
    vec![
        "session_episode".to_string(),
        "schema_v2".to_string(),
        format!("project_{}", episode.project),
        format!("conv_{}", episode.session_id),
    ]
}

/// Store an episode as a reflection, replacing any existing episode for the same session.
pub async fn store_episode(engine: &Engine, episode: &Episode) -> Result<()> {
    let tags = episode_tags(episode);
    let conv_tag = format!("conv_{}", episode.session_id);

    // Find and delete existing episodes for this session
    let existing = engine
        .storage()
        .get_reflections_by_tag(&conv_tag, 10)
        .unwrap_or_default();
    for (id, _content, existing_tags, _ts) in &existing {
        if existing_tags.iter().any(|t| t == "session_episode") {
            let _ = engine.storage().delete_reflection(id);
        }
    }

    // Serialize episode to JSON
    let content = serde_json::to_string(episode)?;

    // Embed the episode content
    let emb = engine.embeddings().clone();
    let text_for_embed = content.clone();
    let embedding =
        tokio::task::spawn_blocking(move || emb.embed_single(&text_for_embed)).await??;

    // Generate a new ID and insert
    let id = uuid::Uuid::new_v4().to_string();
    engine
        .storage()
        .insert_reflection(&id, &content, &tags, &embedding)?;

    // Also insert into the in-memory search index
    {
        let mut idx = engine.search().write().await;
        idx.insert_reflection(id, embedding);
    }

    Ok(())
}

/// Read transcript, extract episode, and store it. Non-fatal wrapper.
pub async fn extract_and_store_episode(
    input: &HookInput,
    engine: &Engine,
    cwd: &Path,
) -> Result<()> {
    let session_id = input
        .session_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("no session_id"))?;
    let transcript_path = input
        .transcript_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("no transcript_path"))?;

    let tp = std::path::PathBuf::from(transcript_path);
    if !tp.exists() {
        anyhow::bail!("transcript not found: {}", transcript_path);
    }

    let project = resolve_project_from_cwd(&cwd.to_string_lossy());
    let project_name = project.as_deref().unwrap_or("unknown");

    // Read transcript lines
    let raw = std::fs::read_to_string(&tp)?;
    let lines: Vec<&str> = raw.lines().collect();

    let mut episode = extract_episode(&lines, session_id, project_name);

    // AST anchors for modified files (cap 10 files; relative paths resolve to cwd)
    for f in episode.files_modified.iter().take(10) {
        let p = std::path::Path::new(f);
        let path = if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        };
        episode
            .anchors
            .extend(crate::extraction::anchors::capture_file_anchors(&path));
    }

    // Chain link: most recent episode for this project, excluding this session
    let project_tag = format!("project_{}", project_name);
    if let Ok(existing) = engine.storage().get_reflections_by_tag(&project_tag, 50) {
        let candidates: Vec<(String, String)> = existing
            .iter()
            .filter(|(_, _, tags, _)| tags.iter().any(|t| t == "session_episode"))
            .filter_map(|(_, content, _, ts)| {
                let v: serde_json::Value = serde_json::from_str(content).ok()?;
                Some((v.get("session_id")?.as_str()?.to_string(), ts.clone()))
            })
            .collect();
        episode.prev_episode_id = pick_prev_episode(&candidates, session_id);
    }

    store_episode(engine, &episode).await?;

    // Persist anchors for fast birth-time symbol join (non-fatal)
    if let Err(e) =
        engine
            .storage()
            .replace_session_anchors(session_id, project_name, &episode.anchors)
    {
        eprintln!("CSR: anchor persist error (non-fatal): {}", e);
    }

    eprintln!(
        "CSR: episode stored (outcome={}, msgs={}, tools={}, anchors={})",
        episode.outcome,
        episode.message_count,
        episode.tools_used.len(),
        episode.anchors.len()
    );

    Ok(())
}

/// Handle the stop hook.
/// Always returns Ok(()) to never block Claude Code (C-1 fix).
pub async fn handle(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    // Import growing transcript for ALL sessions (real-time searchability)
    super::import_current_transcript(input, engine, cwd).await;

    // Write rolling session summary — survives Ctrl+C (session-end may not fire)
    if let Err(e) = write_rolling_summary(input, engine, cwd) {
        eprintln!("CSR: rolling summary error (non-fatal): {}", e);
    }

    // Extract and store structured episode (non-fatal)
    if let Err(e) = extract_and_store_episode(input, engine, cwd).await {
        eprintln!("CSR: episode extraction error (non-fatal): {}", e);
    }

    Ok(())
}

/// Write a rolling "session_latest" reflection with current session state.
/// This is overwritten on every stop event, ensuring SessionStart always has
/// *something* to show even if the Haiku story generation never runs.
fn write_rolling_summary(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    if input.session_id.is_none() {
        return Ok(()); // No session ID → nothing to summarize
    }

    let project = resolve_project_from_cwd(&cwd.to_string_lossy());
    let project_name = project.as_deref().unwrap_or("unknown");

    // Build a minimal rolling summary from the latest enrichment
    let sessions = engine
        .storage()
        .get_recent_sessions(1, Some(project_name))
        .unwrap_or_default();

    let summary = if let Some(session) = sessions.first() {
        let title = session.summary.as_deref().unwrap_or("(active session)");
        let msg_count = session.total_messages;
        let enrichment_hint = session
            .enrichment
            .as_deref()
            .and_then(|e| {
                // Extract tools/files from heuristic enrichment
                let tools_line = e.lines().find(|l| l.starts_with("Tools: "));
                let files_line = e.lines().find(|l| l.starts_with("Files: "));
                match (tools_line, files_line) {
                    (Some(t), Some(f)) => Some(format!("{}\n{}", t, f)),
                    (Some(t), None) => Some(t.to_string()),
                    (None, Some(f)) => Some(f.to_string()),
                    _ => None,
                }
            })
            .unwrap_or_default();

        format!(
            "[Rolling] {} ({} msgs)\n{}",
            title, msg_count, enrichment_hint
        )
    } else {
        return Ok(()); // No sessions to summarize
    };

    // Write rolling summary to a well-known file that SessionStart can read.
    // Simpler than the reflection system — no embeddings needed, just text.
    if let Some(home) = dirs::home_dir() {
        let dir = home.join(".claude-self-reflect");
        // S-1 fix: sanitize project_name to prevent directory traversal
        let safe_name: String = project_name
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .take(255)
            .collect();
        let path = dir.join(format!("rolling-summary-{}.txt", safe_name));
        let _ = std::fs::write(&path, &summary);
    }

    Ok(())
}

/// Extract text content from a Claude message content field.
/// Handles both string content and array-of-blocks format.
fn extract_text_from_content(content: &serde_json::Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        let mut parts = Vec::new();
        for block in arr {
            if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                }
            }
            // Also extract tool_result content
            if block.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                if let Some(text) = block.get("content").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                }
            }
        }
        return parts.join("\n");
    }
    String::new()
}

/// Truncate a string to at most `max_chars` characters at a valid UTF-8 boundary.
fn truncate_str(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    let boundary = s.floor_char_boundary(max_chars);
    &s[..boundary]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a JSONL line for a user message.
    fn user_line(text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {
                "content": [{"type": "text", "text": text}]
            }
        })
        .to_string()
    }

    /// Build a JSONL line for an assistant message with tool_use blocks.
    fn assistant_tool_line(tools: &[(&str, &str)]) -> String {
        let mut blocks: Vec<serde_json::Value> = Vec::new();
        for (name, file_path) in tools {
            blocks.push(serde_json::json!({
                "type": "tool_use",
                "name": name,
                "input": {"file_path": file_path}
            }));
        }
        serde_json::json!({
            "type": "assistant",
            "message": {
                "content": blocks
            }
        })
        .to_string()
    }

    /// Build a JSONL line for an assistant message with text.
    fn assistant_text_line(text: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{"type": "text", "text": text}]
            }
        })
        .to_string()
    }

    /// Build a JSONL line for a tool_result with error content.
    fn tool_result_line(content: &str) -> String {
        serde_json::json!({
            "type": "tool_result",
            "message": {
                "content": [{"type": "tool_result", "content": content}]
            }
        })
        .to_string()
    }

    #[test]
    fn test_extract_episode_from_transcript() {
        let lines_owned = [
            user_line("Please fix the authentication bug in the login handler"),
            assistant_tool_line(&[("Read", "/src/auth/login.rs")]),
            assistant_tool_line(&[("Grep", "/src/auth/mod.rs")]),
            assistant_tool_line(&[("Edit", "/src/auth/login.rs")]),
            assistant_text_line(
                "I've fixed the authentication bug. The issue was done and complete.",
            ),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();

        let ep = extract_episode(&lines, "sess-123", "my-project");

        assert_eq!(ep.schema, "v2");
        assert_eq!(ep.session_id, "sess-123");
        assert_eq!(ep.project, "my-project");
        assert!(ep.request.contains("fix the authentication bug"));
        assert!(ep.investigated.contains(&"/src/auth/login.rs".to_string()));
        assert!(ep.investigated.contains(&"/src/auth/mod.rs".to_string()));
        assert!(ep
            .files_modified
            .contains(&"/src/auth/login.rs".to_string()));
        assert!(ep.tools_used.contains(&"Read".to_string()));
        assert!(ep.tools_used.contains(&"Grep".to_string()));
        assert!(ep.tools_used.contains(&"Edit".to_string()));
        assert!(ep.completed.contains("fixed the authentication bug"));
        assert_eq!(ep.outcome, "success"); // "fixed" and "complete" in completed text
        assert_eq!(ep.message_count, 5);
        assert!(ep.error_signatures.is_empty());
    }

    #[test]
    fn test_extract_episode_short_session() {
        let lines_owned = [user_line("Hello")];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();

        let ep = extract_episode(&lines, "sess-short", "test-proj");

        assert_eq!(ep.outcome, "interrupted");
        assert_eq!(ep.message_count, 1);
        assert_eq!(ep.request, "Hello");
    }

    #[test]
    fn test_extract_episode_with_errors() {
        let lines_owned = [
            user_line("Build the project"),
            assistant_text_line("Let me try building..."),
            assistant_tool_line(&[("Read", "/Cargo.toml")]),
            tool_result_line("error[E0308]: mismatched types\n  --> src/main.rs:42"),
            assistant_text_line("There was a compilation error."),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();

        let ep = extract_episode(&lines, "sess-err", "my-project");

        assert_eq!(ep.outcome, "failed");
        assert!(!ep.error_signatures.is_empty());
        assert!(ep
            .error_signatures
            .iter()
            .any(|s| s.contains("error[E0308]")));
    }

    #[test]
    fn test_episode_to_json_roundtrip() {
        let ep = Episode {
            schema: "v1".to_string(),
            session_id: "sess-rt".to_string(),
            project: "roundtrip-proj".to_string(),
            timestamp: "2026-05-17T00:00:00+00:00".to_string(),
            request: "Fix the bug".to_string(),
            investigated: vec!["/src/main.rs".to_string()],
            completed: "Bug is fixed and done.".to_string(),
            next_steps: Some("Deploy to production".to_string()),
            blockers: None,
            outcome: "success".to_string(),
            error_signatures: vec![],
            tools_used: vec!["Read".to_string(), "Edit".to_string()],
            files_modified: vec!["/src/main.rs".to_string()],
            message_count: 10,
            duration_minutes: 5,
            todos: vec![],
            approved_plan: None,
            prev_episode_id: None,
            anchors: vec![],
        };

        let json = serde_json::to_string(&ep).unwrap();
        let deserialized: Episode = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.schema, "v1");
        assert_eq!(deserialized.session_id, "sess-rt");
        assert_eq!(deserialized.project, "roundtrip-proj");
        assert_eq!(deserialized.request, "Fix the bug");
        assert_eq!(deserialized.investigated, vec!["/src/main.rs"]);
        assert_eq!(deserialized.completed, "Bug is fixed and done.");
        assert_eq!(
            deserialized.next_steps,
            Some("Deploy to production".to_string())
        );
        assert_eq!(deserialized.blockers, None);
        assert_eq!(deserialized.outcome, "success");
        assert!(deserialized.error_signatures.is_empty());
        assert_eq!(deserialized.tools_used, vec!["Read", "Edit"]);
        assert_eq!(deserialized.files_modified, vec!["/src/main.rs"]);
        assert_eq!(deserialized.message_count, 10);
        assert_eq!(deserialized.duration_minutes, 5);
    }

    #[test]
    fn test_episode_tags() {
        let ep = Episode {
            schema: "v1".to_string(),
            session_id: "sess-abc".to_string(),
            project: "cool-project".to_string(),
            timestamp: String::new(),
            request: String::new(),
            investigated: vec![],
            completed: String::new(),
            next_steps: None,
            blockers: None,
            outcome: "partial".to_string(),
            error_signatures: vec![],
            tools_used: vec![],
            files_modified: vec![],
            message_count: 0,
            duration_minutes: 0,
            todos: vec![],
            approved_plan: None,
            prev_episode_id: None,
            anchors: vec![],
        };

        let tags = episode_tags(&ep);

        assert_eq!(tags.len(), 4);
        assert!(tags.contains(&"session_episode".to_string()));
        assert!(tags.contains(&"schema_v2".to_string()));
        assert!(tags.contains(&"project_cool-project".to_string()));
        assert!(tags.contains(&"conv_sess-abc".to_string()));
    }

    #[test]
    fn episode_v2_extracts_todos_and_plan() {
        let lines = vec![
            r#"{"type":"user","message":{"content":"fix the auth bug"}}"#,
            r###"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"ExitPlanMode","input":{"plan":"## Plan\n1. Fix validate_token\n2. Add test"}}]}}"###,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"fix validate_token","status":"completed"},{"content":"add regression test","status":"pending"}]}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Fixed the validation."}]}}"#,
        ];
        let ep = extract_episode(&lines, "sess-1", "proj");
        assert_eq!(ep.schema, "v2");
        assert_eq!(ep.todos.len(), 2);
        assert_eq!(ep.todos[1].content, "add regression test");
        assert_eq!(ep.todos[1].status, "pending");
        let plan = ep.approved_plan.expect("plan captured");
        assert!(plan.contains("Fix validate_token"));
        assert!(ep.prev_episode_id.is_none());
        assert!(ep.anchors.is_empty());
    }

    #[test]
    fn episode_v2_keeps_last_todowrite_only() {
        let lines = vec![
            r#"{"type":"user","message":{"content":"task"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"old","status":"pending"}]}}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"new","status":"in_progress"}]}}]}}"#,
        ];
        let ep = extract_episode(&lines, "sess-1", "proj");
        assert_eq!(ep.todos.len(), 1);
        assert_eq!(ep.todos[0].content, "new");
    }

    #[test]
    fn chain_link_finds_previous_episode_id() {
        // Pure-logic test for the helper that picks prev episode from candidates:
        // (session_id, timestamp) pairs, excluding the current session.
        let candidates = vec![
            ("sess-old".to_string(), "2026-06-09T10:00:00Z".to_string()),
            (
                "sess-current".to_string(),
                "2026-06-10T09:00:00Z".to_string(),
            ),
            ("sess-mid".to_string(), "2026-06-10T08:00:00Z".to_string()),
        ];
        assert_eq!(
            pick_prev_episode(&candidates, "sess-current"),
            Some("sess-mid".to_string())
        );
        assert_eq!(pick_prev_episode(&[], "x"), None);
    }

    #[test]
    fn episode_v1_json_still_deserializes() {
        let v1 = r#"{"schema":"v1","session_id":"s","project":"p","timestamp":"t",
            "request":"r","investigated":[],"completed":"c","next_steps":null,
            "blockers":null,"outcome":"partial","error_signatures":[],"tools_used":[],
            "files_modified":[],"message_count":1,"duration_minutes":0}"#;
        let ep: Episode = serde_json::from_str(v1).expect("v1 compat");
        assert!(ep.todos.is_empty());
        assert!(ep.approved_plan.is_none());
    }
}
