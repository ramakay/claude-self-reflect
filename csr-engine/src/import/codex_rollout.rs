//! Optional Codex rollout corpus adapter. Modern envelope records and legacy
//! direct messages are normalized into the same message shape used by Claude
//! transcripts, then passed through the shared CSR sanitizer before embedding.

use std::fs;
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::engine::Engine;
use crate::import::scrub::is_database_busy;
use crate::import::{ConversationChunk, CsrSuppressionStats};
use crate::provenance::{ChunkEvidence, ChunkProvenance, ProvenanceEvent, TrustTier};
use crate::storage::Storage;

const ROLLOUT_EMBED_BATCH_SIZE: usize = 32;
/// How many times one rollout is re-attempted after SQLite reports the database
/// busy. A rollout is thousands of autocommit writes racing every other
/// csr-engine process on the box, so a collision is a normal event, not a
/// corpus-level failure.
const ROLLOUT_BUSY_RETRIES: usize = 4;
/// Base backoff between those attempts, multiplied by the attempt number.
const ROLLOUT_BUSY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(500);

#[derive(Debug, Clone)]
pub(crate) struct RolloutFile {
    pub path: PathBuf,
    pub mtime: String,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct ParsedRollout {
    pub conversation_id: String,
    pub project_name: String,
    pub timestamp: String,
    pub messages: Vec<serde_json::Value>,
    pub schema_misses: usize,
    pub suppression: CsrSuppressionStats,
    pub harness_context_stripped: usize,
    pub is_subagent: bool,
}

#[derive(Debug, Clone)]
struct RolloutMetadata {
    conversation_id: String,
    project_name: String,
    timestamp: String,
    summary: Option<String>,
    /// True when `session_meta.payload.source` carries a `subagent` key —
    /// this rollout is a `thread_spawn` (or other) subagent child, not a
    /// top-level session a person typed into. Stored top-level like every
    /// other codex_rollout conversation (Codex has no separate sidechain
    /// file layout the way Claude Code does), so this is the only signal
    /// that distinguishes it. Threaded into every chunk's `is_sidechain` so
    /// the existing Claude-side sidechain exclusion in `get_recent_chunks`
    /// applies here too, instead of a spawned child taking its own
    /// recent-work slot next to its parent.
    is_subagent: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct RolloutStreamOutcome {
    schema_misses: usize,
    suppression: CsrSuppressionStats,
    /// Harness-injected content items dropped by `canonical_message` — see
    /// `CODEX_HARNESS_MARKERS`. Not a schema miss: this is a deliberate,
    /// successful redaction, counted separately.
    harness_context_stripped: usize,
    chunks: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RolloutImportStats {
    pub files_discovered: usize,
    pub files_imported: usize,
    pub chunks_imported: usize,
    pub vanished: usize,
    pub failed: usize,
    pub schema_misses: usize,
    pub csr_tool_blocks_suppressed: usize,
    pub csr_hook_wrappers_scrubbed: usize,
    pub harness_context_stripped: usize,
}

/// Recursively discover changed `rollout-*.jsonl` files. A missing vendor root
/// is a normal optional-source state: empty result, no logging, no error.
pub(crate) fn discover_rollouts<F>(root: &Path, mut stored_mtime: F) -> Result<Vec<RolloutFile>>
where
    F: FnMut(&str) -> Result<Option<String>>,
{
    fn visit<F>(dir: &Path, stored_mtime: &mut F, out: &mut Vec<RolloutFile>) -> Result<()>
    where
        F: FnMut(&str) -> Result<Option<String>>,
    {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                visit(&path, stored_mtime, out)?;
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let is_rollout = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"));
            if !is_rollout {
                continue;
            }
            let Some(mtime) = file_mtime(&path) else {
                continue;
            };
            let key = path.to_string_lossy();
            if stored_mtime(&key)?.as_deref() == Some(mtime.as_str()) {
                continue;
            }
            out.push(RolloutFile { path, mtime });
        }
        Ok(())
    }

    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    visit(root, &mut stored_mtime, &mut out)?;
    out.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(out)
}

fn file_mtime(path: &Path) -> Option<String> {
    let modified = path.metadata().ok()?.modified().ok()?;
    let datetime: DateTime<Utc> = modified.into();
    Some(datetime.to_rfc3339())
}

/// Parse one rollout. `Ok(None)` means it vanished between discovery and open.
#[cfg(test)]
pub(crate) fn parse_rollout(path: &Path) -> Result<Option<ParsedRollout>> {
    let mut messages = Vec::new();
    let Some((metadata, outcome)) = visit_rollout_messages(path, |message| {
        messages.push(message);
        Ok(())
    })?
    else {
        return Ok(None);
    };
    Ok(Some(ParsedRollout {
        conversation_id: metadata.conversation_id,
        project_name: metadata.project_name,
        timestamp: metadata.timestamp,
        messages,
        schema_misses: outcome.schema_misses,
        suppression: outcome.suppression,
        harness_context_stripped: outcome.harness_context_stripped,
        is_subagent: metadata.is_subagent,
    }))
}

fn visit_rollout_messages<F>(
    path: &Path,
    visit: F,
) -> Result<Option<(RolloutMetadata, RolloutStreamOutcome)>>
where
    F: FnMut(serde_json::Value) -> Result<()>,
{
    visit_rollout_messages_inner(path, true, visit)
}

pub(super) fn visit_raw_rollout_messages<F>(path: &Path, visit: F) -> Result<()>
where
    F: FnMut(serde_json::Value) -> Result<()>,
{
    visit_rollout_messages_inner(path, false, visit)?;
    Ok(())
}

fn visit_rollout_messages_inner<F>(
    path: &Path,
    sanitize: bool,
    mut visit: F,
) -> Result<Option<(RolloutMetadata, RolloutStreamOutcome)>>
where
    F: FnMut(serde_json::Value) -> Result<()>,
{
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("opening Codex rollout"),
    };
    let mut conversation_id = None;
    let mut cwd = None;
    let mut timestamp = None;
    let mut schema_misses = 0usize;
    let mut harness_context_stripped = 0usize;
    let mut summary = None;
    let mut is_subagent = false;
    let mut sanitizer = super::CsrMessageSanitizer::default();

    let mut reader = BufReader::new(file);
    let mut byte_offset = 0u64;
    loop {
        let line_start = byte_offset;
        let mut line = String::new();
        let bytes_read = match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(bytes) => bytes,
            Err(_) => {
                anyhow::ensure!(sanitize, "cannot read rollout source");
                schema_misses += 1;
                continue;
            }
        };
        byte_offset += bytes_read as u64;
        while matches!(line.as_bytes().last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = match sonic_rs::from_str(&line) {
            Ok(value) => value,
            Err(_) => {
                anyhow::ensure!(sanitize, "cannot parse rollout source");
                schema_misses += 1;
                continue;
            }
        };
        timestamp = timestamp.or_else(|| string_at(&value, &["timestamp"]));
        let mut line_messages = Vec::new();
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("session_meta") => {
                let payload = value.get("payload").unwrap_or(&serde_json::Value::Null);
                conversation_id = conversation_id
                    .or_else(|| string_at(payload, &["id"]))
                    .or_else(|| string_at(payload, &["session_id"]));
                cwd = cwd.or_else(|| string_at(payload, &["cwd"]));
                timestamp = timestamp.or_else(|| string_at(payload, &["timestamp"]));
                is_subagent = is_subagent
                    || payload
                        .get("source")
                        .and_then(|source| source.get("subagent"))
                        .is_some();
            }
            Some("turn_context") => {
                cwd = cwd.or_else(|| string_at(&value, &["payload", "cwd"]));
            }
            Some("response_item") => {
                parse_response_item(
                    &value,
                    &mut line_messages,
                    &mut schema_misses,
                    &mut harness_context_stripped,
                );
            }
            Some("event_msg") => {
                parse_event_message(&value, &mut line_messages, &mut schema_misses)
            }
            Some("message") => {
                // The outer `timestamp` is latched to the FIRST line that had
                // one (see the `.or_else` above), so by this point it no
                // longer reflects the current line for any line past the
                // first. Prefer this line's own timestamp; only fall back to
                // the latch when the line genuinely has none of its own.
                let line_ts = string_at(&value, &["timestamp"]).or_else(|| timestamp.clone());
                match canonical_message(&value, line_ts.as_deref(), &mut harness_context_stripped) {
                    CanonicalOutcome::Message(message) => line_messages.push(message),
                    CanonicalOutcome::Stripped => {}
                    CanonicalOutcome::Missing => schema_misses += 1,
                }
            }
            Some("world_state" | "inter_agent_communication_metadata") => {}
            Some(_) => {
                let line_ts = string_at(&value, &["timestamp"]).or_else(|| timestamp.clone());
                if let Some(text) = extract_unknown_text(&value) {
                    line_messages.push(unclassified_message(
                        "assistant",
                        &text,
                        line_ts.as_deref(),
                    ));
                } else {
                    schema_misses += 1;
                }
            }
            None => {
                // Legacy metadata envelope (`id`, `timestamp`, `git`, `instructions`).
                if value.get("record_type").is_some() {
                    continue;
                }
                if value.get("id").is_some() && value.get("timestamp").is_some() {
                    conversation_id = conversation_id.or_else(|| string_at(&value, &["id"]));
                    continue;
                }
                if let Some(text) = extract_unknown_text(&value) {
                    line_messages.push(unclassified_message(
                        "assistant",
                        &text,
                        timestamp.as_deref(),
                    ));
                } else {
                    schema_misses += 1;
                }
            }
        }
        for mut message in line_messages {
            message["_csr_receipt_ref"] =
                serde_json::Value::String(format!("{}#byte={line_start}", path.display()));
            if sanitize {
                super::sanitize_message_for_search(&mut message, &mut sanitizer);
            }
            if summary.is_none()
                && message.get("type").and_then(serde_json::Value::as_str) == Some("user")
            {
                summary =
                    Some(super::extract_message_text(&message)).filter(|text| !text.is_empty());
            }
            visit(message)?;
        }
    }

    let fallback_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("unknown")
        .strip_prefix("rollout-")
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("unknown")
        });
    let project_name = cwd
        .as_deref()
        .and_then(|cwd| Path::new(cwd).file_name())
        .and_then(|name| name.to_str())
        .map(crate::import::normalize_project_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "_unscoped".to_string());
    Ok(Some((
        RolloutMetadata {
            conversation_id: format!(
                "codex:{}",
                conversation_id.as_deref().unwrap_or(fallback_id)
            ),
            project_name,
            timestamp: timestamp
                .or_else(|| file_mtime(path))
                .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string()),
            summary,
            is_subagent,
        },
        RolloutStreamOutcome {
            schema_misses,
            suppression: sanitizer.stats,
            harness_context_stripped,
            chunks: 0,
        },
    )))
}

fn string_at(value: &serde_json::Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str().map(str::to_string)
}

