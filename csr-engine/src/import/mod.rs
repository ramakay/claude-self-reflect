pub mod backfill;
pub mod catchup;
pub mod codex_rollout;
pub mod coedit_backfill;
pub mod contamination;
pub mod dream_marker;
pub mod entrypoint_backfill;
pub(crate) mod legacy_resume;
pub mod memory_registry;
pub mod plans;
pub mod provenance_backfill;
mod provenance_matcher;
pub(crate) mod queued_prompts;
pub mod registry;
pub mod scrub;
pub mod watcher;

use std::collections::{HashMap, HashSet};
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

/// Single tool-use marker token produced by `extract_tool_context`:
/// `[Word]` or `[Word: detail]` (detail may contain spaces, never brackets).
static TOOL_MARKER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[A-Za-z0-9_]+(?::[^\[\]]*)?\]").unwrap());

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
    /// ToolResult). Legacy display metadata only; ranking reads `min_trust`.
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
    pub evidence: HashMap<String, crate::provenance::ChunkEvidence>,
    /// The transcript's own `"entrypoint"` field (`cli`, `sdk-cli`, `sdk-*`,
    /// `claude-desktop`, ...), read verbatim off the first JSONL line that
    /// carries one. `None` when no line in the file has the field (older
    /// transcripts, or a non-Claude-Code source) — callers must treat that as
    /// "unknown", never as a headless signal. See
    /// `storage::queries::is_headless_entrypoint`.
    pub entrypoint: Option<String>,
    /// Prompts the person typed while Claude was busy, each with no user row of
    /// its own to carry its text, in transcript order. Never part of `chunks`.
    pub queued: Vec<queued_prompts::QueuedRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParentContext {
    pub floor: crate::provenance::TrustTier,
    pub event_id: Option<String>,
}

#[derive(Debug, Clone)]
struct EventPart {
    event: crate::provenance::ProvenanceEvent,
    text: String,
    combined_start: usize,
    combined_end: usize,
    /// Byte offset of `text` in the chunk it was sliced into. Only the chunk
    /// edge trim reads it.
    chunk_offset: usize,
}

#[derive(Debug, Clone, Default)]
struct MessageEvidence {
    parts: Vec<EventPart>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CsrSuppressionStats {
    pub csr_tool_blocks_suppressed: usize,
    pub csr_hook_wrappers_scrubbed: usize,
    /// Credentials redacted from the text, per kind. Counts only, never values.
    pub secrets: crate::redact::SecretCounts,
}

struct CsrMessageSanitizer {
    /// Derived artifacts (learnings, episodes, narratives, recap feeds) may skip
    /// CSR's own text; the stored conversation never does. Chunk import keeps
    /// every byte a grammar matches and only cuts what is structural: CSR's
    /// own tool calls and results, the printed result of a CSR call inside a
    /// Codex script, and a complete hook-labelled wrapper.
    derived: bool,
    suppressed_tool_use_ids: HashSet<String>,
    /// Codex code-mode scripts that call a CSR tool among other work. Their
    /// output keeps every line except a printed CSR result.
    csr_mixed_script_ids: HashSet<String>,
    stats: CsrSuppressionStats,
}

impl CsrMessageSanitizer {
    /// The stored conversation: chunk import, and the matchers that mirror it.
    fn for_chunks() -> Self {
        Self {
            derived: false,
            suppressed_tool_use_ids: HashSet::new(),
            csr_mixed_script_ids: HashSet::new(),
            stats: CsrSuppressionStats::default(),
        }
    }

