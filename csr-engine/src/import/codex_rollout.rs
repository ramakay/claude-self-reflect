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
    visit_rollout_messages_inner(path, true, None, visit)
}

/// `limit` reads only the first that many bytes, for a caller that took the
/// file's length earlier and must not see rows appended since.
pub(super) fn visit_raw_rollout_messages<F>(path: &Path, limit: Option<u64>, visit: F) -> Result<()>
where
    F: FnMut(serde_json::Value) -> Result<()>,
{
    visit_rollout_messages_inner(path, false, limit, visit)?;
    Ok(())
}

fn visit_rollout_messages_inner<F>(
    path: &Path,
    sanitize: bool,
    limit: Option<u64>,
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
    let mut sanitizer = super::CsrMessageSanitizer::for_chunks();

    let source: Box<dyn std::io::Read> = match limit {
        Some(limit) => Box::new(std::io::Read::take(file, limit)),
        None => Box::new(file),
    };
    let mut reader = BufReader::new(source);
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
            // A call with no id is stored with none. The shared sanitizer
            // pairs a CSR call with its output by id; any stand-in would be a
            // string a real call or output can also carry.
            let id = payload
                .get("call_id")
                .or_else(|| payload.get("id"))
                .and_then(serde_json::Value::as_str);
            let input = payload
                .get("arguments")
                .or_else(|| payload.get("input"))
                .map(parse_json_string)
                .unwrap_or(serde_json::Value::Null);
            let mut tool = serde_json::json!({
                "type":"tool_use", "name":name, "input":input
            });
            if let Some(id) = id {
                tool["id"] = serde_json::Value::String(id.to_string());
            }
            if let Some(namespace) = payload.get("namespace") {
                tool["namespace"] = namespace.clone();
            }
            messages.push(block_message("assistant", tool, timestamp));
        }
        Some("function_call_output" | "custom_tool_call_output") => {
            let id = payload
                .get("call_id")
                .or_else(|| payload.get("id"))
                .and_then(serde_json::Value::as_str);
            let output = payload.get("output").map(value_text).unwrap_or_default();
            let mut result = serde_json::json!({"type":"tool_result", "content":output});
            if let Some(id) = id {
                result["tool_use_id"] = serde_json::Value::String(id.to_string());
            }
            messages.push(block_message("user", result, timestamp));
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
            chunk_offset: 0,
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
    let _operation = engine.storage().operation_lock()?;
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
            // SQLite first, the index after, as `Engine::import_file` does: a
            // waiting writer blocks every new prompt reader, so the write guard
            // must not be held across a storage call. Holding it here for a
            // 32-row batch made a prompt wait 4.4 s for the index (2026-11-16).
            let fence = storage.change_fence()?;
            let mut stored = Vec::with_capacity(chunks.len());
            let mut failure = None;
            for ((chunk, chunk_evidence), embedding) in chunks.iter().zip(evidence).zip(embeddings)
            {
                let written = (|| -> Result<()> {
                    storage.insert_chunk_with_source(chunk, &embedding, "codex_rollout")?;
                    storage.insert_chunk_provenance(
                        &chunk.id,
                        &ChunkProvenance {
                            author: chunk.author,
                            source_conv_id: metadata.conversation_id.clone(),
                            supersedes: None,
                        },
                    )?;
                    storage.replace_chunk_evidence(chunk_evidence)
                })();
                if let Err(error) = written {
                    failure = Some(error);
                    break;
                }
                stored.push((chunk.id.clone(), embedding));
            }
            let mut index = engine.search().blocking_write();
            if !index.defers_to_sync(false, fence) {
                for (id, embedding) in stored {
                    index.insert_chunk(id, embedding);
                }
            }
            drop(index);
            failure.map_or(Ok(()), Err)
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
    storage.record_secrets_redacted(&key, &outcome.suppression.secrets)?;
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
#[path = "codex_rollout_tests.rs"]
mod tests;