fn parse_response_item(
    value: &serde_json::Value,
    messages: &mut Vec<serde_json::Value>,
    schema_misses: &mut usize,
    harness_context_stripped: &mut usize,
) {
    let Some(payload) = value.get("payload") else {
        *schema_misses += 1;
        return;
    };
    let timestamp = value.get("timestamp").and_then(serde_json::Value::as_str);
    match payload.get("type").and_then(serde_json::Value::as_str) {
        Some("message") => match canonical_message(payload, timestamp, harness_context_stripped) {
            CanonicalOutcome::Message(message) => messages.push(message),
            CanonicalOutcome::Stripped => {}
            CanonicalOutcome::Missing
                if matches!(
                    payload.get("role").and_then(serde_json::Value::as_str),
                    Some("developer" | "system")
                ) => {}
            CanonicalOutcome::Missing => *schema_misses += 1,
        },
        Some("agent_message") => {
            if let Some(text) = content_text(payload.get("content")) {
                messages.push(text_message("assistant", &text, timestamp));
            } else {
                *schema_misses += 1;
            }
        }
        Some("function_call" | "custom_tool_call") => {
            let name = payload
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let id = payload
                .get("call_id")
                .or_else(|| payload.get("id"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let input = payload
                .get("arguments")
                .or_else(|| payload.get("input"))
                .map(parse_json_string)
                .unwrap_or(serde_json::Value::Null);
            let mut tool = serde_json::json!({
                "type":"tool_use", "id":id, "name":name, "input":input
            });
            if let Some(namespace) = payload.get("namespace") {
                tool["namespace"] = namespace.clone();
            }
            messages.push(block_message("assistant", tool, timestamp));
        }
        Some("function_call_output" | "custom_tool_call_output") => {
            let id = payload
                .get("call_id")
                .or_else(|| payload.get("id"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let output = payload.get("output").map(value_text).unwrap_or_default();
            messages.push(block_message(
                "user",
                serde_json::json!({"type":"tool_result", "tool_use_id":id, "content":output}),
                timestamp,
            ));
        }
        // Reasoning and encrypted internals are intentionally not searchable text.
        Some("reasoning") => {}
        Some(_) | None => {
            if let Some(text) = extract_unknown_text(payload) {
                messages.push(unclassified_message("assistant", &text, timestamp));
            } else {
                *schema_misses += 1;
            }
        }
    }
}

fn parse_event_message(
    value: &serde_json::Value,
    messages: &mut Vec<serde_json::Value>,
    schema_misses: &mut usize,
) {
    let Some(payload) = value.get("payload") else {
        *schema_misses += 1;
        return;
    };
    let payload_type = payload.get("type").and_then(serde_json::Value::as_str);
    let role = match payload_type {
        Some("user_message") => "user",
        Some("agent_message") => "assistant",
        _ => {
            if let Some(text) = extract_unknown_text(payload) {
                messages.push(unclassified_message(
                    "assistant",
                    &text,
                    value.get("timestamp").and_then(serde_json::Value::as_str),
                ));
            } else if !matches!(
                payload_type,
                Some(
                    "token_count"
                        | "task_started"
                        | "task_complete"
                        | "thread_settings_applied"
                        | "sub_agent_activity"
                )
            ) {
                *schema_misses += 1;
            }
            return;
        }
    };
    if let Some(text) = payload
        .get("message")
        .and_then(serde_json::Value::as_str)
        .or_else(|| payload.get("text").and_then(serde_json::Value::as_str))
    {
        messages.push(text_message(
            role,
            text,
            value.get("timestamp").and_then(serde_json::Value::as_str),
        ));
    } else {
        *schema_misses += 1;
    }
}

/// Harness-injected context markers Codex writes as synthetic user-role
/// content items ahead of the real user turn (recommended plugins, AGENTS.md
/// instructions, environment/context dumps, permission grants). Scanned from
/// the live `~/.codex/sessions` corpus. Only exact, line-anchored tag
/// prefixes belong here — never anything broad enough to also match real
/// user prose that happens to mention one of these strings.
const CODEX_HARNESS_MARKERS: &[&str] = &[
    "<recommended_plugins>",
    "# AGENTS.md instructions",
    "<environment_context>",
    "<user_instructions>",
    "<permissions instructions>",
    "<codex_internal_context>",
    "<skills_instructions>",
];

/// Whether `text` is a Codex harness-injected content item rather than real
/// user prose, checked as a prefix of the *trimmed* text (never a
/// substring), so prose that merely mentions one of these tags mid-message
/// is never stripped. `pub(crate)` so `storage::queries`'s recent-work
/// preview picker can skip a harness-boilerplate chunk 0 the same way the
/// harness-strip backfill does.
pub(crate) fn is_harness_context_item(text: &str) -> bool {
    let trimmed = text.trim_start();
    CODEX_HARNESS_MARKERS
        .iter()
        .any(|marker| trimmed.starts_with(marker))
}

/// Outcome of building a canonical user/assistant message from a raw Codex
/// envelope. `Stripped` (every content item present was harness-injected) is
/// deliberately distinct from `Missing` (genuinely no usable text): the
/// former is a successful redaction and must never increment a schema-miss
/// counter or emit an empty message.
enum CanonicalOutcome {
    Message(serde_json::Value),
    Stripped,
    Missing,
}

fn canonical_message(
    value: &serde_json::Value,
    timestamp: Option<&str>,
    harness_context_stripped: &mut usize,
) -> CanonicalOutcome {
    let Some(role) = value.get("role").and_then(serde_json::Value::as_str) else {
        return CanonicalOutcome::Missing;
    };
    if role != "user" && role != "assistant" {
        return CanonicalOutcome::Missing;
    }
    // Only Codex's own harness injects synthetic context ahead of the real
    // turn, and it always does so as a user-role message — assistant
    // content never needs the strip.
    let content_outcome = if role == "user" {
        content_text_stripping_harness(value.get("content"), harness_context_stripped)
    } else {
        match content_text(value.get("content")) {
            Some(text) => ContentOutcome::Text(text),
            None => ContentOutcome::Missing,
        }
    };
    let text = match content_outcome {
        ContentOutcome::Text(text) => text,
        ContentOutcome::Stripped => return CanonicalOutcome::Stripped,
        ContentOutcome::Missing => return CanonicalOutcome::Missing,
    };
    let mut message = text_message(role, &text, timestamp);
    if let Some(id) = value.get("id").and_then(serde_json::Value::as_str) {
        message["uuid"] = serde_json::Value::String(id.to_string());
    }
    CanonicalOutcome::Message(message)
}

/// Outcome of extracting text from a `content` field with harness-item
/// filtering applied — see `CanonicalOutcome` for why `Stripped` is not
/// folded into `Missing`.
enum ContentOutcome {
    Text(String),
    Stripped,
    Missing,
}

fn content_text_stripping_harness(
    content: Option<&serde_json::Value>,
    stripped: &mut usize,
) -> ContentOutcome {
    match content {
        None => ContentOutcome::Missing,
        Some(serde_json::Value::String(text)) => {
            if text.is_empty() {
                ContentOutcome::Missing
            } else if is_harness_context_item(text) {
                *stripped += 1;
                ContentOutcome::Stripped
            } else {
                ContentOutcome::Text(text.clone())
            }
        }
        Some(serde_json::Value::Array(items)) => {
            let mut any_stripped = false;
            let mut kept = Vec::new();
            for item in items {
                let Some(text) = item.get("text").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                if is_harness_context_item(text) {
                    any_stripped = true;
                    *stripped += 1;
                } else {
                    kept.push(text);
                }
            }
            let text = kept.join("\n");
            if !text.is_empty() {
                ContentOutcome::Text(text)
            } else if any_stripped {
                ContentOutcome::Stripped
            } else {
                ContentOutcome::Missing
            }
        }
        Some(_) => ContentOutcome::Missing,
    }
}

fn content_text(content: Option<&serde_json::Value>) -> Option<String> {
    match content? {
        serde_json::Value::String(text) if !text.is_empty() => Some(text.clone()),
        serde_json::Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(|item| item.get("text").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn text_message(role: &str, text: &str, timestamp: Option<&str>) -> serde_json::Value {
    block_message(
        role,
        serde_json::json!({"type":"text", "text":text}),
        timestamp,
    )
}

fn unclassified_message(role: &str, text: &str, timestamp: Option<&str>) -> serde_json::Value {
    let mut message = text_message(role, text, timestamp);
    message["_csr_channel"] = serde_json::Value::String("unclassified".to_string());
    message
}

fn block_message(
    role: &str,
    block: serde_json::Value,
    timestamp: Option<&str>,
) -> serde_json::Value {
    let mut message = serde_json::json!({
        "type": role,
        "message": {"content": [block]}
    });
    if let Some(timestamp) = timestamp {
        message["timestamp"] = serde_json::Value::String(timestamp.to_string());
    }
    message
}

fn parse_json_string(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(raw) => {
            sonic_rs::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.clone()))
        }
        other => other.clone(),
    }
}

fn value_text(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn extract_unknown_text(value: &serde_json::Value) -> Option<String> {
    for key in ["text", "message", "content", "output"] {
        if let Some(candidate) = value.get(key) {
            if let Some(text) = content_text(Some(candidate)) {
                return Some(text);
            }
            if let Some(text) = candidate.as_str().filter(|text| !text.is_empty()) {
                return Some(text.to_string());
            }
        }
    }
    value
        .get("payload")
        .and_then(extract_unknown_text)
        .or_else(|| value.get("item").and_then(extract_unknown_text))
}

fn visit_rollout_message_chunks<F>(
    metadata: &RolloutMetadata,
    message: &serde_json::Value,
    next_seq: &mut usize,
    event_seq: usize,
    tool_names: &mut std::collections::HashMap<String, String>,
    running_floor: &mut Option<TrustTier>,
    mut visit: F,
) -> Result<()>
where
    F: FnMut(ConversationChunk, ChunkEvidence) -> Result<()>,
{
    const BUDGET: usize = 900;
    super::register_tool_uses(message, tool_names);
    let text = super::extract_message_text(message);
    let tool_context = super::extract_tool_context(message);
    let role = message
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let override_channel = message
        .get("_csr_channel")
        .and_then(serde_json::Value::as_str);
    let mut components = Vec::new();
    if !text.is_empty() {
        let channel = override_channel.unwrap_or(if role == "user" {
            "codex_user"
        } else {
            "codex_assistant"
        });
        components.push((channel.to_string(), text));
    }
    if !tool_context.is_empty() {
        components.push(("codex_assistant".to_string(), tool_context));
    }
    for (tool_name, body) in super::extract_tool_result_parts(message, tool_names) {
        components.push((format!("codex_tool:{tool_name}"), body));
    }
    let content = components
        .iter()
        .map(|(_, part)| part.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if content.is_empty() {
        return Ok(());
    }
    let author = super::classify_message_author(message);
    let message_key = message
        .get("uuid")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| crate::provenance::content_hash(&content));
    let observed_at = message
        .get("timestamp")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&metadata.timestamp)
        .to_string();
    let receipt_ref = message
        .get("_csr_receipt_ref")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let mut combined_start = 0usize;
    let mut parts = Vec::new();
    for (ordinal, (channel, component)) in components.into_iter().enumerate() {
        let context_floor = running_floor.unwrap_or(TrustTier::Unknown);
        let tier = crate::provenance::trust_for_channel(&channel, context_floor);
        let event = ProvenanceEvent {
            event_id: super::provenance_event_id(
                &metadata.conversation_id,
                &message_key,
                event_seq,
                &channel,
                ordinal,
            ),
            conversation_id: metadata.conversation_id.clone(),
            message_key: message_key.clone(),
            seq: event_seq,
            channel,
            trust_tier: tier,
            parent_event_id: None,
            receipt_kind: "jsonl".to_string(),
            receipt_ref: receipt_ref.clone(),
            observed_at: observed_at.clone(),
        };
        let combined_end = combined_start + component.len();
        parts.push(super::EventPart {
            event,
            text: component,
            combined_start,
            combined_end,
        });
        *running_floor = Some(match *running_floor {
            Some(floor) => floor.min(tier),
            None => tier,
        });
        combined_start = combined_end + 1;
    }
    let message_evidence = super::MessageEvidence { parts };
    let mut start = 0;
    while start < content.len() {
        let mut end = (start + BUDGET).min(content.len());
        end = content.floor_char_boundary(end);
        if end <= start {
            end = content.len();
        }
        let seq = *next_seq;
        let chunk = ConversationChunk {
            id: super::generate_chunk_id(&metadata.conversation_id, seq),
            conversation_id: metadata.conversation_id.clone(),
            project_name: metadata.project_name.clone(),
            timestamp: observed_at.clone(),
            content: content[start..end].to_string(),
            message_count: 1,
            summary: metadata.summary.clone(),
            author,
            seq,
            is_sidechain: metadata.is_subagent,
        };
        let mut evidence = std::collections::HashMap::new();
        super::attach_chunk_evidence(
            &chunk,
            super::slice_event_parts(&message_evidence, start, end),
            &mut evidence,
        );
        let chunk_evidence = evidence
            .remove(&chunk.id)
            .expect("non-empty rollout chunk has structural evidence");
        visit(chunk, chunk_evidence)?;
        *next_seq += 1;
        start = end;
    }
    Ok(())
}

#[cfg(test)]
fn rollout_chunks(parsed: &ParsedRollout) -> Vec<ConversationChunk> {
    let metadata = RolloutMetadata {
        conversation_id: parsed.conversation_id.clone(),
        project_name: parsed.project_name.clone(),
        timestamp: parsed.timestamp.clone(),
        summary: parsed
            .messages
            .iter()
            .find(|message| message.get("type").and_then(serde_json::Value::as_str) == Some("user"))
            .map(super::extract_message_text)
            .filter(|text| !text.is_empty()),
        is_subagent: parsed.is_subagent,
    };
    let mut chunks = Vec::new();
    let mut next_seq = 0;
    let mut tool_names = std::collections::HashMap::new();
    let mut floor = None;
    for (event_seq, message) in parsed.messages.iter().enumerate() {
        visit_rollout_message_chunks(
            &metadata,
            message,
            &mut next_seq,
            event_seq,
            &mut tool_names,
            &mut floor,
            |chunk, _| {
                chunks.push(chunk);
                Ok(())
            },
        )
        .expect("collecting rollout chunks cannot fail");
    }
    chunks
}

fn scan_rollout_metadata(path: &Path) -> Result<Option<RolloutMetadata>> {
    Ok(visit_rollout_messages(path, |_| Ok(()))?.map(|(metadata, _)| metadata))
}

fn stream_rollout_chunk_batches<F>(
    path: &Path,
    metadata: &RolloutMetadata,
    batch_size: usize,
    mut visit_batch: F,
) -> Result<RolloutStreamOutcome>
where
    F: FnMut(&[ConversationChunk], &[ChunkEvidence]) -> Result<()>,
{
    anyhow::ensure!(batch_size > 0, "rollout batch size must be positive");
    let mut pending = Vec::with_capacity(batch_size);
    let mut pending_evidence = Vec::with_capacity(batch_size);
    let mut next_seq = 0usize;
    let mut event_seq = 0usize;
    let mut tool_names = std::collections::HashMap::new();
    let mut floor = None;
    let Some((streamed_metadata, mut outcome)) = visit_rollout_messages(path, |message| {
        visit_rollout_message_chunks(
            metadata,
            &message,
            &mut next_seq,
            event_seq,
            &mut tool_names,
            &mut floor,
            |chunk, evidence| {
                pending.push(chunk);
                pending_evidence.push(evidence);
                if pending.len() == batch_size {
                    visit_batch(&pending, &pending_evidence)?;
                    pending.clear();
                    pending_evidence.clear();
                }
                Ok(())
            },
        )?;
        event_seq += 1;
        Ok(())
    })?
    else {
        anyhow::bail!(
            "Codex rollout vanished before streaming: {}",
            path.display()
        );
    };
    debug_assert_eq!(streamed_metadata.conversation_id, metadata.conversation_id);
    debug_assert_eq!(streamed_metadata.project_name, metadata.project_name);
    if !pending.is_empty() {
        visit_batch(&pending, &pending_evidence)?;
    }
    outcome.chunks = next_seq;
    Ok(outcome)
}

/// Import every changed rollout discovered beneath the optional Codex root.
/// This function is synchronous by design and is run inside `spawn_blocking`
/// by setup/daemon call sites.
pub(crate) fn import_changed_rollouts(engine: &Engine, root: &Path) -> Result<RolloutImportStats> {
    let storage = engine.storage();
    let files = discover_rollouts(root, |key| storage.get_import_state_mtime(key))?;
    let mut stats = RolloutImportStats {
        files_discovered: files.len(),
        ..RolloutImportStats::default()
    };
    for file in files {
        match import_rollout_with_retries(engine, &file) {
            Ok(None) => stats.vanished += 1,
            Ok(Some(outcome)) => {
                stats.schema_misses += outcome.schema_misses;
                stats.csr_tool_blocks_suppressed += outcome.suppression.csr_tool_blocks_suppressed;
                stats.csr_hook_wrappers_scrubbed += outcome.suppression.csr_hook_wrappers_scrubbed;
                stats.harness_context_stripped += outcome.harness_context_stripped;
                stats.files_imported += 1;
                stats.chunks_imported += outcome.chunks;
            }
            // One unreadable or contended rollout is not a reason to abandon
            // the remaining thousands: the pass used to propagate the first
            // error and the loop only came back 30 minutes later. `import_state`
            // is stamped only on success, so the file is retried next cycle.
            Err(error) => {
                stats.failed += 1;
                tracing::warn!(
                    file = %file.path.display(),
                    %error,
                    "Codex rollout file skipped"
                );
            }
        }
    }
    Ok(stats)
}

/// Import one rollout, re-attempting while SQLite reports the database busy.
/// The import is idempotent — chunks are deleted and rebuilt per conversation
/// and `import_state` is stamped last — so replaying a partial attempt neither
/// duplicates rows nor strands the previous ones.
fn import_rollout_with_retries(
    engine: &Engine,
    file: &RolloutFile,
) -> Result<Option<RolloutStreamOutcome>> {
    let mut attempt = 0;
    loop {
        match import_rollout(engine, file) {
            Err(error) if attempt < ROLLOUT_BUSY_RETRIES && is_database_busy(&error) => {
                attempt += 1;
                std::thread::sleep(ROLLOUT_BUSY_BACKOFF * attempt as u32);
            }
            result => return result,
        }
    }
}

/// `Ok(None)` means the rollout vanished between discovery and reading it,
/// which is routine: Codex deletes session files aggressively.
fn import_rollout(engine: &Engine, file: &RolloutFile) -> Result<Option<RolloutStreamOutcome>> {
    let storage = engine.storage();
    let Some(metadata) = scan_rollout_metadata(&file.path)? else {
        return Ok(None);
    };
    let old_ids = storage.get_chunk_ids_for_conversation(&metadata.conversation_id)?;
    storage.delete_chunks_for_conversation(&metadata.conversation_id)?;
    if !old_ids.is_empty() {
        let mut index = engine.search().blocking_write();
        for id in old_ids {
            index.remove_chunk(&id);
        }
    }
    let outcome = stream_rollout_chunk_batches(
        &file.path,
        &metadata,
        ROLLOUT_EMBED_BATCH_SIZE,
        |chunks, evidence| {
            let texts = chunks
                .iter()
                .map(|chunk| chunk.content.as_str())
                .collect::<Vec<_>>();
            let embeddings = engine.embeddings().embed(&texts)?;
            let mut index = engine.search().blocking_write();
            for ((chunk, chunk_evidence), embedding) in chunks.iter().zip(evidence).zip(embeddings)
            {
                storage.insert_chunk_with_source(chunk, &embedding, "codex_rollout")?;
                storage.insert_chunk_provenance(
                    &chunk.id,
                    &ChunkProvenance {
                        author: chunk.author,
                        source_conv_id: metadata.conversation_id.clone(),
                        supersedes: None,
                    },
                )?;
                storage.replace_chunk_evidence(chunk_evidence)?;
                index.insert_chunk(chunk.id.clone(), embedding);
            }
            Ok(())
        },
    )?;
    storage.bump_aux_counter_by("codex_rollout", outcome.schema_misses)?;
    // Same convention as the schema-miss counter above (aux `meta` counter,
    // surfaced next to it in status via `get_aux_counters`), distinctly
    // namespaced so a redacted-harness-item count is never conflated with a
    // parse failure.
    storage.bump_aux_counter_by(
        "codex_harness_context_stripped",
        outcome.harness_context_stripped,
    )?;
    let key = file.path.to_string_lossy();
    storage.upsert_import_state_explicit(
        &key,
        &metadata.conversation_id,
        outcome.chunks,
        &file.mtime,
    )?;
    Ok(Some(outcome))
}

/// Meta key guarding [`restamp_codex_chunk_timestamps`]. Present once the
/// backfill has run to completion (`conversations_unmatched == 0 &&
/// chunks_unmatched == 0` for the run that set it) on a database; a second
/// call is then a no-op (`RestampStats::already_applied`).
///
/// `_v2`: the `_v1` key was set unconditionally even when hundreds of
/// conversations were left with collapsed timestamps (harness-polluted
/// rollouts whose stored content no longer matches the post-strip
/// re-chunked text — see [`polluted_codex_conversation_ids`]), which then
/// froze every later run as a silent no-op. `_v2` is written only on a
/// genuinely clean pass; an old `_v1`-only database is *not* treated as
/// done and gets one real `_v2` pass.
pub const RESTAMP_META_KEY: &str = "codex_rollout_chunk_ts_restamp_v2";

/// Meta key holding the JSON-encoded [`RestampProgress`] for an incomplete
/// `_v2` run — attempt count plus the last run's unmatched totals. Read at
/// the top of [`restamp_codex_chunk_timestamps`] to bound retries; never
/// consulted once [`RESTAMP_META_KEY`] is set.
pub const RESTAMP_PROGRESS_META_KEY: &str = "codex_rollout_chunk_ts_restamp_v2_attempts";

/// Bound on retry attempts for [`restamp_codex_chunk_timestamps`] while
/// genuine (non-harness-polluted) content drift keeps `chunks_unmatched`
/// above zero. Past this the function stops re-scanning the whole corpus
/// every call — a permanently drifted file is never going to start
/// matching — but still never claims [`RESTAMP_META_KEY`] done.
pub const RESTAMP_MAX_ATTEMPTS: usize = 10;

/// Persisted retry state for an incomplete restamp pass. JSON-encoded under
/// [`RESTAMP_PROGRESS_META_KEY`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RestampProgress {
    pub attempts: usize,
    pub conversations_considered: usize,
    pub conversations_updated: usize,
    pub conversations_unmatched: usize,
    pub conversations_missing_file: usize,
    pub conversations_skipped_polluted: usize,
    /// Added after `_v2` first shipped — `#[serde(default)]` so an
    /// already-persisted progress blob from before this field existed still
    /// deserializes (as `0`) instead of getting dropped wholesale by the
    /// `.ok()` fallback in every reader.
    #[serde(default)]
    pub conversations_pending_import: usize,
    pub chunks_updated: usize,
    pub chunks_unmatched: usize,
}

/// Receipt for one run of [`restamp_codex_chunk_timestamps`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct RestampStats {
    /// `import_state` rows with `conversation_id LIKE 'codex:%'` examined.
    pub conversations_considered: usize,
    /// Rows whose file no longer exists on disk (capture-on-appearance:
    /// Codex deletes session files aggressively). Left unchanged, not fixed.
    pub conversations_missing_file: usize,
    /// Rows whose file exists but now parses to a different conversation id
    /// (skipped rather than cross-stamped).
    pub conversations_id_mismatch: usize,
    /// Files that produced at least one content-matched, re-stamped chunk.
    pub conversations_updated: usize,
    /// Files that exist and parsed, but every re-derived chunk's content
    /// failed the guard (the on-disk file drifted since the original
    /// import) — zero chunks touched for that conversation. Harness-polluted
    /// conversations are never counted here — see `conversations_skipped_polluted`.
    pub conversations_unmatched: usize,
    /// Rows skipped because [`polluted_codex_conversation_ids`] still flags
    /// them: their stored (pre-strip) content will never content-match the
    /// post-strip re-chunked text, by construction. These are
    /// [`harness_strip_batch`]'s responsibility, not this restamp's —
    /// skipping them (instead of counting them unmatched)
    /// is what lets a corpus with outstanding harness pollution still reach
    /// a clean, done-marked restamp for everything else.
    pub conversations_skipped_polluted: usize,
    /// Rows skipped because the on-disk file's mtime no longer matches the
    /// `import_state` row this restamp is comparing against
    /// ([`Storage::is_file_imported`] returned `false` for a reason other
    /// than "no row") — the file changed since it was last imported. This is
    /// transient drift, not a fixable defect here: an ordinary
    /// `import_changed_rollouts` pass will pick the new mtime up and
    /// re-derive fresh content on its own next tick. Counting it as
    /// `conversations_unmatched` instead (the pre-fix behavior) burned a
    /// restamp attempt on every single conversation that merely grew between
    /// listing and re-derivation — see the W3-R review finding this closes.
    pub conversations_pending_import: usize,
    /// Total chunk rows whose `timestamp` was rewritten.
    pub chunks_updated: usize,
    /// Total re-derived chunks whose content guard did not match any stored
    /// row (drifted file content, not a fixable case).
    pub chunks_unmatched: usize,
    /// True when the meta marker was already present — no work was done.
    pub already_applied: bool,
    /// This run's 1-based attempt number (persisted across calls via
    /// [`RESTAMP_PROGRESS_META_KEY`]). `0` when `already_applied` or when
    /// `attempts_exhausted` short-circuited before running a pass.
    pub attempts: usize,
    /// `true` when [`RESTAMP_MAX_ATTEMPTS`] was already reached on a prior
    /// call and this call returned immediately without re-scanning.
    pub attempts_exhausted: bool,
}

/// One-time, idempotent backfill for the timestamp-collapse bug fixed in
/// `visit_rollout_message_chunks` (chunk.timestamp now uses `observed_at`
/// instead of the latched session-start `metadata.timestamp`). The code fix
/// alone only reaches rollouts that get reimported because their mtime
/// changes; the ~1,400 already-imported, still-on-disk rollouts in a live
/// corpus would otherwise carry the stale collapsed timestamp forever.
///
/// This re-derives each already-imported conversation's chunks by streaming
/// the on-disk rollout through the exact same chunker used at import time
/// (`stream_rollout_chunk_batches` / `visit_rollout_message_chunks`) — no
/// embedding, no HNSW writes, no delete+reinsert — and issues a
/// content-guarded `UPDATE chunks SET timestamp=...` per chunk. Chunk ids
/// are deterministic (`conversation_id` + `seq`), so an unchanged file
/// reproduces byte-identical chunk ids and content; the content guard is
/// what makes a drifted file a safe no-op instead of a silent mis-stamp.
///
/// Deleted rollouts (capture-on-appearance means many are gone) are left
/// unchanged and counted in `conversations_missing_file`, never fabricated.
///
/// Conversations still flagged by [`polluted_codex_conversation_ids`] are
/// skipped rather than re-derived here: their stored content is the
/// pre-strip harness text, which will never content-match what this same
/// chunker now re-derives (post-strip), so they would otherwise land in
/// `conversations_unmatched` on every single call, forever — the exact
/// defect this function used to have. Fixing those is the codex
/// harness-strip job's job (full delete+reinsert via `import_rollout`,
/// which stamps correct timestamps as a side effect); once that job
/// reimports a conversation it drops out of the polluted set and this
/// function picks it up normally on its next call.
///
/// Guarded by [`RESTAMP_META_KEY`], written only when a pass leaves zero
/// unmatched conversations and zero unmatched chunks. A pass that still has
/// genuine (non-polluted) drift persists [`RestampProgress`] under
/// [`RESTAMP_PROGRESS_META_KEY`] and is retried by the caller on a later
/// tick, up to [`RESTAMP_MAX_ATTEMPTS`] — past that this returns
/// immediately with `attempts_exhausted: true` rather than re-scanning the
/// whole corpus every tick for a file that will never start matching.
pub fn restamp_codex_chunk_timestamps(storage: &Storage) -> Result<RestampStats> {
    if storage.get_meta(RESTAMP_META_KEY)?.is_some() {
        return Ok(RestampStats {
            already_applied: true,
            ..RestampStats::default()
        });
    }

    let prior = storage
        .get_meta(RESTAMP_PROGRESS_META_KEY)?
        .and_then(|raw| serde_json::from_str::<RestampProgress>(&raw).ok())
        .unwrap_or_default();
    if prior.attempts >= RESTAMP_MAX_ATTEMPTS {
        return Ok(RestampStats {
            attempts: prior.attempts,
            attempts_exhausted: true,
            conversations_unmatched: prior.conversations_unmatched,
            chunks_unmatched: prior.chunks_unmatched,
            ..RestampStats::default()
        });
    }

    let polluted = polluted_codex_conversation_ids(storage)?;
    let rows = storage.import_state_rows_for_conversation_prefix("codex:")?;
    let mut stats = RestampStats::default();

    for (file_path, conversation_id) in rows {
        stats.conversations_considered += 1;
        if polluted.contains(&conversation_id) {
            stats.conversations_skipped_polluted += 1;
            continue;
        }
        let path = Path::new(&file_path);
        if !path.exists() {
            stats.conversations_missing_file += 1;
            continue;
        }
        if !storage.is_file_imported(path).unwrap_or(false) {
            // The file changed since it was last imported (grew, or was
            // rewritten) — comparing its current content against the stale
            // `import_state` row would produce a spurious content mismatch
            // that has nothing to do with harness pollution or genuine
            // drift. An ordinary `import_changed_rollouts` pass already
            // picks up the new mtime on its own next tick, so this is left
            // for that path rather than counted toward `conversations_
            // unmatched` (which would otherwise burn a restamp attempt on
            // every conversation that merely grew between listing and
            // re-derivation).
            stats.conversations_pending_import += 1;
            continue;
        }
        let Some(metadata) = scan_rollout_metadata(path)? else {
            // Vanished between the exists() check and open — same bucket as
            // "missing": nothing to fix, not fabricated.
            stats.conversations_missing_file += 1;
            continue;
        };
        if metadata.conversation_id != conversation_id {
            // Filename reused for a different session (rare) — skip rather
            // than cross-stamp another conversation's chunks.
            stats.conversations_id_mismatch += 1;
            continue;
        }

        // Accumulate this conversation's re-derived chunks and apply them in
        // ONE transaction (`restamp_chunk_timestamps_batch`) instead of one
        // autocommit UPDATE per chunk — a few thousand rows at most per
        // conversation, so this stays small in memory.
        let mut pending: Vec<(String, String, String, String)> = Vec::new();
        stream_rollout_chunk_batches(
            path,
            &metadata,
            ROLLOUT_EMBED_BATCH_SIZE,
            |chunks, _evidence| {
                pending.extend(chunks.iter().map(|chunk| {
                    (
                        chunk.id.clone(),
                        chunk.conversation_id.clone(),
                        chunk.content.clone(),
                        chunk.timestamp.clone(),
                    )
                }));
                Ok(())
            },
        )?;

        let (updated_here, unmatched_here) = storage.restamp_chunk_timestamps_batch(&pending)?;

        stats.chunks_updated += updated_here;
        stats.chunks_unmatched += unmatched_here;
        if updated_here > 0 {
            stats.conversations_updated += 1;
        } else if unmatched_here > 0 {
            stats.conversations_unmatched += 1;
        }
    }

    stats.attempts = prior.attempts + 1;
    // Always persist the receipt (not just on an incomplete pass) so
    // `csr-engine status` has something to show even for a run that
    // completed on its first attempt.
    let progress = RestampProgress {
        attempts: stats.attempts,
        conversations_considered: stats.conversations_considered,
        conversations_updated: stats.conversations_updated,
        conversations_unmatched: stats.conversations_unmatched,
        conversations_missing_file: stats.conversations_missing_file,
        conversations_skipped_polluted: stats.conversations_skipped_polluted,
        conversations_pending_import: stats.conversations_pending_import,
        chunks_updated: stats.chunks_updated,
        chunks_unmatched: stats.chunks_unmatched,
    };
    storage.set_meta(
        RESTAMP_PROGRESS_META_KEY,
        &serde_json::to_string(&progress)?,
    )?;
    if stats.conversations_unmatched == 0 && stats.chunks_unmatched == 0 {
        storage.set_meta(RESTAMP_META_KEY, &chrono::Utc::now().to_rfc3339())?;
    }
    Ok(stats)
}

/// Meta key guarding [`backfill_codex_subagent_sidechain`]. Present once the
/// one-shot pass has run to completion on a database.
pub const SUBAGENT_SIDECHAIN_META_KEY: &str = "codex_subagent_sidechain_backfill_v1";

/// Receipt for [`backfill_codex_subagent_sidechain`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct SubagentSidechainBackfillStats {
    /// `import_state` rows with `conversation_id LIKE 'codex:%'` examined.
    pub conversations_considered: usize,
    /// Of those, how many rollouts carry a `session_meta.payload.source.subagent`
    /// key (a `thread_spawn` or other subagent child).
    pub conversations_subagent: usize,
    /// Subagent rows whose source file no longer exists on disk — left
    /// unchanged, not fixable here.
    pub conversations_missing_file: usize,
    /// Stored chunk rows flipped to `is_sidechain = 1` across every subagent
    /// conversation found.
    pub chunks_updated: usize,
    /// True when the meta marker was already present — no work was done.
    pub already_applied: bool,
}

/// One-time, meta-guarded backfill: for every already-imported `codex:`
/// conversation whose rollout file's `session_meta.payload.source` carries a
/// `subagent` key, mark its stored chunks `is_sidechain = 1` so the existing
/// Claude-side sidechain exclusion in `get_recent_chunks` applies to it too
/// — a `thread_spawn` child otherwise took its own recent-work slot right
/// next to its parent (see `RolloutMetadata::is_subagent`'s doc).
///
/// The forward-fix alone (`visit_rollout_message_chunks` now reads
/// `metadata.is_subagent`) only reaches rollouts that get reimported; this
/// reaches the rest with a direct `UPDATE` — no reimport needed, since
/// content and embeddings are unaffected by this flag.
pub fn backfill_codex_subagent_sidechain(
    storage: &Storage,
) -> Result<SubagentSidechainBackfillStats> {
    if storage.get_meta(SUBAGENT_SIDECHAIN_META_KEY)?.is_some() {
        return Ok(SubagentSidechainBackfillStats {
            already_applied: true,
            ..SubagentSidechainBackfillStats::default()
        });
    }

    let rows = storage.import_state_rows_for_conversation_prefix("codex:")?;
    let mut stats = SubagentSidechainBackfillStats::default();
    for (file_path, conversation_id) in rows {
        stats.conversations_considered += 1;
        let path = Path::new(&file_path);
        if !path.exists() {
            stats.conversations_missing_file += 1;
            continue;
        }
        let Some(metadata) = scan_rollout_metadata(path)? else {
            stats.conversations_missing_file += 1;
            continue;
        };
        if metadata.conversation_id != conversation_id || !metadata.is_subagent {
            continue;
        }
        stats.conversations_subagent += 1;
        stats.chunks_updated += storage.mark_conversation_chunks_sidechain(&conversation_id)?;
    }
    storage.set_meta(
        SUBAGENT_SIDECHAIN_META_KEY,
        &chrono::Utc::now().to_rfc3339(),
    )?;
    Ok(stats)
}

/// Distinct `codex_rollout` conversation ids that still carry pre-strip
/// harness-marker content — the live signal for "this conversation was
/// imported (or last reimported) before the harness-marker strip and its
/// stored chunk content is still the pre-strip text". Shared by
/// [`invalidate_polluted_codex_rollouts`] (CLI, mtime-invalidate only), the
/// daemon-owned [`harness_strip_batch`] (reimports directly), and
/// [`restamp_codex_chunk_timestamps`] (skips them rather than mis-flagging
/// them as drifted).
///
/// Checked two ways, unioned:
/// - `summary` (the first stored chunk's content) — cheap, catches the
///   common case where the harness block is the conversation's opening turn.
/// - *any* stored chunk's content — a resumed or multi-turn session can have
///   a real, non-harness first user message but still carry a harness block
///   re-injected on a later turn (Codex re-injects `<recommended_plugins>` /
///   `<environment_context>` etc. on each resume). A `summary`-only scan is
///   permanently blind to that row: see the W3-R review finding this closes.
fn polluted_codex_conversation_ids(storage: &Storage) -> Result<std::collections::HashSet<String>> {
    let mut ids: std::collections::HashSet<String> = storage
        .codex_conversation_summaries()?
        .into_iter()
        .filter(|(_, summary)| summary.as_deref().is_some_and(is_harness_context_item))
        .map(|(conversation_id, _)| conversation_id)
        .collect();
    ids.extend(storage.codex_conversations_with_harness_content(CODEX_HARNESS_MARKERS)?);
    Ok(ids)
}

/// Meta key guarding [`invalidate_polluted_codex_rollouts`]. Present once the
/// one-shot re-import pass has run to completion on a database. CLI-only
/// (`csr-engine backfill-codex-harness-strip`) — the live-corpus fix path is
/// the daemon-owned [`harness_strip_batch`] / [`HARNESS_STRIP_V2_META_KEY`]
/// below, which reimports directly instead of only invalidating mtimes for
/// a subsequent unbounded `import_changed_rollouts` pass to pick up.
pub const HARNESS_STRIP_META_KEY: &str = "codex_harness_strip_v1";

/// Receipt for one run of [`invalidate_polluted_codex_rollouts`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct HarnessStripBackfillStats {
    /// `import_state` rows with `conversation_id LIKE 'codex:%'` examined.
    pub conversations_considered: usize,
    /// Of those, how many carry a harness-marker-prefixed `summary` (the
    /// live-corpus signal for "imported before the WP-A adapter fix").
    pub conversations_polluted: usize,
    /// Polluted rows whose `import_state.file_mtime` was cleared so the next
    /// `import_changed_rollouts` pass re-imports them through the
    /// marker-stripping adapter.
    pub conversations_invalidated: usize,
    /// Polluted rows whose source file no longer exists on disk (capture-on-
    /// appearance) — left unchanged, not fixable here.
    pub conversations_missing_file: usize,
    /// True when the meta marker was already present — no work was done.
    pub already_applied: bool,
}

