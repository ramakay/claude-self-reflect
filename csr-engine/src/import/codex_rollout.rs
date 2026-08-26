//! Optional Codex rollout corpus adapter. Modern envelope records and legacy
//! direct messages are normalized into the same message shape used by Claude
//! transcripts, then passed through the shared CSR sanitizer before embedding.

use std::fs;
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::engine::Engine;
use crate::import::{ConversationChunk, CsrSuppressionStats};
use crate::provenance::ChunkProvenance;

const ROLLOUT_EMBED_BATCH_SIZE: usize = 32;

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
}

#[derive(Debug, Clone)]
struct RolloutMetadata {
    conversation_id: String,
    project_name: String,
    timestamp: String,
    summary: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct RolloutStreamOutcome {
    schema_misses: usize,
    suppression: CsrSuppressionStats,
    chunks: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RolloutImportStats {
    pub files_discovered: usize,
    pub files_imported: usize,
    pub chunks_imported: usize,
    pub vanished: usize,
    pub schema_misses: usize,
    pub csr_tool_blocks_suppressed: usize,
    pub csr_hook_wrappers_scrubbed: usize,
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
    }))
}

fn visit_rollout_messages<F>(
    path: &Path,
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
    let mut summary = None;
    let mut sanitizer = super::CsrMessageSanitizer::default();

    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => {
                schema_misses += 1;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = match sonic_rs::from_str(&line) {
            Ok(value) => value,
            Err(_) => {
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
            }
            Some("turn_context") => {
                cwd = cwd.or_else(|| string_at(&value, &["payload", "cwd"]));
            }
            Some("response_item") => {
                parse_response_item(&value, &mut line_messages, &mut schema_misses);
            }
            Some("event_msg") => {
                parse_event_message(&value, &mut line_messages, &mut schema_misses)
            }
            Some("message") => {
                if let Some(message) = canonical_message(&value, timestamp.as_deref()) {
                    line_messages.push(message);
                } else {
                    schema_misses += 1;
                }
            }
            Some("world_state" | "inter_agent_communication_metadata") => {}
            Some(_) => {
                if let Some(text) = extract_unknown_text(&value) {
                    line_messages.push(text_message("assistant", &text, timestamp.as_deref()));
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
                    line_messages.push(text_message("assistant", &text, timestamp.as_deref()));
                } else {
                    schema_misses += 1;
                }
            }
        }
        for mut message in line_messages {
            super::sanitize_message_for_search(&mut message, &mut sanitizer);
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
        },
        RolloutStreamOutcome {
            schema_misses,
            suppression: sanitizer.stats,
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
) {
    let Some(payload) = value.get("payload") else {
        *schema_misses += 1;
        return;
    };
    let timestamp = value.get("timestamp").and_then(serde_json::Value::as_str);
    match payload.get("type").and_then(serde_json::Value::as_str) {
        Some("message") => match canonical_message(payload, timestamp) {
            Some(message) => messages.push(message),
            None if matches!(
                payload.get("role").and_then(serde_json::Value::as_str),
                Some("developer" | "system")
            ) => {}
            None => *schema_misses += 1,
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
                messages.push(text_message("assistant", &text, timestamp));
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
                messages.push(text_message(
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

fn canonical_message(
    value: &serde_json::Value,
    timestamp: Option<&str>,
) -> Option<serde_json::Value> {
    let role = value.get("role")?.as_str()?;
    if role != "user" && role != "assistant" {
        return None;
    }
    let text = content_text(value.get("content"))?;
    Some(text_message(role, &text, timestamp))
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
    mut visit: F,
) -> Result<()>
where
    F: FnMut(ConversationChunk) -> Result<()>,
{
    const BUDGET: usize = 900;
    let text = super::extract_message_text(message);
    let tool_context = super::extract_tool_context(message);
    let tool_results = super::extract_tool_results(message);
    let content = [text, tool_context, tool_results]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if content.is_empty() {
        return Ok(());
    }
    let author = super::classify_message_author(message);
    let mut start = 0;
    while start < content.len() {
        let mut end = (start + BUDGET).min(content.len());
        end = content.floor_char_boundary(end);
        if end <= start {
            end = content.len();
        }
        let seq = *next_seq;
        visit(ConversationChunk {
            id: super::generate_chunk_id(&metadata.conversation_id, seq),
            conversation_id: metadata.conversation_id.clone(),
            project_name: metadata.project_name.clone(),
            timestamp: metadata.timestamp.clone(),
            content: content[start..end].to_string(),
            message_count: 1,
            summary: metadata.summary.clone(),
            author,
            seq,
            is_sidechain: false,
        })?;
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
    };
    let mut chunks = Vec::new();
    let mut next_seq = 0;
    for message in &parsed.messages {
        visit_rollout_message_chunks(&metadata, message, &mut next_seq, |chunk| {
            chunks.push(chunk);
            Ok(())
        })
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
    F: FnMut(&[ConversationChunk]) -> Result<()>,
{
    anyhow::ensure!(batch_size > 0, "rollout batch size must be positive");
    let mut pending = Vec::with_capacity(batch_size);
    let mut next_seq = 0usize;
    let Some((streamed_metadata, mut outcome)) = visit_rollout_messages(path, |message| {
        visit_rollout_message_chunks(metadata, &message, &mut next_seq, |chunk| {
            pending.push(chunk);
            if pending.len() == batch_size {
                visit_batch(&pending)?;
                pending.clear();
            }
            Ok(())
        })?;
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
        visit_batch(&pending)?;
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
        let Some(metadata) = scan_rollout_metadata(&file.path)? else {
            stats.vanished += 1;
            continue;
        };
        let key = file.path.to_string_lossy().to_string();
        let prev_count = storage.get_imported_chunk_count(&file.path)?;

        // Rollouts used to be deleted and rebuilt in full on any change, so a
        // session that grew by one message re-embedded its entire history.
        //
        // Unlike a Claude transcript, a rollout chunk is frozen the moment it is
        // written: `visit_rollout_message_chunks` splits each message on its own
        // with no carry-over buffer, so there is no trailing partial chunk that
        // grows. That makes a content comparison sufficient — and a primary-key
        // lookup is nothing next to an embedding.
        let mut reused = 0usize;
        let outcome = stream_rollout_chunk_batches(
            &file.path,
            &metadata,
            ROLLOUT_EMBED_BATCH_SIZE,
            |chunks| {
                let mut todo: Vec<(&ConversationChunk, bool)> = Vec::new();
                {
                    let index = engine.search().blocking_read();
                    for chunk in chunks {
                        let unchanged = storage
                            .get_chunk_content(&chunk.id)?
                            .is_some_and(|stored| stored == chunk.content);
                        let indexed = index.has_chunk(&chunk.id);
                        if unchanged && indexed {
                            reused += 1;
                            continue;
                        }
                        todo.push((chunk, indexed));
                    }
                }
                if todo.is_empty() {
                    return Ok(());
                }

                let texts = todo
                    .iter()
                    .map(|(chunk, _)| chunk.content.as_str())
                    .collect::<Vec<_>>();
                let embeddings = engine.embeddings().embed(&texts)?;
                let mut index = engine.search().blocking_write();
                for ((chunk, indexed), embedding) in todo.into_iter().zip(embeddings) {
                    storage.insert_chunk_with_source(chunk, &embedding, "codex_rollout")?;
                    storage.insert_chunk_provenance(
                        &chunk.id,
                        &ChunkProvenance {
                            author: chunk.author,
                            source_conv_id: metadata.conversation_id.clone(),
                            supersedes: None,
                        },
                    )?;
                    // insert_chunk is a no-op for a known id, so a changed chunk
                    // keeps its stale vector unless it is blanked first.
                    if indexed {
                        index.remove_chunk(&chunk.id);
                    }
                    index.insert_chunk(chunk.id.clone(), embedding);
                }
                Ok(())
            },
        )?;

        // A rollout that shrank leaves orphan tail chunks still matching content
        // that no longer exists. The wholesale delete used to cover this.
        if outcome.chunks < prev_count {
            let orphans = (outcome.chunks..prev_count)
                .map(|seq| super::generate_chunk_id(&metadata.conversation_id, seq))
                .collect::<Vec<_>>();
            tracing::warn!(
                conv = %metadata.conversation_id,
                previous = prev_count,
                current = outcome.chunks,
                "codex rollout shrank — dropping orphan tail chunks"
            );
            storage.delete_chunks_by_ids(&orphans)?;
            let mut index = engine.search().blocking_write();
            for id in &orphans {
                index.remove_chunk(id);
            }
        }

        storage.bump_aux_counter_by("codex_rollout", outcome.schema_misses)?;
        stats.schema_misses += outcome.schema_misses;
        stats.csr_tool_blocks_suppressed += outcome.suppression.csr_tool_blocks_suppressed;
        stats.csr_hook_wrappers_scrubbed += outcome.suppression.csr_hook_wrappers_scrubbed;
        storage.upsert_import_state_explicit(
            &key,
            &metadata.conversation_id,
            outcome.chunks,
            &file.mtime,
        )?;
        stats.files_imported += 1;
        stats.chunks_imported += outcome.chunks;
        tracing::debug!(
            conv = %metadata.conversation_id,
            total = outcome.chunks,
            embedded = outcome.chunks.saturating_sub(reused),
            reused,
            "codex rollout imported"
        );
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use std::sync::Arc;

    /// Build a rollout whose messages are long enough to produce several chunks each.
    fn write_rollout(path: &Path, id: &str, messages: usize) {
        let mut lines = vec![serde_json::json!({
            "timestamp": "2026-08-06T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": id,
                "timestamp": "2026-08-06T12:00:00Z",
                "cwd": "/workspace/synthetic-project",
                "source": "synthetic"
            }
        })];
        for i in 0..messages {
            lines.push(serde_json::json!({
                "timestamp": format!("2026-08-06T12:00:{:02}Z", i + 1),
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "id": format!("msg-{i}"),
                    "role": if i % 2 == 0 { "user" } else { "assistant" },
                    "content": [{"type": "input_text", "text": format!("ROLLOUT{i:03}-{}", "x".repeat(1900))}]
                }
            }));
        }
        std::fs::write(
            path,
            lines
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    /// A rollout that grows must re-embed only its new chunks. The old code
    /// deleted the conversation and rebuilt it in full on every change, so a
    /// session that gained one message re-embedded its whole history.
    #[test]
    fn growing_rollout_reuses_existing_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        let rollout = root.join("rollout-2026-08-06T12-00-00-grow.jsonl");

        let engine = crate::engine::Engine::from_parts(
            Arc::new(crate::storage::Storage::open_memory().unwrap()),
            Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
            Arc::new(tokio::sync::RwLock::new(crate::search::SearchEngine::new(
                256,
            ))),
            dir.path().to_path_buf(),
        );

        write_rollout(&rollout, "grow-fixture", 4);
        let first = import_changed_rollouts(&engine, &root).unwrap();
        assert!(
            first.chunks_imported > 4,
            "fixture must yield several chunks"
        );

        let snapshot = |engine: &crate::engine::Engine| -> Vec<(String, i64)> {
            engine
                .storage()
                .get_chunk_ids_for_conversation("codex:grow-fixture")
                .unwrap()
                .into_iter()
                .map(|id| {
                    let rowid = engine.storage().chunk_rowid_for_test(&id).unwrap();
                    (id, rowid)
                })
                .collect()
        };
        let before: std::collections::HashMap<_, _> = snapshot(&engine).into_iter().collect();
        assert_eq!(before.len(), first.chunks_imported);

        write_rollout(&rollout, "grow-fixture", 6);
        let second = import_changed_rollouts(&engine, &root).unwrap();
        assert!(second.chunks_imported > first.chunks_imported);

        // INSERT OR REPLACE assigns a fresh rowid, so an untouched chunk keeps its
        // original one. Every pre-existing chunk must be untouched.
        let after: std::collections::HashMap<_, _> = snapshot(&engine).into_iter().collect();
        let rewritten = before
            .iter()
            .filter(|(id, rowid)| after.get(*id).is_some_and(|now| now != *rowid))
            .count();
        assert_eq!(
            rewritten, 0,
            "a grown rollout must not rewrite chunks it already had"
        );

        // And the new content must actually be there.
        let text = after
            .keys()
            .filter_map(|id| engine.storage().get_chunk_content(id).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("ROLLOUT005"), "new messages must be imported");
    }

    /// A rollout that shrinks must drop the orphan tail the wholesale delete used
    /// to remove.
    #[test]
    fn shrinking_rollout_drops_orphan_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        let rollout = root.join("rollout-2026-08-06T12-00-00-shrink.jsonl");

        let engine = crate::engine::Engine::from_parts(
            Arc::new(crate::storage::Storage::open_memory().unwrap()),
            Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
            Arc::new(tokio::sync::RwLock::new(crate::search::SearchEngine::new(
                256,
            ))),
            dir.path().to_path_buf(),
        );

        write_rollout(&rollout, "shrink-fixture", 6);
        let big = import_changed_rollouts(&engine, &root).unwrap();

        write_rollout(&rollout, "shrink-fixture", 2);
        let small = import_changed_rollouts(&engine, &root).unwrap();
        assert!(small.chunks_imported < big.chunks_imported);

        let stored = engine
            .storage()
            .get_chunk_ids_for_conversation("codex:shrink-fixture")
            .unwrap();
        assert_eq!(
            stored.len(),
            small.chunks_imported,
            "no orphan tail chunks may survive a shrink"
        );
        let text = stored
            .iter()
            .filter_map(|id| engine.storage().get_chunk_content(id).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("ROLLOUT005"), "dropped content must be gone");
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
    fn streamed_chunk_batches_match_single_pass_across_batch_boundaries() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_rollout_modern.jsonl");
        let parsed = parse_rollout(&path).unwrap().unwrap();
        let expected = rollout_chunks(&parsed);
        let metadata = scan_rollout_metadata(&path).unwrap().unwrap();
        let mut streamed = Vec::new();
        let outcome = stream_rollout_chunk_batches(&path, &metadata, 2, |batch| {
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
