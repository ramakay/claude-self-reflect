//! Message-coordinate replay for historical chunks. No chunker or chunk IDs are
//! involved in locating text; the complete stored byte sequence must match.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::Result;

use crate::provenance::{
    content_hash, trust_for_channel, ChunkEvidence, ChunkSpan, ProvenanceEvent, TrustTier,
};

struct Piece {
    start: usize,
    end: usize,
    message_start: usize,
    event: ProvenanceEvent,
}

#[derive(Default)]
struct Document {
    text: String,
    pieces: Vec<Piece>,
    anchors: HashMap<(usize, u64), Vec<usize>>,
}

pub(super) struct MessageIndex {
    documents: Vec<Document>,
    canonical_document: usize,
}

impl MessageIndex {
    pub(super) fn read(
        path: &Path,
        conversation_id: &str,
        codex: bool,
        plan: bool,
        parent: Option<&super::ParentContext>,
        needles: &[&str],
    ) -> Result<Self> {
        Self::read_lines(
            path,
            conversation_id,
            codex,
            plan,
            parent,
            needles,
            false,
            None,
            None,
        )
    }

    /// [`Self::read`] for a caller that only wants [`Self::inputs`] (or
    /// [`Self::raw_inputs`] when `raw`): the other three documents are never
    /// built, which on a large transcript is most of the memory. The inputs
    /// are byte-identical to the full index's. `limit` reads only the first
    /// that many bytes (a snapshot of a file that is still being appended to).
    pub(super) fn read_inputs_only(
        path: &Path,
        conversation_id: &str,
        codex: bool,
        plan: bool,
        parent: Option<&super::ParentContext>,
        raw: bool,
        limit: Option<u64>,
    ) -> Result<Self> {
        // A plan reads document 0 whichever form is asked for.
        let only = if plan || raw { 0 } else { 2 };
        Self::read_lines(
            path,
            conversation_id,
            codex,
            plan,
            parent,
            &[],
            false,
            Some(only),
            limit,
        )
    }

