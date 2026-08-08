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

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::HookInput;
use crate::engine::Engine;
use crate::search::cross_project::resolve_project_from_cwd;

/// Matches Claude Code TaskCreate tool_result text: "Task #N created successfully".
static TASK_CREATED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Task #(\d+) created successfully").unwrap());

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
    let messages: Vec<serde_json::Value> = lines
        .iter()
        .filter_map(|line| serde_json::from_str(line.trim()).ok())
        .collect();
    let (messages, _) = crate::import::sanitize_messages_for_search(&messages);
    extract_episode_from_messages(&messages, session_id, project)
}

fn extract_episode_from_messages(
    messages: &[serde_json::Value],
    session_id: &str,
    project: &str,
) -> Episode {
    let mut request = String::new();
    let mut investigated = HashSet::new();
    let mut files_modified = HashSet::new();
    let mut tools_used = HashSet::new();
    let mut completed = String::new();
    let mut error_signatures = Vec::new();
    let mut message_count: usize = 0;
    let mut todos: Vec<TodoItem> = Vec::new();
    let mut approved_plan: Option<String> = None;
    // TaskCreate/TaskUpdate tracking: numeric task id → index into `todos`.
    // Ids come only from tool_result text ("Task #N created successfully"),
    // never from transcript ordinal position (pre-existing/deleted tasks break that).
    let mut task_id_map: HashMap<String, usize> = HashMap::new();
    // TaskCreate blocks awaiting their tool_result binding: (tool_use_id, todos_index).
    // tool_use_id may be empty if the block had no "id" field.
    let mut pending_task_ids: Vec<(String, usize)> = Vec::new();

    // Tool names whose file_path inputs count as "investigated"
    let read_tools: HashSet<&str> = ["Read", "Glob", "Grep"].into_iter().collect();
    // Tool names whose file_path inputs count as "modified"
    let write_tools: HashSet<&str> = ["Edit", "Write", "MultiEdit"].into_iter().collect();

    // Error signature patterns
    let error_patterns: &[&str] = &["error[", "Error:", "panic", "FAIL", "FAILED", "Exception"];

    for val in messages {
        let msg_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
        message_count += 1;

        // Extract request from the first user message that survives provenance
        // filtering — command wrappers, caveats, and pasted CSR output are
        // plumbing, not the user's request (see extraction::provenance).
        if (msg_type == "user" || msg_type == "human") && request.is_empty() {
            if let Some(content) = val.get("message").and_then(|m| m.get("content")) {
                let text = extract_text_from_content(content);
                if let Some(cleaned) = crate::extraction::provenance::extractable(&text) {
                    request = truncate_str(&cleaned, 200).to_string();
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
                                if crate::import::is_csr_tool_use(block, name) {
                                    continue;
                                }
                                tools_used.insert(name.to_string());

                                if name == "TodoWrite" {
                                    if let Some(items) = block
                                        .get("input")
                                        .and_then(|i| i.get("todos"))
                                        .and_then(|t| t.as_array())
                                    {
                                        // Full-list rewrite — replaces any prior
                                        // TaskCreate-derived state and invalidates
                                        // id bindings (indices no longer apply).
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
                                        task_id_map.clear();
                                        pending_task_ids.clear();
                                    }
                                }
                                if name == "TaskCreate" {
                                    // Shared cap with TodoWrite — silently drop overflow.
                                    if todos.len() < 10 {
                                        if let Some(subject) = block
                                            .get("input")
                                            .and_then(|i| i.get("subject"))
                                            .and_then(|s| s.as_str())
                                        {
                                            let index = todos.len();
                                            todos.push(TodoItem {
                                                content: subject.to_string(),
                                                status: "pending".to_string(),
                                            });
                                            let tool_use_id = block
                                                .get("id")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or_default()
                                                .to_string();
                                            pending_task_ids.push((tool_use_id, index));
                                        }
                                    }
                                }
                                if name == "TaskUpdate" {
                                    // TODO: unknown task ids should bump an aux
                                    // counter in the Stop handler; extract_episode
                                    // stays pure and cannot report this out.
                                    if let Some(input) = block.get("input") {
                                        let task_id = input.get("taskId").and_then(|v| {
                                            if let Some(s) = v.as_str() {
                                                Some(s.to_string())
                                            } else if let Some(n) = v.as_i64() {
                                                Some(n.to_string())
                                            } else {
                                                v.as_u64().map(|n| n.to_string())
                                            }
                                        });
                                        if let Some(id) = task_id {
                                            if let Some(&idx) = task_id_map.get(&id) {
                                                if let Some(status) =
                                                    input.get("status").and_then(|s| s.as_str())
                                                {
                                                    if let Some(item) = todos.get_mut(idx) {
                                                        item.status = status.to_string();
                                                    }
                                                }
                                                if let Some(subject) =
                                                    input.get("subject").and_then(|s| s.as_str())
                                                {
                                                    if let Some(item) = todos.get_mut(idx) {
                                                        item.content = subject.to_string();
                                                    }
                                                }
                                            }
                                        }
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

                        // Track last assistant text for `completed`. Provenance
                        // filtering strips quoted/mentioned text first, so an
                        // assistant message quoting CSR's own injected blocks
                        // contributes only its genuine prose (or nothing).
                        if block_type == "text" {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                if let Some(cleaned) =
                                    crate::extraction::provenance::extractable(text)
                                {
                                    completed = truncate_str(&cleaned, 300).to_string();
                                }
                            }
                        }
                    }
                }
                // Also handle plain string content
                if let Some(text) = content.as_str() {
                    if let Some(cleaned) = crate::extraction::provenance::extractable(text) {
                        completed = truncate_str(&cleaned, 300).to_string();
                    }
                }
            }
        }

        // Extract error signatures from tool_result blocks; also bind
        // TaskCreate numeric ids from per-block tool_result text.
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

                // Per-block TaskCreate id binding — needs tool_use_id, which the
                // merged text path above discards. Only the tool_result content
                // matching "Task #N created successfully" is relevant here.
                if let Some(arr) = content.as_array() {
                    for block in arr {
                        if block.get("type").and_then(|v| v.as_str()) != Some("tool_result") {
                            continue;
                        }
                        let block_text = block
                            .get("content")
                            .map(extract_text_from_content)
                            .unwrap_or_default();
                        let Some(caps) = TASK_CREATED_RE.captures(&block_text) else {
                            continue;
                        };
                        let Some(numeric_id) = caps.get(1).map(|m| m.as_str().to_string()) else {
                            continue;
                        };
                        let tool_use_id = block.get("tool_use_id").and_then(|v| v.as_str());
                        let todos_index = if let Some(tuid) = tool_use_id {
                            if let Some(pos) =
                                pending_task_ids.iter().position(|(id, _)| id == tuid)
                            {
                                let (_, index) = pending_task_ids.remove(pos);
                                Some(index)
                            } else {
                                // Explicit tool_use_id that matches nothing means
                                // this result belongs to a capped, malformed, or
                                // unrelated create — binding it anywhere would
                                // corrupt later TaskUpdates (Codex). Leave unbound.
                                None
                            }
                        } else if pending_task_ids.is_empty() {
                            None
                        } else {
                            // No tool_use_id on the block at all: creations and
                            // results arrive in order, so FIFO keeps unlabeled
                            // creates aligned (CodeRabbit — LIFO cross-bound them).
                            Some(pending_task_ids.remove(0).1)
                        };
                        if let Some(index) = todos_index {
                            task_id_map.insert(numeric_id, index);
                        }
                    }
                }
            }
        }
    }

    // Deleted tasks are not open work: the dir loader skips them, so the
    // transcript path must too, or an all-deleted session gets capped at
    // "partial" on one path and not the other (CodeRabbit). Filtered after the
    // event loop because task_id_map holds indices into `todos` during it.
    todos.retain(|t| t.status != "deleted");

    // next_steps comes ONLY from structured task/todo state (first non-completed
    // item from TodoWrite or TaskCreate/TaskUpdate). Keyword-snippet harvesting
    // of free prose was the recurring self-pollution channel: in sessions about
    // CSR itself, prose legitimately mentions "next:"/"todo:" while describing
    // the extractor, and no content filter can tell that apart from a real next
    // step. Structured todos are authored as tasks, so they can't be mentions.
    let next_steps = todos
        .iter()
        .find(|t| t.status != "completed")
        .map(|t| t.content.clone());

    // Determine outcome (task-aware: incomplete todos cap success at partial)
    let success_signal = crate::extraction::has_success_signal(&completed);
    let outcome = compute_outcome(message_count, &error_signatures, success_signal, &todos);

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