    /// Input to a derived artifact.
    fn for_derived() -> Self {
        Self {
            derived: true,
            ..Self::for_chunks()
        }
    }
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

/// `derive_conversation_attribution`'s project, falling back to
/// `storage.get_project_for_conversation` (the chunk row) only when the path
/// attribution resolves to `"unknown"` — a transcript whose recorded path
/// sits outside `projects_dir` (a `/private/var/folders` temp path,
/// `~/.claude/transcripts`, a codex/grok rollout). Both `process_v3_extraction`
/// (daemon) and `backfill_stories_cli` (summarizer) need this exact fallback
/// order: the path attribution is authoritative when it resolves to
/// something real (it correctly rescopes a sidechain transcript once its
/// source file is rediscovered, which the chunk row's project can be stuck
/// wrong on forever — see `process_v3_extraction`'s doc), but a bare
/// `.map(...).or_else(...)` is not enough, because `.map` always produces
/// `Some("unknown")` and `Option::or_else` never runs on a `Some` — that
/// exact bug once made `backfill_stories_cli` permanently write
/// `project_unknown` for every conversation whose import path fell outside
/// `projects_dir`, which the (b) fix's hard project-scope drop then removed
/// under any explicit scope.
pub(crate) fn project_for_conversation_or_attribution(
    storage: &crate::storage::Storage,
    projects_dir: &Path,
    file_path: &Path,
    conv_id: &str,
) -> String {
    let from_path = derive_conversation_attribution(projects_dir, file_path).project_name;
    if from_path != "unknown" {
        return from_path;
    }
    storage
        .get_project_for_conversation(conv_id)
        .ok()
        .flatten()
        .unwrap_or(from_path)
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
///
/// Must be fed the RAW opening prompt. The search sanitizer deletes the
/// paragraph holding a signature, so testing its output never matches: that
/// ordering let 19,830 agent transcripts (48% of all chunks) into the corpus
/// between 2026-09-01 and 2026-09-19.
pub(crate) fn is_csr_agent_prompt(first_user_message: &str) -> bool {
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

/// The text of `parsed` when it can be a transcript's opening prompt: a user
/// row Claude Code did not inject (`isMeta` caveats, compact summaries) that
/// still says something once CSR's own hook wrappers are removed. Wrappers are
/// stripped from the returned text so a prompt behind one still leads.
pub(crate) fn opening_prompt_candidate(parsed: &serde_json::Value) -> Option<String> {
    let msg_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if msg_type != "user" && msg_type != "human" {
        return None;
    }
    let injected = ["isMeta", "isCompactSummary"]
        .iter()
        .any(|key| parsed.get(key).and_then(|v| v.as_bool()).unwrap_or(false));
    if injected {
        return None;
    }
    let mut ignored = 0;
    let text = scrub_csr_system_reminders(&extract_message_text_raw(parsed), &mut ignored);
    // A slash command is something a person ran: the session opens here.
    if text.contains("<command-name>") {
        return Some(text);
    }
    // Only the wrapper spans go. Prose typed after pasted harness output is the
    // opening prompt, and skipping it would hand the decision to a later row.
    let prose = HARNESS_WRAPPER_RE.replace_all(&text, "");
    let leading = prose.trim_start();
    if leading.is_empty()
        || HARNESS_ROW_PREFIXES
            .iter()
            .any(|prefix| leading.starts_with(prefix))
    {
        return None;
    }
    Some(leading.to_string())
}

/// Tag-delimited blocks the harness writes into user rows.
static HARNESS_WRAPPER_RE: LazyLock<Regex> = LazyLock::new(|| {
    let spans = [
        "local-command-caveat",
        "local-command-stdout",
        "task-notification",
    ]
    .iter()
    .map(|tag| format!("<{tag}>.*?</{tag}>"))
    .collect::<Vec<_>>()
    .join("|");
    Regex::new(&format!("(?s){spans}")).unwrap()
});

/// Openings of whole rows the harness writes as plain text, never a person or an
/// agent prompt. Older transcripts carry them without `isMeta`/`isCompactSummary`.
const HARNESS_ROW_PREFIXES: [&str; 3] = [
    "[SYSTEM NOTIFICATION",
    "Caveat: The messages below were generated by the user while running local commands",
    "This session is being continued from a previous conversation",
];

/// What a transcript's opening prompt says about the whole file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptKind {
    /// Opens with a CSR agent prompt: the importer skips it whole.
    Agent,
    Real,
    /// Could not be opened. Never evidence for deleting anything.
    Unreadable,
}

/// Classify the transcript at `path` by its opening prompt. Reads only as far as
/// that prompt; shares `opening_prompt_candidate` and the predicate with the
/// importer, so `backfill scrub` purges exactly what import skips.
pub(crate) fn classify_transcript(path: &Path) -> TranscriptKind {
    let Ok(file) = fs::File::open(path) else {
        return TranscriptKind::Unreadable;
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(parsed) = sonic_rs::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(prompt) = opening_prompt_candidate(&parsed) {
            return if is_csr_agent_prompt(&prompt) {
                TranscriptKind::Agent
            } else {
                TranscriptKind::Real
            };
        }
    }
    TranscriptKind::Real
}

pub fn parse_jsonl_file(path: &Path, project_name: &str) -> Result<Vec<ConversationChunk>> {
    Ok(parse_jsonl_file_with_stats(path, project_name)?.chunks)
}

pub(crate) fn sidechain_parent_message_key(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines().take(32).flatten() {
        let Ok(parsed) = sonic_rs::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(parent) = parsed.get("parentUuid").and_then(serde_json::Value::as_str) {
            return Some(parent.to_string());
        }
    }
    None
}

/// Cold producer input: reuse the message-coordinate parser and its running
/// context floor, not the legacy MAX speaker aggregate used for display.
pub fn transcript_inputs(
    storage: &crate::storage::Storage,
    path: &Path,
    conversation_id: &str,
) -> Result<crate::storage::artifact_provenance::InputEnvelope> {
    transcript_inputs_variant(storage, path, conversation_id, false)
}

/// [`transcript_inputs`] over the first `limit` bytes of the file only, so a
/// reader that took the file's length earlier sees the same rows as this one
/// even if the transcript has been appended to since.
pub fn transcript_inputs_limited(
    storage: &crate::storage::Storage,
    path: &Path,
    conversation_id: &str,
    limit: u64,
) -> Result<crate::storage::artifact_provenance::InputEnvelope> {
    transcript_inputs_at(storage, path, conversation_id, false, Some(limit))
}

/// Producer-side convenience: the transcript's input envelope, or a single
/// Unknown input naming the path when the file cannot be read or parsed.
/// Never an empty envelope, so a producer's floor can never be an empty-set
/// maximum.
pub fn transcript_inputs_or_unknown(
    storage: &crate::storage::Storage,
    path: &Path,
    conversation_id: &str,
) -> crate::storage::artifact_provenance::InputEnvelope {
    use crate::storage::artifact_provenance::{ArtifactInput, InputEnvelope};
    match transcript_inputs(storage, path, conversation_id) {
        Ok(inputs) if !inputs.inputs().is_empty() => inputs,
        _ => InputEnvelope::new(vec![ArtifactInput::unknown(&path.to_string_lossy())]),
    }
}

pub(crate) fn transcript_inputs_variant(
    storage: &crate::storage::Storage,
    path: &Path,
    conversation_id: &str,
    raw: bool,
) -> Result<crate::storage::artifact_provenance::InputEnvelope> {
    transcript_inputs_at(storage, path, conversation_id, raw, None)
}

fn transcript_inputs_at(
    storage: &crate::storage::Storage,
    path: &Path,
    conversation_id: &str,
    raw: bool,
    limit: Option<u64>,
) -> Result<crate::storage::artifact_provenance::InputEnvelope> {
    let sidechain = path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "subagents");
    let parent = if sidechain {
        let parent_id = path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|n| n.to_str());
        Some(match parent_id {
            Some(id) => storage
                .parent_provenance_context(id, sidechain_parent_message_key(path).as_deref())?,
            None => ParentContext {
                floor: crate::provenance::TrustTier::Unknown,
                event_id: None,
            },
        })
    } else {
        None
    };
    let codex = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("rollout-"));
    // Only one of the index's four documents is read back: build just that one.
    let index = provenance_matcher::MessageIndex::read_inputs_only(
        path,
        conversation_id,
        codex,
        path.extension().is_some_and(|e| e == "md"),
        parent.as_ref(),
        raw,
        limit,
    )?;
    Ok(if raw {
        index.raw_inputs()
    } else {
        index.inputs()
    })
}

pub(crate) fn parse_jsonl_file_with_stats(
    path: &Path,
    project_name: &str,
) -> Result<ParsedConversation> {
    parse_jsonl_file_with_stats_and_parent(path, project_name, None)
}

pub(crate) fn parse_jsonl_file_with_stats_and_parent(
    path: &Path,
    project_name: &str,
    parent: Option<&ParentContext>,
) -> Result<ParsedConversation> {
    parse_jsonl_file_observing(path, project_name, parent, None)
}

/// Let a hook reuse the importer's parsed rows without another transcript pass.
pub(crate) fn parse_jsonl_file_observing(
    path: &Path,
    project_name: &str,
    parent: Option<&ParentContext>,
    observer: Option<&mut (dyn FnMut(&serde_json::Value) + Send)>,
) -> Result<ParsedConversation> {
    parse_jsonl_file_resuming(path, project_name, parent, observer, None)
}

/// Where a conversation stored before the scrubber is picked up again: its
/// stored chunk rows are never rewritten, so only the messages after the last
/// one they cover are chunked, and the new chunks continue the id sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResumePoint {
    /// Highest message sequence number the stored chunks cover.
    pub after_seq: usize,
    /// Index of the first chunk this import may write.
    pub first_chunk: usize,
}

/// [`parse_jsonl_file_observing`] for a conversation that resumes at `resume`.
pub(crate) fn parse_jsonl_file_resuming(
    path: &Path,
    project_name: &str,
    parent: Option<&ParentContext>,
    observer: Option<&mut (dyn FnMut(&serde_json::Value) + Send)>,
    resume: Option<ResumePoint>,
) -> Result<ParsedConversation> {
    let file = fs::File::open(path).context("opening JSONL file")?;
    parse_jsonl_reader_resuming(
        path,
        BufReader::new(file),
        project_name,
        parent,
        observer,
        resume,
    )
}

/// The same parser over any reader; `path` names the transcript (its stem is
/// the conversation id, and receipts point at it).
pub(crate) fn parse_jsonl_reader<R: BufRead>(
    path: &Path,
    reader: R,
    project_name: &str,
    parent: Option<&ParentContext>,
    observer: Option<&mut (dyn FnMut(&serde_json::Value) + Send)>,
) -> Result<ParsedConversation> {
    parse_jsonl_reader_resuming(path, reader, project_name, parent, observer, None)
}

