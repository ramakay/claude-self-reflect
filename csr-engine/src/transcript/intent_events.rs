use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{collections::BTreeMap, collections::BTreeSet, ops::AddAssign};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::dream::backfill::intent_channel::{extract_targets, TargetKind};
use crate::embeddings::EmbeddingEngine;
use crate::hooks::reaction::Reaction;

use super::{parse_transcript, truncate_chars, Role};

const PRIOR_CLAIM_CHARS: usize = 240;
const ABANDONMENT_LEXICON_VERSION: &str = "intent-abandonment-v2";
const ABANDONMENT_MARKERS: &[&str] = &[
    "abandoning",
    "dropping",
    "reverting",
    "not doing",
    "gave up on",
    "out of scope",
    "was rejected",
    "got blocked",
    "cannot-so",
    "retrying with",
    "trying-instead",
    "falling back to",
    "switching-to-instead",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentEventKind {
    Correction,
    Redirect,
    Abandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentDetector {
    Lexical,
    Classifier,
    Both,
}

impl IntentDetector {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::Classifier => "classifier",
            Self::Both => "both",
        }
    }
}

impl IntentEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Correction => "correction",
            Self::Redirect => "redirect",
            Self::Abandoned => "abandoned",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentEvent {
    pub session_id: String,
    pub project: String,
    pub turn: u32,
    pub kind: IntentEventKind,
    pub quote: String,
    pub transcript_path: PathBuf,
    pub byte_start: usize,
    pub byte_end: usize,
    pub prior_claim: String,
    pub symbol: Option<String>,
    pub file: Option<String>,
    pub classifier_hash: String,
    pub detector: Option<IntentDetector>,
    pub classifier_score: Option<String>,
    pub marker: Option<String>,
    pub ts: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntentBackfillStats {
    pub files_scanned: usize,
    pub files_skipped: usize,
    pub inserted: usize,
    pub alignment_misses: usize,
    pub distinct_sessions: usize,
    pub by_kind: BTreeMap<String, usize>,
    pub by_marker: BTreeMap<String, usize>,
    pub by_project: BTreeMap<String, usize>,
    sessions: BTreeSet<String>,
    events: Vec<IntentEvent>,
}

impl IntentBackfillStats {
    pub fn total_events(&self) -> usize {
        self.by_kind.values().sum()
    }

    pub fn format_text(&self, dry_run: bool) -> String {
        let mut output = format!(
            "intent backfill{}: {} file(s) scanned, {} skipped\n",
            if dry_run { " (dry-run)" } else { "" },
            self.files_scanned,
            self.files_skipped
        );
        for (project, total) in &self.by_project {
            output.push_str(&format!("  {project}: {total}\n"));
        }
        output.push_str(&format!(
            "  correction={} redirect={} abandoned={} total={} distinct_sessions={} alignment_misses={} {}={}\n",
            self.by_kind.get("correction").copied().unwrap_or(0),
            self.by_kind.get("redirect").copied().unwrap_or(0),
            self.by_kind.get("abandoned").copied().unwrap_or(0),
            self.total_events(),
            self.distinct_sessions,
            self.alignment_misses,
            if dry_run { "would_insert" } else { "inserted" },
            if dry_run {
                self.total_events()
            } else {
                self.inserted
            }
        ));
        for (marker, total) in &self.by_marker {
            output.push_str(&format!("  marker[{marker}]={total}\n"));
        }
        output
    }

    pub fn events(&self) -> &[IntentEvent] {
        &self.events
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgreementCounts {
    pub ledger_total: usize,
    pub ledger_matched: usize,
    pub event_total: usize,
    pub event_matched: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntentAgreementReport {
    pub shared_sessions: usize,
    pub by_kind: BTreeMap<String, AgreementCounts>,
}

impl IntentAgreementReport {
    pub fn format_text(&self) -> String {
        let mut output = format!(
            "intent agreement: shared_sessions={}\n",
            self.shared_sessions
        );
        for kind in ["correction", "redirect", "abandoned"] {
            let counts = self.by_kind.get(kind).cloned().unwrap_or_default();
            let recall = if counts.ledger_total == 0 {
                "n/a".to_string()
            } else {
                format!(
                    "{:.3}",
                    counts.ledger_matched as f64 / counts.ledger_total as f64
                )
            };
            let precision = if counts.event_total == 0 {
                "n/a".to_string()
            } else {
                format!(
                    "{:.3}",
                    counts.event_matched as f64 / counts.event_total as f64
                )
            };
            output.push_str(&format!(
                "  {kind}: ledger_matched={}/{} recall={} event_matched={}/{} precision={}\n",
                counts.ledger_matched,
                counts.ledger_total,
                recall,
                counts.event_matched,
                counts.event_total,
                precision
            ));
        }
        output
    }
}

#[derive(Debug)]
struct LedgerQuote {
    session_id: String,
    kind: IntentEventKind,
    normalized: String,
}

fn normalize_agreement_quote(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn decoded_event_quote(event: &IntentEvent) -> String {
    let encoded = format!("\"{}\"", event.quote);
    serde_json::from_str::<String>(&encoded).unwrap_or_else(|_| event.quote.clone())
}

fn quotes_match(left: &str, right: &str) -> bool {
    !left.is_empty() && !right.is_empty() && (left.contains(right) || right.contains(left))
}

pub fn agreement_report(
    ledger_dir: &Path,
    events: &[IntentEvent],
) -> Result<IntentAgreementReport> {
    let mut paths = std::fs::read_dir(ledger_dir)
        .with_context(|| format!("reading intent ledger {}", ledger_dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
        .collect::<Vec<_>>();
    paths.sort();
    let mut ledger_quotes = Vec::new();
    let mut ledger_sessions = BTreeSet::new();
    for path in paths {
        let raw = std::fs::read_to_string(&path)?;
        for line in raw.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let Some(session_id) = value.get("session_id").and_then(|value| value.as_str()) else {
                continue;
            };
            ledger_sessions.insert(session_id.to_string());
            let Some(narration) = value.get("narration") else {
                continue;
            };
            for (key, kind) in [
                ("corrections", IntentEventKind::Correction),
                ("redirects", IntentEventKind::Redirect),
                ("abandoned", IntentEventKind::Abandoned),
            ] {
                let Some(items) = narration.get(key).and_then(|value| value.as_array()) else {
                    continue;
                };
                for item in items {
                    if item.get("verified").and_then(|value| value.as_bool()) != Some(true) {
                        continue;
                    }
                    let Some(quote) = item.get("quote").and_then(|value| value.as_str()) else {
                        continue;
                    };
                    ledger_quotes.push(LedgerQuote {
                        session_id: session_id.to_string(),
                        kind,
                        normalized: normalize_agreement_quote(quote),
                    });
                }
            }
        }
    }

    let event_sessions: BTreeSet<String> = events
        .iter()
        .map(|event| event.session_id.clone())
        .collect();
    let shared: BTreeSet<String> = ledger_sessions
        .intersection(&event_sessions)
        .cloned()
        .collect();
    let mut report = IntentAgreementReport {
        shared_sessions: shared.len(),
        ..Default::default()
    };

    for ledger in ledger_quotes
        .iter()
        .filter(|quote| shared.contains(quote.session_id.as_str()))
    {
        let counts = report
            .by_kind
            .entry(ledger.kind.as_str().to_string())
            .or_default();
        counts.ledger_total += 1;
        if events.iter().any(|event| {
            event.session_id == ledger.session_id
                && event.kind == ledger.kind
                && quotes_match(
                    &normalize_agreement_quote(&decoded_event_quote(event)),
                    &ledger.normalized,
                )
        }) {
            counts.ledger_matched += 1;
        }
    }
    for event in events
        .iter()
        .filter(|event| shared.contains(event.session_id.as_str()))
    {
        let counts = report
            .by_kind
            .entry(event.kind.as_str().to_string())
            .or_default();
        counts.event_total += 1;
        let normalized = normalize_agreement_quote(&decoded_event_quote(event));
        if ledger_quotes.iter().any(|ledger| {
            ledger.session_id == event.session_id
                && ledger.kind == event.kind
                && quotes_match(&normalized, &ledger.normalized)
        }) {
            counts.event_matched += 1;
        }
    }
    Ok(report)
}

#[derive(Debug)]
struct BackfillTranscript {
    path: PathBuf,
    project: String,
    session_id: String,
}

fn discover_backfill_transcripts(
    projects_dir: &Path,
    project_filter: Option<&str>,
) -> Result<Vec<BackfillTranscript>> {
    let mut out = Vec::new();
    for (project_dir, project) in crate::import::discover_projects(projects_dir)? {
        if project_filter.is_some_and(|filter| filter != project) {
            continue;
        }
        for path in crate::import::list_conversation_jsonl_files(&project_dir)? {
            let attribution = crate::import::derive_conversation_attribution(projects_dir, &path);
            let session_id = attribution.parent_conversation_id.unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            });
            out.push(BackfillTranscript {
                path,
                project: attribution.project_name,
                session_id,
            });
        }
    }
    out.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(out)
}

fn since_boundary(since: Option<&str>) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    since
        .map(|value| {
            chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .with_context(|| format!("invalid --since date {value}; expected YYYY-MM-DD"))?
                .and_hms_opt(0, 0, 0)
                .map(|value| value.and_utc())
                .ok_or_else(|| anyhow::anyhow!("invalid --since date {value}"))
        })
        .transpose()
}

fn retain_since(events: &mut Vec<IntentEvent>, since: Option<chrono::DateTime<chrono::Utc>>) {
    let Some(since) = since else {
        return;
    };
    events.retain(|event| {
        chrono::DateTime::parse_from_rfc3339(&event.ts)
            .map(|timestamp| timestamp.with_timezone(&chrono::Utc) >= since)
            .unwrap_or(false)
    });
}

fn account_events(stats: &mut IntentBackfillStats, events: &[IntentEvent]) {
    for event in events {
        stats.sessions.insert(event.session_id.clone());
        stats
            .by_kind
            .entry(event.kind.as_str().to_string())
            .or_default()
            .add_assign(1);
        if let Some(marker) = &event.marker {
            stats
                .by_marker
                .entry(marker.clone())
                .or_default()
                .add_assign(1);
        }
        stats
            .by_project
            .entry(event.project.clone())
            .or_default()
            .add_assign(1);
    }
    stats.distinct_sessions = stats.sessions.len();
    stats.events.extend_from_slice(events);
}

#[cfg(test)]
fn backfill_with_classifier(
    storage: &crate::storage::Storage,
    projects_dir: &Path,
    since: Option<&str>,
    project: Option<&str>,
    dry_run: bool,
    mut classify: impl FnMut(&str, &str) -> Option<Reaction>,
) -> Result<IntentBackfillStats> {
    let since = since_boundary(since)?;
    let transcripts = discover_backfill_transcripts(projects_dir, project)?;
    let mut stats = IntentBackfillStats::default();
    for transcript in transcripts {
        stats.files_scanned += 1;
        let Ok(mut events) = extract_with_classifier(
            &transcript.path,
            &transcript.session_id,
            &transcript.project,
            &mut classify,
        ) else {
            stats.files_skipped += 1;
            continue;
        };
        retain_since(&mut events, since);
        account_events(&mut stats, &events);
        if !dry_run {
            stats.inserted += storage.insert_intent_events(&events)?;
        }
    }
    Ok(stats)
}

pub async fn backfill_intent_events(
    storage: Option<&crate::storage::Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    projects_dir: &Path,
    since: Option<&str>,
    project: Option<&str>,
    dry_run: bool,
) -> Result<IntentBackfillStats> {
    let since = since_boundary(since)?;
    let transcripts = discover_backfill_transcripts(projects_dir, project)?;
    let mut stats = IntentBackfillStats::default();
    for transcript in transcripts {
        stats.files_scanned += 1;
        if std::fs::metadata(&transcript.path).is_ok_and(|metadata| {
            metadata.len() > crate::transcript::instrumentation::MAX_TRANSCRIPT_SCAN_BYTES
        }) {
            stats.files_skipped += 1;
            continue;
        }
        let Ok(extraction) = extract_intent_events_since(
            &transcript.path,
            &transcript.session_id,
            &transcript.project,
            embeddings,
        )
        .await
        else {
            stats.files_skipped += 1;
            continue;
        };
        stats.alignment_misses += extraction.alignment_misses;
        let mut events = extraction.events;
        retain_since(&mut events, since);
        account_events(&mut stats, &events);
        if !dry_run {
            let storage =
                storage.ok_or_else(|| anyhow::anyhow!("intent backfill write requires storage"))?;
            stats.inserted += storage.insert_intent_events(&events)?;
        }
    }
    Ok(stats)
}

#[derive(Debug, Deserialize)]
struct TranscriptLine<'a> {
    #[serde(rename = "type")]
    record_type: Option<&'a str>,
    timestamp: Option<&'a str>,
    uuid: Option<&'a str>,
    #[serde(rename = "isMeta")]
    is_meta: Option<bool>,
    #[serde(rename = "isCompactSummary")]
    is_compact_summary: Option<bool>,
    #[serde(borrow)]
    message: Option<TranscriptMessage<'a>>,
}

#[derive(Debug, Deserialize)]
struct TranscriptMessage<'a> {
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

#[derive(Debug, Deserialize)]
struct TranscriptBlock<'a> {
    #[serde(rename = "type")]
    block_type: Option<&'a str>,
    #[serde(borrow)]
    text: Option<&'a RawValue>,
}

#[derive(Debug)]
struct RawTextSpan {
    decoded_start: usize,
    decoded_end: usize,
    raw_start: usize,
    raw_end: usize,
}

#[derive(Debug)]
struct SourceText {
    turn: u32,
    role: Role,
    is_injected: bool,
    timestamp: Option<String>,
    decoded: String,
    spans: Vec<RawTextSpan>,
}

#[derive(Debug)]
struct SourceTexts {
    entries: Vec<SourceText>,
    alignment_misses: usize,
    last_turn: u32,
}

fn borrowed_subslice_offset(haystack: &str, needle: &str) -> Option<usize> {
    let offset = (needle.as_ptr() as usize).checked_sub(haystack.as_ptr() as usize)?;
    let end = offset.checked_add(needle.len())?;
    (haystack.get(offset..end) == Some(needle)).then_some(offset)
}

fn raw_text_values(line: &str, record: &TranscriptLine<'_>) -> Vec<(String, usize, usize)> {
    let Some(content) = record.message.as_ref().and_then(|message| message.content) else {
        return Vec::new();
    };
    let values: Vec<&RawValue> = if content.get().starts_with('"') {
        vec![content]
    } else {
        serde_json::from_str::<Vec<TranscriptBlock<'_>>>(content.get())
            .unwrap_or_default()
            .into_iter()
            .filter(|block| block.block_type == Some("text"))
            .filter_map(|block| block.text)
            .collect()
    };
    values
        .into_iter()
        .filter_map(|raw| {
            let encoded = raw.get();
            if !encoded.starts_with('"') || !encoded.ends_with('"') {
                return None;
            }
            let decoded = serde_json::from_str::<String>(encoded).ok()?;
            let value_start = borrowed_subslice_offset(line, encoded)?;
            let start = value_start.checked_add(1)?;
            let end = value_start.checked_add(encoded.len().checked_sub(1)?)?;
            Some((decoded, start, end))
        })
        .collect()
}

fn source_texts(path: &Path) -> Result<SourceTexts> {
    let parsed = parse_transcript(path)?;
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading transcript bytes from {}", path.display()))?;
    let mut sources = Vec::new();
    let mut absolute = 0usize;
    let mut next_entry = 0usize;
    let mut alignment_misses = 0usize;
    for segment in raw.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let Ok(record) = serde_json::from_str::<TranscriptLine<'_>>(line) else {
            absolute += segment.len();
            continue;
        };
        let role = match record.record_type {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => {
                absolute += segment.len();
                continue;
            }
        };
        let texts = raw_text_values(line, &record);
        if texts.is_empty() {
            absolute += segment.len();
            continue;
        }
        let joined = texts
            .iter()
            .map(|(text, _, _)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let entry_position = parsed.entries[next_entry..]
            .iter()
            .position(|entry| {
                entry.role == role
                    && record
                        .uuid
                        .is_some_and(|uuid| entry.uuid.as_deref() == Some(uuid))
            })
            .or_else(|| {
                parsed.entries[next_entry..].iter().position(|entry| {
                    entry.role == role
                        && entry.timestamp.as_deref() == record.timestamp
                        && entry.text == joined
                })
            });
        let Some(relative) = entry_position else {
            alignment_misses += 1;
            absolute += segment.len();
            continue;
        };
        let entry_index = next_entry + relative;
        let turn = parsed.entries[entry_index].turn as u32;
        next_entry = entry_index + 1;
        let mut decoded_offset = 0usize;
        let spans = texts
            .iter()
            .map(|(decoded, start, end)| {
                let decoded_start = decoded_offset;
                let decoded_end = decoded_start + decoded.len();
                decoded_offset = decoded_end + 1;
                RawTextSpan {
                    decoded_start,
                    decoded_end,
                    raw_start: absolute + start,
                    raw_end: absolute + end,
                }
            })
            .collect();
        let is_injected = record.is_meta == Some(true)
            || record.is_compact_summary == Some(true)
            || (role == Role::User
                && joined
                    .starts_with("This session is being continued from a previous conversation"));
        sources.push(SourceText {
            turn,
            role,
            is_injected,
            timestamp: record.timestamp.map(str::to_string),
            decoded: joined,
            spans,
        });
        absolute += segment.len();
    }
    let last_turn = parsed
        .entries
        .last()
        .map(|entry| entry.turn as u32)
        .unwrap_or(0);
    Ok(SourceTexts {
        entries: sources,
        alignment_misses,
        last_turn,
    })
}