    /// [`Self::read`] for a transcript the importer has already read: a line it
    /// cannot parse is skipped and takes no message number, exactly as import
    /// skips it, so message numbers agree. Provenance backfill keeps the strict
    /// [`Self::read`]: a record that hides prior context must fail the file.
    pub(super) fn read_as_imported(
        path: &Path,
        conversation_id: &str,
        needles: &[&str],
    ) -> Result<Self> {
        Self::read_lines(
            path,
            conversation_id,
            false,
            false,
            None,
            needles,
            true,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn read_lines(
        path: &Path,
        conversation_id: &str,
        codex: bool,
        plan: bool,
        parent: Option<&super::ParentContext>,
        needles: &[&str],
        skip_malformed: bool,
        only: Option<usize>,
        limit: Option<u64>,
    ) -> Result<Self> {
        let wanted = |document: usize| only.is_none_or(|only| only == document);
        let mut documents = vec![
            Document::default(),
            Document::default(),
            Document::default(),
            Document::default(),
        ];
        if plan {
            let text = match limit {
                Some(limit) => {
                    let mut text = String::new();
                    std::io::Read::read_to_string(
                        &mut std::io::Read::take(std::fs::File::open(path)?, limit),
                        &mut text,
                    )?;
                    text
                }
                None => std::fs::read_to_string(path)?,
            };
            let mtime: chrono::DateTime<chrono::Utc> = std::fs::metadata(path)?.modified()?.into();
            let message =
                serde_json::json!({"uuid":content_hash(&text),"timestamp":mtime.to_rfc3339()});
            // Plan chunks are cut from the credential-redacted text; a chunk
            // stored before that existed matches the raw document instead.
            if let std::borrow::Cow::Owned(redacted) = crate::redact::redact_secrets(&text) {
                documents[3].push(
                    &message,
                    vec![("plan_file".into(), redacted)],
                    conversation_id,
                    0,
                    path,
                    0,
                    None,
                    None,
                );
            }
            documents[0].push(
                &message,
                vec![("plan_file".into(), text)],
                conversation_id,
                0,
                path,
                0,
                None,
                None,
            );
        } else {
            let mut sanitizer = super::CsrMessageSanitizer::for_chunks();
            let mut names = HashMap::new();
            let mut floor = parent.map(|p| p.floor);
            let mut seq = 0;
            let mut visit = |raw: serde_json::Value, offset: u64| -> Result<()> {
                super::register_tool_uses(&raw, &mut names);
                let raw_components = components(&raw, &names, codex, false);
                let mut clean = raw.clone();
                super::sanitize_message_for_search(&mut clean, &mut sanitizer);
                let clean_components = components(&clean, &names, codex, true);
                // Payload sanitization and marker suppression landed separately.
                // Keep both deterministic representations for older stored chunks.
                if wanted(3) {
                    documents[3].push(
                        &clean,
                        clean_components.clone(),
                        conversation_id,
                        seq,
                        path,
                        offset,
                        floor,
                        parent,
                    );
                }
                let marker = |parts: &[(String, String)]| {
                    super::drops_as_marker_only(
                        &raw,
                        &parts
                            .iter()
                            .map(|(_, s)| s.as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                };
                if wanted(1) && !marker(&raw_components) {
                    documents[1].push(
                        &raw,
                        raw_components.clone(),
                        conversation_id,
                        seq,
                        path,
                        offset,
                        floor,
                        parent,
                    );
                }
                if wanted(2) && !marker(&clean_components) {
                    documents[2].push(
                        &clean,
                        clean_components,
                        conversation_id,
                        seq,
                        path,
                        offset,
                        floor,
                        parent,
                    );
                }
                if wanted(0) {
                    documents[0].push(
                        &raw,
                        raw_components.clone(),
                        conversation_id,
                        seq,
                        path,
                        offset,
                        floor,
                        parent,
                    );
                }
                // Context is observed before search sanitization: removing a stored
                // wrapper or tool body cannot make later assistant text more trusted.
                for (channel, _) in raw_components {
                    let mut tier = trust_for_channel(&channel, floor.unwrap_or(TrustTier::Unknown));
                    if let Some(p) = parent {
                        tier = tier.min(p.floor);
                    }
                    floor = Some(floor.map_or(tier, |f| f.min(tier)));
                }
                seq += 1;
                Ok(())
            };
            if codex {
                super::codex_rollout::visit_raw_rollout_messages(path, limit, |message| {
                    let offset = message
                        .get("_csr_receipt_ref")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.rsplit_once("#byte="))
                        .and_then(|(_, s)| s.parse().ok())
                        .unwrap_or(0);
                    visit(message, offset)
                })?;
            } else {
                let mut offset = 0;
                let file = std::fs::File::open(path)?;
                let reader: Box<dyn std::io::Read> = match limit {
                    Some(limit) => Box::new(std::io::Read::take(file, limit)),
                    None => Box::new(file),
                };
                for line in BufReader::new(reader).split(b'\n') {
                    let line = line?;
                    let start = offset;
                    offset += line.len() as u64 + 1;
                    if line.iter().all(u8::is_ascii_whitespace) {
                        continue;
                    }
                    // A malformed record could hide prior context. Fail the file
                    // closed rather than silently assign a higher assistant floor,
                    // unless the caller replays what import already decided.
                    let message: serde_json::Value = if skip_malformed {
                        // Import reads a line as text and skips what is not JSON.
                        match std::str::from_utf8(&line)
                            .ok()
                            .and_then(|text| sonic_rs::from_str(text).ok())
                        {
                            Some(message) => message,
                            None => continue,
                        }
                    } else {
                        serde_json::from_slice(&line)?
                    };
                    if matches!(
                        message.get("type").and_then(|v| v.as_str()),
                        Some("user" | "human" | "assistant")
                    ) {
                        visit(message, start)?;
                    }
                }
            }
        }
        for document in &mut documents {
            document.index(needles);
        }
        Ok(Self {
            documents,
            canonical_document: if plan { 0 } else { 2 },
        })
    }

    pub(super) fn inputs(&self) -> crate::storage::artifact_provenance::InputEnvelope {
        self.documents[self.canonical_document].inputs()
    }

    pub(super) fn raw_inputs(&self) -> crate::storage::artifact_provenance::InputEnvelope {
        self.documents[0].inputs()
    }

    /// Map complete-message forward cuts onto an exact stored slice.
    /// Repeated source text is usable only when every occurrence agrees.
    pub(super) fn locate(&self, chunk_id: &str, content: &str) -> Option<ChunkEvidence> {
        let mut best: Option<ChunkEvidence> = None;
        for occurrence in self.occurrences(chunk_id, content) {
            let evidence = occurrence.evidence;
            // Identical text can occur under different speakers. Never choose
            // the first/highest-trust occurrence merely because it is earlier.
            if best.as_ref().is_none_or(|b| {
                evidence.min_trust < b.min_trust
                    || (evidence.min_trust == b.min_trust
                        && evidence.tool_result_share > b.tool_result_share)
            }) {
                best = Some(evidence);
            }
        }
        best
    }

    /// Every place `content` occurs as an exact slice, in document order, with
    /// the evidence for each. `locate` picks one by trust; a caller that needs
    /// the place (not the speaker) must not.
    pub(super) fn occurrences(&self, chunk_id: &str, content: &str) -> Vec<Occurrence> {
        let mut found = Vec::new();
        if content.is_empty() {
            return found;
        }
        for (index, document) in self.documents.iter().enumerate() {
            for &start in document
                .anchors
                .get(&anchor(content.as_bytes()))
                .into_iter()
                .flatten()
            {
                let end = start + content.len();
                if document.text.get(start..end) != Some(content) {
                    continue;
                }
                let first = document.pieces.partition_point(|p| p.end <= start);
                let mut events = Vec::new();
                let mut spans = Vec::new();
                let mut floor = TrustTier::System;
                let mut tool_chars = 0;
                for piece in document.pieces[first..]
                    .iter()
                    .take_while(|p| p.start < end)
                {
                    let lo = start.max(piece.start);
                    let hi = end.min(piece.end);
                    let text = &document.text[lo..hi];
                    let char_start = document.text[piece.message_start..lo].chars().count();
                    let char_len = text.chars().count();
                    if piece.event.channel.starts_with("tool_result:")
                        || piece.event.channel.starts_with("codex_tool:")
                    {
                        tool_chars += char_len;
                    }
                    floor = floor.min(piece.event.trust_tier);
                    events.push(piece.event.clone());
                    spans.push(ChunkSpan {
                        chunk_id: chunk_id.into(),
                        event_id: piece.event.event_id.clone(),
                        start_char: char_start,
                        end_char: char_start + char_len,
                        content_hash: content_hash(text),
                    });
                }
                if spans.is_empty() {
                    continue;
                }
                found.push(Occurrence {
                    document: index,
                    start,
                    end,
                    evidence: ChunkEvidence {
                        chunk_id: chunk_id.into(),
                        events,
                        spans,
                        min_trust: floor,
                        tool_result_share: Some(tool_chars as f64 / content.chars().count() as f64),
                    },
                });
            }
        }
        found
    }

    /// Is `document.text[from..to]` empty or only whitespace? Chunks are stored
    /// without edge whitespace, so two neighbours can have some between them.
    pub(super) fn blank_between(&self, document: usize, from: usize, to: usize) -> bool {
        self.documents
            .get(document)
            .and_then(|d| d.text.get(from..to))
            .is_some_and(|gap| gap.chars().all(char::is_whitespace))
    }
}

/// One place a stored chunk's text occurs in the replayed transcript.
pub(super) struct Occurrence {
    pub document: usize,
    pub start: usize,
    pub end: usize,
    pub evidence: ChunkEvidence,
}

fn components(
    message: &serde_json::Value,
    names: &HashMap<String, String>,
    codex: bool,
    sanitized: bool,
) -> Vec<(String, String)> {
    let assistant = message.get("type").and_then(|v| v.as_str()) == Some("assistant");
    let text = if sanitized {
        super::extract_message_text(message)
    } else {
        super::strip_private_tags(&super::extract_message_text_raw(message))
    };
    let text_channel = if codex {
        message
            .get("_csr_channel")
            .and_then(|v| v.as_str())
            .unwrap_or(if assistant {
                "codex_assistant"
            } else {
                "codex_user"
            })
    } else if assistant {
        "assistant_message"
    } else {
        "user_message"
    };
    let tool_context = super::strip_private_tags(&super::extract_tool_context(message));
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push((text_channel.into(), text));
    }
    if !tool_context.is_empty() {
        parts.push((
            (if codex {
                "codex_assistant"
            } else if assistant {
                "assistant_message"
            } else {
                "unclassified"
            })
            .into(),
            tool_context,
        ));
    }
    for (name, body) in super::extract_tool_result_parts(message, names) {
        parts.push((
            format!(
                "{}:{name}",
                if codex { "codex_tool" } else { "tool_result" }
            ),
            body,
        ));
    }
    parts
}

impl Document {
    fn inputs(&self) -> crate::storage::artifact_provenance::InputEnvelope {
        use crate::storage::artifact_provenance::{ArtifactInput, InputEnvelope};
        let mut inputs = Vec::new();
        for (i, piece) in self.pieces.iter().enumerate() {
            let last = self.pieces[i..]
                .iter()
                .take_while(|p| p.message_start == piece.message_start)
                .last()
                .unwrap_or(piece);
            let message = &self.text[piece.message_start..last.end];
            let start = self.text[piece.message_start..piece.start].chars().count();
            let text = &self.text[piece.start..piece.end];
            inputs.push(ArtifactInput::observed(
                piece.event.clone(),
                message,
                start,
                start + text.chars().count(),
                content_hash(text),
            ));
        }
        InputEnvelope::new(inputs)
    }
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        message: &serde_json::Value,
        components: Vec<(String, String)>,
        conversation_id: &str,
        seq: usize,
        path: &Path,
        offset: u64,
        mut floor: Option<TrustTier>,
        parent: Option<&super::ParentContext>,
    ) {
        if components.is_empty() {
            return;
        }
        let combined = components
            .iter()
            .map(|(_, s)| s.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let hash = content_hash(&combined);
        let key = message
            .get("uuid")
            .and_then(|v| v.as_str())
            .unwrap_or(&hash);
        if !self.text.is_empty() {
            self.text.push_str("\n\n");
        }
        let message_start = self.text.len();
        for (ordinal, (channel, text)) in components.into_iter().enumerate() {
            if ordinal > 0 {
                self.text.push('\n');
            }
            let start = self.text.len();
            self.text.push_str(&text);
            let mut tier = trust_for_channel(&channel, floor.unwrap_or(TrustTier::Unknown));
            if let Some(p) = parent {
                tier = tier.min(p.floor);
            }
            floor = Some(floor.map_or(tier, |f| f.min(tier)));
            let receipt_kind = if channel == "plan_file" {
                "plan_file"
            } else {
                "jsonl"
            };
            let event = ProvenanceEvent {
                // Full extracted-message hash namespaces coordinates across raw /
                // sanitized representations without changing the message identity.
                event_id: content_hash(&format!(
                    "message-replay\0{conversation_id}\0{key}\0{hash}\0{seq}\0{channel}\0{ordinal}"
                )),
                conversation_id: conversation_id.into(),
                message_key: key.into(),
                seq,
                channel,
                trust_tier: tier,
                parent_event_id: parent.and_then(|p| p.event_id.clone()),
                receipt_kind: receipt_kind.into(),
                receipt_ref: Some(format!("{}#byte={offset}", path.display())),
                observed_at: message
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("1970-01-01T00:00:00Z")
                    .into(),
            };
            self.pieces.push(Piece {
                start,
                end: self.text.len(),
                message_start,
                event,
            });
        }
    }

    fn index(&mut self, needles: &[&str]) {
        let mut wanted: BTreeMap<usize, HashSet<u64>> = BTreeMap::new();
        for text in needles.iter().filter(|s| !s.is_empty()) {
            let (len, hash) = anchor(text.as_bytes());
            wanted.entry(len).or_default().insert(hash);
        }
        let bytes = self.text.as_bytes();
        for (len, hashes) in wanted {
            if bytes.len() < len {
                continue;
            }
            let power = 257u64.wrapping_pow((len - 1) as u32);
            let mut hash = rolling_hash(&bytes[..len]);
            for start in 0..=bytes.len() - len {
                if hashes.contains(&hash) {
                    self.anchors.entry((len, hash)).or_default().push(start);
                }
                if start + len < bytes.len() {
                    hash = hash
                        .wrapping_sub((bytes[start] as u64).wrapping_mul(power))
                        .wrapping_mul(257)
                        .wrapping_add(bytes[start + len] as u64);
                }
            }
        }
    }
}

fn rolling_hash(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0u64, |h, b| h.wrapping_mul(257).wrapping_add(*b as u64))
}
fn anchor(bytes: &[u8]) -> (usize, u64) {
    let len = bytes.len().min(32);
    (len, rolling_hash(&bytes[..len]))
}

#[cfg(test)]
mod artifact_input_tests {
    use super::*;