fn parse_jsonl_reader_resuming<R: BufRead>(
    path: &Path,
    mut reader: R,
    project_name: &str,
    parent: Option<&ParentContext>,
    mut observer: Option<&mut (dyn FnMut(&serde_json::Value) + Send)>,
    resume: Option<ResumePoint>,
) -> Result<ParsedConversation> {
    let conversation_id = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let mut messages: Vec<String> = Vec::new();
    let mut authors: Vec<crate::provenance::Speaker> = Vec::new();
    let mut sidechains: Vec<bool> = Vec::new();
    let mut message_evidence: Vec<MessageEvidence> = Vec::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut running_floor: Option<crate::provenance::TrustTier> =
        parent.map(|context| context.floor);
    let mut message_seq = 0usize;
    let mut byte_offset = 0u64;
    let mut first_timestamp: Option<String> = None;
    let mut last_timestamp: Option<String> = None;
    let mut summary: Option<String> = None;
    let mut first_user_message: Option<String> = None;
    let mut agent_prompt_checked = false;
    let mut csr_sanitizer = CsrMessageSanitizer::for_chunks();
    let mut entrypoint: Option<String> = None;
    let mut queued = queued_prompts::Collector::default();

    loop {
        let line_start = byte_offset;
        let mut line = String::new();
        let bytes_read = match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        byte_offset += bytes_read as u64;
        while line.ends_with(['\n', '\r']) {
            line.pop();
        }
        if line.trim().is_empty() {
            continue;
        }
        // sonic-rs: serde-compatible drop-in, ~2.8x faster on aarch64
        let mut parsed: serde_json::Value = match sonic_rs::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if let Some(observe) = observer.as_mut() {
            observe(&parsed);
        }

        // The harness stamps `"entrypoint"` on most record types (user,
        // assistant, system, attachment, ...), not only the ones this loop
        // keeps as messages below — capture it off the first line that has
        // it, before the type filter below would `continue` past it.
        if entrypoint.is_none() {
            if let Some(ep) = parsed.get("entrypoint").and_then(|v| v.as_str()) {
                if !ep.is_empty() {
                    entrypoint = Some(ep.to_string());
                }
            }
        }

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
                    summary = Some(
                        crate::redact::redact_secrets_counted(s, &mut csr_sanitizer.stats.secrets)
                            .into_owned(),
                    );
                }
            }
            continue;
        }

        // A prompt typed while Claude was busy: no user row, only this attachment.
        // Collected on the side; the messages and their numbering are unchanged.
        if msg_type == "attachment" {
            queued.see_attachment(
                &parsed,
                &conversation_id,
                path,
                line_start,
                message_seq,
                running_floor,
                parent,
                &mut csr_sanitizer,
            );
            continue;
        }

        // Include human/user/assistant messages (Claude Code uses "user", not "human")
        if msg_type != "human" && msg_type != "user" && msg_type != "assistant" {
            continue;
        }

        let seq = message_seq;
        message_seq += 1;

        // Skip CSR's own agent-subprocess transcripts (briefing analyst, compaction
        // summarizer, ratification extractor, sentinel-led narrators). Their FIRST
        // user message IS the agent prompt. Tested on the raw text, before the
        // sanitizer below deletes the very paragraph that identifies it.
        if !agent_prompt_checked {
            if let Some(prompt) = opening_prompt_candidate(&parsed) {
                agent_prompt_checked = true;
                if is_csr_agent_prompt(&prompt) {
                    return Ok(ParsedConversation {
                        chunks: Vec::new(),
                        suppression: csr_sanitizer.stats,
                        evidence: HashMap::new(),
                        entrypoint,
                        queued: Vec::new(),
                    });
                }
            }
        }

        // A message the stored chunks already cover is read for everything that
        // carries forward (trust floor, tool names, summary) but is not chunked,
        // and the secrets in it are not counted: the stored rows still hold them.
        let stored = resume.is_some_and(|point| seq <= point.after_seq);
        let secrets_before = stored.then_some(csr_sanitizer.stats.secrets);
        sanitize_message_for_search(&mut parsed, &mut csr_sanitizer);
        if let Some(before) = secrets_before {
            csr_sanitizer.stats.secrets = before;
        }
        register_tool_uses(&parsed, &mut tool_names);
        queued.see_message(seq, &parsed);

        // Capture first and last timestamps
        if let Some(ts) = parsed.get("timestamp").and_then(|v| v.as_str()) {
            if first_timestamp.is_none() {
                first_timestamp = Some(ts.to_string());
            }
            last_timestamp = Some(ts.to_string());
        }

        // Capture first user message as fallback summary (what the user was trying to do)
        if first_user_message.is_none() && (msg_type == "user" || msg_type == "human") {
            let raw_text = extract_message_text(&parsed);
            // A pasted handoff draft or dream card is CSR's prose, not what the
            // person was trying to do: cut it before the preview, and judge
            // what is left. (The stored chunk keeps the paste.)
            let text = crate::storage::dream_attribution::strip_pasted_csr_spans(&raw_text);
            // An agent prompt is never what the person was trying to do; the
            // whole-transcript gate above reads it on the raw opening row.
            if !text.trim().is_empty() && text.len() > 5 && !is_csr_agent_prompt(&raw_text) {
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
        let mut components: Vec<(String, String)> = Vec::new();
        if !text.trim().is_empty() {
            let channel = if msg_type == "assistant" {
                "assistant_message"
            } else {
                "user_message"
            };
            components.push((channel.to_string(), text));
        }
        if !tool_context.trim().is_empty() {
            let channel = if msg_type == "assistant" {
                "assistant_message"
            } else {
                "unclassified"
            };
            components.push((channel.to_string(), tool_context));
        }
        for (tool_name, body) in extract_tool_result_parts(&parsed, &tool_names) {
            components.push((format!("tool_result:{tool_name}"), body));
        }
        let combined_text = components
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let is_sidechain_msg = parsed
            .get("isSidechain")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // Marker-only suppression applies to ASSISTANT turns only. The grammar
        // ("[Word: path]" / "[Word]") describes CSR's own rendering of a tool
        // call, but a human can legitimately type exactly `[TODO]` or
        // `[Note: review this]` as their whole message — dropping that is
        // recall loss on genuine user content, which is strictly worse than
        // keeping one content-free assistant marker.
        let author = classify_message_author(&parsed);
        let drop_as_marker = drops_as_marker_only(&parsed, &combined_text);
        if !combined_text.is_empty() && !drop_as_marker {
            let message_key = parsed
                .get("uuid")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| crate::provenance::content_hash(&combined_text));
            let observed_at = parsed
                .get("timestamp")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("1970-01-01T00:00:00Z")
                .to_string();
            let receipt_ref = Some(format!("{}#byte={line_start}", path.display()));
            let mut combined_start = 0usize;
            let mut parts = Vec::with_capacity(components.len());
            for (ordinal, (channel, component_text)) in components.into_iter().enumerate() {
                let context_floor = running_floor.unwrap_or(crate::provenance::TrustTier::Unknown);
                let mut tier = crate::provenance::trust_for_channel(&channel, context_floor);
                if let Some(parent) = parent {
                    tier = tier.min(parent.floor);
                }
                let event_id =
                    provenance_event_id(&conversation_id, &message_key, seq, &channel, ordinal);
                let event = crate::provenance::ProvenanceEvent {
                    event_id,
                    conversation_id: conversation_id.clone(),
                    message_key: message_key.clone(),
                    seq,
                    channel,
                    trust_tier: tier,
                    parent_event_id: parent.and_then(|context| context.event_id.clone()),
                    receipt_kind: "jsonl".to_string(),
                    receipt_ref: receipt_ref.clone(),
                    observed_at: observed_at.clone(),
                };
                let combined_end = combined_start + component_text.len();
                parts.push(EventPart {
                    event,
                    text: component_text,
                    combined_start,
                    combined_end,
                    chunk_offset: 0,
                });
                running_floor = Some(match running_floor {
                    Some(floor) => floor.min(tier),
                    None => tier,
                });
                combined_start = combined_end + 1;
            }
            if !stored {
                messages.push(combined_text);
                authors.push(author);
                sidechains.push(is_sidechain_msg);
                message_evidence.push(MessageEvidence { parts });
            }
        }
    }

    if messages.is_empty() {
        return Ok(ParsedConversation {
            chunks: Vec::new(),
            suppression: csr_sanitizer.stats,
            evidence: HashMap::new(),
            entrypoint,
            queued: queued.finish(),
        });
    }

    // Use last_timestamp for ordering (shows "last active" not "started at")
    // Fall back to first_timestamp if somehow no last, then to now()
    let timestamp = last_timestamp
        .or(first_timestamp)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    // Priority: JSONL summary > first user message > None
    let chunk_summary = summary.or(first_user_message);

    let (chunks, mut parts_by_chunk) = chunk_message_core_with_evidence(
        &conversation_id,
        project_name,
        &timestamp,
        &messages,
        &authors,
        &sidechains,
        &chunk_summary,
        Some(&message_evidence),
        resume.map_or(0, |point| point.first_chunk),
    );
    // Contamination left at the components, before they were joined; nothing
    // is cut from a finished chunk. The spans and the tool-result share are
    // measured against `chunk.content`, and `replace_chunk_evidence`
    // re-derives the share from the persisted spans and refuses the chunk when
    // the two disagree, so the edge trim adjusts them in the same step.
    let mut evidence = HashMap::new();
    let chunks = chunks
        .into_iter()
        .filter_map(|mut chunk| {
            let parts = parts_by_chunk.remove(&chunk.id).unwrap_or_default();
            let (content, parts) = trim_chunk_edges(&chunk.content, parts)?;
            chunk.content = content;
            attach_chunk_evidence(&chunk, parts, &mut evidence);
            Some(chunk)
        })
        .collect::<Vec<_>>();

    Ok(ParsedConversation {
        chunks,
        suppression: csr_sanitizer.stats,
        evidence,
        entrypoint,
        queued: queued.finish(),
    })
}