fn source_texts_tail(raw: &str, starting_turn: u32) -> SourceTexts {
    let parsed = super::parse_transcript_fragment(raw);
    let mut entries = Vec::new();
    let mut relative = 0usize;
    let mut alignment_misses = 0usize;
    let mut next_entry = 0usize;
    for segment in raw.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let Ok(record) = serde_json::from_str::<TranscriptLine<'_>>(line) else {
            if !line.trim().is_empty() {
                alignment_misses += 1;
            }
            relative += segment.len();
            continue;
        };
        let role = match record.record_type {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => {
                relative += segment.len();
                continue;
            }
        };
        let texts = raw_text_values(line, &record);
        if texts.is_empty() {
            relative += segment.len();
            continue;
        }
        let joined = texts
            .iter()
            .map(|(text, _, _)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let entry_position = parsed.entries[next_entry..]
            .iter()
            .position(|entry| {
                entry.role == role
                    && record
                        .uuid
                        .is_some_and(|uuid| entry.uuid.as_deref() == Some(uuid))
            })
            .or_else(|| {
                parsed.entries[next_entry..].iter().position(|entry| {
                    entry.role == role
                        && entry.timestamp.as_deref() == record.timestamp
                        && entry.text == joined
                })
            });
        let Some(entry_position) = entry_position else {
            alignment_misses += 1;
            relative += segment.len();
            continue;
        };
        let entry_index = next_entry + entry_position;
        let turn = starting_turn.saturating_add(parsed.entries[entry_index].turn as u32);
        next_entry = entry_index + 1;
        let mut decoded_offset = 0usize;
        let spans = texts
            .iter()
            .map(|(decoded, start, end)| {
                let decoded_start = decoded_offset;
                let decoded_end = decoded_start + decoded.len();
                decoded_offset = decoded_end + 1;
                RawTextSpan {
                    decoded_start,
                    decoded_end,
                    raw_start: relative + start,
                    raw_end: relative + end,
                }
            })
            .collect();
        let is_injected = record.is_meta == Some(true)
            || record.is_compact_summary == Some(true)
            || (role == Role::User
                && joined
                    .starts_with("This session is being continued from a previous conversation"));
        entries.push(SourceText {
            turn,
            role,
            is_injected,
            timestamp: record.timestamp.map(str::to_string),
            decoded: joined,
            spans,
        });
        relative += segment.len();
    }
    SourceTexts {
        entries,
        alignment_misses,
        last_turn: starting_turn.saturating_add(
            parsed
                .entries
                .last()
                .map(|entry| entry.turn as u32)
                .unwrap_or(0),
        ),
    }
}

