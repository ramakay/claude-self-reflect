use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{collections::BTreeMap, ops::AddAssign};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::dream::backfill::intent_channel::{extract_targets, TargetKind};
use crate::embeddings::EmbeddingEngine;
use crate::hooks::reaction::Reaction;

use super::{parse_transcript, truncate_chars, Role};

const PRIOR_CLAIM_CHARS: usize = 240;
const ABANDONMENT_MARKERS: &[&str] = &[
    "abandon",
    "dropping",
    "out of scope",
    "reverting",
    "not doing",
    "instead of",
    "gave up",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentEventKind {
    Correction,
    Redirect,
    Abandoned,
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
    pub ts: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntentBackfillStats {
    pub files_scanned: usize,
    pub files_skipped: usize,
    pub inserted: usize,
    pub by_kind: BTreeMap<String, usize>,
    pub by_project: BTreeMap<String, usize>,
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
            "  correction={} redirect={} abandoned={} total={} {}={}\n",
            self.by_kind.get("correction").copied().unwrap_or(0),
            self.by_kind.get("redirect").copied().unwrap_or(0),
            self.by_kind.get("abandoned").copied().unwrap_or(0),
            self.total_events(),
            if dry_run { "would_insert" } else { "inserted" },
            if dry_run {
                self.total_events()
            } else {
                self.inserted
            }
        ));
        output
    }
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
        stats
            .by_kind
            .entry(event.kind.as_str().to_string())
            .or_default()
            .add_assign(1);
        stats
            .by_project
            .entry(event.project.clone())
            .or_default()
            .add_assign(1);
    }
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
        let Ok(mut events) = extract_intent_events(
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
struct SourceText {
    turn: u32,
    role: Role,
    timestamp: Option<String>,
    decoded: String,
    raw_start: usize,
    raw_end: usize,
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

fn source_texts(path: &Path) -> Result<Vec<SourceText>> {
    let parsed = parse_transcript(path)?;
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading transcript bytes from {}", path.display()))?;
    let mut sources = Vec::new();
    let mut absolute = 0usize;
    let mut next_entry = 0usize;
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
            absolute += segment.len();
            continue;
        };
        let entry_index = next_entry + relative;
        let turn = parsed.entries[entry_index].turn as u32;
        next_entry = entry_index + 1;
        for (decoded, start, end) in texts {
            sources.push(SourceText {
                turn,
                role,
                timestamp: record.timestamp.map(str::to_string),
                decoded,
                raw_start: absolute + start,
                raw_end: absolute + end,
            });
        }
        absolute += segment.len();
    }
    Ok(sources)
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
    for (offset, ch) in text.char_indices() {
        if matches!(ch, '.' | '!' | '?') {
            let end = offset + ch.len_utf8();
            let trimmed_start = start
                + text[start..end]
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(end - start);
            if trimmed_start < end {
                ranges.push((trimmed_start, end));
            }
            start = end;
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
}

fn make_event(
    source: &SourceText,
    decoded_range: std::ops::Range<usize>,
    kind: IntentEventKind,
    prior_claim: &str,
    context: &EventContext<'_>,
) -> Option<IntentEvent> {
    let encoded = context
        .raw_transcript
        .get(source.raw_start..source.raw_end)?;
    let relative_start = decoded_boundary_to_raw(encoded, decoded_range.start)?;
    let relative_end = decoded_boundary_to_raw(encoded, decoded_range.end)?;
    let byte_start = source.raw_start.checked_add(relative_start)?;
    let byte_end = source.raw_start.checked_add(relative_end)?;
    let quote = context
        .raw_transcript
        .get(byte_start..byte_end)?
        .to_string();
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
        classifier_hash: crate::hooks::reaction::classifier_hash(),
        ts: source
            .timestamp
            .clone()
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339()),
    })
}

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
    };
    let mut events = Vec::new();
    let mut prior_user = String::new();
    let mut prior_assistant = String::new();
    for source in &sources {
        match source.role {
            Role::User => {
                let text = &source.decoded;
                let gated = !prior_user.is_empty()
                    && !crate::transcript::instrumentation::is_noisy_steer_text(text)
                    && !crate::extraction::provenance::is_csr_emission(text)
                    && crate::extraction::provenance::extractable(text).is_some();
                if gated {
                    let kind = match classify(text, &prior_user) {
                        Some(Reaction::Correction) => Some(IntentEventKind::Correction),
                        Some(Reaction::Redirect) => Some(IntentEventKind::Redirect),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        if let Some(event) =
                            make_event(source, 0..text.len(), kind, &prior_assistant, &context)
                        {
                            events.push(event);
                        }
                    }
                }
                prior_user = text.clone();
            }
            Role::Assistant => {
                let text = &source.decoded;
                if !crate::extraction::provenance::is_csr_emission(text)
                    && crate::extraction::provenance::extractable(text).is_some()
                {
                    for (start, end) in sentence_ranges(text) {
                        let sentence = &text[start..end];
                        let lower = sentence.to_ascii_lowercase();
                        if ABANDONMENT_MARKERS
                            .iter()
                            .any(|marker| lower.contains(marker))
                        {
                            if let Some(event) = make_event(
                                source,
                                start..end,
                                IntentEventKind::Abandoned,
                                &prior_assistant,
                                &context,
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
    Ok(events)
}

/// Deterministically extract receipted corrections, redirects, and explicit
/// abandonment statements from one Claude transcript. The only model work is
/// the existing local MiniLM reaction classifier; no generative model is used.
pub async fn extract_intent_events(
    transcript_path: &Path,
    session_id: &str,
    project: &str,
    embeddings: &Arc<EmbeddingEngine>,
) -> Result<Vec<IntentEvent>> {
    let sources = source_texts(transcript_path)?;
    let mut prior_user = String::new();
    let mut classifiable = Vec::new();
    for source in &sources {
        if source.role != Role::User {
            continue;
        }
        let text = &source.decoded;
        if !prior_user.is_empty()
            && !crate::transcript::instrumentation::is_noisy_steer_text(text)
            && !crate::extraction::provenance::is_csr_emission(text)
            && crate::extraction::provenance::extractable(text).is_some()
        {
            classifiable.push(text.clone());
        }
        prior_user = text.clone();
    }

    if classifiable.is_empty() {
        return extract_with_classifier(transcript_path, session_id, project, |_text, _prior| None);
    }

    let probes = crate::hooks::reaction::ProbeSet::load_or_build(embeddings)
        .await
        .ok_or_else(|| anyhow::anyhow!("reaction exemplar probes could not be loaded or built"))?;
    let engine = embeddings.clone();
    let vectors = tokio::task::spawn_blocking(move || {
        let refs: Vec<&str> = classifiable.iter().map(String::as_str).collect();
        engine.embed(&refs)
    })
    .await??;
    let mut vectors = vectors.into_iter();
    extract_with_classifier(transcript_path, session_id, project, |text, prior_user| {
        let vector = vectors.next()?;
        probes.classify(text, prior_user, &vector, None).reaction
    })
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
}