/// Character budget per chunk. Kept under the embedding model's ~256-token window
/// (all-MiniLM-L6-v2) so each chunk embeds in full rather than head-truncated.
///
/// One exception: a marker-closed handoff draft (`storage::handoff_marker`) is cut as a single
/// chunk of up to 12 kB so the search label sees its opening and its marker together; its vector
/// then covers only the title and lead, which are CSR's words.
pub(crate) const CHUNK_CHAR_BUDGET: usize = 900;

/// Chunk an already-parsed sequence of messages through the same bounded
/// transcript path used by JSONL imports. Benchmark adapters use this seam so
/// their scratch corpora cannot silently get a different chunking policy.
pub(crate) fn chunk_messages(
    conversation_id: &str,
    project_name: &str,
    timestamp: &str,
    messages: &[(crate::provenance::Speaker, String)],
    is_sidechain: bool,
) -> Vec<ConversationChunk> {
    let contents = messages
        .iter()
        .map(|(_, message)| message.clone())
        .collect::<Vec<_>>();
    let authors = messages
        .iter()
        .map(|(author, _)| *author)
        .collect::<Vec<_>>();
    let sidechains = vec![is_sidechain; messages.len()];
    chunk_message_core(
        conversation_id,
        project_name,
        timestamp,
        &contents,
        &authors,
        &sidechains,
        &None,
    )
}

#[allow(clippy::too_many_arguments)]
fn chunk_message_core(
    conversation_id: &str,
    project_name: &str,
    timestamp: &str,
    messages: &[String],
    message_authors: &[crate::provenance::Speaker],
    message_sidechains: &[bool],
    summary: &Option<String>,
) -> Vec<ConversationChunk> {
    chunk_message_core_with_evidence(
        conversation_id,
        project_name,
        timestamp,
        messages,
        message_authors,
        message_sidechains,
        summary,
        None,
        0,
    )
    .0
}

#[allow(clippy::too_many_arguments)]
fn chunk_message_core_with_evidence(
    conversation_id: &str,
    project_name: &str,
    timestamp: &str,
    messages: &[String],
    message_authors: &[crate::provenance::Speaker],
    message_sidechains: &[bool],
    summary: &Option<String>,
    message_evidence: Option<&[MessageEvidence]>,
    first_chunk: usize,
) -> (Vec<ConversationChunk>, HashMap<String, Vec<EventPart>>) {
    debug_assert_eq!(messages.len(), message_authors.len());
    debug_assert_eq!(messages.len(), message_sidechains.len());
    debug_assert!(message_evidence.is_none_or(|evidence| evidence.len() == messages.len()));
    let mut chunks = Vec::new();
    // Event parts per chunk, not finished evidence: the caller trims the chunk
    // edges, and spans have to be measured against what it persists.
    let mut parts_by_chunk = HashMap::new();
    let mut buffer = String::new();
    let mut authors = Vec::new();
    let mut sidechains = Vec::new();
    let mut buffered_parts: Vec<EventPart> = Vec::new();
    let mut message_count = 0;

    for (message_index, ((message, author), is_sidechain)) in messages
        .iter()
        .zip(message_authors)
        .zip(message_sidechains)
        .enumerate()
    {
        let source = message_evidence.and_then(|all| all.get(message_index));
        if message.len() > CHUNK_CHAR_BUDGET {
            if !buffer.is_empty() {
                push_chunk(
                    first_chunk,
                    &mut chunks,
                    conversation_id,
                    project_name,
                    timestamp,
                    std::mem::take(&mut buffer),
                    message_count,
                    summary,
                    chunk_author(&authors),
                    chunk_is_sidechain(&sidechains, conversation_id),
                );
                stash_chunk_parts(
                    chunks.last().expect("chunk was just pushed"),
                    std::mem::take(&mut buffered_parts),
                    &mut parts_by_chunk,
                );
                authors.clear();
                sidechains.clear();
                message_count = 0;
            }
            // A handoff draft is longer than the budget but is cut as one chunk: the
            // search label needs its opening and its closing marker in the same chunk.
            // The person's own words around it are chunked as usual.
            let drafts = crate::storage::handoff_marker::draft_ranges(message);
            let mut start = 0;
            while start < message.len() {
                let mut end = (start + CHUNK_CHAR_BUDGET).min(message.len());
                match drafts.iter().find(|(_, to)| *to > start) {
                    Some(&(from, to)) if from <= start => end = to,
                    Some(&(from, _)) => end = end.min(from),
                    None => {}
                }
                end = message.floor_char_boundary(end);
                if end <= start {
                    end = message.len();
                }
                push_chunk(
                    first_chunk,
                    &mut chunks,
                    conversation_id,
                    project_name,
                    timestamp,
                    message[start..end].to_string(),
                    1,
                    summary,
                    *author,
                    chunk_is_sidechain(std::slice::from_ref(is_sidechain), conversation_id),
                );
                stash_chunk_parts(
                    chunks.last().expect("chunk was just pushed"),
                    source
                        .map(|evidence| slice_event_parts(evidence, start, end))
                        .unwrap_or_default(),
                    &mut parts_by_chunk,
                );
                start = end;
            }
            continue;
        }

        if !buffer.is_empty() && buffer.len() + message.len() + 2 > CHUNK_CHAR_BUDGET {
            push_chunk(
                first_chunk,
                &mut chunks,
                conversation_id,
                project_name,
                timestamp,
                std::mem::take(&mut buffer),
                message_count,
                summary,
                chunk_author(&authors),
                chunk_is_sidechain(&sidechains, conversation_id),
            );
            stash_chunk_parts(
                chunks.last().expect("chunk was just pushed"),
                std::mem::take(&mut buffered_parts),
                &mut parts_by_chunk,
            );
            authors.clear();
            sidechains.clear();
            message_count = 0;
        }
        if !buffer.is_empty() {
            buffer.push_str("\n\n");
        }
        let message_offset = buffer.len();
        buffer.push_str(message);
        authors.push(*author);
        sidechains.push(*is_sidechain);
        if let Some(source) = source {
            buffered_parts.extend(slice_event_parts(source, 0, message.len()).into_iter().map(
                |mut part| {
                    part.chunk_offset += message_offset;
                    part
                },
            ));
        }
        message_count += 1;
    }
    if !buffer.is_empty() {
        push_chunk(
            first_chunk,
            &mut chunks,
            conversation_id,
            project_name,
            timestamp,
            buffer,
            message_count,
            summary,
            chunk_author(&authors),
            chunk_is_sidechain(&sidechains, conversation_id),
        );
        stash_chunk_parts(
            chunks.last().expect("chunk was just pushed"),
            buffered_parts,
            &mut parts_by_chunk,
        );
    }
    (chunks, parts_by_chunk)
}