fn decoded_boundary_to_raw(encoded: &str, target: usize) -> Option<usize> {
    if target == 0 {
        return Some(0);
    }
    let bytes = encoded.as_bytes();
    let mut raw = 0usize;
    let mut decoded = 0usize;
    while raw < bytes.len() {
        if decoded == target {
            return Some(raw);
        }
        if bytes[raw] != b'\\' {
            let ch = encoded[raw..].chars().next()?;
            raw += ch.len_utf8();
            decoded += ch.len_utf8();
            continue;
        }
        let escape = *bytes.get(raw + 1)?;
        if escape != b'u' {
            raw += 2;
            decoded += 1;
            continue;
        }
        let first = u16::from_str_radix(encoded.get(raw + 2..raw + 6)?, 16).ok()?;
        let (character, consumed) =
            if (0xD800..=0xDBFF).contains(&first) && bytes.get(raw + 6..raw + 8) == Some(b"\\u") {
                let second = u16::from_str_radix(encoded.get(raw + 8..raw + 12)?, 16).ok()?;
                let scalar =
                    0x1_0000 + (((u32::from(first) - 0xD800) << 10) | (u32::from(second) - 0xDC00));
                (char::from_u32(scalar)?, 12)
            } else {
                (char::from_u32(u32::from(first))?, 6)
            };
        raw += consumed;
        decoded += character.len_utf8();
    }
    (decoded == target).then_some(raw)
}

fn sentence_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut in_backticks = false;
    for (offset, ch) in text.char_indices() {
        if ch == '`' {
            in_backticks = !in_backticks;
            continue;
        }
        let next_is_boundary = text
            .get(offset + ch.len_utf8()..)
            .and_then(|tail| tail.chars().next())
            .is_none_or(char::is_whitespace);
        if ch == '\n' || (!in_backticks && matches!(ch, '.' | '!' | '?') && next_is_boundary) {
            let end = if ch == '\n' {
                offset
            } else {
                offset + ch.len_utf8()
            };
            let trimmed_start = start
                + text[start..end]
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(end - start);
            if trimmed_start < end {
                ranges.push((trimmed_start, end));
            }
            start = offset + ch.len_utf8();
        }
    }
    if start < text.len() {
        let tail = &text[start..];
        let leading = tail
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(tail.len());
        let trailing = tail.trim_end().len();
        if leading < trailing {
            ranges.push((start + leading, start + trailing));
        }
    }
    ranges
}

fn blank_range_preserving_lines(bytes: &mut [u8], start: usize, end: usize) {
    for byte in &mut bytes[start..end] {
        if !matches!(*byte, b'\n' | b'\r') {
            *byte = b' ';
        }
    }
}