/// One-time, idempotent backfill for rollouts imported before the WP-A
/// adapter fix (`canonical_message`'s harness-marker stripping): every
/// already-imported `codex_rollout` conversation whose stored `summary`
/// starts with a `CODEX_HARNESS_MARKERS` entry is a conversation that was
/// chunked with the harness boilerplate as its first, second-class-citizen
/// content. The fix alone only reaches rollouts that get reimported because
/// their mtime changes; this reaches the rest by clearing their stored
/// `import_state.file_mtime` (`Storage::invalidate_import_state_mtime`) so
/// the next `import_changed_rollouts` discovery pass picks them up and
/// re-runs them through the now-fixed adapter — full delete+reinsert via the
/// existing `import_rollout` path, not an in-place patch.
///
/// Rollouts whose file is gone on disk are left untouched and counted in
/// `conversations_missing_file`, never fabricated. This function only
/// invalidates; it does not itself call `import_changed_rollouts` — the
/// caller does that as a normal follow-up pass.
/// Guarded by [`HARNESS_STRIP_META_KEY`] so it runs at most once per
/// database.
pub fn invalidate_polluted_codex_rollouts(engine: &Engine) -> Result<HarnessStripBackfillStats> {
    let storage = engine.storage();
    if storage.get_meta(HARNESS_STRIP_META_KEY)?.is_some() {
        return Ok(HarnessStripBackfillStats {
            already_applied: true,
            ..HarnessStripBackfillStats::default()
        });
    }

    let polluted = polluted_codex_conversation_ids(storage)?;

    let rows = storage.import_state_rows_for_conversation_prefix("codex:")?;
    let mut stats = HarnessStripBackfillStats::default();
    for (file_path, conversation_id) in rows {
        stats.conversations_considered += 1;
        if !polluted.contains(&conversation_id) {
            continue;
        }
        stats.conversations_polluted += 1;
        if !Path::new(&file_path).exists() {
            stats.conversations_missing_file += 1;
            continue;
        }
        storage.invalidate_import_state_mtime(&file_path)?;
        stats.conversations_invalidated += 1;
    }

    storage.set_meta(HARNESS_STRIP_META_KEY, &chrono::Utc::now().to_rfc3339())?;
    Ok(stats)
}