fn stash_chunk_parts(
    chunk: &ConversationChunk,
    parts: Vec<EventPart>,
    parts_by_chunk: &mut HashMap<String, Vec<EventPart>>,
) {
    if !parts.is_empty() {
        parts_by_chunk.insert(chunk.id.clone(), parts);
    }
}

/// Strip the whitespace a chunk starts and ends with, and move the event parts
/// with it. Chunks have always been stored without edge whitespace, so a chunk
/// nothing was removed from keeps the exact text it was stored with. The parts
/// are cut by the offsets the chunker recorded: no searching, no guessing.
/// `None` when nothing but whitespace is left.
fn trim_chunk_edges(content: &str, parts: Vec<EventPart>) -> Option<(String, Vec<EventPart>)> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() == content.len() {
        return Some((content.to_string(), parts));
    }
    let keep_start = content.len() - content.trim_start().len();
    let keep_end = keep_start + trimmed.len();
    let parts = parts
        .into_iter()
        .filter_map(|part| {
            let from = keep_start.max(part.chunk_offset);
            let to = keep_end.min(part.chunk_offset + part.text.len());
            if from >= to {
                return None;
            }
            let local_from = from - part.chunk_offset;
            let local_to = to - part.chunk_offset;
            let combined_start = part.combined_start + part.text[..local_from].chars().count();
            let text = part.text[local_from..local_to].to_string();
            Some(EventPart {
                event: part.event,
                combined_end: combined_start + text.chars().count(),
                combined_start,
                chunk_offset: from - keep_start,
                text,
            })
        })
        .collect();
    Some((trimmed.to_string(), parts))
}

fn slice_event_parts(message: &MessageEvidence, start: usize, end: usize) -> Vec<EventPart> {
    message
        .parts
        .iter()
        .filter_map(|part| {
            let overlap_start = start.max(part.combined_start);
            let overlap_end = end.min(part.combined_end);
            if overlap_start >= overlap_end {
                return None;
            }
            let local_start = overlap_start - part.combined_start;
            let local_end = overlap_end - part.combined_start;
            Some(EventPart {
                event: part.event.clone(),
                text: part.text[local_start..local_end].to_string(),
                combined_start: part.text[..local_start].chars().count(),
                combined_end: part.text[..local_end].chars().count(),
                chunk_offset: overlap_start - start,
            })
        })
        .collect()
}

fn attach_chunk_evidence(
    chunk: &ConversationChunk,
    parts: Vec<EventPart>,
    evidence_by_chunk: &mut HashMap<String, crate::provenance::ChunkEvidence>,
) {
    if parts.is_empty() {
        return;
    }
    let mut events = Vec::new();
    let mut seen = HashSet::new();
    let mut spans = Vec::with_capacity(parts.len());
    let mut min_trust = crate::provenance::TrustTier::System;
    let mut tool_chars = 0usize;
    for part in parts {
        min_trust = min_trust.min(part.event.trust_tier);
        if part.event.channel.starts_with("tool_result:")
            || part.event.channel.starts_with("codex_tool:")
        {
            tool_chars += part.text.chars().count();
        }
        spans.push(crate::provenance::ChunkSpan {
            chunk_id: chunk.id.clone(),
            event_id: part.event.event_id.clone(),
            start_char: part.combined_start,
            end_char: part.combined_end,
            content_hash: crate::provenance::content_hash(&part.text),
        });
        if seen.insert(part.event.event_id.clone()) {
            events.push(part.event);
        }
    }
    let chunk_chars = chunk.content.chars().count();
    let tool_result_share = (chunk_chars > 0).then_some(tool_chars as f64 / chunk_chars as f64);
    evidence_by_chunk.insert(
        chunk.id.clone(),
        crate::provenance::ChunkEvidence {
            chunk_id: chunk.id.clone(),
            events,
            spans,
            min_trust,
            tool_result_share,
        },
    );
}

/// Per-tool_result character cap. Bounds giant logs while preserving enough of a
/// fetched doc / subagent report for the size-based chunker to slice and embed.
pub(crate) const MAX_TOOL_RESULT_CHARS: usize = 4000;

fn provenance_event_id(
    conversation_id: &str,
    message_key: &str,
    seq: usize,
    channel: &str,
    ordinal: usize,
) -> String {
    let material = format!("{conversation_id}\0{message_key}\0{seq}\0{channel}\0{ordinal}");
    format!("event:{}", blake3::hash(material.as_bytes()).to_hex())
}

fn register_tool_uses(message: &serde_json::Value, names: &mut HashMap<String, String>) {
    let Some(content) = message
        .get("message")
        .and_then(|value| value.get("content"))
        .and_then(serde_json::Value::as_array)
    else {
        return;
    };
    for item in content {
        if item.get("type").and_then(serde_json::Value::as_str) != Some("tool_use") {
            continue;
        }
        let Some(id) = item.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let name = item
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let qualified_name = item
            .get("namespace")
            .and_then(serde_json::Value::as_str)
            .filter(|namespace| !namespace.is_empty())
            .map(|namespace| format!("{namespace}__{name}"))
            .unwrap_or_else(|| name.to_string());
        names.insert(id.to_string(), qualified_name);
    }
}