fn marker_scan_text(text: &str) -> String {
    let mut masked = text.as_bytes().to_vec();

    let mut cursor = 0usize;
    while let Some(relative_open) = text[cursor..].find("```") {
        let open = cursor + relative_open;
        let after_open = open + 3;
        let end = text[after_open..]
            .find("```")
            .map_or(text.len(), |relative_close| after_open + relative_close + 3);
        blank_range_preserving_lines(&mut masked, open, end);
        cursor = end;
        if cursor == text.len() {
            break;
        }
    }

    let mut line_start = 0usize;
    for line in text.split_inclusive('\n') {
        let line_end = line_start + line.trim_end_matches(['\r', '\n']).len();
        if text[line_start..line_end].starts_with("> ") {
            blank_range_preserving_lines(&mut masked, line_start, line_end);
        }
        line_start += line.len();
    }

    let mut open = 0usize;
    while open < masked.len() {
        let Some(relative_open) = masked[open..].iter().position(|byte| *byte == b'`') else {
            break;
        };
        let open_tick = open + relative_open;
        let Some(relative_close) = masked[open_tick + 1..]
            .iter()
            .position(|byte| *byte == b'`')
        else {
            break;
        };
        let close_tick = open_tick + 1 + relative_close;
        if text[open_tick + 1..close_tick].chars().count() > 40 {
            blank_range_preserving_lines(&mut masked, open_tick, close_tick + 1);
        }
        open = close_tick + 1;
    }

    String::from_utf8(masked).expect("masking preserves UTF-8")
}

const WRAPPER_TAGS: &[&str] = &[
    "system-reminder",
    "command-message",
    "command-name",
    "command-args",
    "local-command-stdout",
    "local-command-caveat",
    "user-prompt-submit-hook",
    "task-notification",
    "cross-session-message",
];

fn opening_tag_at(text: &str, start: usize) -> Option<(&str, usize)> {
    let tail = text.get(start..)?;
    if !tail.starts_with('<') {
        return None;
    }
    let name_start = start + 1;
    let name_end = text[name_start..]
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')))
        .map(|relative| name_start + relative)?;
    if name_end == name_start {
        return None;
    }
    let open_end = text[name_end..]
        .find('>')
        .map(|relative| name_end + relative + 1)?;
    Some((&text[name_start..name_end], open_end))
}

fn wrapper_block_end(text: &str, name: &str, open_end: usize) -> usize {
    if text[..open_end].trim_end().ends_with("/>") {
        return open_end;
    }
    let close = format!("</{name}>");
    text[open_end..]
        .find(&close)
        .map_or(text.len(), |relative| open_end + relative + close.len())
}

fn mask_wrapper_blocks(text: &str, masked: &mut [u8]) {
    if let Some((name, open_end)) = opening_tag_at(text, 0) {
        let end = wrapper_block_end(text, name, open_end);
        blank_range_preserving_lines(masked, 0, end);
    }

    let mut cursor = 0usize;
    while let Some(relative) = text[cursor..].find('<') {
        let start = cursor + relative;
        let Some((name, open_end)) = opening_tag_at(text, start) else {
            cursor = start + 1;
            continue;
        };
        if WRAPPER_TAGS.contains(&name) || name.starts_with("ide_") {
            let end = wrapper_block_end(text, name, open_end);
            blank_range_preserving_lines(masked, start, end);
            cursor = end;
        } else {
            cursor = open_end;
        }
    }
}

fn is_bullet_prefix(text: &str) -> bool {
    if text.starts_with("- ") || text.starts_with("* ") {
        return true;
    }
    let digit_count = text.bytes().take_while(u8::is_ascii_digit).count();
    digit_count > 0 && text[digit_count..].starts_with(". ")
}

fn mask_bulleted_lines(text: &str, masked: &mut [u8]) {
    let mut line_start = 0usize;
    for line in text.split_inclusive('\n') {
        let line_end = line_start + line.trim_end_matches(['\r', '\n']).len();
        let content_start = line_start
            + text[line_start..line_end]
                .find(|ch: char| !ch.is_whitespace())
                .unwrap_or(line_end - line_start);
        if is_bullet_prefix(&text[content_start..line_end]) {
            blank_range_preserving_lines(masked, line_start, line_end);
        }
        line_start += line.len();
    }
}

fn reaction_scan_text(text: &str) -> String {
    let mut masked = marker_scan_text(text).into_bytes();
    mask_wrapper_blocks(text, &mut masked);
    mask_bulleted_lines(text, &mut masked);
    String::from_utf8(masked).expect("masking preserves UTF-8")
}

fn target_fields(text: &str) -> (Option<String>, Option<String>) {
    let Some(target) = extract_targets(text).into_iter().next() else {
        return (None, None);
    };
    match target.kind {
        TargetKind::Identifier => (Some(target.phrase), None),
        TargetKind::PathLike => (None, Some(target.phrase)),
    }
}

struct EventContext<'a> {
    raw_transcript: &'a str,
    session_id: &'a str,
    project: &'a str,
    transcript_path: &'a Path,
    byte_base: usize,
    min_turn_exclusive: u32,
    initial_prior_user: &'a str,
    initial_prior_assistant: &'a str,
}