/// Infer session outcome from message volume, errors, closing success signal,
/// and structured task completion state.
///
/// Incomplete todos cap the result at `"partial"` even when prose signals
/// success — unresolved task state means the session is not a clean win.
fn compute_outcome(
    message_count: usize,
    error_signatures: &[String],
    success_signal: bool,
    todos: &[TodoItem],
) -> String {
    let outcome = if message_count < 3 {
        "interrupted".to_string()
    } else if !error_signatures.is_empty() {
        // Errors occurred, but a closing success signal means the session
        // recovered — "partial", not "failed". A dev session that hits and
        // fixes test failures should not be remembered as a failure.
        if success_signal {
            "partial".to_string()
        } else {
            "failed".to_string()
        }
    } else if success_signal {
        "success".to_string()
    } else {
        "partial".to_string()
    };

    // Incomplete tasks cannot be a clean success even if closing prose says so.
    if outcome == "success" && todos.iter().any(|t| t.status != "completed") {
        "partial".to_string()
    } else {
        outcome
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

    // Authoritative on-disk task directory overrides transcript-mined todos
    // when present and non-empty (Claude Code's ~/.claude/tasks/<session_id>/).
    // Empty vec is "no signal" — leave transcript-derived state alone.
    let task_dir_state = load_task_dir_state(session_id);
    if task_dir_state.is_none() {
        // None with the dir actually present = unreadable/format churn, the
        // silent-rot channel this release exists to close. Missing dir is the
        // normal no-tasks case and does not count.
        let dir_exists = dirs::home_dir()
            .map(|h| h.join(".claude/tasks").join(session_id).is_dir())
            .unwrap_or(false);
        if dir_exists {
            let _ = engine.storage().bump_aux_counter("tasks");
        }
    }
    if let Some(state) = task_dir_state {
        if state.parse_failures > 0 {
            // Files existed but didn't parse — the schema-drift signal (Codex:
            // all-failures used to read as "no tasks" and stay invisible).
            let _ = engine.storage().bump_aux_counter("tasks");
        }
        // Authoritative whenever the dir actually held task files — INCLUDING an
        // all-deleted session, which must override stale transcript tasks rather
        // than let them set next_steps and cap the outcome (Codex). A dir with
        // zero task files carries no signal; transcript state stands.
        if state.files_seen > state.parse_failures {
            episode.todos = state.todos;
            episode.next_steps = episode
                .todos
                .iter()
                .find(|t| t.status != "completed")
                .map(|t| t.content.clone());
            let success_signal = crate::extraction::has_success_signal(&episode.completed);
            episode.outcome = compute_outcome(
                episode.message_count,
                &episode.error_signatures,
                success_signal,
                &episode.todos,
            );
        }
    }

    // Task-derived resolution proposals: a completed task whose subject matches a
    // chunk carrying a still-open verdict suggests that item is now resolved.
    // Proposals only — never verdicts (see Storage::insert_resolution_proposal).
    // Kill switch mirrors the narrative one. Fail-soft: proposal errors never
    // block the Stop hook.
    if std::env::var("CSR_NO_TASK_RESOLUTION").is_err() {
        propose_task_resolutions(engine.storage(), &episode.todos, project_name, session_id);
    }

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

    // Chain link: most recent episode for this project, excluding this session.
    // Exact project-tag match avoids LIKE '%project_foo%' matching 'project_foobar'.
    let project_tag = format!("project_{}", project_name);
    if let Ok(existing) = engine.storage().get_reflections_by_tag(&project_tag, 50) {
        let candidates: Vec<(String, String)> = existing
            .iter()
            .filter(|(_, _, tags, _)| {
                tags.iter().any(|t| t == "session_episode")
                    && tags.iter().any(|t| t == &project_tag)
            })
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

/// For each completed task, look for a chunk in this project carrying a
/// still-open resolution verdict whose content matches the task subject; write
/// a proposal row for the top match. Bounded here to 10 completed tasks — the
/// transcript path caps todos at 10, but dir-loaded task state is uncapped
/// (CodeRabbit), and this fn must not fan out FTS probes with it.
fn propose_task_resolutions(
    storage: &crate::storage::Storage,
    todos: &[TodoItem],
    project_name: &str,
    session_id: &str,
) {
    let completed: Vec<&TodoItem> = todos
        .iter()
        .filter(|t| t.status == "completed")
        .take(10)
        .collect();
    if completed.is_empty() {
        return;
    }
    for todo in completed {
        let Ok(hits) = storage.fts5_search(&todo.content, 3, Some(project_name)) else {
            continue;
        };
        // fts5_search OR-joins terms, so a hit can rank on one shared word.
        // Require most of the subject's significant tokens verbatim in the hit
        // before proposing — identity, not similarity (Codex: single-word
        // matches attached completed tasks to unrelated still-open chunks).
        let subject_tokens: Vec<String> = todo
            .content
            .split(|c: char| !c.is_alphanumeric())
            .map(|w| w.to_lowercase())
            .filter(|w| w.len() > 3)
            .collect();
        if subject_tokens.is_empty() {
            continue;
        }
        let Some(hit) = hits.iter().find(|h| {
            let content_tokens: std::collections::HashSet<String> = h
                .content
                .split(|c: char| !c.is_alphanumeric())
                .map(|w| w.to_lowercase())
                .collect();
            let matched = subject_tokens
                .iter()
                .filter(|t| content_tokens.contains(*t))
                .count();
            matched * 10 >= subject_tokens.len() * 6 // >= 60% overlap
        }) else {
            continue;
        };
        let ids = vec![hit.id.clone()];
        let Ok(resolutions) = storage.get_resolutions_batch(&ids) else {
            continue;
        };
        let Some(entry) = resolutions.get(&hit.id) else {
            continue; // no verdict recorded — nothing to propose against
        };
        if entry.status != "still_open" {
            continue;
        }
        let evidence = format!("task completed in session {session_id}: {}", todo.content);
        let _ =
            storage.insert_resolution_proposal(&hit.id, Some(&todo.content), &evidence, session_id);
    }
}

/// Authoritative final task state from `~/.claude/tasks/<session_id>/`.
/// None on any error (missing dir, unreadable) — fail-soft so the Stop hook
/// never errors or panics because of task-dir I/O.
fn load_task_dir_state(session_id: &str) -> Option<TaskDirState> {
    let dir = dirs::home_dir()?.join(".claude/tasks").join(session_id);
    load_task_state_from_dir(&dir)
}

/// What a task-directory read found. `files_seen` counts task JSON files present
/// (including deleted ones); `parse_failures` counts files that existed but
/// could not be read/parsed — the schema-drift signal the aux counter needs
/// (Codex: `Some([])` from all-failures used to be indistinguishable from
/// "no tasks", leaving drift invisible).
struct TaskDirState {
    todos: Vec<TodoItem>,
    files_seen: usize,
    parse_failures: usize,
}

/// Read numeric `N.json` task files from a directory. Testable helper used by
/// `load_task_dir_state`; returns `None` only when the directory itself cannot
/// be read. Per-file parse failures are skipped (caller with Storage/Engine
/// could `bump_aux_counter("tasks")` — this fn stays Storage-free).
fn load_task_state_from_dir(dir: &Path) -> Option<TaskDirState> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut files: Vec<(u32, std::path::PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "json" {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(n) = stem.parse::<u32>() else {
            continue;
        };
        files.push((n, path));
    }
    files.sort_by_key(|(n, _)| *n);

    let files_seen = files.len();
    let mut parse_failures = 0usize;
    let mut todos = Vec::new();
    for (_, path) in files {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            parse_failures += 1;
            continue;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
            parse_failures += 1;
            continue;
        };
        let Some(subject) = val.get("subject").and_then(|s| s.as_str()) else {
            parse_failures += 1;
            continue;
        };
        let Some(status) = val.get("status").and_then(|s| s.as_str()) else {
            parse_failures += 1;
            continue;
        };
        if status == "deleted" {
            continue;
        }
        todos.push(TodoItem {
            content: subject.to_string(),
            status: status.to_string(),
        });
    }
    Some(TaskDirState {
        todos,
        files_seen,
        parse_failures,
    })
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
        // Don't roll up a meta/probe or bare-question session ("what were we
        // discussing recently?") — it would surface as a self-referential
        // RECENT SESSIONS line. Same gate the live continuity paths use.
        if !crate::extraction::provenance::is_substantive(title) || title.contains('?') {
            return Ok(());
        }
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
pub(crate) fn extract_text_from_content(content: &serde_json::Value) -> String {
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

    #[test]
    fn propose_task_resolutions_only_for_still_open_matches() {
        use crate::import::ConversationChunk;
        use crate::provenance::Speaker;
        let storage = crate::storage::Storage::open_memory().unwrap();
        let mk = |id: &str, content: &str, status: Option<&str>| {
            let chunk = ConversationChunk {
                id: id.into(),
                conversation_id: format!("conv-{id}"),
                project_name: "proj".into(),
                timestamp: "2026-07-27T12:00:00Z".into(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &[0.0; 4]).unwrap();
            if let Some(s) = status {
                storage
                    .insert_resolutions(&[id.to_string()], s, "seed", None, "agent")
                    .unwrap();
            }
        };
        mk(
            "open",
            "fix registry offset checkpoint races",
            Some("still_open"),
        );
        mk(
            "done",
            "narrative cache invalidation stale",
            Some("resolved"),
        );
        mk("naked", "unrelated quantum topic", None);

        let todos = vec![
            TodoItem {
                content: "fix registry offset checkpoint races".into(),
                status: "completed".into(),
            },
            TodoItem {
                content: "narrative cache invalidation stale".into(),
                status: "completed".into(),
            },
            TodoItem {
                content: "unrelated quantum topic".into(),
                status: "pending".into(), // not completed — never proposes
            },
        ];
        propose_task_resolutions(&storage, &todos, "proj", "sess-1");
        // Only the still_open match yields a proposal; resolved + pending don't.
        assert_eq!(storage.count_resolution_proposals().unwrap(), 1);
        // Idempotent per (chunk, session).
        propose_task_resolutions(&storage, &todos, "proj", "sess-1");
        assert_eq!(storage.count_resolution_proposals().unwrap(), 1);
    }

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
    fn episode_excludes_correlated_csr_pair_but_keeps_sibling_tool_and_user_error() {
        let lines_owned = [
            serde_json::json!({
                "type": "assistant",
                "message": {"content": [
                    {"type": "tool_use", "id": "csr-1", "name": "csr_reflect_on_past", "input": {"query": "history"}},
                    {"type": "tool_use", "id": "read-1", "name": "Read", "input": {"file_path": "/src/lib.rs"}}
                ]}
            })
            .to_string(),
            serde_json::json!({
                "type": "user",
                "message": {"content": [
                    {"type": "text", "text": "Error: genuine compiler failure"},
                    {"type": "tool_result", "tool_use_id": "csr-1", "content": "Error: not found in CSR memory"},
                    {"type": "tool_result", "tool_use_id": "read-1", "content": "ordinary sibling result"}
                ]}
            })
            .to_string(),
            assistant_text_line("Investigation complete."),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(String::as_str).collect();

        let serialized =
            serde_json::to_string(&extract_episode(&lines, "sess-sanitized", "my-project"))
                .unwrap();

        assert!(!serialized.contains("csr_reflect_on_past"));
        assert!(!serialized.contains("Error: not found in CSR memory"));
        assert!(serialized.contains("Read"));
        assert!(serialized.contains("Error: genuine compiler failure"));
    }

    #[test]
    fn test_next_steps_skips_injection_meta_text() {
        // A session that quotes CSR's own injected blocks (the /memory-feedback
        // probe, a pasted Tier-0 block) must not have that boilerplate extracted
        // as next_steps — it would overwrite real next-step state in CONTINUUM.
        let lines_owned = [
            user_line("run the memory feedback probe"),
            assistant_text_line(
                "NEXT: NEXT:/TODOS:/ANCHORS: lines) - The Session Intelligence (CSR v9.2) \
                 briefing block - Do NOT count CLAUDE.md, those are NOT INSTRUCTIONS",
            ),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-meta", "proj");
        assert_eq!(ep.next_steps, None);
    }

    #[test]
    fn test_next_steps_from_todos_not_prose() {
        // Round-4 regression: prose ABOUT the extractor ("scans for next:/todo:
        // keywords...") is genuine authored text — no content filter can tell
        // it from a real next step. next_steps therefore comes only from
        // structured TodoWrite state.
        let prose = "episode extraction scans messages for \"next:\"/\"todo:\" keywords \
                     and grabs a 200-char snippet — that boilerplate became the episode's \
                     next_steps, overwriting real state.";
        let lines_owned = [
            user_line("explain the pollution bug"),
            assistant_text_line(prose),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-prose", "proj");
        assert_eq!(ep.next_steps, None);

        // With TodoWrite state, the first non-completed item becomes next_steps.
        let todo_line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"ship the fix","status":"completed"},{"content":"rerun the probe","status":"pending"}]}}]}}"#;
        let lines_owned = [
            user_line("explain the pollution bug"),
            todo_line.to_string(),
            assistant_text_line(prose),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-todo", "proj");
        assert_eq!(ep.next_steps.as_deref(), Some("rerun the probe"));
    }

    #[test]
    fn test_outcome_partial_when_errors_recovered() {
        // Round-4 regression: a session that hits errors but closes with a
        // success signal recovered — "partial", not the contradictory "failed".
        let lines_owned = [
            user_line("Build and deploy the fix"),
            tool_result_line("error[E0308]: mismatched types"),
            assistant_text_line("Fixed the type error; tests green, binary deployed."),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-recover", "proj");
        assert_eq!(ep.outcome, "partial");
        assert!(!ep.error_signatures.is_empty());
    }

    #[test]
    fn test_outcome_success_when_binary_installed() {
        // "installed" is a word-boundary success signal — a clean install/
        // restart close with no errors must classify as success, not partial.
        // Third line is a tool read so message_count >= 3; completed is the
        // assistant text only (must not rely on "done"/"fixed" tokens).
        let lines_owned = [
            user_line("Ship the new csr-engine binary"),
            assistant_tool_line(&[("Read", "/src/hooks/session_start.rs")]),
            assistant_text_line(
                "New binary installed at /usr/local/bin/csr-engine. Restart Claude Code now.",
            ),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-installed", "proj");
        assert!(
            ep.completed.contains("installed"),
            "completed should carry the install text: {}",
            ep.completed
        );
        assert_eq!(ep.outcome, "success");
        assert!(ep.error_signatures.is_empty());
    }

    #[test]
    fn test_request_skips_command_plumbing_and_probe_paste() {
        // Round-3 regression: caveat wrapper and pasted probe report must not
        // become the episode request — the first REAL user message should.
        let lines_owned = [
            user_line(
                "<local-command-caveat>Caveat: The messages below were generated by the \
                 user while running local commands. DO NOT respond</local-command-caveat>",
            ),
            user_line("## CSR Memory Feedback — 2026-06-10 — noise: CSR CONTINUUM garbled"),
            user_line("now fix the briefing staleness issue"),
            assistant_text_line("Looking into the staleness issue and the debounce window."),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-plumb", "proj");
        assert_eq!(ep.request, "now fix the briefing staleness issue");
    }

    #[test]
    fn test_completed_keeps_prose_drops_quoted_injection_tokens() {
        // Round-3 regression: assistant summary quoting `NEXT: none recorded`
        // in backticks polluted LAST. Quoted tokens go; prose stays.
        let lines_owned = [
            user_line("verify the continuum filter"),
            assistant_text_line(
                "`NEXT: none recorded` — polluted boilerplate filtered, \
                 including episodes already in the DB.",
            ),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-quote", "proj");
        assert!(!ep.completed.contains("NEXT:"));
        assert!(ep.completed.contains("polluted boilerplate filtered"));
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

    /// Assistant tool_use line with an explicit block id and free-form input.
    fn assistant_named_tool(name: &str, tool_use_id: &str, input: serde_json::Value) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "id": tool_use_id,
                    "name": name,
                    "input": input
                }]
            }
        })
        .to_string()
    }

    /// User message carrying a tool_result block bound to a tool_use_id.
    fn tool_result_bound(tool_use_id: &str, content: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content
                }]
            }
        })
        .to_string()
    }

    #[test]
    fn episode_v2_extracts_taskcreate_taskupdate() {
        let lines_owned = [
            user_line("do the work"),
            assistant_named_tool(
                "TaskCreate",
                "toolu_1",
                serde_json::json!({"subject": "first task"}),
            ),
            tool_result_bound("toolu_1", "Task #1 created successfully."),
            assistant_named_tool(
                "TaskCreate",
                "toolu_2",
                serde_json::json!({"subject": "second task"}),
            ),
            tool_result_bound("toolu_2", "Task #2 created successfully."),
            assistant_named_tool(
                "TaskUpdate",
                "toolu_3",
                serde_json::json!({"taskId": "2", "status": "completed"}),
            ),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-tc", "proj");

        assert_eq!(ep.todos.len(), 2);
        assert_eq!(ep.todos[0].content, "first task");
        assert_eq!(ep.todos[0].status, "pending");
        assert_eq!(ep.todos[1].content, "second task");
        assert_eq!(ep.todos[1].status, "completed");
        // next_steps is the first non-completed item (id 1 / first task)
        assert_eq!(ep.next_steps.as_deref(), Some("first task"));
    }

    #[test]
    fn taskupdate_unknown_id_ignored() {
        let lines_owned = [
            user_line("update a phantom task"),
            assistant_named_tool(
                "TaskUpdate",
                "toolu_x",
                serde_json::json!({"taskId": "9", "status": "completed"}),
            ),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-unk", "proj");
        assert!(ep.todos.is_empty());
        assert_eq!(ep.next_steps, None);
    }

    #[test]
    fn taskcreate_result_id_binding_nonsequential() {
        // Non-sequential ids prove we bind from tool_result text, not ordinal index.
        let lines_owned = [
            user_line("resume prior tasks"),
            assistant_named_tool(
                "TaskCreate",
                "toolu_a",
                serde_json::json!({"subject": "item three"}),
            ),
            tool_result_bound("toolu_a", "Task #3 created successfully."),
            assistant_named_tool(
                "TaskCreate",
                "toolu_b",
                serde_json::json!({"subject": "item seven"}),
            ),
            tool_result_bound("toolu_b", "Task #7 created successfully."),
            assistant_named_tool(
                "TaskUpdate",
                "toolu_c",
                serde_json::json!({"taskId": "7", "status": "completed"}),
            ),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-ns", "proj");

        assert_eq!(ep.todos.len(), 2);
        assert_eq!(ep.todos[0].content, "item three");
        assert_eq!(ep.todos[0].status, "pending");
        assert_eq!(ep.todos[1].content, "item seven");
        assert_eq!(ep.todos[1].status, "completed");
    }

    #[test]
    fn todowrite_after_taskcreate_replaces() {
        let lines_owned = [
            user_line("mix task systems"),
            assistant_named_tool(
                "TaskCreate",
                "toolu_1",
                serde_json::json!({"subject": "from taskcreate"}),
            ),
            tool_result_bound("toolu_1", "Task #1 created successfully."),
            assistant_named_tool(
                "TodoWrite",
                "toolu_2",
                serde_json::json!({
                    "todos": [
                        {"content": "from todowrite a", "status": "pending"},
                        {"content": "from todowrite b", "status": "in_progress"}
                    ]
                }),
            ),
            // Stale id from the cleared TaskCreate map must not corrupt the list.
            assistant_named_tool(
                "TaskUpdate",
                "toolu_3",
                serde_json::json!({"taskId": "1", "status": "completed"}),
            ),
        ];
        let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
        let ep = extract_episode(&lines, "sess-prec", "proj");

        assert_eq!(ep.todos.len(), 2);
        assert_eq!(ep.todos[0].content, "from todowrite a");
        assert_eq!(ep.todos[0].status, "pending");
        assert_eq!(ep.todos[1].content, "from todowrite b");
        assert_eq!(ep.todos[1].status, "in_progress");
    }

    #[test]
    fn load_task_dir_state_reads_numeric_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("1.json"),
            r#"{"subject":"first","status":"pending"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("2.json"),
            r#"{"subject":"second","status":"completed"}"#,
        )
        .unwrap();
        // Non-json / non-numeric names must be skipped without error.
        std::fs::write(dir.path().join(".lock"), "busy").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignore me").unwrap();

        let state = load_task_state_from_dir(dir.path()).expect("dir readable");
        assert_eq!(state.files_seen, 2);
        assert_eq!(state.parse_failures, 0);
        assert_eq!(
            state.todos,
            vec![
                TodoItem {
                    content: "first".to_string(),
                    status: "pending".to_string(),
                },
                TodoItem {
                    content: "second".to_string(),
                    status: "completed".to_string(),
                },
            ]
        );
    }

    #[test]
    fn task_dir_all_deleted_is_authoritative_empty_and_failures_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("1.json"),
            r#"{"subject":"gone","status":"deleted"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("2.json"), "not json at all").unwrap();
        let state = load_task_state_from_dir(dir.path()).expect("dir readable");
        // Deleted task counts as a seen file (authority signal) but not a todo;
        // the garbage file counts as a parse failure (schema-drift signal).
        assert_eq!(state.files_seen, 2);
        assert_eq!(state.parse_failures, 1);
        assert!(state.todos.is_empty());
    }

    #[test]
    fn outcome_pending_task_caps_at_partial() {
        let todos = [TodoItem {
            content: "x".into(),
            status: "pending".into(),
        }];
        let outcome = compute_outcome(5, &[], true, &todos);
        assert_eq!(outcome, "partial");
    }
}