/// Meta key guarding [`harness_strip_batch`]'s completion. Present once a
/// batch left zero *file-having* polluted conversations remaining. Distinct
/// from [`HARNESS_STRIP_META_KEY`] (`_v1`, CLI mtime-invalidate) — `_v2` is
/// the daemon-owned, directly-reimporting path and is the one that actually
/// clears the live corpus, since `_v1` alone still needs an unbounded
/// `import_changed_rollouts` pass afterward to do the reimporting.
pub const HARNESS_STRIP_V2_META_KEY: &str = "codex_harness_strip_v2";

/// Meta key holding the JSON-encoded [`HarnessStripProgress`] accumulated
/// across every [`harness_strip_batch`] call so far — cumulative totals plus
/// the most recent batch's timing, surfaced in `csr-engine status`.
pub const HARNESS_STRIP_V2_PROGRESS_META_KEY: &str = "codex_harness_strip_v2_progress";

/// Conversations reimported per [`harness_strip_batch`] call. Small enough
/// that one batch (file I/O + chunk + embed + HNSW insert per conversation)
/// stays well inside a single idle-gated daemon tick, and the shared
/// `heavy_work` permit is held per-batch rather than for the whole backlog.
pub const HARNESS_STRIP_V2_BATCH_SIZE: usize = 25;

/// Cumulative, persisted progress for the daemon-owned harness-strip job.
/// JSON-encoded under [`HARNESS_STRIP_V2_PROGRESS_META_KEY`].
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HarnessStripProgress {
    pub reimported_total: usize,
    pub missing_file_total: usize,
    pub failed_total: usize,
    pub last_batch_at: Option<String>,
    pub last_batch_chunks: usize,
    pub last_batch_secs: f64,
    /// File-having polluted conversations remaining as of the most recent
    /// batch (mirrors [`HarnessStripBatchStats::polluted_remaining`]).
    /// Persisted here so `csr-engine status` — including the `--compact`
    /// statusline refresh, which shells out on a tight interval — can report
    /// this without running its own live
    /// `polluted_codex_rows_by_file_presence` scan on every call. `None`
    /// before the very first batch has run: genuinely unknown, not zero.
    #[serde(default)]
    pub polluted_remaining: Option<usize>,
}

/// Receipt for one [`harness_strip_batch`] call.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
pub struct HarnessStripBatchStats {
    /// Still-polluted conversations seen at the start of this batch
    /// (file-having or not), before this batch's work.
    pub polluted_considered: usize,
    /// Conversations this batch successfully reimported (delete+reinsert
    /// via `import_rollout`, harness markers now stripped by the fixed
    /// adapter, timestamps correct as a side effect).
    pub reimported: usize,
    /// Conversations in this batch whose file no longer exists on disk —
    /// left polluted forever (capture-on-appearance), never fixable here,
    /// and never counted against [`HARNESS_STRIP_V2_META_KEY`]'s done gate.
    pub missing_file: usize,
    /// Conversations in this batch that errored during reimport (e.g.
    /// database busy retries exhausted) — retried on a later batch since
    /// they remain polluted (their summary is unchanged).
    pub failed: usize,
    /// Total chunks re-embedded across this batch's reimports.
    pub chunks_reimported: usize,
    /// File-having polluted conversations remaining after this batch. The
    /// done-key is written exactly when this reaches 0.
    pub polluted_remaining: usize,
    /// Wall time for this batch alone (reimport + requery), for throughput
    /// projection (`chunks_reimported / batch_secs`).
    pub batch_secs: f64,
    /// True once this batch drove `polluted_remaining` to 0 and the done
    /// key was written.
    pub done: bool,
    /// True when the done key was already present — no work was done.
    pub already_applied: bool,
}

/// Still-polluted `codex:` conversations with a `(file_path, conversation_id)`
/// pair, split by whether the file still exists on disk. Requeried fresh on
/// every call — reimporting a conversation clears its stored `summary`, so a
/// conversation drops out of this set the moment [`harness_strip_batch`]
/// fixes it. This is what makes the job resumable with no separate cursor:
/// an interrupted run just leaves more rows for the next call to find.
pub(crate) fn polluted_codex_rows_by_file_presence(
    storage: &Storage,
) -> Result<(Vec<(PathBuf, String)>, usize)> {
    let polluted = polluted_codex_conversation_ids(storage)?;
    let rows = storage.import_state_rows_for_conversation_prefix("codex:")?;
    let mut with_file = Vec::new();
    let mut missing = 0usize;
    for (file_path, conversation_id) in rows {
        if !polluted.contains(&conversation_id) {
            continue;
        }
        let path = PathBuf::from(&file_path);
        if path.exists() {
            with_file.push((path, conversation_id));
        } else {
            missing += 1;
        }
    }
    Ok((with_file, missing))
}