fn tool_result_body(item: &serde_json::Value) -> String {
    let body = match item.get("content") {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|block| {
                (block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                    .then(|| block.get("text").and_then(serde_json::Value::as_str))
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let body = body.trim();
    if body.len() > MAX_TOOL_RESULT_CHARS {
        body[..body.floor_char_boundary(MAX_TOOL_RESULT_CHARS)].to_string()
    } else {
        body.to_string()
    }
}

fn extract_tool_result_parts(
    message: &serde_json::Value,
    names: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let Some(content) = message
        .get("message")
        .and_then(|value| value.get("content"))
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    content
        .iter()
        .filter(|item| item.get("type").and_then(serde_json::Value::as_str) == Some("tool_result"))
        .filter_map(|item| {
            let body = strip_private_tags(&tool_result_body(item));
            if body.is_empty() {
                return None;
            }
            let tool_name = item
                .get("tool_use_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|id| names.get(id))
                .cloned()
                .unwrap_or_else(|| "unknown".to_string());
            Some((tool_name, body))
        })
        .collect()
}

/// Push one finished chunk, assigning a deterministic sequential id.
#[allow(clippy::too_many_arguments)]
fn push_chunk(
    first_chunk: usize,
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
    let i = first_chunk + chunks.len();
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
#[cfg(test)]
fn extract_tool_results(msg: &serde_json::Value) -> String {
    extract_tool_result_parts(msg, &HashMap::new())
        .into_iter()
        .map(|(_, body)| body)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse a JSONL file into raw serde_json::Value messages (for extraction module).
/// Returns all messages with their original structure intact.
pub fn parse_jsonl_messages(path: &Path) -> Result<Vec<serde_json::Value>> {
    Ok(parse_jsonl_messages_with_queued(path)?.0)
}

/// A prompt the person typed while Claude was busy. The transcript holds it only as
/// a `queued_command` attachment, not as a user row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedPrompt {
    /// How many messages precede it: where it sits among them.
    pub place: usize,
    pub text: String,
    /// When it was typed, from the row; None when the row carries no readable time.
    pub at: Option<chrono::DateTime<chrono::Utc>>,
}

/// [`parse_jsonl_messages`] plus the human-typed `queued_command` attachments, in
/// order, each with its place among the messages. The messages themselves are
/// unchanged, so message counts stored against them stay valid.
pub(crate) fn parse_jsonl_messages_with_queued(
    path: &Path,
) -> Result<(Vec<serde_json::Value>, Vec<QueuedPrompt>)> {
    let file = fs::File::open(path).context("opening JSONL file for extraction")?;
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    let mut queued = Vec::new();
    let mut seen = HashSet::new();

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
        } else if msg_type == "attachment" {
            let Some(command) = parsed
                .get("attachment")
                .and_then(crate::transcript::queued_command)
            else {
                continue;
            };
            // The same enqueue can be written twice; one prompt, one count.
            let stamp = parsed
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let at = crate::temporal::parse_timestamp(&stamp);
            if command.human_turn && seen.insert((stamp, command.prompt.clone())) {
                queued.push(QueuedPrompt {
                    place: messages.len(),
                    text: command.prompt,
                    at,
                });
            }
        }
    }

    Ok((messages, queued))
}

/// Parse messages for any pipeline that creates searchable reflections.
/// Provenance and codegraph callers intentionally use [`parse_jsonl_messages`]
/// so their structural analysis retains the original transcript.
pub(crate) fn parse_jsonl_messages_for_search(path: &Path) -> Result<Vec<serde_json::Value>> {
    let messages = parse_jsonl_messages(path)?;
    Ok(sanitize_messages_for_search(&messages).0)
}

/// [`parse_jsonl_messages_for_search`] plus the prompts typed while Claude was busy.
pub(crate) fn parse_jsonl_messages_for_search_with_queued(
    path: &Path,
) -> Result<(Vec<serde_json::Value>, Vec<QueuedPrompt>)> {
    let (messages, queued) = parse_jsonl_messages_with_queued(path)?;
    // A queued prompt is compared with the user row it later becomes, which is
    // sanitized and scrubbed: sanitize the queued text the same way, or a prompt
    // holding a secret would never match its own row.
    let mut sanitizer = CsrMessageSanitizer::for_derived();
    let queued = queued
        .into_iter()
        .map(|prompt| {
            let mut row = serde_json::json!({"type": "user", "message": {"content": prompt.text}});
            sanitize_message_for_search(&mut row, &mut sanitizer);
            QueuedPrompt {
                place: prompt.place,
                text: row["message"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                at: prompt.at,
            }
        })
        .collect();
    Ok((sanitize_messages_for_search(&messages).0, queued))
}

/// Remove CSR's own tool payloads and exact hook-wrapper blocks while preserving
/// unrelated sibling blocks and surrounding prose. This is the single sanitizer
/// shared by chunk import, V3, heuristic, narrative, and story inputs.
pub(crate) fn sanitize_messages_for_search(
    messages: &[serde_json::Value],
) -> (Vec<serde_json::Value>, CsrSuppressionStats) {
    let mut sanitizer = CsrMessageSanitizer::for_derived();
    let mut sanitized = messages.to_vec();
    for message in &mut sanitized {
        sanitize_message_for_search(message, &mut sanitizer);
    }
    (sanitized, sanitizer.stats)
}

/// Row-at-a-time form of [`sanitize_messages_for_search`]: feeding it the
/// rows in order gives each row the same result as the slice form, without
/// holding a second copy of the whole transcript.
pub(crate) struct RowSanitizer(CsrMessageSanitizer);

impl RowSanitizer {
    pub(crate) fn for_derived() -> Self {
        Self(CsrMessageSanitizer::for_derived())
    }

    pub(crate) fn sanitize(&mut self, message: &mut serde_json::Value) {
        sanitize_message_for_search(message, &mut self.0);
    }
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
    let text = if scrub_wrappers && is_harness_written_row(message) {
        RowText::Drop
    } else {
        RowText::Keep { scrub_wrappers }
    };

    if let Some(content) = message
        .get_mut("message")
        .and_then(|value| value.get_mut("content"))
    {
        sanitize_content(content, text, sanitizer);
        redact_content_secrets(content, &mut sanitizer.stats.secrets);
    } else if let Some(content) = message.get_mut("content") {
        sanitize_content(content, text, sanitizer);
        redact_content_secrets(content, &mut sanitizer.stats.secrets);
    }
}

/// Redact credentials from every string in a message's content: prose, tool
/// inputs and tool results alike. Runs on the whole string, ahead of the
/// per-field truncation and the chunk budget, so a key is never cut in half
/// and left half-visible. Object keys are redacted too.
fn redact_content_secrets(value: &mut serde_json::Value, counts: &mut crate::redact::SecretCounts) {
    match value {
        serde_json::Value::String(text) => {
            if let std::borrow::Cow::Owned(redacted) =
                crate::redact::redact_secrets_counted(text, counts)
            {
                *text = redacted;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                redact_content_secrets(item, counts);
            }
        }
        serde_json::Value::Object(map) => {
            // Keys too: a token-to-account map keeps its credential in the key.
            // Keys the scrub leaves alone go in first, so a renamed key never
            // lands on one of them (it may sort before or after); two keys that
            // redact to the same marker stay apart (`#2`).
            let mut renamed = Vec::new();
            for (key, mut item) in std::mem::take(map) {
                redact_content_secrets(&mut item, counts);
                let redacted = match crate::redact::redact_secrets_counted(&key, counts) {
                    std::borrow::Cow::Owned(redacted) => Some(redacted),
                    std::borrow::Cow::Borrowed(_) => None,
                };
                match redacted {
                    Some(base) => renamed.push((base, item)),
                    None => {
                        map.insert(key, item);
                    }
                }
            }
            for (base, item) in renamed {
                let mut name = base.clone();
                let mut n = 1;
                while map.contains_key(&name) {
                    n += 1;
                    name = format!("{base} #{n}");
                }
                map.insert(name, item);
            }
        }
        _ => {}
    }
}

/// What the sanitizer does with a row's prose. Tool blocks are judged on their own.
#[derive(Clone, Copy)]
enum RowText {
    Keep {
        scrub_wrappers: bool,
    },
    /// The harness wrote this row into the user channel; nobody typed it.
    Drop,
}

/// Rows Claude Code writes into the user channel itself: task notifications,
/// messages from peer sessions, idle notices. Indexed as prose they came back as
/// the owner's own history. A coordinator's message to a subagent
/// (`origin.kind == "coordinator"`) is a real instruction and stays.
fn is_harness_written_row(message: &serde_json::Value) -> bool {
    let prompt_source = message
        .get("promptSource")
        .and_then(serde_json::Value::as_str);
    let origin = message
        .get("origin")
        .and_then(|origin| origin.get("kind"))
        .and_then(serde_json::Value::as_str);
    prompt_source == Some("system") || matches!(origin, Some("task-notification" | "peer"))
}

fn sanitize_content(
    content: &mut serde_json::Value,
    row_text: RowText,
    sanitizer: &mut CsrMessageSanitizer,
) {
    if let Some(text) = content.as_str() {
        *content = serde_json::Value::String(match row_text {
            RowText::Keep { scrub_wrappers } => {
                sanitize_text_for_search(text, scrub_wrappers, sanitizer)
            }
            RowText::Drop => String::new(),
        });
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
                if is_code_mode_script_calling_csr(&item, name) {
                    if let Some(id) = item.get("id").and_then(|value| value.as_str()) {
                        sanitizer.csr_mixed_script_ids.insert(id.to_string());
                    }
                }
            }
            Some("tool_result") => {
                let id = item.get("tool_use_id").and_then(|value| value.as_str());
                if id.is_some_and(|id| sanitizer.suppressed_tool_use_ids.contains(id)) {
                    sanitizer.stats.csr_tool_blocks_suppressed += 1;
                    continue;
                }
                if id.is_some_and(|id| sanitizer.csr_mixed_script_ids.contains(id)) {
                    if let Some(content) = item.get_mut("content") {
                        sanitizer.stats.csr_tool_blocks_suppressed +=
                            scrub_code_mode_csr_results(content);
                    }
                }
            }
            Some("text") => {
                let RowText::Keep { scrub_wrappers } = row_text else {
                    continue;
                };
                if let Some(text) = item.get_mut("text") {
                    if let Some(raw) = text.as_str() {
                        *text = serde_json::Value::String(sanitize_text_for_search(
                            raw,
                            scrub_wrappers,
                            sanitizer,
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

/// One text span's search-sanitization. The stored conversation keeps every
/// byte except a complete hook-labelled `<system-reminder>` wrapper in the
/// person's turn (`scrub_wrappers`), which the importer has always cut. A
/// derived artifact additionally drops a pasted dreams-CLI card's judgment
/// spans, a pasted handoff draft's CSR-authored sections and whatever the CSR
/// text detector matches: those inputs are not the conversation, and must not
/// teach a summary CSR's own words.
fn sanitize_text_for_search(
    text: &str,
    scrub_wrappers: bool,
    sanitizer: &mut CsrMessageSanitizer,
) -> String {
    let stripped = if sanitizer.derived {
        crate::storage::dream_attribution::strip_pasted_csr_spans(text)
    } else {
        text.to_string()
    };
    let unwrapped = if scrub_wrappers {
        scrub_csr_system_reminders(&stripped, &mut sanitizer.stats.csr_hook_wrappers_scrubbed)
    } else {
        stripped
    };
    if sanitizer.derived {
        contamination::strip_hits(&unwrapped)
    } else {
        unwrapped
    }
}

fn scrub_csr_system_reminders(text: &str, count: &mut usize) -> String {
    contamination::remove_hook_wrappers(text, count)
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
        // Blocks the sanitizer emptied are gone, not blank lines.
        let texts: Vec<&str> = content
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    item.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .filter(|text| !text.is_empty())
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
pub(crate) fn strip_private_tags(text: &str) -> String {
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

/// Characters of a `command` input kept in a tool_use marker.
pub(crate) const TOOL_COMMAND_DETAIL_CHARS: usize = 120;

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
            let truncated: String = cmd.chars().take(TOOL_COMMAND_DETAIL_CHARS).collect();
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

/// A tool-use marker has the shape `[Word]` or `[Word: detail]` (produced by
/// extract_tool_context). Returns true if `text`, after trimming, consists ONLY of
/// one or more such markers separated by whitespace and nothing else — i.e. it carries
/// no real prose or tool-result content and should not be embedded as a search chunk.
fn is_marker_only_text(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Strip every marker token; pure tool-call noise leaves only whitespace.
    let remainder = TOOL_MARKER_RE.replace_all(trimmed, "");
    remainder.chars().all(|c| c.is_whitespace())
}

/// Whether an assistant message is only CSR's rendering of tool calls, and so
/// is not embedded as a chunk. A real text block is never dropped, whatever it
/// looks like (`[TODO]` is words); only a message with no text block at all,
/// nothing but rendered tool markers, is. Import and reparse both decide here.
pub(crate) fn drops_as_marker_only(msg: &serde_json::Value, combined_text: &str) -> bool {
    matches!(
        classify_message_author(msg),
        crate::provenance::Speaker::Assistant
    ) && extract_message_text(msg).trim().is_empty()
        && is_marker_only_text(combined_text)
}

/// Codex code mode runs every tool call inside one script tool of this name.
const CODEX_CODE_MODE_TOOL: &str = "exec";

/// One whole statement that calls one CSR tool and prints only its result,
/// anchored at the start of what is left of the script. Two forms:
/// `text(await tools.mcp__claude_self_reflect__<name>({...}));` and
/// `const r = await tools.mcp__claude_self_reflect__<name>({...}); text(r);`.
/// The argument is a flat object of literal values only: a quoted string
/// with no backslash, a number, `true`, `false` or `null`. No expression can
/// appear in it, so it can run nothing and rebind nothing.
static CODE_MODE_CSR_STATEMENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    const KEY: &str = r#"(?:[A-Za-z_][A-Za-z0-9_]*|"[^"\\\n]*"|'[^'\\\n]*')"#;
    const VALUE: &str = r#"(?:"[^"\\\n]*"|'[^'\\\n]*'|-?[0-9]+(?:\.[0-9]+)?|true|false|null)"#;
    let pair = format!(r"{KEY}\s*:\s*{VALUE}");
    let args = format!(r"\{{\s*(?:{pair}\s*(?:,\s*{pair}\s*)*,?\s*)?\}}");
    let call = format!(
        r"await\s+tools\s*\.\s*mcp__claude_self_reflect__[A-Za-z0-9_]+\(\s*(?:{args})?\s*\)"
    );
    Regex::new(&format!(
        concat!(
            r"\A\s*(?:",
            r"text\(\s*{call}\s*\)",
            r"|(?:const|let|var)\s+(?P<bound>[A-Za-z_][A-Za-z0-9_]*)\s*=\s*{call}\s*;\s*text\(\s*(?P<used>[A-Za-z_][A-Za-z0-9_]*)\s*\)",
            r")[ \t]*(?:;|\r?\n|\z)",
        ),
        call = call
    ))
    .expect("code-mode CSR statement regex")
});

/// Whether a Codex code-mode script calls CSR tools and prints nothing else.
///
/// A whitelist of the whole script that abstains: every statement must be one
/// of the two forms in [`CODE_MODE_CSR_STATEMENT_RE`]. Anything else (another
/// tool, a second `text(...)`, an alias, a batch, a comment, a string that
/// only looks like a call) keeps the whole output. The output of a mixed
/// script also holds real work, and dropping it would lose that work; a
/// missed CSR-only script only leaks, as before this check.
fn code_mode_script_calls_only_csr(script: &str) -> bool {
    let mut rest = script;
    let mut statements = 0usize;
    while !rest.trim().is_empty() {
        let Some(statement) = CODE_MODE_CSR_STATEMENT_RE.captures(rest) else {
            return false;
        };
        if let (Some(bound), Some(used)) = (statement.name("bound"), statement.name("used")) {
            if bound.as_str() != used.as_str() {
                return false;
            }
        }
        let end = statement.get(0).map_or(0, |whole| whole.end());
        if end == 0 {
            return false;
        }
        rest = &rest[end..];
        statements += 1;
    }
    statements > 0
}

/// What follows `tools` in a call to a CSR tool, and in any reference to one.
static CODE_MODE_CSR_MEMBER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A\.\s*mcp__claude_self_reflect__").expect("code-mode CSR member regex")
});
static CODE_MODE_CSR_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A\.\s*mcp__claude_self_reflect__[A-Za-z0-9_]+\s*\(")
        .expect("code-mode CSR call regex")
});

/// Whether a code-mode script calls a CSR tool and is not wholly CSR's (that
/// case drops the whole call and output in [`is_csr_tool_use`]). It only marks
/// the script's output for [`scrub_code_mode_csr_results`]; the line check
/// there decides what goes.
fn is_code_mode_script_calling_csr(item: &serde_json::Value, name: &str) -> bool {
    name == CODEX_CODE_MODE_TOOL
        && item
            .get("input")
            .and_then(serde_json::Value::as_str)
            .and_then(code_mode_code_only)
            .is_some_and(|code| code_calls_csr_tool(&code))
}

/// Whether script code, with strings and comments already blanked, calls a
/// CSR tool as `tools.mcp__claude_self_reflect__<name>(`. It abstains (false)
/// when the script uses `tools` any other way (aliased, passed, rebound,
/// destructured, or as some object's property) or names a CSR tool without
/// calling it, since either can
/// make the printed "result" something the script built itself.
fn code_calls_csr_tool(code: &str) -> bool {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    let mut calls = false;
    for (at, word) in code.match_indices("tools") {
        let after = &code[at + word.len()..];
        if code[..at].chars().next_back().is_some_and(is_ident) || after.starts_with(is_ident) {
            continue;
        }
        // `x.tools`, `x . tools`, `x?.tools`: some other object's `tools`.
        if code[..at].trim_end().ends_with('.') {
            return false;
        }
        let member = after.trim_start();
        if !member.starts_with('.') {
            return false;
        }
        if CODE_MODE_CSR_MEMBER_RE.is_match(member) {
            if !CODE_MODE_CSR_CALL_RE.is_match(member) {
                return false;
            }
            calls = true;
        }
    }
    calls
}

/// A code-mode script with every string, template and comment blanked to one
/// space, so a CSR tool name inside one is not read as a call. `None` when
/// the script holds anything this cannot read for sure: a `/` that does not
/// open a comment (a regex literal or a division), a template with `${`, or
/// an unterminated string or comment.
fn code_mode_code_only(script: &str) -> Option<String> {
    let mut code = String::with_capacity(script.len());
    let mut chars = script.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' | '`' => {
                loop {
                    match chars.next()? {
                        '\\' => {
                            chars.next()?;
                        }
                        '\n' if c != '`' => return None,
                        '$' if c == '`' && chars.peek() == Some(&'{') => return None,
                        close if close == c => break,
                        _ => {}
                    }
                }
                code.push(' ');
            }
            '/' => match chars.next()? {
                '/' => {
                    for skipped in chars.by_ref() {
                        if skipped == '\n' {
                            break;
                        }
                    }
                    code.push('\n');
                }
                '*' => {
                    let mut star = false;
                    loop {
                        let next = chars.next()?;
                        if star && next == '/' {
                            break;
                        }
                        star = next == '*';
                    }
                    code.push(' ');
                }
                _ => return None,
            },
            other => code.push(other),
        }
    }
    Some(code)
}

/// Removes the printed CSR results from the output of a code-mode script that
/// calls CSR among other work, and returns how many lines it removed.
///
/// A whitelist that abstains, line by line. Code mode prints each `text(x)` as
/// one line, and an object as its JSON, so a printed CSR result is one line
/// that [`is_printed_csr_result_line`] accepts. Every other line stays: the
/// run's header, a truncation note, another tool's result, a line cut short,
/// and a CSR result printed any other way (that one leaks, as before).
///
/// Codex stores the output as a list of text items, which reaches here as its
/// JSON. That list is read, scrubbed per item and written back; a string that
/// is not such a list is scrubbed as it is. Nothing is rewritten when no line
/// goes.
fn scrub_code_mode_csr_results(content: &mut serde_json::Value) -> usize {
    match content {
        serde_json::Value::String(text) => {
            if let Ok(serde_json::Value::Array(mut items)) =
                serde_json::from_str::<serde_json::Value>(text)
            {
                let removed = scrub_text_items(&mut items);
                if removed > 0 {
                    *text = serde_json::Value::Array(items).to_string();
                }
                return removed;
            }
            let (kept, removed) = scrub_printed_csr_lines(text);
            if removed > 0 {
                *text = kept;
            }
            removed
        }
        serde_json::Value::Array(items) => scrub_text_items(items),
        _ => 0,
    }
}

fn scrub_text_items(items: &mut [serde_json::Value]) -> usize {
    let mut removed = 0;
    for item in items {
        if let Some(serde_json::Value::String(text)) = item.get_mut("text") {
            let (kept, count) = scrub_printed_csr_lines(text);
            if count > 0 {
                *text = kept;
                removed += count;
            }
        }
    }
    removed
}

fn scrub_printed_csr_lines(text: &str) -> (String, usize) {
    let mut kept = String::with_capacity(text.len());
    let mut removed = 0;
    for line in text.split_inclusive('\n') {
        if is_printed_csr_result_line(line.trim_end_matches(['\n', '\r'])) {
            removed += 1;
        } else {
            kept.push_str(line);
        }
    }
    (kept, removed)
}

/// Whether one printed line is, whole, the JSON of one CSR tool result: the
/// MCP result itself (`text(r)`), or that result inside a wrapper made only
/// of an integer `i` or `index`, the status `"fulfilled"`, and the result
/// under `value` or `result` (`text({i, ...settled})`,
/// `text({i, result: settled})`). The result must
/// hold text items only, not be an error, and its first text must be one
/// whole search result as the emitter writes it
/// ([`crate::import::contamination::is_whole_search_envelope`]);
/// any later item must be CSR's stale-binary notice. A wrapper that carries
/// anything else, a second result, or text cut short is kept.
fn is_printed_csr_result_line(line: &str) -> bool {
    let line = line.trim();
    if !(line.starts_with('{') && line.ends_with('}')) {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    let mut results = 0usize;
    wraps_only_csr_results(&value, &mut results) && results == 1
}

fn wraps_only_csr_results(value: &serde_json::Value, results: &mut usize) -> bool {
    let serde_json::Value::Object(map) = value else {
        return false;
    };
    if map.contains_key("content") {
        *results += 1;
        return is_csr_mcp_result(map);
    }
    // Only the fields a settled promise and a loop index add. Any other field
    // can be real data printed beside the result (an exit code, a flag).
    !map.is_empty()
        && map.iter().all(|(key, field)| match key.as_str() {
            "i" | "index" => field.is_u64(),
            "status" => field.as_str() == Some("fulfilled"),
            "value" | "result" => wraps_only_csr_results(field, results),
            _ => false,
        })
}

fn is_csr_mcp_result(map: &serde_json::Map<String, serde_json::Value>) -> bool {
    if map.keys().any(|key| key != "content" && key != "isError") {
        return false;
    }
    if map
        .get("isError")
        .is_some_and(|flag| flag != &serde_json::Value::Bool(false))
    {
        return false;
    }
    let Some(serde_json::Value::Array(items)) = map.get("content") else {
        return false;
    };
    let mut texts = items.iter().map(|item| {
        let block = item.as_object()?;
        let only_type_and_text = block.len() == 2
            && block.get("type").and_then(serde_json::Value::as_str) == Some("text");
        only_type_and_text
            .then(|| block.get("text").and_then(serde_json::Value::as_str))
            .flatten()
    });
    let Some(Some(first)) = texts.next() else {
        return false;
    };
    crate::import::contamination::is_whole_search_envelope(first)
        && texts.all(|text| text.is_some_and(is_csr_stale_binary_notice))
}

/// The notice CSR appends to a tool result when the server runs a superseded
/// build ([`crate::binary_identity::replacement_notice`]), matched by the
/// emitter's own text around its one variable part.
fn is_csr_stale_binary_notice(text: &str) -> bool {
    const SLOT: &str = "\u{0}";
    let template = crate::binary_identity::replacement_notice(SLOT);
    let Some((head, tail)) = template.split_once(SLOT) else {
        return false;
    };
    let text = text.trim();
    text.len() > head.len() + tail.len()
        && text.starts_with(head)
        && text.ends_with(tail)
        && !text[head.len()..text.len() - tail.len()].contains(['<', '\n'])
}

pub(crate) fn is_csr_tool_use(item: &serde_json::Value, name: &str) -> bool {
    if name == CODEX_CODE_MODE_TOOL {
        return item
            .get("input")
            .and_then(serde_json::Value::as_str)
            .is_some_and(code_mode_script_calls_only_csr);
    }
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
mod tests;