struct DetectionReceipt {
    detector: Option<IntentDetector>,
    classifier_score: Option<String>,
    marker: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct ClassifierEvidence {
    reaction: Option<Reaction>,
    proposed: Option<Reaction>,
    confidence: f32,
}

fn classifier_score(evidence: ClassifierEvidence) -> Option<String> {
    evidence
        .proposed
        .map(|reaction| format!("{}:{:.6}", reaction.as_str(), evidence.confidence))
}

fn abandonment_lexicon_hash() -> String {
    let material = format!(
        "{}\n{}",
        ABANDONMENT_LEXICON_VERSION,
        ABANDONMENT_MARKERS.join("\n")
    );
    blake3::hash(material.as_bytes()).to_hex().to_string()
}

fn make_event(
    source: &SourceText,
    decoded_range: std::ops::Range<usize>,
    kind: IntentEventKind,
    prior_claim: &str,
    context: &EventContext<'_>,
    detection: DetectionReceipt,
) -> Option<IntentEvent> {
    let span = source.spans.iter().find(|span| {
        decoded_range.start >= span.decoded_start && decoded_range.end <= span.decoded_end
    })?;
    let encoded = context.raw_transcript.get(span.raw_start..span.raw_end)?;
    let relative_start =
        decoded_boundary_to_raw(encoded, decoded_range.start - span.decoded_start)?;
    let relative_end = decoded_boundary_to_raw(encoded, decoded_range.end - span.decoded_start)?;
    let relative_byte_start = span.raw_start.checked_add(relative_start)?;
    let relative_byte_end = span.raw_start.checked_add(relative_end)?;
    let quote = context
        .raw_transcript
        .get(relative_byte_start..relative_byte_end)?
        .to_string();
    let byte_start = context.byte_base.checked_add(relative_byte_start)?;
    let byte_end = context.byte_base.checked_add(relative_byte_end)?;
    let decoded_quote = source.decoded.get(decoded_range)?;
    let (symbol, file) = target_fields(decoded_quote);
    Some(IntentEvent {
        session_id: context.session_id.to_string(),
        project: context.project.to_string(),
        turn: source.turn,
        kind,
        quote,
        transcript_path: context.transcript_path.to_path_buf(),
        byte_start,
        byte_end,
        prior_claim: truncate_chars(prior_claim, PRIOR_CLAIM_CHARS),
        symbol,
        file,
        classifier_hash: if kind == IntentEventKind::Abandoned {
            abandonment_lexicon_hash()
        } else {
            crate::hooks::reaction::classifier_hash()
        },
        detector: detection.detector,
        classifier_score: detection.classifier_score,
        marker: detection.marker,
        ts: source
            .timestamp
            .clone()
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
    })
}

fn is_classifiable_user(text: &str, prior_user: &str) -> bool {
    !prior_user.is_empty()
        && !crate::hooks::reaction::is_queued_message(text)
        && !crate::transcript::instrumentation::is_noisy_steer_text(text)
        && !crate::extraction::provenance::is_csr_emission(text)
        && crate::extraction::provenance::extractable(text).is_some()
}

fn classification_plan(
    sources: &SourceTexts,
    min_turn_exclusive: u32,
    initial_prior_user: &str,
) -> (Vec<String>, BTreeSet<u32>) {
    let mut prior_user = initial_prior_user.to_string();
    let mut texts = Vec::new();
    let mut turns = BTreeSet::new();
    for source in &sources.entries {
        if source.role != Role::User || source.is_injected {
            continue;
        }
        if source.turn > min_turn_exclusive && is_classifiable_user(&source.decoded, &prior_user) {
            texts.push(source.decoded.clone());
            turns.insert(source.turn);
        }
        prior_user = source.decoded.clone();
    }
    (texts, turns)
}

fn starts_with_command_after(text: &str, prefix: &str) -> bool {
    text.strip_prefix(prefix)
        .is_some_and(|tail| tail.split_whitespace().next().is_some())
}

fn lexical_reaction_sentence(text: &str) -> Option<(IntentEventKind, usize, usize, &'static str)> {
    let scan_text = reaction_scan_text(text);
    for (start, end) in sentence_ranges(&scan_text) {
        let sentence = scan_text[start..end].trim();
        let lower = sentence.to_ascii_lowercase();
        let correction = if lower.starts_with("no,") || lower.starts_with("no.") {
            Some("no")
        } else if lower.starts_with("no that") || lower.starts_with("no, that") {
            Some("no-that")
        } else if lower.starts_with("not what i asked")
            || lower.starts_with("that is not what i asked")
            || lower.starts_with("that's not what i asked")
        {
            Some("not-what-i-asked")
        } else if [
            "that's wrong",
            "that is wrong",
            "that's not right",
            "that is not right",
            "that's incorrect",
            "that is incorrect",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        {
            Some("that-is-wrong")
        } else if [
            "wrong file",
            "wrong agent",
            "wrong branch",
            "wrong approach",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        {
            Some("wrong-target")
        } else if lower == "stop."
            || lower == "stop!"
            || lower == "stop"
            || lower.starts_with("stop,")
        {
            Some("stop")
        } else if starts_with_command_after(&lower, "don't ")
            || starts_with_command_after(&lower, "do not ")
            || (starts_with_command_after(&lower, "never ") && !lower.starts_with("never mind"))
        {
            Some("negative-command")
        } else if lower.starts_with("i said ") {
            Some("i-said")
        } else if starts_with_command_after(&lower, "revert") {
            Some("revert")
        } else if starts_with_command_after(&lower, "undo") {
            Some("undo")
        } else {
            None
        };
        if let Some(marker) = correction {
            return Some((IntentEventKind::Correction, start, end, marker));
        }

        let redirect = if lower.starts_with("actually, forget ")
            || lower.starts_with("actually forget ")
            || lower.starts_with("actually, scrap ")
            || lower.starts_with("actually scrap ")
            || lower.starts_with("actually, drop ")
            || lower.starts_with("actually drop ")
        {
            Some("actually-pivot")
        } else if lower.starts_with("forget that") || lower.starts_with("forget the ") {
            Some("forget")
        } else if lower.starts_with("never mind") {
            Some("never-mind")
        } else if lower.starts_with("let's ") && lower.contains(" first instead") {
            Some("first-instead")
        } else if (lower.ends_with(" instead.") || lower.ends_with(" instead"))
            && (lower.contains(" don't ")
                || lower.contains(" do not ")
                || lower.contains(" not ")
                || lower.starts_with("not ")
                || lower.starts_with("no"))
        {
            Some("negated-instead")
        } else {
            None
        };
        if let Some(marker) = redirect {
            return Some((IntentEventKind::Redirect, start, end, marker));
        }
    }
    None
}

fn classifier_marker_sentence(text: &str) -> Option<(usize, usize, &'static str)> {
    let scan_text = reaction_scan_text(text);
    for (start, end) in sentence_ranges(&scan_text) {
        let lower = scan_text[start..end].trim().to_ascii_lowercase();
        let marker = if lower.starts_with("you misunderstood") {
            Some("you-misunderstood")
        } else if lower.starts_with("this does not meet")
            || lower.starts_with("this is still wrong")
        {
            Some("explicit-failure")
        } else if lower.starts_with("change of direction") || lower.starts_with("new task:") {
            Some("explicit-pivot")
        } else if lower.starts_with("put that aside") {
            Some("put-aside")
        } else {
            None
        };
        if let Some(marker) = marker {
            return Some((start, end, marker));
        }
    }
    None
}

fn abandonment_marker(sentence: &str) -> Option<&'static str> {
    let lower = sentence.trim().to_ascii_lowercase();
    let first_person = |verb: &str| {
        lower.starts_with(verb)
            || [
                "i am ", "i'm ", "i will ", "i'll ", "we are ", "we're ", "we will ", "we'll ",
            ]
            .iter()
            .any(|prefix| lower.starts_with(&format!("{prefix}{verb}")))
    };
    let cannot_so = lower.split_once(" so ").is_some_and(|(_, outcome)| {
        let has_inability = lower.contains("cannot ") || lower.contains("can't ");
        let explicit_pivot = [
            "i'll ",
            "i will ",
            "we'll ",
            "we will ",
            "switching ",
            "trying ",
            "retrying ",
            "using ",
            "falling back ",
            "skipping ",
            "dropping ",
        ]
        .iter()
        .any(|prefix| outcome.trim_start().starts_with(prefix));
        has_inability && explicit_pivot
    });
    if first_person("abandoning") {
        Some("abandoning")
    } else if first_person("dropping") {
        Some("dropping")
    } else if first_person("reverting") {
        Some("reverting")
    } else if lower.starts_with("not doing")
        || lower.starts_with("i am not doing")
        || lower.starts_with("i'm not doing")
        || lower.starts_with("we are not doing")
        || lower.starts_with("we're not doing")
    {
        Some("not doing")
    } else if lower.starts_with("gave up on")
        || lower.starts_with("i gave up on")
        || lower.starts_with("we gave up on")
    {
        Some("gave up on")
    } else if lower.contains("out of scope") {
        Some("out of scope")
    } else if lower.contains("was rejected") {
        Some("was rejected")
    } else if lower.contains("got blocked") {
        Some("got blocked")
    } else if cannot_so {
        Some("cannot-so")
    } else if lower.contains("retrying with") {
        Some("retrying with")
    } else if lower.contains("trying ") && lower.contains(" instead") {
        Some("trying-instead")
    } else if lower.contains("falling back to") {
        Some("falling back to")
    } else if lower.contains("switching to") && lower.contains(" instead") {
        Some("switching-to-instead")
    } else {
        None
    }
}

fn extract_from_sources(
    source_texts: &SourceTexts,
    context: &EventContext<'_>,
    classifiable_turns: &BTreeSet<u32>,
    mut classify: impl FnMut(&str, &str) -> ClassifierEvidence,
) -> (Vec<IntentEvent>, String, String) {
    let mut events = Vec::new();
    let mut prior_user = context.initial_prior_user.to_string();
    let mut prior_assistant = context.initial_prior_assistant.to_string();
    for source in &source_texts.entries {
        match source.role {
            Role::User => {
                if source.is_injected {
                    continue;
                }
                let text = &source.decoded;
                if classifiable_turns.contains(&source.turn) {
                    let evidence = classify(text, &prior_user);
                    let lexical = lexical_reaction_sentence(text);
                    if let Some((kind, start, end, marker)) = lexical {
                        let classifier_kind = match evidence.reaction {
                            Some(Reaction::Correction) => Some(IntentEventKind::Correction),
                            Some(Reaction::Redirect) => Some(IntentEventKind::Redirect),
                            _ => None,
                        };
                        let detector = if classifier_kind == Some(kind) {
                            IntentDetector::Both
                        } else {
                            IntentDetector::Lexical
                        };
                        if let Some(event) = make_event(
                            source,
                            start..end,
                            kind,
                            &prior_assistant,
                            context,
                            DetectionReceipt {
                                detector: Some(detector),
                                classifier_score: classifier_score(evidence),
                                marker: Some(marker.to_string()),
                            },
                        ) {
                            events.push(event);
                        }
                    } else if let (Some((start, end, marker)), Some(kind)) = (
                        classifier_marker_sentence(text),
                        match evidence.reaction {
                            Some(Reaction::Correction) => Some(IntentEventKind::Correction),
                            Some(Reaction::Redirect) => Some(IntentEventKind::Redirect),
                            _ => None,
                        },
                    ) {
                        if let Some(event) = make_event(
                            source,
                            start..end,
                            kind,
                            &prior_assistant,
                            context,
                            DetectionReceipt {
                                detector: Some(IntentDetector::Classifier),
                                classifier_score: classifier_score(evidence),
                                marker: Some(marker.to_string()),
                            },
                        ) {
                            events.push(event);
                        }
                    }
                }
                prior_user = text.clone();
            }
            Role::Assistant => {
                if source.is_injected {
                    continue;
                }
                let text = &source.decoded;
                if source.turn > context.min_turn_exclusive
                    && !crate::extraction::provenance::is_csr_emission(text)
                    && crate::extraction::provenance::extractable(text).is_some()
                {
                    let scan_text = marker_scan_text(text);
                    for (start, end) in sentence_ranges(&scan_text) {
                        let sentence = &scan_text[start..end];
                        if let Some(marker) = abandonment_marker(sentence) {
                            if let Some(event) = make_event(
                                source,
                                start..end,
                                IntentEventKind::Abandoned,
                                &prior_assistant,
                                context,
                                DetectionReceipt {
                                    detector: Some(IntentDetector::Lexical),
                                    classifier_score: None,
                                    marker: Some(marker.to_string()),
                                },
                            ) {
                                events.push(event);
                            }
                        }
                    }
                }
                prior_assistant = text.clone();
            }
            Role::System => {}
        }
    }
    (events, prior_user, prior_assistant)
}

#[cfg(test)]
fn extract_with_classifier(
    transcript_path: &Path,
    session_id: &str,
    project: &str,
    mut classify: impl FnMut(&str, &str) -> Option<Reaction>,
) -> Result<Vec<IntentEvent>> {
    let sources = source_texts(transcript_path)?;
    let raw = std::fs::read_to_string(transcript_path)?;
    let context = EventContext {
        raw_transcript: &raw,
        session_id,
        project,
        transcript_path,
        byte_base: 0,
        min_turn_exclusive: 0,
        initial_prior_user: "",
        initial_prior_assistant: "",
    };
    let (_, classifiable_turns) = classification_plan(&sources, 0, "");
    Ok(
        extract_from_sources(&sources, &context, &classifiable_turns, |text, prior| {
            let reaction = classify(text, prior);
            ClassifierEvidence {
                reaction,
                proposed: reaction,
                confidence: reaction.map_or(0.0, |_| 1.0),
            }
        })
        .0,
    )
}

/// Deterministically extract receipted corrections, redirects, and explicit
/// abandonment statements from one Claude transcript. The only model work is
/// the existing local MiniLM reaction classifier; no generative model is used.
#[derive(Debug)]
pub struct IntentExtraction {
    pub events: Vec<IntentEvent>,
    pub alignment_misses: usize,
    pub last_turn: u32,
    pub byte_offset: usize,
    pub prior_user: String,
    pub prior_assistant: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IntentHighWater {
    pub byte_offset: usize,
    pub last_turn: u32,
    pub prior_user: String,
    pub prior_assistant: String,
}

impl From<&IntentExtraction> for IntentHighWater {
    fn from(extraction: &IntentExtraction) -> Self {
        Self {
            byte_offset: extraction.byte_offset,
            last_turn: extraction.last_turn,
            prior_user: extraction.prior_user.clone(),
            prior_assistant: extraction.prior_assistant.clone(),
        }
    }
}

pub async fn extract_intent_events_since(
    transcript_path: &Path,
    session_id: &str,
    project: &str,
    embeddings: &Arc<EmbeddingEngine>,
) -> Result<IntentExtraction> {
    extract_intent_events_after(transcript_path, session_id, project, embeddings, 0).await
}

pub async fn extract_intent_events_after(
    transcript_path: &Path,
    session_id: &str,
    project: &str,
    embeddings: &Arc<EmbeddingEngine>,
    min_turn_exclusive: u32,
) -> Result<IntentExtraction> {
    let sources = source_texts(transcript_path)?;
    let (classifiable, classifiable_turns) = classification_plan(&sources, min_turn_exclusive, "");

    let raw = std::fs::read_to_string(transcript_path)?;
    let last_turn = sources.last_turn.max(min_turn_exclusive);
    let context = EventContext {
        raw_transcript: &raw,
        session_id,
        project,
        transcript_path,
        byte_base: 0,
        min_turn_exclusive,
        initial_prior_user: "",
        initial_prior_assistant: "",
    };

    if classifiable.is_empty() {
        let (events, prior_user, prior_assistant) =
            extract_from_sources(&sources, &context, &classifiable_turns, |_text, _prior| {
                ClassifierEvidence {
                    reaction: None,
                    proposed: None,
                    confidence: 0.0,
                }
            });
        return Ok(IntentExtraction {
            events,
            alignment_misses: sources.alignment_misses,
            last_turn,
            byte_offset: raw.len(),
            prior_user,
            prior_assistant,
        });
    }

    let probes = crate::hooks::reaction::ProbeSet::load_or_build(embeddings).await;
    let vectors = if probes.is_some() {
        let engine = embeddings.clone();
        tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = classifiable.iter().map(String::as_str).collect();
            engine.embed(&refs)
        })
        .await
        .ok()
        .and_then(std::result::Result::ok)
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut vectors = vectors.into_iter();
    let (events, prior_user, prior_assistant) = extract_from_sources(
        &sources,
        &context,
        &classifiable_turns,
        |text, prior_user| {
            let Some(vector) = vectors.next() else {
                return ClassifierEvidence {
                    reaction: None,
                    proposed: None,
                    confidence: 0.0,
                };
            };
            let Some(probes) = probes.as_ref() else {
                return ClassifierEvidence {
                    reaction: None,
                    proposed: None,
                    confidence: 0.0,
                };
            };
            let decision = probes.classify(text, prior_user, &vector, None);
            ClassifierEvidence {
                reaction: decision.reaction,
                proposed: decision.proposed_reaction,
                confidence: decision.confidence,
            }
        },
    );
    Ok(IntentExtraction {
        events,
        alignment_misses: sources.alignment_misses,
        last_turn,
        byte_offset: raw.len(),
        prior_user,
        prior_assistant,
    })
}

/// Stop-hook extractor: read only bytes appended after the durable high-water
/// mark. The carried user/assistant context is the minimum state needed to
/// classify the first new user entry without reopening the old prefix.
pub async fn extract_intent_events_incremental(
    transcript_path: &Path,
    session_id: &str,
    project: &str,
    embeddings: &Arc<EmbeddingEngine>,
    high_water: &IntentHighWater,
) -> Result<IntentExtraction> {
    let mut state = high_water.clone();
    let metadata = std::fs::metadata(transcript_path)?;
    if state.byte_offset as u64 > metadata.len() {
        state = IntentHighWater::default();
    }
    let mut file = std::fs::File::open(transcript_path)?;
    file.seek(SeekFrom::Start(state.byte_offset as u64))?;
    let mut raw = String::new();
    file.read_to_string(&mut raw)?;
    let sources = source_texts_tail(&raw, state.last_turn);

    let (classifiable, classifiable_turns) =
        classification_plan(&sources, state.last_turn, &state.prior_user);

    let probes = if classifiable.is_empty() {
        None
    } else {
        crate::hooks::reaction::ProbeSet::load_or_build(embeddings).await
    };
    let mut vectors = if probes.is_some() {
        let engine = embeddings.clone();
        tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = classifiable.iter().map(String::as_str).collect();
            engine.embed(&refs)
        })
        .await
        .ok()
        .and_then(std::result::Result::ok)
        .unwrap_or_default()
        .into_iter()
    } else {
        Vec::new().into_iter()
    };
    let context = EventContext {
        raw_transcript: &raw,
        session_id,
        project,
        transcript_path,
        byte_base: state.byte_offset,
        min_turn_exclusive: state.last_turn,
        initial_prior_user: &state.prior_user,
        initial_prior_assistant: &state.prior_assistant,
    };
    let (events, prior_user, prior_assistant) = extract_from_sources(
        &sources,
        &context,
        &classifiable_turns,
        |text, preceding_user| {
            let Some(vector) = vectors.next() else {
                return ClassifierEvidence {
                    reaction: None,
                    proposed: None,
                    confidence: 0.0,
                };
            };
            let Some(probes) = probes.as_ref() else {
                return ClassifierEvidence {
                    reaction: None,
                    proposed: None,
                    confidence: 0.0,
                };
            };
            let decision = probes.classify(text, preceding_user, &vector, None);
            ClassifierEvidence {
                reaction: decision.reaction,
                proposed: decision.proposed_reaction,
                confidence: decision.confidence,
            }
        },
    );
    Ok(IntentExtraction {
        events,
        alignment_misses: sources.alignment_misses,
        last_turn: sources.last_turn,
        byte_offset: state.byte_offset + raw.len(),
        prior_user,
        prior_assistant,
    })
}