/// One daemon-tick batch of the harness-strip re-import: takes up to
/// `batch_size` still-polluted conversations whose source file exists on
/// disk and reimports each directly through `import_rollout` — the same
/// delete+reinsert path (incl. HNSW tombstone-then-reinsert) an ordinary
/// changed-mtime pickup uses — rather than only invalidating `file_mtime`
/// and deferring the actual work to a subsequent unbounded
/// `import_changed_rollouts` pass (`invalidate_polluted_codex_rollouts`'s
/// approach, kept for the CLI). No embedding batching across conversations:
/// each conversation's own `import_rollout` already batches internally
/// (`ROLLOUT_EMBED_BATCH_SIZE`).
///
/// Idempotent per call and safe to interleave with normal codex-rollout
/// import passes: reimporting the same conversation twice just repeats the
/// same delete+reinsert. Guarded by [`HARNESS_STRIP_V2_META_KEY`], written
/// only when the file-having polluted count reaches 0 — conversations whose
/// file is gone stay polluted forever and never block completion (mirrors
/// every other backfill's `conversations_missing_file` convention).
///
/// `shutdown` is checked between conversations (never mid-conversation, so a
/// single `import_rollout` always finishes its own delete+reinsert
/// transaction cleanly) — the daemon's shutdown sequence awaits this
/// function with no timeout, and the largest real codex conversations run
/// tens of seconds each, comfortably past `launchctl`'s default
/// `ExitTimeOut`. Breaking early here just leaves the remainder polluted for
/// the next batch to pick up (this job is already fully resumable with no
/// separate cursor), instead of risking a SIGKILL mid-batch that leaves the
/// DB holding chunks the on-disk HNSW index doesn't.
pub fn harness_strip_batch(
    engine: &Engine,
    batch_size: usize,
    shutdown: &AtomicBool,
) -> Result<HarnessStripBatchStats> {
    anyhow::ensure!(batch_size > 0, "harness-strip batch size must be positive");
    let storage = engine.storage();
    if storage.get_meta(HARNESS_STRIP_V2_META_KEY)?.is_some() {
        return Ok(HarnessStripBatchStats {
            already_applied: true,
            ..HarnessStripBatchStats::default()
        });
    }

    let started = std::time::Instant::now();
    let (candidates, _missing_before) = polluted_codex_rows_by_file_presence(storage)?;
    let mut stats = HarnessStripBatchStats {
        polluted_considered: candidates.len(),
        ..HarnessStripBatchStats::default()
    };

    for (path, _conversation_id) in candidates.into_iter().take(batch_size) {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let Some(mtime) = file_mtime(&path) else {
            // Vanished between the listing above and now — same bucket as
            // "missing", not fabricated, retried next batch if it reappears.
            stats.missing_file += 1;
            continue;
        };
        let file = RolloutFile { path, mtime };
        match import_rollout_with_retries(engine, &file) {
            Ok(Some(outcome)) => {
                stats.reimported += 1;
                stats.chunks_reimported += outcome.chunks;
            }
            Ok(None) => stats.missing_file += 1, // vanished before it could be read
            Err(error) => {
                stats.failed += 1;
                tracing::warn!(
                    file = %file.path.display(),
                    %error,
                    "codex harness-strip batch reimport failed"
                );
            }
        }
    }
    stats.batch_secs = started.elapsed().as_secs_f64();

    let (remaining, _missing_after) = polluted_codex_rows_by_file_presence(storage)?;
    stats.polluted_remaining = remaining.len();

    let mut progress = storage
        .get_meta(HARNESS_STRIP_V2_PROGRESS_META_KEY)?
        .and_then(|raw| serde_json::from_str::<HarnessStripProgress>(&raw).ok())
        .unwrap_or_default();
    progress.reimported_total += stats.reimported;
    progress.missing_file_total += stats.missing_file;
    progress.failed_total += stats.failed;
    progress.last_batch_at = Some(chrono::Utc::now().to_rfc3339());
    progress.last_batch_chunks = stats.chunks_reimported;
    progress.last_batch_secs = stats.batch_secs;
    progress.polluted_remaining = Some(stats.polluted_remaining);
    storage.set_meta(
        HARNESS_STRIP_V2_PROGRESS_META_KEY,
        &serde_json::to_string(&progress)?,
    )?;

    if stats.polluted_remaining == 0 {
        storage.set_meta(HARNESS_STRIP_V2_META_KEY, &chrono::Utc::now().to_rfc3339())?;
        stats.done = true;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::EmbeddingEngine;
    use crate::search::SearchEngine;
    use crate::storage::Storage;
    use std::path::Path;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn test_engine(storage: Arc<Storage>, root: &Path) -> Engine {
        Engine::from_parts(
            storage,
            Arc::new(EmbeddingEngine::new().unwrap()),
            Arc::new(RwLock::new(SearchEngine::new(64))),
            root.to_path_buf(),
        )
    }

    #[test]
    fn rollout_receipt_uses_the_actual_jsonl_byte_offset() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rollout-offset.jsonl");
        let metadata =
            r#"{"type":"session_meta","payload":{"id":"offset-fixture","cwd":"/tmp/project"}}"#;
        let message = r#"{"type":"message","role":"user","content":"remember this"}"#;
        std::fs::write(&path, format!("{metadata}\n{message}\n")).unwrap();

        let mut observed = Vec::new();
        visit_rollout_messages(&path, |message| {
            observed.push(
                message
                    .get("_csr_receipt_ref")
                    .and_then(serde_json::Value::as_str)
                    .unwrap()
                    .to_string(),
            );
            Ok(())
        })
        .unwrap();

        assert_eq!(
            observed,
            vec![format!("{}#byte={}", path.display(), metadata.len() + 1)]
        );
    }

    #[test]
    fn codex_response_channels_distinguish_assistant_from_unclassified_payloads() {
        let mut assistant = Vec::new();
        let mut misses = 0;
        let mut stripped = 0;
        parse_response_item(
            &serde_json::json!({
                "type":"response_item",
                "payload":{"type":"agent_message", "content":"assistant prose"}
            }),
            &mut assistant,
            &mut misses,
            &mut stripped,
        );
        assert_eq!(assistant.len(), 1);
        assert!(assistant[0].get("_csr_channel").is_none());

        let mut unknown = Vec::new();
        parse_response_item(
            &serde_json::json!({
                "type":"response_item",
                "payload":{"type":"future_payload", "text":"unclassified prose"}
            }),
            &mut unknown,
            &mut misses,
            &mut stripped,
        );
        assert_eq!(unknown.len(), 1);
        assert_eq!(
            unknown[0]
                .get("_csr_channel")
                .and_then(serde_json::Value::as_str),
            Some("unclassified")
        );
    }

    #[test]
    fn imported_rollout_chunks_receive_cached_structural_floors() {
        let root = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let target = root.path().join("rollout-copy.jsonl");
        std::fs::copy(source, &target).unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());

        let stats = import_changed_rollouts(&engine, root.path()).unwrap();
        assert_eq!(stats.files_imported, 1);
        let ids = storage
            .get_chunk_ids_for_conversation("codex:modern-fixture")
            .unwrap();
        assert!(!ids.is_empty());
        let tiers = ids
            .iter()
            .map(|id| storage.get_chunk_min_trust(id).unwrap())
            .collect::<Vec<_>>();
        assert!(tiers.contains(&crate::provenance::TrustTier::UserHistory));
        assert!(tiers.contains(&crate::provenance::TrustTier::TrustedTool));
        assert!(tiers.contains(&crate::provenance::TrustTier::Unknown));
    }

    #[test]
    fn rollout_chunks_carry_distinct_per_message_timestamps() {
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let parsed = parse_rollout(&source).unwrap().expect("fixture parses");
        let chunks = rollout_chunks(&parsed);
        assert!(!chunks.is_empty());
        let timestamps = chunks
            .iter()
            .map(|chunk| chunk.timestamp.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            timestamps.len() >= 3,
            "expected at least 3 distinct chunk timestamps, got {timestamps:?}"
        );
        assert!(
            !timestamps.iter().all(|ts| ts == &parsed.timestamp),
            "chunks must not all collapse onto the session-start timestamp {}",
            parsed.timestamp
        );
    }

    /// Fixture line 8 is an unknown top-level type (`future_envelope`) that
    /// carries its own timestamp (12:00:07), arriving well after the
    /// session-start latch (12:00:00) was already set by line 1. The
    /// `Some(_)` branch in `visit_rollout_messages_inner` used to pass the
    /// latched `timestamp.as_deref()` regardless, so this chunk landed on
    /// 12:00:00 instead of its own line's time.
    #[test]
    fn unknown_type_lines_keep_their_own_timestamp_not_the_session_latch() {
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let parsed = parse_rollout(&source).unwrap().expect("fixture parses");
        let chunks = rollout_chunks(&parsed);
        let future_envelope = chunks
            .iter()
            .find(|chunk| chunk.content.contains("UNKNOWN TEXT RETAINED"))
            .expect("future_envelope line produces a chunk");
        assert_eq!(future_envelope.timestamp, "2026-08-06T12:00:07Z");
    }

    #[test]
    fn imported_rollout_chunks_carry_the_messages_own_timestamp_not_the_session_start() {
        let root = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let target = root.path().join("rollout-copy.jsonl");
        std::fs::copy(source, &target).unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());

        let stats = import_changed_rollouts(&engine, root.path()).unwrap();
        assert_eq!(stats.files_imported, 1);
        let ids = storage
            .get_chunk_ids_for_conversation("codex:modern-fixture")
            .unwrap();
        assert!(!ids.is_empty());
        let chunks = storage.get_chunks_by_ids(&ids).unwrap();
        let timestamps = chunks
            .iter()
            .map(|chunk| chunk.timestamp.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            timestamps.len() > 1,
            "expected chunks to carry distinct per-message timestamps, got {timestamps:?}"
        );
        assert!(
            !timestamps.iter().all(|ts| ts == "2026-08-06T12:00:00Z"),
            "chunks must not all collapse onto the session-start timestamp"
        );
    }

    #[test]
    fn restamp_backfill_repairs_collapsed_timestamps_and_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let target = root.path().join("rollout-copy.jsonl");
        std::fs::copy(source, &target).unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());

        let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
        assert_eq!(import_stats.files_imported, 1);

        let ids = storage
            .get_chunk_ids_for_conversation("codex:modern-fixture")
            .unwrap();
        assert!(!ids.is_empty());
        let chunks = storage.get_chunks_by_ids(&ids).unwrap();

        // Simulate the pre-fix bug directly: collapse every chunk's
        // timestamp onto the session-start time, exactly what the old
        // `timestamp: metadata.timestamp.clone()` produced. Content is
        // unchanged, so the content-guarded setter still matches.
        for chunk in &chunks {
            let touched = storage
                .restamp_chunk_timestamp_if_content_matches(
                    &chunk.id,
                    &chunk.conversation_id,
                    &chunk.content,
                    "2026-08-06T12:00:00Z",
                )
                .unwrap();
            assert!(
                touched,
                "setup: content guard should match unchanged content"
            );
        }
        let collapsed = storage.get_chunks_by_ids(&ids).unwrap();
        let collapsed_timestamps: std::collections::BTreeSet<_> =
            collapsed.iter().map(|c| c.timestamp.clone()).collect();
        assert_eq!(
            collapsed_timestamps.len(),
            1,
            "setup: simulated collapse should leave exactly one distinct timestamp"
        );

        let fts_blocks_before = storage
            .with_connection(|conn| {
                conn.query_row("SELECT COUNT(*) FROM chunks_fts_data", [], |r| {
                    r.get::<_, i64>(0)
                })
                .map_err(Into::into)
            })
            .unwrap();

        let restamp_stats = restamp_codex_chunk_timestamps(&storage).unwrap();
        assert!(!restamp_stats.already_applied);
        assert_eq!(restamp_stats.conversations_considered, 1);

        // The content-guarded restamp UPDATE is timestamp-only — with
        // chunks_fts_au scoped to `UPDATE OF content`, it must not touch
        // chunks_fts_data at all (the FTS-bloat regression this fix closes).
        let fts_blocks_after = storage
            .with_connection(|conn| {
                conn.query_row("SELECT COUNT(*) FROM chunks_fts_data", [], |r| {
                    r.get::<_, i64>(0)
                })
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(
            fts_blocks_before, fts_blocks_after,
            "restamp must not grow chunks_fts_data"
        );
        assert_eq!(restamp_stats.conversations_updated, 1);
        assert_eq!(restamp_stats.conversations_missing_file, 0);
        assert_eq!(restamp_stats.conversations_id_mismatch, 0);
        assert_eq!(restamp_stats.conversations_unmatched, 0);
        assert_eq!(restamp_stats.chunks_updated, ids.len());
        assert_eq!(restamp_stats.chunks_unmatched, 0);

        let repaired = storage.get_chunks_by_ids(&ids).unwrap();
        let repaired_timestamps: std::collections::BTreeSet<_> =
            repaired.iter().map(|c| c.timestamp.clone()).collect();
        assert!(
            repaired_timestamps.len() > 1,
            "restamp should restore distinct per-message timestamps, got {repaired_timestamps:?}"
        );

        // Idempotent: a second run is a no-op guarded by the meta marker.
        let second = restamp_codex_chunk_timestamps(&storage).unwrap();
        assert!(second.already_applied);
        assert_eq!(second.chunks_updated, 0);
        assert_eq!(second.conversations_considered, 0);
    }

    #[test]
    fn restamp_backfill_counts_missing_files_without_fabricating() {
        let root = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let target = root.path().join("rollout-copy.jsonl");
        std::fs::copy(source, &target).unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());

        let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
        assert_eq!(import_stats.files_imported, 1);

        // Capture-on-appearance: the source rollout is gone by backfill time.
        std::fs::remove_file(&target).unwrap();

        let stats = restamp_codex_chunk_timestamps(&storage).unwrap();
        assert!(!stats.already_applied);
        assert_eq!(stats.conversations_considered, 1);
        assert_eq!(stats.conversations_missing_file, 1);
        assert_eq!(stats.conversations_updated, 0);
        assert_eq!(stats.chunks_updated, 0);
    }

    #[test]
    fn harness_strip_backfill_invalidates_only_polluted_rows_and_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let target = root.path().join("rollout-copy.jsonl");
        std::fs::copy(source, &target).unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());

        let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
        assert_eq!(import_stats.files_imported, 1);

        // Simulate the pre-fix pollution directly, the same way the restamp
        // tests simulate the pre-fix timestamp bug: stamp the stored
        // `summary` with harness boilerplate, as if this row had been
        // imported before the WP-A adapter strip existed.
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET summary = ?1 WHERE conversation_id = 'codex:modern-fixture'",
                    rusqlite::params![
                        "<recommended_plugins>\nHere is a list of plugins that are available but not installed"
                    ],
                )?;
                Ok(())
            })
            .unwrap();

        let target_key = target.to_string_lossy().to_string();
        let original_mtime = storage
            .get_import_state_mtime(&target_key)
            .unwrap()
            .unwrap();
        assert!(!original_mtime.is_empty());

        let stats = invalidate_polluted_codex_rollouts(&engine).unwrap();
        assert!(!stats.already_applied);
        assert_eq!(stats.conversations_considered, 1);
        assert_eq!(stats.conversations_polluted, 1);
        assert_eq!(stats.conversations_invalidated, 1);
        assert_eq!(stats.conversations_missing_file, 0);

        let cleared_mtime = storage
            .get_import_state_mtime(&target_key)
            .unwrap()
            .unwrap();
        assert_eq!(
            cleared_mtime, "",
            "invalidated row's mtime must no longer match the real file"
        );

        // The next discovery pass re-imports the invalidated row through the
        // (already-fixed) adapter, overwriting the simulated pollution.
        let reimport_stats = import_changed_rollouts(&engine, root.path()).unwrap();
        assert_eq!(reimport_stats.files_imported, 1);
        let ids = storage
            .get_chunk_ids_for_conversation("codex:modern-fixture")
            .unwrap();
        let chunks = storage.get_chunks_by_ids(&ids).unwrap();
        assert!(
            chunks.iter().all(|c| !c
                .summary
                .as_deref()
                .unwrap_or_default()
                .starts_with("<recommended_plugins>")),
            "reimport must clear the simulated pollution: {:?}",
            chunks.iter().map(|c| &c.summary).collect::<Vec<_>>()
        );

        // Idempotent: a second backfill run is a no-op guarded by the meta marker.
        let second = invalidate_polluted_codex_rollouts(&engine).unwrap();
        assert!(second.already_applied);
        assert_eq!(second.conversations_considered, 0);
        assert_eq!(second.conversations_invalidated, 0);
    }

    #[test]
    fn harness_strip_backfill_counts_missing_files_without_touching_clean_rows() {
        let root = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        // One polluted row whose file will vanish, one clean row that must be
        // left completely alone.
        let polluted_target = root.path().join("rollout-polluted.jsonl");
        let clean_target = root.path().join("rollout-clean.jsonl");
        std::fs::copy(&source, &polluted_target).unwrap();
        std::fs::copy(&source, &clean_target).unwrap();
        // Distinct conversation ids: the fixture's session_meta id is fixed,
        // so give the second copy its own id by rewriting the file.
        let rewritten = std::fs::read_to_string(&source)
            .unwrap()
            .replace("modern-fixture", "modern-fixture-clean");
        std::fs::write(&clean_target, rewritten).unwrap();

        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());
        let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
        assert_eq!(import_stats.files_imported, 2);

        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET summary = ?1 WHERE conversation_id = 'codex:modern-fixture'",
                    rusqlite::params!["<environment_context>\ncwd: /tmp\n</environment_context>"],
                )?;
                Ok(())
            })
            .unwrap();
        let clean_mtime_before = storage
            .get_import_state_mtime(&clean_target.to_string_lossy())
            .unwrap()
            .unwrap();

        // Capture-on-appearance: the polluted rollout's source file is gone
        // by backfill time.
        std::fs::remove_file(&polluted_target).unwrap();

        let stats = invalidate_polluted_codex_rollouts(&engine).unwrap();
        assert!(!stats.already_applied);
        assert_eq!(stats.conversations_considered, 2);
        assert_eq!(stats.conversations_polluted, 1);
        assert_eq!(stats.conversations_invalidated, 0);
        assert_eq!(stats.conversations_missing_file, 1);

        // The clean row's mtime must be untouched.
        let clean_mtime_after = storage
            .get_import_state_mtime(&clean_target.to_string_lossy())
            .unwrap()
            .unwrap();
        assert_eq!(clean_mtime_before, clean_mtime_after);
    }

    /// Regression for the wave-2 finding: a harness-polluted conversation
    /// (stored content is pre-strip, so the content guard can never match
    /// what the fixed chunker re-derives from the on-disk file) must be
    /// *skipped*, not counted `conversations_unmatched` — otherwise the done
    /// key can never be set while any pollution remains anywhere in the
    /// corpus, freezing the fix for every other conversation too. Reverting
    /// the `polluted.contains(&conversation_id)` skip in
    /// `restamp_codex_chunk_timestamps` makes this fail: `conversations_unmatched`
    /// becomes 1, `conversations_skipped_polluted` stays 0, and the meta key
    /// is never set.
    #[test]
    fn restamp_skips_harness_polluted_conversations_and_still_completes_for_the_rest() {
        let root = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let polluted_target = root.path().join("rollout-polluted.jsonl");
        let clean_target = root.path().join("rollout-clean.jsonl");
        std::fs::copy(&source, &polluted_target).unwrap();
        let rewritten = std::fs::read_to_string(&source)
            .unwrap()
            .replace("modern-fixture", "modern-fixture-clean");
        std::fs::write(&clean_target, rewritten).unwrap();

        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());
        let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
        assert_eq!(import_stats.files_imported, 2);

        // Simulate the pre-strip pollution directly (same technique as the
        // harness-strip backfill tests above), but also corrupt the stored
        // `content` on seq 0 so the content guard genuinely cannot match —
        // the on-disk file (already fixed) re-derives clean text, while the
        // stored row still carries the old harness-prefixed content a
        // pre-fix import would have produced.
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET summary = ?1 WHERE conversation_id = 'codex:modern-fixture'",
                    rusqlite::params![
                        "<recommended_plugins>\nHere is a list of plugins that are available but not installed"
                    ],
                )?;
                conn.execute(
                    "UPDATE chunks SET content = ?1 || content
                     WHERE conversation_id = 'codex:modern-fixture' AND seq = 0",
                    rusqlite::params![
                        "<recommended_plugins>\nHere is a list of plugins that are available but not installed\n"
                    ],
                )?;
                Ok(())
            })
            .unwrap();

        let stats = restamp_codex_chunk_timestamps(&storage).unwrap();
        assert!(!stats.already_applied);
        assert_eq!(stats.conversations_considered, 2);
        assert_eq!(
            stats.conversations_skipped_polluted, 1,
            "the polluted conversation must be skipped, not re-derived"
        );
        assert_eq!(
            stats.conversations_unmatched, 0,
            "a skipped polluted row must never be counted as drift-unmatched"
        );
        assert_eq!(stats.chunks_unmatched, 0);
        assert_eq!(
            stats.conversations_updated, 1,
            "the clean conversation must still be restamped"
        );
        assert_eq!(stats.attempts, 1);

        assert!(
            storage.get_meta(RESTAMP_META_KEY).unwrap().is_some(),
            "outstanding harness pollution elsewhere must not block the done key \
             for an otherwise-clean pass"
        );
    }

    /// Regression for the wave-2 finding: genuine (non-harness) content
    /// drift must leave the done key unset and must be retried, bounded, on
    /// later calls — not silently marked done. Reverting the
    /// `if stats.conversations_unmatched == 0 && stats.chunks_unmatched == 0`
    /// gate (i.e. restoring the old unconditional `set_meta`) makes the
    /// first two assertions fail: the meta key would be set immediately with
    /// `conversations_unmatched == 1`.
    #[test]
    fn restamp_withholds_done_key_while_genuine_drift_leaves_chunks_unmatched() {
        let root = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let target = root.path().join("rollout-copy.jsonl");
        std::fs::copy(&source, &target).unwrap();

        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());
        import_changed_rollouts(&engine, root.path()).unwrap();

        // Genuine drift, unrelated to harness pollution: the stored content
        // no longer matches what the on-disk file re-derives, and the
        // summary is never touched (not a harness marker), so this row is
        // never in `polluted_codex_conversation_ids`.
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET content = content || ' DRIFTED'
                     WHERE conversation_id = 'codex:modern-fixture'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let first = restamp_codex_chunk_timestamps(&storage).unwrap();
        assert!(!first.already_applied);
        assert_eq!(first.conversations_skipped_polluted, 0);
        assert_eq!(first.conversations_unmatched, 1);
        assert!(first.chunks_unmatched > 0);
        assert_eq!(first.attempts, 1);
        assert!(
            storage.get_meta(RESTAMP_META_KEY).unwrap().is_none(),
            "must not claim done while unmatched chunks remain"
        );
        assert!(storage
            .get_meta(RESTAMP_PROGRESS_META_KEY)
            .unwrap()
            .is_some());

        // A later daemon tick retries and bumps the attempt count, still
        // withholding the done key.
        let second = restamp_codex_chunk_timestamps(&storage).unwrap();
        assert_eq!(second.attempts, 2);
        assert_eq!(second.conversations_unmatched, 1);
        assert!(storage.get_meta(RESTAMP_META_KEY).unwrap().is_none());
    }

    /// W3-R high review finding ("the same trap catches transient drift"):
    /// a codex rollout that grew on disk since it was last imported (new
    /// mtime, new tail content) must never be treated the same as a
    /// permanently drifted file. Pre-fix, the new tail message's re-derived
    /// chunk has no matching stored row (it was never imported), so it
    /// lands in `chunks_unmatched` and blocks the done key forever — a
    /// normal `import_changed_rollouts` pass would pick the new mtime up on
    /// its own the very next tick, so this is not this restamp's problem to
    /// solve, and definitely not an attempt-burning one. Must fail at HEAD
    /// (pre-fix: no mtime check before content re-derivation).
    #[test]
    fn restamp_treats_a_stale_mtime_as_pending_import_not_unmatched() {
        let root = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let target = root.path().join("rollout-copy.jsonl");
        std::fs::copy(&source, &target).unwrap();

        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());
        import_changed_rollouts(&engine, root.path()).unwrap();

        // The file grows after this import (a real, in-flight session
        // write) — its mtime and tail content both move on, but nothing has
        // reimported it yet.
        let extra_line = r#"{"timestamp":"2026-08-06T12:10:00Z","type":"response_item","payload":{"type":"message","id":"grew-1","role":"user","content":[{"type":"input_text","text":"a later message the stored chunks do not know about yet"}]}}"#;
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&target)
                .unwrap();
            writeln!(file, "{extra_line}").unwrap();
        }

        let stats = restamp_codex_chunk_timestamps(&storage).unwrap();
        assert_eq!(
            stats.conversations_pending_import, 1,
            "a file that changed since the last import must be skipped as pending, not scanned"
        );
        assert_eq!(
            stats.conversations_unmatched, 0,
            "transient mtime drift must never be counted as an unmatched conversation"
        );
        assert_eq!(
            stats.chunks_unmatched, 0,
            "transient mtime drift must never burn a restamp attempt via chunks_unmatched"
        );
        assert!(
            storage.get_meta(RESTAMP_META_KEY).unwrap().is_some(),
            "nothing genuinely unmatched or polluted remains, so this must still reach done"
        );
    }

    /// Retries are bounded: past `RESTAMP_MAX_ATTEMPTS` the function stops
    /// re-scanning the whole corpus on every daemon tick, but still never
    /// claims the done key for a file that will never start matching.
    #[test]
    fn restamp_stops_retrying_after_max_attempts_without_claiming_done() {
        let root = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let target = root.path().join("rollout-copy.jsonl");
        std::fs::copy(&source, &target).unwrap();

        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());
        import_changed_rollouts(&engine, root.path()).unwrap();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET content = content || ' DRIFTED'
                     WHERE conversation_id = 'codex:modern-fixture'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        for attempt in 1..=RESTAMP_MAX_ATTEMPTS {
            let stats = restamp_codex_chunk_timestamps(&storage).unwrap();
            assert!(!stats.attempts_exhausted);
            assert_eq!(stats.attempts, attempt);
        }

        let exhausted = restamp_codex_chunk_timestamps(&storage).unwrap();
        assert!(exhausted.attempts_exhausted);
        assert_eq!(exhausted.attempts, RESTAMP_MAX_ATTEMPTS);
        assert_eq!(
            exhausted.conversations_considered, 0,
            "no re-scan once exhausted"
        );
        assert!(storage.get_meta(RESTAMP_META_KEY).unwrap().is_none());
    }

    /// The daemon-owned harness-strip job processes bounded batches and is
    /// resumable with no separate cursor: each call re-queries what is still
    /// polluted, so an interrupted run just leaves more for the next call.
    #[test]
    fn harness_strip_batch_reimports_in_bounded_batches_and_is_resumable() {
        let root = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let raw = std::fs::read_to_string(&source).unwrap();
        let mut conversation_ids = Vec::new();
        for n in 0..3 {
            let id = format!("modern-fixture-{n}");
            std::fs::write(
                root.path().join(format!("rollout-{n}.jsonl")),
                raw.replace("modern-fixture", &id),
            )
            .unwrap();
            conversation_ids.push(format!("codex:{id}"));
        }

        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());
        let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
        assert_eq!(import_stats.files_imported, 3);

        for conversation_id in &conversation_ids {
            storage
                .with_connection(|conn| {
                    conn.execute(
                        "UPDATE chunks SET summary = ?1 WHERE conversation_id = ?2",
                        rusqlite::params![
                            "<recommended_plugins>\nHere is a list of plugins that are available but not installed",
                            conversation_id,
                        ],
                    )?;
                    conn.execute(
                        "UPDATE chunks SET content = ?1 || content WHERE conversation_id = ?2 AND seq = 0",
                        rusqlite::params![
                            "<recommended_plugins>\nHere is a list of plugins that are available but not installed\n",
                            conversation_id,
                        ],
                    )?;
                    Ok(())
                })
                .unwrap();
        }

        let batch1 = harness_strip_batch(&engine, 1, &AtomicBool::new(false)).unwrap();
        assert!(!batch1.already_applied);
        assert_eq!(batch1.polluted_considered, 3);
        assert_eq!(batch1.reimported, 1);
        assert_eq!(batch1.polluted_remaining, 2);
        assert!(!batch1.done);
        assert!(storage
            .get_meta(HARNESS_STRIP_V2_META_KEY)
            .unwrap()
            .is_none());

        let batch2 = harness_strip_batch(&engine, 1, &AtomicBool::new(false)).unwrap();
        assert_eq!(batch2.polluted_considered, 2);
        assert_eq!(batch2.reimported, 1);
        assert_eq!(batch2.polluted_remaining, 1);
        assert!(!batch2.done);

        // A batch larger than the remaining backlog drains it completely.
        let batch3 = harness_strip_batch(&engine, 10, &AtomicBool::new(false)).unwrap();
        assert_eq!(batch3.reimported, 1);
        assert_eq!(batch3.polluted_remaining, 0);
        assert!(batch3.done);
        assert!(storage
            .get_meta(HARNESS_STRIP_V2_META_KEY)
            .unwrap()
            .is_some());

        let batch4 = harness_strip_batch(&engine, 10, &AtomicBool::new(false)).unwrap();
        assert!(batch4.already_applied);

        // Acceptance: no chunk of any of the three conversations still
        // starts with a harness marker.
        for conversation_id in &conversation_ids {
            let ids = storage
                .get_chunk_ids_for_conversation(conversation_id)
                .unwrap();
            let chunks = storage.get_chunks_by_ids(&ids).unwrap();
            assert!(
                chunks
                    .iter()
                    .all(|c| !c.content.trim_start().starts_with("<recommended_plugins>")),
                "{conversation_id} still has a harness-prefixed chunk: {:?}",
                chunks.iter().map(|c| &c.content).collect::<Vec<_>>()
            );
        }

        // Cumulative progress persisted across the three batches.
        let progress: HarnessStripProgress = serde_json::from_str(
            &storage
                .get_meta(HARNESS_STRIP_V2_PROGRESS_META_KEY)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(progress.reimported_total, 3);
        assert_eq!(progress.missing_file_total, 0);
    }

    /// A polluted conversation whose source file is gone must never block
    /// completion — it is left polluted forever, the same
    /// `conversations_missing_file` convention every other backfill here
    /// uses, and `harness_strip_batch` must still reach `done: true` once
    /// every *file-having* polluted row is cleared.
    #[test]
    fn harness_strip_batch_completes_even_when_a_polluted_file_is_gone() {
        let root = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let present = root.path().join("rollout-present.jsonl");
        let gone = root.path().join("rollout-gone.jsonl");
        std::fs::copy(&source, &present).unwrap();
        std::fs::write(
            &gone,
            std::fs::read_to_string(&source)
                .unwrap()
                .replace("modern-fixture", "modern-fixture-gone"),
        )
        .unwrap();

        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());
        import_changed_rollouts(&engine, root.path()).unwrap();

        for (conversation_id, _) in [
            ("codex:modern-fixture", &present),
            ("codex:modern-fixture-gone", &gone),
        ] {
            storage
                .with_connection(|conn| {
                    conn.execute(
                        "UPDATE chunks SET summary = ?1 WHERE conversation_id = ?2",
                        rusqlite::params![
                            "<environment_context>\ncwd: /tmp\n</environment_context>",
                            conversation_id,
                        ],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        std::fs::remove_file(&gone).unwrap();

        let batch = harness_strip_batch(&engine, 10, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            batch.polluted_considered, 1,
            "the missing-file row is excluded up front"
        );
        assert_eq!(batch.reimported, 1);
        assert_eq!(batch.polluted_remaining, 0);
        assert!(
            batch.done,
            "a permanently missing-file row must never block completion"
        );
        assert!(storage
            .get_meta(HARNESS_STRIP_V2_META_KEY)
            .unwrap()
            .is_some());
    }

    /// End-to-end regression for the "medium" wave-2 finding: a rollout
    /// whose first user item is a real `<recommended_plugins>` harness block
    /// (seeded directly, simulating an import that predates the strip),
    /// collapsed onto a single timestamp (simulating the pre-fix
    /// timestamp-collapse bug too — both defects at once, as the live
    /// corpus actually has them). Running the harness-strip batch job and
    /// then the restamp must leave distinct per-message timestamps, no
    /// harness-prefixed chunk content, and both meta keys set only once
    /// nothing remains to fix.
    #[test]
    fn harness_strip_then_restamp_clears_a_pre_strip_recommended_plugins_rollout() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rollout-legacy.jsonl");
        let lines = [
            r#"{"timestamp":"2026-09-01T09:00:00Z","type":"session_meta","payload":{"id":"legacy-fixture","timestamp":"2026-09-01T09:00:00Z","cwd":"/workspace/legacy-project","source":"synthetic"}}"#,
            r#"{"timestamp":"2026-09-01T09:00:01Z","type":"response_item","payload":{"type":"message","id":"plugins-1","role":"user","content":[{"type":"input_text","text":"<recommended_plugins>\nHere is a list of plugins that are available but not installed"}]}}"#,
            r#"{"timestamp":"2026-09-01T09:00:02Z","type":"response_item","payload":{"type":"message","id":"user-1","role":"user","content":[{"type":"input_text","text":"real user request"}]}}"#,
            r#"{"timestamp":"2026-09-01T09:00:03Z","type":"response_item","payload":{"type":"message","id":"assistant-1","role":"assistant","content":[{"type":"output_text","text":"real assistant response"}]}}"#,
        ];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());
        let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
        assert_eq!(import_stats.files_imported, 1);

        // The already-fixed adapter strips the `<recommended_plugins>` item
        // at import time, so simulate a conversation imported *before* that
        // fix existed: restore the pollution directly onto the stored rows,
        // and collapse every chunk onto one timestamp — exactly the
        // pre-fix, pre-strip state the live corpus has for real rollouts
        // that predate both fixes.
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET summary = ?1 WHERE conversation_id = 'codex:legacy-fixture'",
                    rusqlite::params![
                        "<recommended_plugins>\nHere is a list of plugins that are available but not installed"
                    ],
                )?;
                conn.execute(
                    "UPDATE chunks SET content = ?1 WHERE conversation_id = 'codex:legacy-fixture' AND seq = 0",
                    rusqlite::params![
                        "<recommended_plugins>\nHere is a list of plugins that are available but not installed"
                    ],
                )?;
                conn.execute(
                    "UPDATE chunks SET timestamp = '2026-09-01T09:00:00Z'
                     WHERE conversation_id = 'codex:legacy-fixture'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let ids = storage
            .get_chunk_ids_for_conversation("codex:legacy-fixture")
            .unwrap();
        let seeded = storage.get_chunks_by_ids(&ids).unwrap();
        let seeded_timestamps: std::collections::BTreeSet<_> =
            seeded.iter().map(|c| c.timestamp.clone()).collect();
        assert_eq!(
            seeded_timestamps.len(),
            1,
            "setup: seeded state is collapsed"
        );

        // Restamp alone cannot fix this conversation — it is still polluted,
        // so it must be skipped, not mismatched.
        let restamp_before_strip = restamp_codex_chunk_timestamps(&storage).unwrap();
        assert_eq!(restamp_before_strip.conversations_skipped_polluted, 1);
        assert!(storage.get_meta(RESTAMP_META_KEY).unwrap().is_some());

        // The harness-strip batch reimports it directly, which both strips
        // the marker and re-stamps correct per-message timestamps as a side
        // effect of the normal import path.
        let strip = harness_strip_batch(&engine, 10, &AtomicBool::new(false)).unwrap();
        assert_eq!(strip.reimported, 1);
        assert!(strip.done);

        let ids = storage
            .get_chunk_ids_for_conversation("codex:legacy-fixture")
            .unwrap();
        let repaired = storage.get_chunks_by_ids(&ids).unwrap();
        assert!(
            repaired
                .iter()
                .all(|c| !c.content.trim_start().starts_with("<recommended_plugins>")),
            "harness marker must be gone: {:?}",
            repaired.iter().map(|c| &c.content).collect::<Vec<_>>()
        );
        let repaired_timestamps: std::collections::BTreeSet<_> =
            repaired.iter().map(|c| c.timestamp.clone()).collect();
        assert!(
            repaired_timestamps.len() > 1,
            "reimport must restore distinct per-message timestamps, got {repaired_timestamps:?}"
        );
    }

    /// W3-R high review finding: pollution used to be detected only from a
    /// conversation's cached `summary` (the first stored chunk's content). A
    /// resumed or multi-turn Codex session can have a real, non-harness
    /// first user message but still carry a harness-injected block
    /// (`<recommended_plugins>` / `<environment_context>` / etc.) re-injected
    /// on a *later* turn — imported before the harness-stripping fix
    /// existed, that block is still sitting untouched in stored content.
    /// `summary`-only detection never sees it: the restamp can never reach
    /// `chunks_unmatched == 0` for that row (content mismatch forever), and
    /// `harness_strip_batch` never queues it for reimport (it's not in the
    /// polluted set), so it shows a harness preview forever too. Content-
    /// based detection (any chunk, not just the first) must catch it. Must
    /// fail at HEAD (pre-fix, `polluted_codex_conversation_ids` summary-only).
    #[test]
    fn polluted_conversation_ids_detects_a_harness_marker_injected_after_a_real_first_message() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rollout-resumed.jsonl");
        let lines = [
            r#"{"timestamp":"2026-07-19T09:00:00Z","type":"session_meta","payload":{"id":"resumed-fixture","timestamp":"2026-07-19T09:00:00Z","cwd":"/workspace/resumed-project","source":"synthetic"}}"#,
            r#"{"timestamp":"2026-07-19T09:00:01Z","type":"response_item","payload":{"type":"message","id":"user-1","role":"user","content":[{"type":"input_text","text":"real user request to start the session"}]}}"#,
            r#"{"timestamp":"2026-07-19T09:00:02Z","type":"response_item","payload":{"type":"message","id":"assistant-1","role":"assistant","content":[{"type":"output_text","text":"real assistant response"}]}}"#,
            r#"{"timestamp":"2026-07-19T09:05:01Z","type":"response_item","payload":{"type":"message","id":"user-2","role":"user","content":[{"type":"input_text","text":"real follow-up request after resume"}]}}"#,
        ];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());
        let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
        assert_eq!(import_stats.files_imported, 1);

        // The now-fixed adapter would strip a genuine `<recommended_plugins>`
        // item at import time, so simulate a conversation imported *before*
        // that fix existed: restore a harness block directly onto the third
        // (resumed-turn) stored chunk, while leaving `summary` (built from
        // the first, real, chunk) untouched. This is the crux of the
        // finding: the conversation's opening turn is real prose.
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET content = ?1
                     WHERE conversation_id = 'codex:resumed-fixture' AND seq = 2",
                    rusqlite::params![
                        "<recommended_plugins>\nHere is a list of plugins that are available but not installed"
                    ],
                )?;
                Ok(())
            })
            .unwrap();

        let (_, summary) = storage
            .codex_conversation_summaries()
            .unwrap()
            .into_iter()
            .find(|(id, _)| id == "codex:resumed-fixture")
            .expect("conversation must be present");
        assert!(
            summary
                .as_deref()
                .is_some_and(|s| !is_harness_context_item(s)),
            "setup: summary must stay real prose — this is exactly what \
             summary-only detection misses"
        );

        let polluted = polluted_codex_conversation_ids(&storage).unwrap();
        assert!(
            polluted.contains("codex:resumed-fixture"),
            "content-based detection must flag the later harness chunk even \
             though summary is real prose"
        );

        let harness_strip = harness_strip_batch(&engine, 10, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            harness_strip.reimported, 1,
            "harness_strip_batch must queue and reimport a conversation \
             flagged only by content, not summary"
        );
        assert!(harness_strip.done);

        let ids = storage
            .get_chunk_ids_for_conversation("codex:resumed-fixture")
            .unwrap();
        let repaired = storage.get_chunks_by_ids(&ids).unwrap();
        assert!(
            repaired
                .iter()
                .all(|c| !c.content.trim_start().starts_with("<recommended_plugins>")),
            "harness marker must be gone: {:?}",
            repaired.iter().map(|c| &c.content).collect::<Vec<_>>()
        );

        // The restamp must also reach done for this conversation now that
        // the harness-strip job has reimported it.
        let restamp = restamp_codex_chunk_timestamps(&storage).unwrap();
        assert_eq!(restamp.conversations_unmatched, 0);
        assert!(storage.get_meta(RESTAMP_META_KEY).unwrap().is_some());
    }

    /// W3-R low review finding: `harness_strip_batch` used to ignore the
    /// daemon's shutdown flag between conversations entirely, and the
    /// daemon's shutdown sequence awaits it with no timeout — a launchctl
    /// stop (default 20s `ExitTimeOut`) could SIGKILL mid-batch. Must break
    /// between conversations (never mid-conversation) once shutdown is
    /// requested, leaving the remainder polluted for the next batch to pick
    /// up. Must fail at HEAD (pre-fix, no shutdown check in the loop).
    #[test]
    fn harness_strip_batch_stops_between_conversations_on_shutdown() {
        let root = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let raw = std::fs::read_to_string(&source).unwrap();
        let mut conversation_ids = Vec::new();
        for n in 0..3 {
            let id = format!("shutdown-fixture-{n}");
            std::fs::write(
                root.path().join(format!("rollout-{n}.jsonl")),
                raw.replace("modern-fixture", &id),
            )
            .unwrap();
            conversation_ids.push(format!("codex:{id}"));
        }

        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());
        let import_stats = import_changed_rollouts(&engine, root.path()).unwrap();
        assert_eq!(import_stats.files_imported, 3);

        for conversation_id in &conversation_ids {
            storage
                .with_connection(|conn| {
                    conn.execute(
                        "UPDATE chunks SET summary = ?1 WHERE conversation_id = ?2",
                        rusqlite::params![
                            "<recommended_plugins>\nHere is a list of plugins that are available but not installed",
                            conversation_id,
                        ],
                    )?;
                    conn.execute(
                        "UPDATE chunks SET content = ?1 || content WHERE conversation_id = ?2 AND seq = 0",
                        rusqlite::params![
                            "<recommended_plugins>\nHere is a list of plugins that are available but not installed\n",
                            conversation_id,
                        ],
                    )?;
                    Ok(())
                })
                .unwrap();
        }

        // Already shut down before the batch even starts: not one
        // conversation should be reimported, and the polluted count must be
        // untouched.
        let shutdown = AtomicBool::new(true);
        let batch = harness_strip_batch(&engine, 10, &shutdown).unwrap();
        assert_eq!(
            batch.reimported, 0,
            "must not reimport any conversation once shutdown is requested"
        );
        assert_eq!(batch.polluted_considered, 3);
        assert_eq!(
            batch.polluted_remaining, 3,
            "nothing was reimported, so nothing should have cleared"
        );
        assert!(!batch.done);
        assert!(storage
            .get_meta(HARNESS_STRIP_V2_META_KEY)
            .unwrap()
            .is_none());

        // A later, non-shutdown call resumes and drains the backlog — this
        // is the resumability the job's doc comment promises.
        let resumed = harness_strip_batch(&engine, 10, &AtomicBool::new(false)).unwrap();
        assert_eq!(resumed.reimported, 3);
        assert!(resumed.done);
    }

    /// The pass used to propagate the first per-file error, so one unreadable
    /// or write-locked rollout cost every file behind it until the next
    /// 30-minute daemon cycle.
    #[cfg(unix)]
    #[test]
    fn one_unreadable_rollout_does_not_abort_the_pass() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        std::fs::copy(&source, root.path().join("rollout-good.jsonl")).unwrap();
        let unreadable = root.path().join("rollout-aaa-unreadable.jsonl");
        std::fs::copy(&source, &unreadable).unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();

        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());
        let stats = import_changed_rollouts(&engine, root.path()).unwrap();

        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(stats.files_discovered, 2);
        assert_eq!(stats.files_imported, 1);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.vanished, 0);
        assert!(!storage
            .get_chunk_ids_for_conversation("codex:modern-fixture")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn modern_fixture_parses_messages_cwd_and_scrubs_csr_calls() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let parsed = parse_rollout(&path).unwrap().unwrap();
        let serialized = serde_json::to_string(&parsed.messages).unwrap();

        assert_eq!(parsed.project_name, "synthetic-project");
        assert_eq!(parsed.conversation_id, "codex:modern-fixture");
        assert!(serialized.contains("SYNTHETIC USER REQUEST"));
        assert!(serialized.contains("SYNTHETIC ASSISTANT RESPONSE"));
        assert!(serialized.contains("UNKNOWN TEXT RETAINED"));
        assert!(serialized.contains("NON-CSR TOOL OUTPUT RETAINED"));
        assert!(!serialized.contains("CSR QUERY SUPPRESSED"));
        assert!(!serialized.contains("CSR OUTPUT SUPPRESSED"));
        assert_eq!(parsed.schema_misses, 3);
        assert_eq!(parsed.suppression.csr_tool_blocks_suppressed, 2);
    }

    #[test]
    fn legacy_fixture_parses_direct_messages_and_falls_back_unscoped() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_legacy.jsonl");
        let parsed = parse_rollout(&path).unwrap().unwrap();
        let serialized = serde_json::to_string(&parsed.messages).unwrap();

        assert_eq!(parsed.project_name, "_unscoped");
        assert_eq!(parsed.conversation_id, "codex:legacy-fixture");
        assert!(serialized.contains("LEGACY USER REQUEST"));
        assert!(serialized.contains("LEGACY ASSISTANT RESPONSE"));
        assert_eq!(parsed.schema_misses, 0);
    }

    #[test]
    #[ignore = "acceptance receipt only: reads the operator's real ~/.codex/sessions corpus, never committed as a running gate"]
    fn acceptance_receipt_01a0c5df_real_file_strips_cleanly() {
        let real = std::path::Path::new(
            "/Users/you/.codex/sessions/2026/09/21/rollout-2026-09-21T14-29-18-019aaaaa-0000-7000-8000-000000000002.jsonl",
        );
        if !real.exists() {
            eprintln!("acceptance receipt skipped: fixture file not present on this machine");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("rollout-01a0c5df.jsonl");
        std::fs::copy(real, &target).unwrap();

        let metadata = scan_rollout_metadata(&target).unwrap().unwrap();
        eprintln!(
            "RECEIPT 01a0c5df summary (after fix): {:?}",
            metadata.summary
        );
        assert!(
            metadata
                .summary
                .as_deref()
                .unwrap_or_default()
                .starts_with("dont look at the literature install graphify"),
            "summary must be the real first user turn, not harness boilerplate: {:?}",
            metadata.summary
        );

        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = test_engine(storage.clone(), root.path());
        let stats = import_changed_rollouts(&engine, root.path()).unwrap();
        eprintln!("RECEIPT import stats: {stats:?}");
        assert_eq!(stats.files_imported, 1);

        let ids = storage
            .get_chunk_ids_for_conversation("codex:019aaaaa-0000-7000-8000-000000000002")
            .unwrap();
        let chunks = storage.get_chunks_by_ids(&ids).unwrap();
        let boilerplate: Vec<_> = chunks
            .iter()
            .filter(|c| {
                c.summary
                    .as_deref()
                    .unwrap_or_default()
                    .starts_with("<recommended_plugins>")
            })
            .collect();
        eprintln!(
            "RECEIPT: {} chunks total, {} still boilerplate-summarized",
            chunks.len(),
            boilerplate.len()
        );
        assert!(
            boilerplate.is_empty(),
            "no chunk may carry the boilerplate summary after the fix"
        );
    }

    #[test]
    fn codex_harness_context_items_are_stripped_and_counted() {
        // First user message is entirely Codex harness boilerplate (three
        // injected items); second is the real prompt. The harness-only
        // message must emit no chunk at all — not an empty one — and the
        // real prompt becomes the conversation's summary.
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rollout-harness.jsonl");
        let session_meta = r#"{"type":"session_meta","payload":{"id":"harness-fixture","cwd":"/tmp/harness-project"}}"#;
        let injected = serde_json::json!({
            "type": "response_item",
            "timestamp": "2026-09-20T10:00:00Z",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "<recommended_plugins>\nHere is a list of plugins that are available but not installed"},
                    {"type": "input_text", "text": "# AGENTS.md instructions\n\n<INSTRUCTIONS>do the agents thing</INSTRUCTIONS>"},
                    {"type": "input_text", "text": "<environment_context>\ncwd: /tmp/harness-project\n</environment_context>"}
                ]
            }
        });
        let real = serde_json::json!({
            "type": "response_item",
            "timestamp": "2026-09-20T10:00:05Z",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "dont look at the literature install graphify"}
                ]
            }
        });
        std::fs::write(
            &path,
            format!(
                "{session_meta}\n{}\n{}\n",
                serde_json::to_string(&injected).unwrap(),
                serde_json::to_string(&real).unwrap(),
            ),
        )
        .unwrap();

        let parsed = parse_rollout(&path).unwrap().unwrap();
        let serialized = serde_json::to_string(&parsed.messages).unwrap();

        assert_eq!(
            parsed.harness_context_stripped, 3,
            "all 3 injected items in the first message must be counted"
        );
        assert!(!serialized.contains("<recommended_plugins>"));
        assert!(!serialized.contains("AGENTS.md instructions"));
        assert!(!serialized.contains("<environment_context>"));
        assert!(serialized.contains("dont look at the literature install graphify"));
        assert_eq!(
            parsed.schema_misses, 0,
            "a fully-stripped message is not a schema miss"
        );

        let user_messages: Vec<_> = parsed
            .messages
            .iter()
            .filter(|m| m.get("type").and_then(serde_json::Value::as_str) == Some("user"))
            .collect();
        assert_eq!(
            user_messages.len(),
            1,
            "the harness-only message must emit nothing at all: {user_messages:?}"
        );

        let metadata = scan_rollout_metadata(&path).unwrap().unwrap();
        assert_eq!(
            metadata.summary.as_deref(),
            Some("dont look at the literature install graphify"),
            "summary must be the first REAL user message, not the harness boilerplate"
        );
    }

    fn write_rollout_fixture(path: &Path, session_meta: &serde_json::Value) {
        let real = serde_json::json!({
            "type": "response_item",
            "timestamp": "2026-09-20T10:00:05Z",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "real subagent prose"}
                ]
            }
        });
        std::fs::write(
            path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(session_meta).unwrap(),
                serde_json::to_string(&real).unwrap(),
            ),
        )
        .unwrap();
    }

    #[test]
    fn codex_session_meta_subagent_source_sets_is_subagent() {
        // W3-B1 review#2 finding: a `thread_spawn` child rollout's
        // `session_meta.payload.source` carries a `subagent` key. That must
        // flow through to `RolloutMetadata::is_subagent`, not stay
        // hard-coded false.
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rollout-subagent.jsonl");
        let session_meta = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": "019aaaaa-0000-7000-8000-000000000003",
                "cwd": "/tmp/subagent-project",
                "source": {
                    "subagent": {
                        "thread_spawn": {
                            "parent_thread_id": "019aaaaa-0000-7000-8000-000000000002"
                        }
                    }
                }
            }
        });
        write_rollout_fixture(&path, &session_meta);

        let metadata = scan_rollout_metadata(&path).unwrap().unwrap();
        assert!(
            metadata.is_subagent,
            "a session_meta with a `source.subagent` key must set is_subagent"
        );
    }

    #[test]
    fn codex_session_meta_without_subagent_source_leaves_is_subagent_false() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rollout-top-level.jsonl");
        let session_meta = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": "019aaaaa-0000-7000-8000-000000000002",
                "cwd": "/tmp/subagent-project"
            }
        });
        write_rollout_fixture(&path, &session_meta);

        let metadata = scan_rollout_metadata(&path).unwrap().unwrap();
        assert!(
            !metadata.is_subagent,
            "a top-level session_meta with no `source.subagent` key must not be flagged"
        );
    }

    #[test]
    fn codex_subagent_rollout_chunks_are_marked_sidechain() {
        // The chunk-level effect of `is_subagent`: every chunk built from a
        // subagent rollout must carry `is_sidechain = true`, not the
        // previously hard-coded `false` — otherwise the Claude-side
        // sidechain exclusion in `get_recent_chunks` never applies to it and
        // a `thread_spawn` child takes its own recent-work slot next to its
        // parent.
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rollout-subagent-chunks.jsonl");
        let session_meta = serde_json::json!({
            "type": "session_meta",
            "payload": {
                "id": "019aaaaa-0000-7000-8000-000000000003",
                "cwd": "/tmp/subagent-project",
                "source": {"subagent": {"thread_spawn": {"parent_thread_id": "parent-id"}}}
            }
        });
        write_rollout_fixture(&path, &session_meta);

        let parsed = parse_rollout(&path).unwrap().unwrap();
        assert!(parsed.is_subagent);
        let chunks = rollout_chunks(&parsed);
        assert!(
            !chunks.is_empty(),
            "fixture must produce at least one chunk"
        );
        assert!(
            chunks.iter().all(|c| c.is_sidechain),
            "every chunk of a subagent rollout must be marked is_sidechain: {chunks:?}"
        );
    }

    #[test]
    fn codex_harness_mixed_message_keeps_real_prose() {
        // One content item is harness-injected, the other is real prose in
        // the SAME message — only the injected item is dropped.
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rollout-mixed.jsonl");
        let session_meta = r#"{"type":"session_meta","payload":{"id":"mixed-fixture","cwd":"/tmp/mixed-project"}}"#;
        let mixed = serde_json::json!({
            "type": "response_item",
            "timestamp": "2026-09-20T11:00:00Z",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "<user_instructions>\nsome harness instructions\n</user_instructions>"},
                    {"type": "input_text", "text": "real prose alongside the injected item"}
                ]
            }
        });
        std::fs::write(
            &path,
            format!(
                "{session_meta}\n{}\n",
                serde_json::to_string(&mixed).unwrap()
            ),
        )
        .unwrap();

        let parsed = parse_rollout(&path).unwrap().unwrap();
        let serialized = serde_json::to_string(&parsed.messages).unwrap();

        assert_eq!(parsed.harness_context_stripped, 1);
        assert!(!serialized.contains("user_instructions"));
        assert!(serialized.contains("real prose alongside the injected item"));
        assert_eq!(
            parsed.messages.len(),
            1,
            "the surviving prose must still emit exactly one message"
        );
    }

    #[test]
    fn streamed_chunk_batches_match_single_pass_across_batch_boundaries() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let parsed = parse_rollout(&path).unwrap().unwrap();
        let expected = rollout_chunks(&parsed);
        let metadata = scan_rollout_metadata(&path).unwrap().unwrap();
        let mut streamed = Vec::new();
        let outcome = stream_rollout_chunk_batches(&path, &metadata, 2, |batch, _| {
            assert!(batch.len() <= 2);
            streamed.extend_from_slice(batch);
            Ok(())
        })
        .unwrap();

        let signature = |chunk: &ConversationChunk| {
            (
                chunk.id.clone(),
                chunk.conversation_id.clone(),
                chunk.project_name.clone(),
                chunk.timestamp.clone(),
                chunk.content.clone(),
                chunk.message_count,
                chunk.summary.clone(),
                chunk.author,
                chunk.seq,
                chunk.is_sidechain,
            )
        };
        assert_eq!(
            streamed.iter().map(signature).collect::<Vec<_>>(),
            expected.iter().map(signature).collect::<Vec<_>>()
        );
        assert_eq!(outcome.schema_misses, parsed.schema_misses);
        assert_eq!(outcome.suppression, parsed.suppression);
    }

    #[test]
    fn absent_rollout_directory_is_inert() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("does-not-exist");
        assert!(discover_rollouts(&absent, |_| Ok(None)).unwrap().is_empty());
    }

    #[test]
    fn rollout_that_vanishes_before_open_is_a_soft_skip() {
        let dir = tempfile::tempdir().unwrap();
        let vanished = dir.path().join("rollout-vanished.jsonl");
        assert!(parse_rollout(&vanished).unwrap().is_none());
    }
}