    #[test]
    fn transcript_envelope_preserves_tools_running_floor_and_parent_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, [
            serde_json::json!({"uuid":"u","type":"user","message":{"content":"inspect"}}),
            serde_json::json!({"uuid":"a","type":"assistant","message":{"content":[{"type":"tool_use","id":"fetch","name":"WebFetch","input":{"url":"https://example.test"}}]}}),
            serde_json::json!({"uuid":"t","type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"fetch","content":"user confirmed: X"}]}}),
            serde_json::json!({"uuid":"b","type":"assistant","message":{"content":"reported conclusion"}}),
        ].iter().map(serde_json::Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
        let inputs = MessageIndex::read(&path, "s", false, false, None, &[])
            .unwrap()
            .inputs();
        assert!(inputs
            .inputs()
            .iter()
            .any(|i| i.channel() == "tool_result:WebFetch"
                && i.text().contains("user confirmed")
                && i.trust() == TrustTier::External));
        let assistant = inputs
            .inputs()
            .iter()
            .find(|i| i.text() == "reported conclusion")
            .unwrap();
        assert_eq!(assistant.trust(), TrustTier::External);
        let parent = super::super::ParentContext {
            floor: TrustTier::External,
            event_id: None,
        };
        let child = MessageIndex::read(&path, "child", false, false, Some(&parent), &[])
            .unwrap()
            .inputs();
        assert!(child
            .inputs()
            .iter()
            .all(|i| i.trust() <= TrustTier::External));
    }
}