pub async fn extract_intent_events(
    transcript_path: &Path,
    session_id: &str,
    project: &str,
    embeddings: &Arc<EmbeddingEngine>,
) -> Result<Vec<IntentEvent>> {
    Ok(
        extract_intent_events_since(transcript_path, session_id, project, embeddings)
            .await?
            .events,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::reaction::Reaction;

    #[test]
    fn correction_receipt_reslices_to_the_exact_raw_quote() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        let raw = concat!(
            "{\"type\":\"user\",\"timestamp\":\"2026-09-01T10:00:00Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Build the first approach\"}]}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-09-01T10:01:00Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"I changed `src/old.rs`.\"}]}}\n",
            "{\"type\":\"user\",\"timestamp\":\"2026-09-01T10:02:00Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"No, change `src/new.rs` instead\"}]}}\n",
        );
        std::fs::write(&transcript, raw).unwrap();

        let events = extract_with_classifier(
            &transcript,
            "parent-session",
            "project-a",
            |text, _prior_user| {
                (text == "No, change `src/new.rs` instead").then_some(Reaction::Correction)
            },
        )
        .unwrap();

        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.kind, IntentEventKind::Correction);
        assert_eq!(event.turn, 3);
        assert_eq!(event.prior_claim, "I changed `src/old.rs`.");
        assert_eq!(event.file.as_deref(), Some("src/new.rs"));
        assert_eq!(event.symbol, None);
        assert_eq!(event.session_id, "parent-session");
        let bytes = std::fs::read(&transcript).unwrap();
        assert_eq!(
            &bytes[event.byte_start..event.byte_end],
            event.quote.as_bytes()
        );
        assert_eq!(event.quote, "No, change `src/new.rs` instead");
    }

    #[test]
    fn abandoned_marker_is_sentence_scoped_and_unmarked_prose_abstains() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        let raw = concat!(
            "{\"type\":\"assistant\",\"timestamp\":\"2026-09-01T10:00:00Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"The first idea may work. We are dropping `OldParser` instead of changing it. The final design is ready.\"}]}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-09-01T10:01:00Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"This is ordinary progress without a marker.\"}]}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-09-01T10:02:00Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"The network is dropping packets. The operation cannot continue so the caller receives an error.\"}]}}\n",
        );
        std::fs::write(&transcript, raw).unwrap();

        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, IntentEventKind::Abandoned);
        assert_eq!(
            events[0].quote,
            "We are dropping `OldParser` instead of changing it."
        );
        assert_eq!(events[0].symbol.as_deref(), Some("OldParser"));
        let bytes = std::fs::read(&transcript).unwrap();
        assert_eq!(
            &bytes[events[0].byte_start..events[0].byte_end],
            events[0].quote.as_bytes()
        );
    }

    #[test]
    fn csr_emission_is_never_classified_as_a_correction() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        let raw = format!(
            "{{\"type\":\"user\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{} no, use OtherThing\"}}]}}}}\n",
            crate::extraction::provenance::RECAP_SENTINEL
        );
        std::fs::write(&transcript, raw).unwrap();

        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| {
            Some(Reaction::Correction)
        })
        .unwrap();

        assert!(events.is_empty());
    }

    #[test]
    fn multiple_text_blocks_in_one_user_entry_are_one_logical_turn() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","uuid":"u1","timestamp":"2026-09-01T10:00:00Z","message":{"content":[{"type":"text","text":"Implement parser"},{"type":"text","text":"No, use NewParser"}]}}"#,
                "\n",
            ),
        )
        .unwrap();

        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| {
            Some(Reaction::Correction)
        })
        .unwrap();

        assert!(
            events.is_empty(),
            "one opening entry is not a reaction turn"
        );
    }

    #[test]
    fn meta_user_entries_are_not_corrections_or_prior_user_context() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
                "\n",
                r#"{"type":"user","uuid":"u2","isMeta":true,"message":{"content":"Never --no-verify."}}"#,
                "\n",
            ),
        )
        .unwrap();

        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
        assert!(events.is_empty());

        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","uuid":"u1","isMeta":true,"message":{"content":"Injected setup"}}"#,
                "\n",
                r#"{"type":"user","uuid":"u2","message":{"content":"Never --no-verify."}}"#,
                "\n",
            ),
        )
        .unwrap();
        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
        assert!(
            events.is_empty(),
            "metadata must not establish prior-user context"
        );
    }

    #[test]
    fn non_meta_negative_command_is_a_correction_with_its_marker() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
                "\n",
                r#"{"type":"user","uuid":"u2","message":{"content":"Never --no-verify."}}"#,
                "\n",
            ),
        )
        .unwrap();

        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, IntentEventKind::Correction);
        assert_eq!(events[0].marker.as_deref(), Some("negative-command"));
    }

    #[test]
    fn compact_summary_is_not_a_correction_but_the_same_human_turn_is() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
                "\n",
                r#"{"type":"user","uuid":"u2","isCompactSummary":true,"message":{"content":"Never --no-verify."}}"#,
                "\n",
            ),
        )
        .unwrap();

        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
        assert!(events.is_empty());

        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
                "\n",
                r#"{"type":"user","uuid":"u2","message":{"content":"Never --no-verify."}}"#,
                "\n",
            ),
        )
        .unwrap();
        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, IntentEventKind::Correction);
    }

    #[test]
    fn legacy_continuation_summary_is_not_a_correction_or_prior_user_context() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        let summary =
            "This session is being continued from a previous conversation\nNever --no-verify.";
        let first = serde_json::json!({
            "type": "user",
            "uuid": "u1",
            "message": { "content": "Build parser" }
        });
        let second = serde_json::json!({
            "type": "user",
            "uuid": "u2",
            "message": { "content": summary }
        });
        std::fs::write(&transcript, format!("{first}\n{second}\n")).unwrap();

        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
        assert!(events.is_empty());

        let first = serde_json::json!({
            "type": "user",
            "uuid": "u1",
            "message": { "content": summary }
        });
        let second = serde_json::json!({
            "type": "user",
            "uuid": "u2",
            "message": { "content": "Never --no-verify." }
        });
        std::fs::write(&transcript, format!("{first}\n{second}\n")).unwrap();
        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
        assert!(
            events.is_empty(),
            "a continuation summary must not establish prior-user context"
        );
    }

    #[test]
    fn wrapper_tagged_correction_marker_is_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
                "\n",
                r#"{"type":"user","uuid":"u2","message":{"content":"Pasted context:\n<system-reminder>\nNever --no-verify.\n</system-reminder>\nEnd context."}}"#,
                "\n",
            ),
        )
        .unwrap();

        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn bulleted_negative_command_is_ignored_but_plain_command_fires() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        for bullet in ["- do not push", "* do not push", "1. do not push"] {
            let first = serde_json::json!({
                "type": "user",
                "uuid": "u1",
                "message": { "content": "Build parser" }
            });
            let second = serde_json::json!({
                "type": "user",
                "uuid": "u2",
                "message": { "content": bullet }
            });
            std::fs::write(&transcript, format!("{first}\n{second}\n")).unwrap();
            let events =
                extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
            assert!(events.is_empty(), "bullet {bullet:?} must abstain");
        }

        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
                "\n",
                r#"{"type":"user","uuid":"u2","message":{"content":"do not push"}}"#,
                "\n",
            ),
        )
        .unwrap();
        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, IntentEventKind::Correction);
    }

    #[test]
    fn fenced_and_quoted_correction_markers_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
                "\n",
                r#"{"type":"user","uuid":"u2","message":{"content":"Example:\n```text\nNever --no-verify.\n```\n> Do not forward this task.\nDone."}}"#,
                "\n",
            ),
        )
        .unwrap();

        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn long_inline_code_correction_markers_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        let inline = format!("`Never {}`", "x".repeat(64));
        let first = serde_json::json!({
            "type": "user",
            "uuid": "u1",
            "message": { "content": "Build parser" }
        });
        let second = serde_json::json!({
            "type": "user",
            "uuid": "u2",
            "message": { "content": inline }
        });
        std::fs::write(&transcript, format!("{first}\n{second}\n")).unwrap();

        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn prior_claim_joins_all_text_blocks_from_the_preceding_assistant_entry() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
                "\n",
                r#"{"type":"assistant","uuid":"a1","message":{"content":[{"type":"text","text":"I chose OldParser."},{"type":"text","text":"It lives in old.rs."}]}}"#,
                "\n",
                r#"{"type":"user","uuid":"u2","message":{"content":"No, use NewParser."}}"#,
                "\n",
            ),
        )
        .unwrap();

        let events = extract_with_classifier(&transcript, "s", "p", |text, _prior| {
            (text == "No, use NewParser.").then_some(Reaction::Correction)
        })
        .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].prior_claim,
            "I chose OldParser.\nIt lives in old.rs."
        );
    }

    #[test]
    fn meta_assistant_entry_does_not_replace_prior_claim() {
        let temp = tempfile::tempdir().unwrap();
        let transcript = temp.path().join("session.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
                "\n",
                r#"{"type":"assistant","uuid":"a1","message":{"content":"I chose OldParser."}}"#,
                "\n",
                r#"{"type":"assistant","uuid":"a2","isMeta":true,"message":{"content":"Injected hook output."}}"#,
                "\n",
                r#"{"type":"user","uuid":"u2","message":{"content":"No, use NewParser."}}"#,
                "\n",
            ),
        )
        .unwrap();

        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].prior_claim, "I chose OldParser.");
    }

    #[test]
    fn textbook_probe_turns_produce_four_corrections_and_one_redirect() {
        let probe = Path::new("/tmp/csr-b2/probe-projects/-tmp-probe/probe-1.jsonl");
        assert!(probe.is_file(), "B2 probe transcript must be present");
        let events =
            extract_with_classifier(probe, "probe-1", "-tmp-probe", |_text, _prior| None).unwrap();
        let user_events: Vec<_> = events
            .iter()
            .filter(|event| event.kind != IntentEventKind::Abandoned)
            .collect();
        assert_eq!(
            user_events
                .iter()
                .filter(|event| event.kind == IntentEventKind::Correction)
                .count(),
            4
        );
        assert_eq!(
            user_events
                .iter()
                .filter(|event| event.kind == IntentEventKind::Redirect)
                .count(),
            1
        );
        assert_eq!(
            user_events
                .iter()
                .find(|event| event.kind == IntentEventKind::Redirect)
                .map(|event| event.turn),
            Some(15)
        );
        assert!(user_events
            .iter()
            .all(|event| !event.quote.contains("thanks") && event.quote != "continue"));
    }

    #[test]
    fn two_child_transcripts_with_identical_coordinates_both_persist() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("agent-first.jsonl");
        let second = temp.path().join("agent-second.jsonl");
        let line = concat!(
            r#"{"type":"assistant","timestamp":"2026-09-01T10:00:00Z","message":{"content":"I am dropping OldParser."}}"#,
            "\n"
        );
        std::fs::write(&first, line).unwrap();
        std::fs::write(&second, line).unwrap();
        let mut events =
            extract_with_classifier(&first, "parent", "p", |_text, _prior| None).unwrap();
        events
            .extend(extract_with_classifier(&second, "parent", "p", |_text, _prior| None).unwrap());
        let storage = crate::storage::Storage::open_memory().unwrap();

        assert_eq!(storage.insert_intent_events(&events).unwrap(), 2);
        assert_eq!(storage.list_intent_events(None, None).unwrap().len(), 2);
    }

    #[test]
    fn backfill_dry_run_counts_events_without_writing() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        let project_dir = projects.join("project-a");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("session-1.jsonl"),
            concat!(
                r#"{"type":"assistant","timestamp":"2026-09-01T10:00:00Z","message":{"content":"We are abandoning `OldParser`."}}"#,
                "\n",
            ),
        )
        .unwrap();
        let storage = crate::storage::Storage::open_memory().unwrap();

        let stats =
            backfill_with_classifier(&storage, &projects, None, None, true, |_text, _prior| None)
                .unwrap();

        assert_eq!(stats.files_scanned, 1);
        assert_eq!(stats.total_events(), 1);
        assert_eq!(stats.by_kind.get("abandoned"), Some(&1));
        assert_eq!(stats.by_project.get("project-a"), Some(&1));
        assert_eq!(stats.inserted, 0);
        assert!(storage.list_intent_events(None, None).unwrap().is_empty());
    }

    #[test]
    fn agreement_is_whitespace_normalized_and_limited_to_shared_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = temp.path().join("ledger");
        std::fs::create_dir(&ledger).unwrap();
        std::fs::write(
            ledger.join("records.jsonl"),
            concat!(
                r#"{"session_id":"shared","narration":{"abandoned":[{"quote":"--bare was rejected because auth failed","verified":true}]}}"#,
                "\n",
                r#"{"session_id":"ledger-only","narration":{"abandoned":[{"quote":"never seen","verified":true}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        let event = IntentEvent {
            session_id: "shared".into(),
            project: "p".into(),
            turn: 2,
            kind: IntentEventKind::Abandoned,
            quote: "--bare  was rejected because auth failed.".into(),
            transcript_path: temp.path().join("shared.jsonl"),
            byte_start: 0,
            byte_end: 40,
            prior_claim: String::new(),
            symbol: None,
            file: None,
            classifier_hash: abandonment_lexicon_hash(),
            detector: Some(IntentDetector::Lexical),
            classifier_score: None,
            marker: Some("was rejected".into()),
            ts: "2026-09-01T00:00:00Z".into(),
        };

        let report = agreement_report(&ledger, &[event]).unwrap();
        assert_eq!(report.shared_sessions, 1);
        assert_eq!(
            report.by_kind.get("abandoned"),
            Some(&AgreementCounts {
                ledger_total: 1,
                ledger_matched: 1,
                event_total: 1,
                event_matched: 1,
            })
        );
    }
}
