use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use crate::engine::Engine;
use crate::import::{
    classify_transcript, derive_conversation_attribution, parse_jsonl_file_with_stats_and_parent,
    scrub_contaminated_text, ConversationChunk, ParentContext, TranscriptKind,
};
use crate::provenance::{ChunkEvidence, ChunkProvenance, TrustTier};
use crate::storage::{queries, Storage};

/// A conversation whose source file or newest chunk changed this recently may
/// still have a live session writing it. Hooks from an older binary write a
/// chunk's text and its vector in two statements that no transaction of ours
/// can fence, so the scrub leaves such a conversation for a later run
/// (`--include-active` overrides).
pub const ACTIVE_WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScrubReport {
    pub conversations_scrubbed: usize,
    pub chunks_rewritten: usize,
    pub chunks_dropped: usize,
    pub vectors_replaced: usize,
    /// Conversations whose re-parse plan would have wiped most of the stored
    /// corpus. Nothing is written for them.
    pub refused: usize,
    /// Conversations whose missing source or spans leave a forward cut ambiguous.
    pub unresolved_ambiguous_source: usize,
    /// Conversations left alone because they were active inside
    /// [`ACTIVE_WINDOW`]. Nothing is read or written for them.
    pub deferred_active: usize,
    /// Conversations whose stored rows changed between planning and applying.
    /// Their plan is stale; nothing is written for them.
    pub skipped_changed: usize,
    /// The longest any one plan held the database write lock, in milliseconds.
    pub longest_transaction_ms: u64,
    /// The preflight's findings: other processes holding the database open.
    pub preflight: Vec<String>,
    /// Conversations removed whole because their transcript is a CSR agent prompt.
    pub agent_transcripts_purged: usize,
    pub agent_chunks_purged: usize,
    pub agent_reflections_purged: usize,
    /// `(chunks, reflections)` in the compact index persisted after a real run.
    pub index_rebuilt: Option<(usize, usize)>,
    pub actions: Vec<String>,
}

impl ScrubReport {
    pub fn format_text(&self, dry_run: bool) -> String {
        let mut output = String::new();
        for line in &self.preflight {
            output.push_str(line);
            output.push('\n');
        }
        for action in &self.actions {
            output.push_str(action);
            output.push('\n');
        }
        output.push_str(&format!(
            "{} summary\nconversations scrubbed: {}\nchunks rewritten: {}\nchunks dropped: {}\nvectors replaced: {}\nrefused: {}\nunresolved (ambiguous source): {}\ndeferred (active): {}\nskipped (changed during scrub): {}\n",
            if dry_run { "scrub dry-run" } else { "scrub" },
            self.conversations_scrubbed,
            self.chunks_rewritten,
            self.chunks_dropped,
            self.vectors_replaced,
            self.refused,
            self.unresolved_ambiguous_source,
            self.deferred_active,
            self.skipped_changed
        ));
        output.push_str(&format!(
            "agent transcripts purged: {} ({} chunks, {} reflections)\n",
            self.agent_transcripts_purged, self.agent_chunks_purged, self.agent_reflections_purged
        ));
        if !dry_run {
            output.push_str(&format!(
                "longest plan transaction: {} ms\n",
                self.longest_transaction_ms
            ));
        }
        if let Some((chunks, reflections)) = self.index_rebuilt {
            output.push_str(&format!(
                "index rebuilt: {chunks} chunks, {reflections} reflections\n"
            ));
        }
        output
    }
}

#[derive(Debug)]
enum ScrubPlanKind {
    Stable {
        source_path: PathBuf,
        chunks: Vec<ConversationChunk>,
        changed_ids: HashSet<String>,
    },
    Shifted {
        source_path: PathBuf,
        chunks: Vec<ConversationChunk>,
    },
    /// Scrubbed chunk by chunk, without a re-parse: why is in `reason`.
    Missing {
        rewritten: Vec<ConversationChunk>,
        dropped_ids: Vec<String>,
        reason: String,
        unresolved: bool,
    },
    /// A re-parse that would rebuild nothing, or drop most of what is stored,
    /// is a parser mismatch rather than a scrub. No writes.
    Refused { reason: String },
}

/// Chunk sources the Claude transcript parser produced. Anything else
/// (`codex_rollout`, `plan`, future adapters) has its own on-disk format and
/// must never be re-parsed as a Claude transcript.
fn is_claude_transcript_source(source: &str) -> bool {
    matches!(source, "conversation" | "sidechain")
}

fn is_claude_transcript_conversation(
    conversation_id: &str,
    sources: &HashMap<String, String>,
) -> bool {
    !conversation_id.starts_with("codex:")
        && !conversation_id.starts_with("plan:")
        && sources
            .values()
            .all(|source| is_claude_transcript_source(source))
}

/// `Some(reason)` when a re-parse plan must not be applied: it rebuilds zero
/// chunks, or drops more than half of the conversation's stored chunks.
fn reparse_refusal(
    old_chunks: &[ConversationChunk],
    new_chunks: &[ConversationChunk],
    source_path: &std::path::Path,
) -> Option<String> {
    if old_chunks.is_empty() {
        return None;
    }
    let new_ids = new_chunks
        .iter()
        .map(|chunk| chunk.id.as_str())
        .collect::<HashSet<_>>();
    let dropped = old_chunks
        .iter()
        .filter(|chunk| !new_ids.contains(chunk.id.as_str()))
        .count();
    if new_chunks.is_empty() || dropped * 2 > old_chunks.len() {
        Some(format!(
            "refused, re-parse would rebuild {} chunks and drop {} of {} stored from {}; no writes",
            new_chunks.len(),
            dropped,
            old_chunks.len(),
            source_path.display()
        ))
    } else {
        None
    }
}

/// True when a stored chunk holds words of the person's (what is left after the
/// scrub's own cut) that the re-parse no longer yields anywhere. A transcript
/// that lost part of itself is no evidence that the part was contamination
/// (astra #3). Compared without whitespace, across the re-parse's chunks joined,
/// so chunk boundaries and re-wrapping never count as loss.
fn reparse_lacks_stored_text(
    old_chunks: &[ConversationChunk],
    new_chunks: &[ConversationChunk],
    continues: &[bool],
    source_cuts: &HashMap<String, String>,
) -> bool {
    let squash = |text: &str| {
        text.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    let rebuilt = squash(
        &new_chunks
            .iter()
            .map(|chunk| chunk.content.as_str())
            .collect::<String>(),
    );
    let (carried, ambiguous) = cuts_with_source(old_chunks, continues, source_cuts);
    carried.iter().enumerate().any(|(index, content)| {
        let own = if ambiguous[index] {
            Some(content.clone())
        } else {
            scrub_contaminated_text(content)
        };
        own.is_some_and(|own| {
            own.lines().any(|line| {
                let line = squash(line);
                !line.is_empty() && !rebuilt.contains(&line)
            })
        })
    })
}

/// A source file as the scrub read it: what `import_state` records for it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceRevision {
    mtime: String,
    size: u64,
}

fn source_revision(path: &Path) -> Option<SourceRevision> {
    let metadata = path.metadata().ok()?;
    Some(SourceRevision {
        mtime: queries::file_mtime_str(path),
        size: metadata.len(),
    })
}

#[derive(Debug)]
struct ScrubPlan {
    conversation_id: String,
    old_chunks: Vec<ConversationChunk>,
    sources: HashMap<String, String>,
    provenance: HashMap<String, ChunkProvenance>,
    structural_evidence: HashMap<String, ChunkEvidence>,
    /// The source and the revision the re-parse read, recorded as imported when
    /// the plan is applied. `None` when the file changed while it was parsed
    /// (astra #2): what was appended is left for the watcher to import.
    import_mark: Option<(PathBuf, String)>,
    kind: ScrubPlanKind,
}

/// Every transcript `import_state` records for the conversation.
fn source_paths(storage: &Storage, conversation_id: &str) -> Result<Vec<PathBuf>> {
    storage.with_connection(|conn| {
        let mut statement = conn.prepare(
            "SELECT file_path FROM import_state WHERE conversation_id = ?1 ORDER BY file_path",
        )?;
        let paths = statement
            .query_map([conversation_id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(paths.into_iter().map(PathBuf::from).collect())
    })
}

/// True when a live session may still be writing the conversation: a source
/// file, or its newest chunk, changed inside [`ACTIVE_WINDOW`] before `now`.
fn is_active(storage: &Storage, conversation_id: &str, now: SystemTime) -> Result<bool> {
    storage.with_connection(|conn| is_active_in(conn, conversation_id, now))
}

/// [`is_active`] on a connection the caller holds: the apply transaction asks
/// again right before it writes (astra r2 #3).
fn is_active_in(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    now: SystemTime,
) -> Result<bool> {
    let horizon = now
        .checked_sub(ACTIVE_WINDOW)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let paths = conn
        .prepare_cached("SELECT file_path FROM import_state WHERE conversation_id = ?1")?
        .query_map([conversation_id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for path in paths {
        if Path::new(&path)
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified > horizon)
        {
            return Ok(true);
        }
    }
    let newest: Option<String> = conn.query_row(
        "SELECT MAX(timestamp) FROM chunks WHERE conversation_id = ?1",
        [conversation_id],
        |row| row.get(0),
    )?;
    Ok(newest
        .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(&stamp).ok())
        .is_some_and(|stamp| SystemTime::from(stamp) > horizon))
}

/// For each chunk, whether it continues the message the chunk before it ended
/// in: the last span of the one and the first span of the other name the same
/// provenance event (the importer cut one long message at its byte budget).
/// False wherever the spans cannot say.
fn message_continuity(storage: &Storage, chunks: &[ConversationChunk]) -> Result<Vec<bool>> {
    let ids = serde_json::to_string(&chunks.iter().map(|chunk| &chunk.id).collect::<Vec<_>>())?;
    let spans: Vec<(String, String, i64, i64, String)> = storage.with_connection(|conn| {
        let mut statement = conn.prepare(
            "SELECT s.chunk_id, s.event_id, s.start_char, s.end_char, s.content_hash
             FROM chunk_spans s JOIN provenance_events e ON e.event_id = s.event_id
             JOIN chunks c ON c.id = s.chunk_id
             WHERE s.chunk_id IN (SELECT value FROM json_each(?1))
             AND e.conversation_id = c.conversation_id",
        )?;
        let rows = statement
            .query_map([&ids], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    let mut by_chunk: HashMap<&str, Vec<_>> = HashMap::new();
    for (chunk, event, start, end, hash) in &spans {
        by_chunk
            .entry(chunk)
            .or_default()
            .push((event, *start, *end, hash));
    }
    // The importer slices long messages into their own chunks. Multiple events
    // or a changed hash cannot establish a complete slice of one message.
    let proven = chunks
        .iter()
        .map(|chunk| {
            let spans = by_chunk.get(chunk.id.as_str())?;
            let [(event, start, end, hash)] = spans.as_slice() else {
                return None;
            };
            (*start >= 0
                && *end - *start == chunk.content.chars().count() as i64
                && **hash == crate::provenance::content_hash(&chunk.content))
            .then_some((event.as_str(), *start, *end))
        })
        .collect::<Vec<_>>();
    Ok(proven
        .iter()
        .enumerate()
        .map(|(index, span)| {
            index > 0
                && matches!((proven[index - 1], *span),
            (Some((before, _, end)), Some((after, start, _))) if before == after && end == start)
        })
        .collect())
}

/// Compute every forward cut in message context and map it to chunk bytes.
/// Unproven boundaries never join messages for a destructive cut.
fn cut_forwards_across_chunks(
    chunks: &[ConversationChunk],
    continues: &[bool],
) -> (Vec<String>, Vec<bool>) {
    let mut out = chunks
        .iter()
        .map(|chunk| chunk.content.clone())
        .collect::<Vec<_>>();
    let mut ambiguous = vec![false; chunks.len()];
    let mut start = 0;
    while start < chunks.len() {
        let mut end = start + 1;
        while end < chunks.len() && continues[end] {
            end += 1;
        }
        let mut offsets = Vec::with_capacity(end - start + 1);
        let mut joined = String::new();
        for chunk in &chunks[start..end] {
            offsets.push(joined.len());
            joined.push_str(&chunk.content);
        }
        offsets.push(joined.len());
        let mut cuts: Vec<Vec<(usize, usize)>> = vec![Vec::new(); end - start];
        for span in crate::extraction::provenance::forward_spans(&joined) {
            let first = offsets.partition_point(|&offset| offset <= span.start) - 1;
            let last = offsets.partition_point(|&offset| offset < span.end) - 1;
            if span.needs_message_end && end - start == 1 && end < chunks.len() {
                ambiguous[start + first..=start + last].fill(true);
                continue;
            }
            for (chunk, cut) in cuts.iter_mut().enumerate().take(last + 1).skip(first) {
                let lo = span.start.max(offsets[chunk]) - offsets[chunk];
                let hi = span.end.min(offsets[chunk + 1]) - offsets[chunk];
                if lo < hi {
                    cut.push((lo, hi));
                }
            }
        }
        for (chunk, mut cut) in cuts.into_iter().enumerate() {
            cut.sort_unstable();
            for (lo, hi) in cut.into_iter().rev() {
                out[start + chunk].replace_range(lo..hi, "");
            }
        }
        start = end;
    }
    // A split header can select a conversation without proving a message.
    // Detect that uncertainty for reporting only; never use this join to cut.
    let mut joined = String::new();
    let mut offsets = Vec::new();
    for chunk in chunks {
        offsets.push(joined.len());
        joined.push_str(&chunk.content);
    }
    offsets.push(joined.len());
    for span in crate::extraction::provenance::forward_spans(&joined) {
        let first = offsets.partition_point(|&offset| offset <= span.start) - 1;
        let last = offsets.partition_point(|&offset| offset < span.end) - 1;
        if (first + 1..=last).any(|index| !continues[index]) {
            ambiguous[first..=last].fill(true);
        }
    }
    for (index, uncertain) in ambiguous.iter().enumerate() {
        if *uncertain {
            out[index] = chunks[index].content.clone();
        }
    }
    (out, ambiguous)
}

/// Source adapters can prove cuts even when historical chunks lack spans.
fn source_forward_cuts(
    path: &Path,
    conversation_id: &str,
    chunks: &[ConversationChunk],
    sources: &HashMap<String, String>,
) -> HashMap<String, String> {
    let codex = conversation_id.starts_with("codex:")
        || sources.values().any(|source| source == "codex_rollout");
    let plan =
        conversation_id.starts_with("plan:") || sources.values().any(|source| source == "plan");
    if !codex && !plan && !is_claude_transcript_conversation(conversation_id, sources) {
        return HashMap::new();
    }
    let needles = chunks
        .iter()
        .map(|chunk| chunk.content.as_str())
        .collect::<Vec<_>>();
    let Ok(index) = super::provenance_matcher::MessageIndex::read(
        path,
        conversation_id,
        codex,
        plan,
        None,
        &needles,
    ) else {
        return HashMap::new();
    };
    chunks
        .iter()
        .filter_map(|chunk| {
            index
                .without_forwards(&chunk.content)
                .map(|own| (chunk.id.clone(), own))
        })
        .collect()
}

fn cuts_with_source(
    chunks: &[ConversationChunk],
    continues: &[bool],
    source_cuts: &HashMap<String, String>,
) -> (Vec<String>, Vec<bool>) {
    let (mut own, mut ambiguous) = cut_forwards_across_chunks(chunks, continues);
    for (index, chunk) in chunks.iter().enumerate() {
        if let Some(proven) = source_cuts.get(&chunk.id) {
            own[index] = proven.clone();
            ambiguous[index] = false;
        }
    }
    (own, ambiguous)
}

fn in_place(
    old_chunks: &[ConversationChunk],
    continues: &[bool],
    source_cuts: &HashMap<String, String>,
    reason: String,
) -> ScrubPlanKind {
    let (carried, ambiguous) = cuts_with_source(old_chunks, continues, source_cuts);
    let mut rewritten = Vec::new();
    let mut dropped_ids = Vec::new();
    for (index, (chunk, carried)) in old_chunks.iter().zip(&carried).enumerate() {
        if ambiguous[index] {
            continue;
        }
        match scrub_contaminated_text(carried) {
            Some(content) if content != chunk.content => {
                let mut clean = chunk.clone();
                clean.content = content;
                rewritten.push(clean);
            }
            Some(_) => {}
            None => dropped_ids.push(chunk.id.clone()),
        }
    }
    ScrubPlanKind::Missing {
        rewritten,
        dropped_ids,
        reason,
        unresolved: ambiguous.into_iter().any(|value| value),
    }
}

fn load_plan(storage: &Storage, conversation_id: &str) -> Result<ScrubPlan> {
    let old_ids = storage.get_chunk_ids_for_conversation(conversation_id)?;
    let old_chunks = storage.get_chunks_by_ids_with_provenance(&old_ids)?;
    let sources = old_ids
        .iter()
        .map(|id| {
            Ok((
                id.clone(),
                storage
                    .get_chunk_source(id)?
                    .unwrap_or_else(|| "conversation".to_string()),
            ))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    let provenance = old_ids
        .iter()
        .filter_map(|id| {
            storage
                .get_chunk_provenance(id)
                .transpose()
                .map(|result| result.map(|value| (id.clone(), value)))
        })
        .collect::<Result<HashMap<_, _>>>()?;

    let paths = source_paths(storage, conversation_id)?;
    let continues = message_continuity(storage, &old_chunks)?;
    let mut structural_evidence = HashMap::new();
    let mut import_mark = None;
    let claude = is_claude_transcript_conversation(conversation_id, &sources);
    let source_before = match paths.as_slice() {
        [path] => source_revision(path),
        _ => None,
    };
    let mut source_cuts = match paths.as_slice() {
        [path] if source_before.is_some() => {
            source_forward_cuts(path, conversation_id, &old_chunks, &sources)
        }
        _ => HashMap::new(),
    };
    if paths.first().and_then(|path| source_revision(path)) != source_before {
        source_cuts.clear();
    }
    let kind = match paths.as_slice() {
        // More than one transcript recorded for one id: the one picked may be a
        // partial copy, so no re-parse is trusted to say what the rest held.
        [_, _, ..] if claude => in_place(
            &old_chunks,
            &continues,
            &source_cuts,
            format!("{} source paths registered", paths.len()),
        ),
        [path] if claude && path.exists() => {
            let path = path.clone();
            let project = old_chunks
                .first()
                .map(|chunk| chunk.project_name.as_str())
                .unwrap_or_default();
            let parent =
                old_chunks
                    .iter()
                    .any(|chunk| chunk.is_sidechain)
                    .then_some(ParentContext {
                        floor: TrustTier::Unknown,
                        event_id: None,
                    });
            // The revision is read before the parse and checked after it: a file
            // that grew meanwhile is never recorded as read in full.
            let before = source_before;
            let parsed = parse_jsonl_file_with_stats_and_parent(&path, project, parent.as_ref())
                .with_context(|| format!("re-parsing {}", path.display()))?;
            let after = source_revision(&path);
            if before != after {
                source_cuts.clear();
            }
            let chunks = parsed.chunks;
            let new_ids = chunks
                .iter()
                .map(|chunk| chunk.id.clone())
                .collect::<Vec<_>>();
            if let Some(reason) = reparse_refusal(&old_chunks, &chunks, &path) {
                ScrubPlanKind::Refused { reason }
            } else if reparse_lacks_stored_text(&old_chunks, &chunks, &continues, &source_cuts) {
                in_place(
                    &old_chunks,
                    &continues,
                    &source_cuts,
                    "re-parse lacks stored text".to_string(),
                )
            } else {
                structural_evidence = parsed.evidence;
                import_mark = before
                    .filter(|read| after.as_ref() == Some(read))
                    .map(|read| (path.clone(), read.mtime));
                if old_ids == new_ids {
                    let old_content = old_chunks
                        .iter()
                        .map(|chunk| (chunk.id.as_str(), chunk.content.as_str()))
                        .collect::<HashMap<_, _>>();
                    let changed_ids = chunks
                        .iter()
                        .filter(|chunk| {
                            old_content.get(chunk.id.as_str()) != Some(&chunk.content.as_str())
                        })
                        .map(|chunk| chunk.id.clone())
                        .collect();
                    ScrubPlanKind::Stable {
                        source_path: path,
                        chunks,
                        changed_ids,
                    }
                } else {
                    ScrubPlanKind::Shifted {
                        source_path: path,
                        chunks,
                    }
                }
            }
        }
        _ => in_place(
            &old_chunks,
            &continues,
            &source_cuts,
            "source missing or not a Claude transcript".to_string(),
        ),
    };

    Ok(ScrubPlan {
        conversation_id: conversation_id.to_string(),
        old_chunks,
        sources,
        provenance,
        structural_evidence,
        import_mark,
        kind,
    })
}

fn provenance_for(
    plan: &ScrubPlan,
    chunk: &ConversationChunk,
    default_source_conv_id: &str,
) -> ChunkProvenance {
    plan.provenance
        .get(&chunk.id)
        .cloned()
        .unwrap_or_else(|| ChunkProvenance {
            author: chunk.author,
            source_conv_id: default_source_conv_id.to_string(),
            supersedes: None,
        })
}

fn source_of<'a>(plan: &'a ScrubPlan, id: &str) -> &'a str {
    plan.sources
        .get(id)
        .map(String::as_str)
        .unwrap_or("conversation")
}

fn summarize_plan(plan: &ScrubPlan, report: &mut ScrubReport) {
    if let ScrubPlanKind::Refused { reason } = &plan.kind {
        report.refused += 1;
        report
            .actions
            .push(format!("{}: {reason}", plan.conversation_id));
        return;
    }
    if matches!(
        plan.kind,
        ScrubPlanKind::Missing {
            unresolved: true,
            ..
        }
    ) {
        report.actions.push(format!(
            "{}: unresolved (ambiguous source); uncertain text preserved",
            plan.conversation_id
        ));
    } else {
        report.conversations_scrubbed += 1;
    }
    match &plan.kind {
        ScrubPlanKind::Refused { .. } => unreachable!("returned above"),
        ScrubPlanKind::Stable {
            source_path,
            changed_ids,
            ..
        } => {
            report.chunks_rewritten += changed_ids.len();
            report.vectors_replaced += changed_ids.len();
            report.actions.push(format!(
                "{}: stable ids, rewrite {} chunks and replace {} vectors from {}",
                plan.conversation_id,
                changed_ids.len(),
                changed_ids.len(),
                source_path.display()
            ));
        }
        ScrubPlanKind::Shifted {
            source_path,
            chunks,
        } => {
            let new_ids = chunks
                .iter()
                .map(|chunk| chunk.id.as_str())
                .collect::<HashSet<_>>();
            let dropped = plan
                .old_chunks
                .iter()
                .filter(|chunk| !new_ids.contains(chunk.id.as_str()))
                .count();
            report.chunks_rewritten += chunks.len();
            report.chunks_dropped += dropped;
            report.vectors_replaced += chunks.len();
            report.actions.push(format!(
                "{}: shifted ids, rebuild {} chunks, drop {}, and replace {} vectors from {}",
                plan.conversation_id,
                chunks.len(),
                dropped,
                chunks.len(),
                source_path.display()
            ));
        }
        ScrubPlanKind::Missing {
            rewritten,
            dropped_ids,
            reason,
            unresolved,
        } => {
            report.unresolved_ambiguous_source += usize::from(*unresolved);
            report.chunks_rewritten += rewritten.len();
            report.chunks_dropped += dropped_ids.len();
            report.vectors_replaced += rewritten.len();
            report.actions.push(format!(
                "{}: no re-parse ({reason}), rewrite {} chunks, drop {}, and replace {} vectors in place",
                plan.conversation_id,
                rewritten.len(),
                dropped_ids.len(),
                rewritten.len()
            ));
        }
    }
}

/// A conversation the importer now skips whole: every registered source transcript
/// opens with a CSR agent prompt, so nothing derived from it is corpus. The
/// skip ran after the sanitizer between 2026-09-01 and 2026-09-19 and never
/// matched, which is how these got in.
#[derive(Debug)]
struct AgentTranscript {
    conversation_id: String,
    source_paths: Vec<PathBuf>,
    chunks: usize,
    /// Enrichment and ratification rows: residue worth a purge on its own.
    state_rows: usize,
    reflection_ids: Vec<String>,
}

fn agent_transcripts(
    storage: &Storage,
    conversation: Option<&str>,
) -> Result<Vec<AgentTranscript>> {
    let sources = storage.with_connection(|conn| {
        let mut statement = conn.prepare(
            "SELECT s.conversation_id, s.file_path,
                    (SELECT COUNT(*) FROM chunks c WHERE c.conversation_id = s.conversation_id),
                    (SELECT COUNT(*) FROM enrichment_state e WHERE e.conversation_id = s.conversation_id)
                  + (SELECT COUNT(*) FROM ratification_scores r WHERE r.conversation_id = s.conversation_id)
             FROM import_state s
             WHERE s.conversation_id IS NOT NULL AND s.file_path LIKE '%.jsonl'
             ORDER BY s.conversation_id, s.file_path",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;

    // Classified over the whole corpus, before any `--conversation` filter, so a
    // reflection's fate never depends on which conversation a run was aimed at.
    // Every registered source must be on disk, readable, and an agent prompt: a
    // source we cannot read may have contributed real chunks under the same id.
    let mut agents: Vec<AgentTranscript> = Vec::new();
    let mut disqualified: HashSet<String> = HashSet::new();
    for (conversation_id, file_path, chunks, state_rows) in sources {
        let path = PathBuf::from(file_path);
        if classify_transcript(&path) != TranscriptKind::Agent {
            disqualified.insert(conversation_id);
            continue;
        }
        match agents.last_mut() {
            Some(last) if last.conversation_id == conversation_id => last.source_paths.push(path),
            _ => agents.push(AgentTranscript {
                conversation_id,
                source_paths: vec![path],
                chunks: chunks as usize,
                state_rows: state_rows as usize,
                reflection_ids: Vec::new(),
            }),
        }
    }
    agents.retain(|agent| !disqualified.contains(&agent.conversation_id));
    let agent_ids = agents
        .iter()
        .map(|agent| agent.conversation_id.clone())
        .collect::<HashSet<_>>();

    // A reflection goes when every conversation it is tagged with is an agent
    // transcript and no conversation outside THIS run's deletion set cites it.
    // An agent transcript a `--conversation` run is not aimed at still protects
    // what it cites; the reflection goes with whichever run removes the last one.
    let selected =
        |id: &str| agent_ids.contains(id) && conversation.is_none_or(|wanted| wanted == id);
    let cited_by_kept = storage
        .enrichment_reflection_refs()?
        .into_iter()
        .filter(|(conversation_id, _)| !selected(conversation_id))
        .map(|(_, reflection_id)| reflection_id)
        .collect::<HashSet<_>>();
    let mut owned: HashMap<String, Vec<String>> = HashMap::new();
    for (reflection_id, conversations) in storage.reflection_conversation_tags()? {
        if cited_by_kept.contains(&reflection_id)
            || !conversations.iter().all(|id| agent_ids.contains(id))
        {
            continue;
        }
        if let Some(owner) = conversations.iter().find(|id| selected(id)) {
            owned.entry(owner.clone()).or_default().push(reflection_id);
        }
    }

    agents.retain(|agent| selected(&agent.conversation_id));
    for agent in &mut agents {
        agent.reflection_ids = owned.remove(&agent.conversation_id).unwrap_or_default();
    }
    agents.retain(|agent| {
        agent.chunks > 0 || agent.state_rows > 0 || !agent.reflection_ids.is_empty()
    });
    Ok(agents)
}

fn summarize_agent_transcript(agent: &AgentTranscript, report: &mut ScrubReport) {
    report.agent_transcripts_purged += 1;
    report.agent_chunks_purged += agent.chunks;
    report.agent_reflections_purged += agent.reflection_ids.len();
    report.actions.push(format!(
        "{}: agent transcript, purge {} chunks and {} reflections",
        agent.conversation_id,
        agent.chunks,
        agent.reflection_ids.len()
    ));
}

/// Reflections deleted per transaction during an agent-transcript purge.
const PURGE_REFLECTION_BATCH: usize = 500;

/// A purge shares the database with hooks firing in live sessions. It waits
/// for them rather than dying part-way with the index manifest already gone.
const MAINTENANCE_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const BUSY_RETRIES: usize = 10;

/// Shared by every writer that has to survive another csr-engine process
/// holding the SQLite write lock (scrub, Codex rollout import).
pub(crate) fn is_database_busy(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(failure, _))
                if matches!(
                    failure.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                )
        )
    })
}

async fn retry_busy<T>(mut operation: impl FnMut() -> Result<T>) -> Result<T> {
    let mut attempt = 0;
    loop {
        match operation() {
            Err(error) if attempt < BUSY_RETRIES && is_database_busy(&error) => {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            result => return result,
        }
    }
}

/// Plans for every contaminated conversation `conversation` selects, minus the
/// ones in `skip` (purged as agent transcripts) and, unless `include_active`,
/// the ones a live session may still be writing ([`is_active`]).
fn selected_plans(
    storage: &Storage,
    conversation: Option<&str>,
    include_active: bool,
    skip: &HashSet<String>,
    report: &mut ScrubReport,
) -> Result<Vec<ScrubPlan>> {
    let now = SystemTime::now();
    let mut plans = Vec::new();
    for (id, _) in storage.contaminated_conversations()? {
        if conversation.is_some_and(|wanted| wanted != id) || skip.contains(&id) {
            continue;
        }
        if !include_active && is_active(storage, &id, now)? {
            report.deferred_active += 1;
            report.actions.push(format!("active, deferred: {id}"));
            continue;
        }
        plans.push(load_plan(storage, &id)?);
    }
    Ok(plans)
}

pub fn dry_run_scrub(
    storage: &Storage,
    conversation: Option<&str>,
    agent_transcripts_only: bool,
    include_active: bool,
) -> Result<ScrubReport> {
    let mut report = ScrubReport::default();
    let agents = agent_transcripts(storage, conversation)?;
    for agent in &agents {
        summarize_agent_transcript(agent, &mut report);
    }
    if agent_transcripts_only {
        return Ok(report);
    }
    // A real run purges first, so the emission scrub never sees these.
    let purged = agents
        .iter()
        .map(|agent| agent.conversation_id.clone())
        .collect::<HashSet<_>>();
    for plan in selected_plans(storage, conversation, include_active, &purged, &mut report)? {
        summarize_plan(&plan, &mut report);
    }
    Ok(report)
}

/// What became of a plan at apply time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Applied {
    Written,
    /// A planned row changed after the plan was read: nothing was written.
    Changed,
    /// The conversation turned active after it was planned: nothing was written.
    Active,
}

/// True when every `id` still holds the content the plan read for it.
fn rows_unchanged(
    conn: &rusqlite::Connection,
    ids: &[&str],
    planned: &HashMap<&str, &str>,
) -> Result<bool> {
    let mut statement = conn.prepare_cached("SELECT content FROM chunks WHERE id = ?1")?;
    for id in ids {
        let current: Option<String> =
            rusqlite::OptionalExtension::optional(statement.query_row([id], |row| row.get(0)))?;
        if current.as_deref() != planned.get(id).copied() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn embed_rows(engine: &Engine, rows: &[&ConversationChunk]) -> Result<Vec<Vec<f32>>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let texts = rows
        .iter()
        .map(|chunk| chunk.content.as_str())
        .collect::<Vec<_>>();
    engine.embeddings().embed(&texts)
}

/// What one plan writes, whatever its kind.
struct Writes<'a> {
    /// Rows upserted, each with its source and provenance.
    rows: Vec<(&'a ConversationChunk, &'a str, ChunkProvenance)>,
    /// Ids whose stored content must still be what the plan read.
    validate: Vec<&'a str>,
    /// Replace the whole conversation: its current id set must be exactly the
    /// planned one, and every row goes before `rows` are written.
    whole: bool,
    deletes: Vec<&'a str>,
    evidence: Vec<ChunkEvidence>,
    /// Chunk count for the import mark.
    imported: usize,
}

fn writes_of<'a>(engine: &Engine, plan: &'a ScrubPlan) -> Option<Writes<'a>> {
    let attribution = |source_path: &Path| {
        derive_conversation_attribution(engine.projects_dir(), source_path)
            .parent_conversation_id
            .unwrap_or_else(|| plan.conversation_id.clone())
    };
    Some(match &plan.kind {
        ScrubPlanKind::Refused { .. } => return None,
        ScrubPlanKind::Missing {
            rewritten,
            dropped_ids,
            ..
        } if rewritten.is_empty() && dropped_ids.is_empty() => return None,
        ScrubPlanKind::Stable {
            source_path,
            chunks,
            changed_ids,
        } => {
            let default_source = attribution(source_path);
            let rows = chunks
                .iter()
                .filter(|chunk| changed_ids.contains(&chunk.id))
                .map(|chunk| {
                    (
                        chunk,
                        source_of(plan, &chunk.id),
                        provenance_for(plan, chunk, &default_source),
                    )
                })
                .collect::<Vec<_>>();
            Writes {
                validate: rows.iter().map(|(chunk, ..)| chunk.id.as_str()).collect(),
                evidence: rows
                    .iter()
                    .filter_map(|(chunk, ..)| plan.structural_evidence.get(&chunk.id).cloned())
                    .collect(),
                rows,
                whole: false,
                deletes: Vec::new(),
                imported: chunks.len(),
            }
        }
        ScrubPlanKind::Shifted {
            source_path,
            chunks,
        } => {
            let default_source = attribution(source_path);
            let source = plan
                .old_chunks
                .first()
                .map(|chunk| source_of(plan, &chunk.id))
                .unwrap_or("conversation");
            Writes {
                rows: chunks
                    .iter()
                    .map(|chunk| (chunk, source, provenance_for(plan, chunk, &default_source)))
                    .collect(),
                validate: plan
                    .old_chunks
                    .iter()
                    .map(|chunk| chunk.id.as_str())
                    .collect(),
                whole: true,
                deletes: Vec::new(),
                evidence: chunks
                    .iter()
                    .filter_map(|chunk| plan.structural_evidence.get(&chunk.id).cloned())
                    .collect(),
                imported: chunks.len(),
            }
        }
        ScrubPlanKind::Missing {
            rewritten,
            dropped_ids,
            ..
        } => Writes {
            rows: rewritten
                .iter()
                .map(|chunk| {
                    (
                        chunk,
                        source_of(plan, &chunk.id),
                        provenance_for(plan, chunk, &plan.conversation_id),
                    )
                })
                .collect(),
            validate: rewritten
                .iter()
                .map(|chunk| chunk.id.as_str())
                .chain(dropped_ids.iter().map(String::as_str))
                .collect(),
            whole: false,
            deletes: dropped_ids.iter().map(String::as_str).collect(),
            evidence: rewritten
                .iter()
                .map(|chunk| {
                    super::provenance_backfill::unreconstructible_chunk_evidence(chunk, None)
                })
                .collect(),
            imported: 0,
        },
    })
}

/// Apply one plan. Embeddings are computed first, outside any transaction. Then
/// one `BEGIN IMMEDIATE` transaction asks again whether the conversation is
/// active (unless `include_active`), re-reads the planned rows, and writes
/// content, vectors, provenance, evidence (relinked once, seeded by this
/// conversation's family), deletions and the import mark only if every planned
/// row still holds what the plan read (for a shifted plan, the conversation
/// also still has exactly the planned ids). Otherwise nothing is written: a
/// concurrent writer's rows survive (astra #1). Unchanged rows of a stable plan
/// are never rewritten. Returns the outcome and how long the write lock was held.
async fn apply_plan(
    engine: &Engine,
    plan: &ScrubPlan,
    include_active: bool,
) -> Result<(Applied, Duration)> {
    let Some(writes) = writes_of(engine, plan) else {
        return Ok((Applied::Written, Duration::ZERO));
    };
    let storage = engine.storage();
    let planned = plan
        .old_chunks
        .iter()
        .map(|chunk| (chunk.id.as_str(), chunk.content.as_str()))
        .collect::<HashMap<_, _>>();
    let rows = writes
        .rows
        .iter()
        .map(|(chunk, ..)| *chunk)
        .collect::<Vec<_>>();
    let embeddings = embed_rows(engine, &rows)?;
    let now = SystemTime::now();
    let held = std::cell::Cell::new(None::<std::time::Instant>);
    let applied = retry_busy(|| {
        storage.with_immediate_transaction(|tx| {
            held.set(Some(std::time::Instant::now()));
            if !include_active && is_active_in(tx, &plan.conversation_id, now)? {
                return Ok(Applied::Active);
            }
            if writes.whole {
                let current = queries::get_chunk_ids_for_conversation(tx, &plan.conversation_id)?;
                let same_ids = current.len() == writes.validate.len()
                    && current.iter().map(String::as_str).collect::<HashSet<_>>()
                        == writes.validate.iter().copied().collect::<HashSet<_>>();
                if !same_ids {
                    return Ok(Applied::Changed);
                }
            }
            if !rows_unchanged(tx, &writes.validate, &planned)? {
                return Ok(Applied::Changed);
            }
            if writes.whole {
                queries::delete_chunks_for_conversation(tx, &plan.conversation_id)?;
            }
            for ((chunk, source, provenance), embedding) in writes.rows.iter().zip(&embeddings) {
                queries::insert_chunk_with_source(tx, chunk, embedding, source)?;
                queries::insert_chunk_provenance(tx, &chunk.id, provenance)?;
            }
            for id in &writes.deletes {
                queries::delete_chunk(tx, id)?;
            }
            if !writes.evidence.is_empty() {
                queries::replace_conversation_evidence_in(
                    tx,
                    &plan.conversation_id,
                    &writes.evidence,
                )?;
            }
            if let Some((path, mtime)) = &plan.import_mark {
                queries::mark_file_imported_at(tx, path, writes.imported, mtime)?;
            }
            Ok(Applied::Written)
        })
    })
    .await?;
    let held = held.get().map_or(Duration::ZERO, |start| start.elapsed());
    if applied != Applied::Written {
        return Ok((applied, held));
    }
    let mut index = engine.search().write().await;
    if writes.whole {
        for chunk in &plan.old_chunks {
            index.remove_chunk(&chunk.id);
        }
    }
    for (chunk, embedding) in rows.into_iter().zip(embeddings) {
        index.remove_chunk(&chunk.id);
        index.insert_chunk(chunk.id.clone(), embedding);
    }
    for id in &writes.deletes {
        index.remove_chunk(id);
    }
    Ok((Applied::Written, held))
}

pub async fn run_scrub(
    engine: &Engine,
    dry_run: bool,
    conversation: Option<&str>,
    agent_transcripts_only: bool,
    include_active: bool,
) -> Result<ScrubReport> {
    if dry_run {
        return dry_run_scrub(
            engine.storage(),
            conversation,
            agent_transcripts_only,
            include_active,
        );
    }

    let mut report = ScrubReport::default();
    engine
        .storage()
        .set_busy_timeout(MAINTENANCE_BUSY_TIMEOUT)?;
    let agents = agent_transcripts(engine.storage(), conversation)?;
    let mut invalidated = false;
    if !agents.is_empty() {
        engine.invalidate_index_manifest()?;
        invalidated = true;
    }
    // One short transaction per conversation keeps each write bounded. The index is not edited slot by slot
    // (`remove_chunk` is a linear scan); the rebuild below drops every purged id.
    // Reflections go last, after every enrichment row that cites them is gone,
    // so an interrupted run never leaves a row pointing at a deleted reflection.
    for agent in &agents {
        summarize_agent_transcript(agent, &mut report);
        retry_busy(|| engine.storage().purge_conversation(&agent.conversation_id)).await?;
        for path in &agent.source_paths {
            retry_busy(|| engine.storage().mark_file_imported(path, 0)).await?;
        }
    }
    let reflection_ids = agents
        .iter()
        .flat_map(|agent| agent.reflection_ids.iter().cloned())
        .collect::<Vec<_>>();
    // Report what was deleted, not what was planned: the delete skips any
    // reflection a kept conversation came to cite since the plan was read.
    let mut reflections_deleted = 0;
    for batch in reflection_ids.chunks(PURGE_REFLECTION_BATCH) {
        reflections_deleted += retry_busy(|| engine.storage().purge_reflections(batch)).await?;
    }
    report.agent_reflections_purged = reflections_deleted;

    let plans = if agent_transcripts_only {
        Vec::new()
    } else {
        selected_plans(
            engine.storage(),
            conversation,
            include_active,
            &HashSet::new(),
            &mut report,
        )?
    };
    #[cfg(test)]
    tests::after_planning();
    if plans.iter().any(|plan| match &plan.kind {
        ScrubPlanKind::Refused { .. } => false,
        ScrubPlanKind::Missing {
            rewritten,
            dropped_ids,
            ..
        } => !rewritten.is_empty() || !dropped_ids.is_empty(),
        ScrubPlanKind::Stable { changed_ids, .. } => !changed_ids.is_empty(),
        ScrubPlanKind::Shifted { .. } => true,
    }) {
        engine.invalidate_index_manifest()?;
        invalidated = true;
    }
    for plan in plans {
        let (applied, held) = apply_plan(engine, &plan, include_active).await?;
        report.longest_transaction_ms = report
            .longest_transaction_ms
            .max(u64::try_from(held.as_millis()).unwrap_or(u64::MAX));
        match applied {
            Applied::Written => summarize_plan(&plan, &mut report),
            Applied::Changed => {
                report.skipped_changed += 1;
                report.actions.push(format!(
                    "changed during scrub, skipped: {}",
                    plan.conversation_id
                ));
            }
            Applied::Active => {
                report.deferred_active += 1;
                report
                    .actions
                    .push(format!("active, deferred: {}", plan.conversation_id));
            }
        }
    }
    if invalidated {
        // Replacing vectors leaves blank tombstones in the in-memory HNSW.
        // Rebuild a compact index from SQLite and persist it so queries never
        // have to over-fetch past tombstones and the next start loads from
        // cache. A run that never invalidated the manifest touches neither.
        report.index_rebuilt = Some(engine.rebuild_search_index().await?);
    }
    engine.storage().refresh_contamination_cache()?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    #[test]
    fn round3_guard_does_not_guess_message_continuity() {
        let old = vec![
            chunk_at(
                "probe",
                0,
                "CSR's compaction card (compacting): Generated summary.",
            ),
            chunk_at("probe", 1, "Keep the rollback plan."),
            chunk_at("probe", 2, "Already backed up."),
        ];
        let rebuilt = vec![chunk_at("probe", 0, "Already backed up.")];
        let lost = super::reparse_lacks_stored_text(
            &old,
            &rebuilt,
            &[false, false, false],
            &std::collections::HashMap::new(),
        );
        println!("missing_personal_text_detected={lost}");
        assert!(lost);
    }
    #[tokio::test]
    async fn round3_incomplete_source_must_keep_a_separate_personal_message() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let source = projects.join("incomplete.jsonl");
        let safe = "S".repeat(901);
        let header = "CSR's compaction card (compacting): ";
        let card = format!("{header}{}", "x".repeat(850 - header.len()));
        let own = format!("Keep my only rollback plan.{}", "z".repeat(73));
        std::fs::write(
            &source,
            format!("{}\n{}\n", user_line(&safe), user_line(&card)),
        )
        .unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(32)));
        let engine = Engine::from_parts(storage.clone(), embeddings.clone(), search, projects);
        let chunks = crate::import::chunk_messages(
            "incomplete",
            "project",
            "2026-09-01T00:00:00Z",
            &[
                (Speaker::User, safe),
                (Speaker::User, card),
                (Speaker::User, own.clone()),
            ],
            false,
        );
        println!(
            "old_sizes={:?}",
            chunks.iter().map(|c| c.content.len()).collect::<Vec<_>>()
        );
        for c in &chunks {
            storage
                .insert_chunk_with_source(c, &vec![0.1; 384], "conversation")
                .unwrap();
        }
        storage
            .upsert_import_state_explicit(
                source.to_str().unwrap(),
                "incomplete",
                chunks.len(),
                "fixture",
            )
            .unwrap();
        let plan = super::load_plan(&storage, "incomplete").unwrap();
        println!("chosen_plan={:?}", plan.kind);
        let (applied, _) = super::apply_plan(&engine, &plan, true).await.unwrap();
        let after = conversation_text(&storage, "incomplete");
        println!(
            "applied={applied:?} personal_survives={} rows_after={}",
            after.contains(&own),
            storage
                .get_chunk_ids_for_conversation("incomplete")
                .unwrap()
                .len()
        );
        assert!(
            after.contains(&own),
            "scrub must keep a distinct personal message missing from the source"
        );
    }
    #[test]
    fn round3_combined_marker_hotkey_keeps_reply() {
        let own = "Keep the deployment frozen.";
        for prefix in [">2", "> 2", ">>2"] {
            let raw = format!("{prefix}CSR's compaction card (compacting), quoted: “One.” {own}");
            let cut = crate::extraction::provenance::strip_csr_forwards(&raw);
            let extracted = crate::extraction::provenance::extractable(&raw);
            println!(
                "prefix={prefix} forwards={cut:?} extractable={extracted:?} scrub={:?}",
                super::scrub_contaminated_text(&raw)
            );
            assert_eq!(extracted.as_deref(), Some(own));
            assert_eq!(super::scrub_contaminated_text(&raw).as_deref(), Some(own));
        }
    }
    #[test]
    fn round3_a_split_header_must_be_selected() {
        let storage = Storage::open_memory().unwrap();
        let raw = format!(
            "{}CSR's compaction card (compacting), quoted: “One.” Keep the deployment frozen.",
            "P".repeat(890)
        );
        let chunks = crate::import::chunk_messages(
            "split-head",
            "p",
            "2026-09-01T00:00:00Z",
            &[(Speaker::User, raw.clone())],
            false,
        );
        for c in &chunks {
            storage.insert_chunk(c, &vec![0.1; 384]).unwrap();
        }
        let selected = storage.contaminated_conversations().unwrap();
        println!(
            "whole={:?} sizes={:?} selected={selected:?}",
            crate::import::contamination_reason(&raw),
            chunks.iter().map(|c| c.content.len()).collect::<Vec<_>>()
        );
        assert!(!selected.is_empty());
        let report = super::dry_run_scrub(&storage, Some("split-head"), false, true).unwrap();
        assert_eq!(report.unresolved_ambiguous_source, 1);
        assert_eq!((report.chunks_rewritten, report.chunks_dropped), (0, 0));
    }
    #[tokio::test]
    async fn round3_missing_source_without_spans_reports_unresolved() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings.clone(),
            Arc::new(RwLock::new(SearchEngine::new(16))),
            temp.path().to_path_buf(),
        );
        let header = "CSR's compaction card (compacting): ";
        let raw = format!(
            "{header}{}… Keep the deployment frozen.",
            "x".repeat(1199 - header.chars().count())
        );
        seed_split_message(&storage, &embeddings, "no-spans", &raw, false);
        let report = super::run_scrub(&engine, false, Some("no-spans"), false, true)
            .await
            .unwrap();
        assert_eq!(report.unresolved_ambiguous_source, 1);
        assert_eq!(report.conversations_scrubbed, 0);
        assert!(report.index_rebuilt.is_none());
        assert!(report
            .format_text(false)
            .contains("unresolved (ambiguous source): 1"));
        let after = conversation_text(&storage, "no-spans");
        println!("after={after:?}");
        assert_eq!(after.replace('|', ""), raw);
    }
    #[tokio::test]
    async fn round3_band_continuation_must_be_cut() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings.clone(),
            Arc::new(RwLock::new(SearchEngine::new(16))),
            temp.path().to_path_buf(),
        );
        let raw=format!("“Question?”\n\nCSR's dreamer asked this while you worked, quoted. Its reasoning:\n\n> {}\nKeep the deployment frozen.","x".repeat(1200));
        seed_split_message(&storage, &embeddings, "band-split", &raw, true);
        super::run_scrub(&engine, false, Some("band-split"), false, true)
            .await
            .unwrap();
        let after = conversation_text(&storage, "band-split");
        println!("after={after:?}");
        assert_eq!(after, "Keep the deployment frozen.");
    }

    #[test]
    fn round3_numeric_reply_split_at_boundary() {
        let raw = format!(
            "{}42CSR's compaction card (compacting), quoted: “One.”",
            "P".repeat(899)
        );
        let chunks = crate::import::chunk_messages(
            "numeric-boundary",
            "p",
            "2026-09-01T00:00:00Z",
            &[(Speaker::User, raw.clone())],
            false,
        );
        let (carried, ambiguous) = super::cut_forwards_across_chunks(&chunks, &[false, true]);
        assert!(!ambiguous.iter().any(|value| *value));
        let local = carried
            .iter()
            .filter_map(|s| super::scrub_contaminated_text(s))
            .collect::<Vec<_>>()
            .join("");
        let whole = super::scrub_contaminated_text(&raw).unwrap();
        println!(
            "whole_ends_42={} local_ends_42={} local_last={:?}",
            whole.ends_with("42"),
            local.ends_with("42"),
            &local[local.len() - 3..]
        );
        assert_eq!(local, whole, "numeric reply must survive a chunk boundary");
    }
    #[tokio::test]
    async fn round3_source_recovers_legacy_forward_without_stored_spans() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source-forward.jsonl");
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings.clone(),
            Arc::new(RwLock::new(SearchEngine::new(16))),
            temp.path().to_path_buf(),
        );
        let header = "CSR's compaction card (compacting): ";
        let raw = format!(
            "{header}{}… Keep the deployment frozen.",
            "x".repeat(1199 - header.chars().count())
        );
        std::fs::write(&source, user_line(&raw)).unwrap();
        seed_split_message(&storage, &embeddings, "source-forward", &raw, false);
        storage
            .upsert_import_state_explicit(source.to_str().unwrap(), "source-forward", 2, "fixture")
            .unwrap();
        let report = super::run_scrub(&engine, false, Some("source-forward"), false, true)
            .await
            .unwrap();
        assert_eq!(report.unresolved_ambiguous_source, 0);
        assert_eq!(
            conversation_text(&storage, "source-forward"),
            "Keep the deployment frozen."
        );
    }

    #[tokio::test]
    async fn round3_codex_source_keeps_both_numeric_reply_digits() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("rollout-forward.jsonl");
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings.clone(),
            Arc::new(RwLock::new(SearchEngine::new(16))),
            temp.path().to_path_buf(),
        );
        let prefix = format!("{}42", "P".repeat(899));
        let raw = format!("{prefix}CSR's compaction card (compacting), quoted: “One.”");
        std::fs::write(&source, serde_json::json!({"timestamp":"2026-09-01T00:00:00Z", "type":"response_item", "payload":{"type":"message", "role":"user", "content":[{"type":"input_text", "text":raw}]}}).to_string()).unwrap();
        seed_split_message(&storage, &embeddings, "codex:source-forward", &raw, false);
        storage
            .upsert_import_state_explicit(
                source.to_str().unwrap(),
                "codex:source-forward",
                2,
                "fixture",
            )
            .unwrap();
        let report = super::run_scrub(&engine, false, Some("codex:source-forward"), false, true)
            .await
            .unwrap();
        assert_eq!(report.unresolved_ambiguous_source, 0);
        assert_eq!(
            conversation_text(&storage, "codex:source-forward").replace('|', ""),
            prefix
        );
    }

    #[test]
    fn round3_nonadjacent_or_changed_spans_do_not_prove_continuity() {
        let storage = Storage::open_memory().unwrap();
        let embeddings = EmbeddingEngine::new().unwrap();
        let raw = format!(
            "CSR's compaction card (compacting), quoted: “{}” Keep the rollback plan.",
            "x".repeat(950)
        );
        let ids = seed_split_message(&storage, &embeddings, "span-gap", &raw, true);
        let chunks = storage.get_chunks_by_ids_with_provenance(&ids).unwrap();
        assert_eq!(
            super::message_continuity(&storage, &chunks).unwrap(),
            vec![false, true]
        );
        storage.with_connection(|conn| {
            conn.execute("UPDATE chunk_spans SET start_char=start_char+5,end_char=end_char+5 WHERE chunk_id=?1", [&ids[1]])?;
            Ok(())
        }).unwrap();
        assert_eq!(
            super::message_continuity(&storage, &chunks).unwrap(),
            vec![false, false]
        );
        storage.with_connection(|conn| {
            conn.execute("UPDATE chunk_spans SET start_char=start_char-5,end_char=end_char-5,content_hash='changed' WHERE chunk_id=?1", [&ids[1]])?;
            Ok(())
        }).unwrap();
        assert_eq!(
            super::message_continuity(&storage, &chunks).unwrap(),
            vec![false, false]
        );
    }

    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::sync::Arc;

    thread_local! {
        /// Runs once between planning and applying: a concurrent writer's turn.
        static AFTER_PLANNING: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
    }

    pub(super) fn after_planning() {
        if let Some(hook) = AFTER_PLANNING.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
    }

    fn when_planned(hook: impl FnOnce() + 'static) {
        AFTER_PLANNING.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    /// A file-backed database and a second connection to it: another process writing.
    fn shared_database(temp: &tempfile::TempDir) -> (Arc<Storage>, Arc<Storage>) {
        let path = temp.path().join("csr-engine.db");
        let storage = Arc::new(Storage::open(&path).unwrap());
        let other = Arc::new(Storage::open(&path).unwrap());
        (storage, other)
    }

    fn user_line(text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "timestamp": "2026-09-01T00:00:00Z",
            "message": {"content": text}
        })
        .to_string()
    }

    fn chunk_at(conversation: &str, seq: usize, content: &str) -> ConversationChunk {
        ConversationChunk {
            id: super::super::generate_chunk_id(conversation, seq),
            conversation_id: conversation.into(),
            project_name: "project".into(),
            timestamp: "2026-09-01T00:00:00Z".into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: Speaker::User,
            seq,
            is_sidechain: false,
        }
    }

    fn stored_vector(storage: &Storage, id: &str) -> Vec<f32> {
        storage
            .get_chunk_vectors_by_ids(&[id.to_string()])
            .unwrap()
            .pop()
            .map(|(_, vector)| vector)
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn astra_1_a_row_changed_after_planning_survives_an_in_place_plan() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let (storage, other) = shared_database(&temp);
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(4)));
        let engine = Engine::from_parts(storage.clone(), embeddings.clone(), search, projects);
        let conversation = "concurrent-missing";
        let first = seed_source_chunk(
            &storage,
            &embeddings,
            conversation,
            0,
            "Keep this fact.\n\n[[CSR:RECAP]] generated recap",
            "conversation",
        );
        let second = seed_source_chunk(
            &storage,
            &embeddings,
            conversation,
            1,
            "[[CSR:RECAP]] generated only",
            "conversation",
        );
        let latest = embeddings
            .embed(&["Keep my latest decision."])
            .unwrap()
            .remove(0);
        let (writer, vector) = (other.clone(), latest.clone());
        when_planned(move || {
            writer
                .insert_chunk_with_source(
                    &chunk_at(conversation, 0, "Keep my latest decision."),
                    &vector,
                    "conversation",
                )
                .unwrap();
        });

        let report = super::run_scrub(&engine, false, Some(conversation), false, true)
            .await
            .unwrap();

        assert_eq!(
            storage.get_chunk_content(&first).unwrap().as_deref(),
            Some("Keep my latest decision.")
        );
        assert_eq!(stored_vector(&storage, &first), latest);
        assert!(storage.get_chunk_content(&second).unwrap().is_some());
        assert!(
            report
                .actions
                .iter()
                .any(|line| line == "changed during scrub, skipped: concurrent-missing"),
            "{:?}",
            report.actions
        );
    }

    #[tokio::test]
    async fn astra_1_a_chunk_appended_after_planning_survives_a_shifted_plan() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let source = projects.join("concurrent-shifted.jsonl");
        std::fs::write(&source, user_line("Clean replacement text.")).unwrap();
        let (storage, other) = shared_database(&temp);
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(4)));
        let engine = Engine::from_parts(storage.clone(), embeddings.clone(), search, projects);
        let conversation = "concurrent-shifted";
        seed_source_chunk(
            &storage,
            &embeddings,
            conversation,
            0,
            "[[CSR:RECAP]] contaminated old content",
            "conversation",
        );
        seed_source_chunk(
            &storage,
            &embeddings,
            conversation,
            1,
            "[[CSR:RECAP]] stale generated tail",
            "conversation",
        );
        storage
            .upsert_import_state_explicit(
                source.to_string_lossy().as_ref(),
                conversation,
                2,
                "fixture",
            )
            .unwrap();
        let appended = chunk_at(conversation, 2, "A new message the watcher imported.");
        let vector = embeddings
            .embed(&[appended.content.as_str()])
            .unwrap()
            .remove(0);
        let (writer, row, written) = (other.clone(), appended.clone(), vector.clone());
        when_planned(move || {
            writer
                .insert_chunk_with_source(&row, &written, "conversation")
                .unwrap();
        });

        super::run_scrub(&engine, false, Some(conversation), false, true)
            .await
            .unwrap();

        assert_eq!(
            storage.get_chunk_content(&appended.id).unwrap().as_deref(),
            Some("A new message the watcher imported.")
        );
        assert_eq!(stored_vector(&storage, &appended.id), vector);
    }

    #[tokio::test]
    async fn astra_1_an_unchanged_stable_row_is_never_rewritten() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let conversation = "concurrent-stable";
        let first_message = format!("Keep this user-authored fact. {}", "a".repeat(600));
        let second_message = format!("Plain fact. {}", "b".repeat(600));
        let source = projects.join(format!("{conversation}.jsonl"));
        std::fs::write(
            &source,
            format!(
                "{}\n{}\n",
                user_line(&first_message),
                user_line(&second_message)
            ),
        )
        .unwrap();
        let (storage, other) = shared_database(&temp);
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(4)));
        let engine = Engine::from_parts(storage.clone(), embeddings.clone(), search, projects);
        let first = seed_source_chunk(
            &storage,
            &embeddings,
            conversation,
            0,
            &format!("{first_message}\n\n[[CSR:RECAP]] generated recap"),
            "conversation",
        );
        let second = seed_source_chunk(
            &storage,
            &embeddings,
            conversation,
            1,
            &second_message,
            "conversation",
        );
        storage
            .upsert_import_state_explicit(
                source.to_string_lossy().as_ref(),
                conversation,
                2,
                "fixture",
            )
            .unwrap();
        let newer = embeddings
            .embed(&["Newer watcher text."])
            .unwrap()
            .remove(0);
        let (writer, vector) = (other.clone(), newer.clone());
        when_planned(move || {
            writer
                .insert_chunk_with_source(
                    &chunk_at(conversation, 1, "Newer watcher text."),
                    &vector,
                    "conversation",
                )
                .unwrap();
        });

        super::run_scrub(&engine, false, Some(conversation), false, true)
            .await
            .unwrap();

        assert_eq!(
            storage.get_chunk_content(&first).unwrap().as_deref(),
            Some(first_message.as_str())
        );
        assert_eq!(
            storage.get_chunk_content(&second).unwrap().as_deref(),
            Some("Newer watcher text.")
        );
        assert_eq!(stored_vector(&storage, &second), newer);
    }

    #[tokio::test]
    async fn astra_2_a_source_that_grew_after_parsing_stays_unimported() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let conversation = "grew-after-parse";
        let source = projects.join(format!("{conversation}.jsonl"));
        std::fs::write(
            &source,
            format!("{}\n", user_line("Keep this user-authored fact.")),
        )
        .unwrap();
        let (storage, _) = shared_database(&temp);
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(4)));
        let engine = Engine::from_parts(storage.clone(), embeddings.clone(), search, projects);
        seed_source_chunk(
            &storage,
            &embeddings,
            conversation,
            0,
            "Keep this user-authored fact.\n\n[[CSR:RECAP]] generated recap paragraph",
            "conversation",
        );
        storage
            .upsert_import_state_explicit(
                source.to_string_lossy().as_ref(),
                conversation,
                1,
                "fixture",
            )
            .unwrap();
        let grown = source.clone();
        when_planned(move || {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&grown)
                .unwrap();
            // Long enough for a chunk of its own: the importer embeds new chunks
            // past the recorded count, it never re-embeds a grown last one.
            let decision = format!(
                "An appended decision the scrub never read. {}",
                "z".repeat(900)
            );
            writeln!(file, "{}", user_line(&decision)).unwrap();
            file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(2))
                .unwrap();
        });

        super::run_scrub(&engine, false, Some(conversation), false, true)
            .await
            .unwrap();

        assert!(!storage.is_file_imported(&source).unwrap());
        engine.import_file(&source, "project").await.unwrap();
        let ids = storage
            .get_chunk_ids_for_conversation(conversation)
            .unwrap();
        let everything = ids
            .iter()
            .filter_map(|id| storage.get_chunk_content(id).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            everything.contains("An appended decision the scrub never read."),
            "{everything}"
        );
    }

    #[tokio::test]
    async fn astra_3_a_second_registered_source_keeps_the_scrub_in_place() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let conversation = "two-sources";
        // An older, partial copy sorts first; the complete transcript sorts second.
        let partial = projects
            .join("a-older")
            .join(format!("{conversation}.jsonl"));
        let complete = projects
            .join("b-current")
            .join(format!("{conversation}.jsonl"));
        for (path, body) in [
            (&partial, user_line("Clean first message.")),
            (
                &complete,
                format!(
                    "{}\n{}",
                    user_line("Clean first message."),
                    user_line("My only rollback plan.")
                ),
            ),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(4)));
        let engine = Engine::from_parts(storage.clone(), embeddings.clone(), search, projects);
        seed_source_chunk(
            &storage,
            &embeddings,
            conversation,
            0,
            "[[CSR:RECAP]] contaminated first chunk",
            "conversation",
        );
        let kept = seed_source_chunk(
            &storage,
            &embeddings,
            conversation,
            1,
            "My only rollback plan.",
            "conversation",
        );
        for path in [&partial, &complete] {
            storage
                .upsert_import_state_explicit(
                    path.to_string_lossy().as_ref(),
                    conversation,
                    1,
                    "fixture",
                )
                .unwrap();
        }

        super::run_scrub(&engine, false, Some(conversation), false, true)
            .await
            .unwrap();

        assert_eq!(
            storage.get_chunk_content(&kept).unwrap().as_deref(),
            Some("My only rollback plan.")
        );
    }

    #[tokio::test]
    async fn astra_r2_3_a_writer_resuming_after_planning_defers_the_plan() {
        // An old hook writes a chunk's text and its vector in two autocommit
        // statements. Its first lands after planning and touches the chunk's
        // timestamp; the apply transaction sees the conversation active again
        // and writes nothing, so its second statement cannot leave an old
        // vector beside scrubbed text.
        let temp = tempfile::tempdir().unwrap();
        let (storage, old) = shared_database(&temp);
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(100)));
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings.clone(),
            search,
            temp.path().join("projects"),
        );
        let dirty = chunk_at(
            "probe-session",
            0,
            "Keep the deployment frozen.\n\n[[CSR:RECAP]] generated recap",
        );
        let old_vector = embeddings
            .embed(&[dirty.content.as_str()])
            .unwrap()
            .remove(0);
        storage.insert_chunk(&dirty, &old_vector).unwrap();
        let mut report = super::ScrubReport::default();
        let plans = super::selected_plans(
            &storage,
            Some("probe-session"),
            false,
            &HashSet::new(),
            &mut report,
        )
        .unwrap();
        assert_eq!((plans.len(), report.deferred_active), (1, 0));
        old.with_connection(|conn| {
            conn.execute(
                "UPDATE chunks SET timestamp = ?1, content = ?2 WHERE id = ?3",
                rusqlite::params![chrono::Utc::now().to_rfc3339(), dirty.content, dirty.id],
            )?;
            Ok(())
        })
        .unwrap();

        let (applied, _) = super::apply_plan(&engine, &plans[0], false).await.unwrap();
        assert_eq!(applied, super::Applied::Active);
        let bytes: Vec<u8> = old_vector.iter().flat_map(|v| v.to_le_bytes()).collect();
        old.with_connection(|conn| {
            conn.execute(
                "INSERT INTO chunk_embeddings (chunk_id, embedding) VALUES (?1, ?2)
                 ON CONFLICT(chunk_id) DO UPDATE SET embedding = excluded.embedding",
                rusqlite::params![dirty.id, bytes],
            )?;
            Ok(())
        })
        .unwrap();
        // Text and vector still agree: the plan waits for a later, quiet run.
        assert_eq!(
            storage.get_chunk_content(&dirty.id).unwrap().as_deref(),
            Some(dirty.content.as_str())
        );
        assert_eq!(stored_vector(&storage, &dirty.id), old_vector);
    }

    /// One user message as an older binary stored it: cut into byte-budget chunks
    /// by the production chunker, forwards left in, each chunk with a vector and,
    /// when `spans`, a span naming the message's one provenance event.
    fn seed_split_message(
        storage: &Storage,
        embeddings: &EmbeddingEngine,
        conversation: &str,
        message: &str,
        spans: bool,
    ) -> Vec<String> {
        let chunks = crate::import::chunk_messages(
            conversation,
            "project",
            "2026-09-01T00:00:00Z",
            &[(Speaker::User, message.to_string())],
            false,
        );
        assert!(chunks.len() >= 2, "the message must span chunks");
        for chunk in &chunks {
            let vector = embeddings
                .embed(&[chunk.content.as_str()])
                .unwrap()
                .remove(0);
            storage
                .insert_chunk_with_source(chunk, &vector, "conversation")
                .unwrap();
        }
        if spans {
            storage
                .with_connection(|conn| {
                    crate::storage::queries::insert_provenance_event(
                        conn,
                        &crate::provenance::ProvenanceEvent {
                            event_id: format!("{conversation}-message"),
                            conversation_id: conversation.into(),
                            message_key: "m0".into(),
                            seq: 0,
                            channel: "user_message".into(),
                            trust_tier: crate::provenance::TrustTier::UserHistory,
                            parent_event_id: None,
                            receipt_kind: "jsonl".into(),
                            receipt_ref: None,
                            observed_at: "2026-09-01T00:00:00Z".into(),
                        },
                    )?;
                    let mut start_char = 0_i64;
                    for chunk in &chunks {
                        let end_char = start_char + chunk.content.chars().count() as i64;
                        conn.execute(
                            "INSERT INTO chunk_spans (chunk_id, event_id, start_char, end_char, content_hash)
                             VALUES (?1, ?2, ?3, ?4, ?5)",
                            rusqlite::params![
                                chunk.id,
                                format!("{conversation}-message"),
                                start_char,
                                end_char,
                                crate::provenance::content_hash(&chunk.content)
                            ],
                        )?;
                        start_char = end_char;
                    }
                    Ok(())
                })
                .unwrap();
        }
        chunks.into_iter().map(|chunk| chunk.id).collect()
    }

    /// The person's words left across the conversation's chunks, in order.
    fn conversation_text(storage: &Storage, conversation: &str) -> String {
        storage
            .get_chunk_ids_for_conversation(conversation)
            .unwrap()
            .iter()
            .filter_map(|id| storage.get_chunk_content(id).unwrap())
            .collect::<Vec<_>>()
            .join("|")
    }

    #[tokio::test]
    async fn astra_r2_7_a_forward_a_chunk_boundary_split_is_cut_in_every_chunk() {
        let head = "CSR's compaction card (compacting): ";
        let latin = format!("{head}{}\u{2026}", "x".repeat(1199 - head.chars().count()));
        let cjk = format!(
            "{head}{}\u{2026}",
            "\u{8981}".repeat(1199 - head.chars().count())
        );
        let own = "Keep the deployment frozen.";
        for (conversation, send) in [("split-latin", &latin), ("split-cjk", &cjk)] {
            let storage = Arc::new(Storage::open_memory().unwrap());
            let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
            let search = Arc::new(RwLock::new(SearchEngine::new(16)));
            let temp = tempfile::tempdir().unwrap();
            let engine = Engine::from_parts(
                storage.clone(),
                embeddings.clone(),
                search,
                temp.path().to_path_buf(),
            );
            let message = format!("{send} {own}");
            let ids = seed_split_message(&storage, &embeddings, conversation, &message, true);
            super::run_scrub(&engine, false, Some(conversation), false, true)
                .await
                .unwrap();
            assert_eq!(
                conversation_text(&storage, conversation),
                own,
                "{conversation}: {} chunks",
                ids.len()
            );
        }
    }

    #[test]
    fn astra_r2_7_the_guard_never_counts_a_generated_tail_as_the_persons() {
        // Explicit provenance continuity lets the guard remove the generated tail.
        let head = "CSR's compaction card (compacting): ";
        let send = format!("{head}{}\u{2026}", "x".repeat(1199 - head.chars().count()));
        let own = "Keep the deployment frozen.";
        let chunk_of = |text: &str| {
            crate::import::chunk_messages(
                "probe-session",
                "probe",
                "2026-09-01T00:00:00Z",
                &[(Speaker::User, text.to_string())],
                false,
            )
        };
        let stored = chunk_of(&format!("{send} {own}"));
        assert_eq!(
            stored
                .iter()
                .map(|chunk| chunk.content.len())
                .collect::<Vec<_>>(),
            vec![900, 330]
        );
        assert!(!super::reparse_lacks_stored_text(
            &stored,
            &chunk_of(own),
            &[false, true],
            &std::collections::HashMap::new(),
        ));
        // A stored word the re-parse truly lacks still counts.
        assert!(super::reparse_lacks_stored_text(
            &chunk_of(&format!("{send} {own} My only rollback plan.")),
            &chunk_of(own),
            &[false, true],
            &std::collections::HashMap::new(),
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_hook_import_skips_while_a_maintenance_run_holds_the_database() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let storage = Arc::new(Storage::open(&temp.path().join("csr-engine.db")).unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(4)));
        let engine = Engine::from_parts(storage.clone(), embeddings, search, projects.clone());
        let source = projects.join("held-session.jsonl");
        std::fs::write(&source, user_line("Keep this fact.")).unwrap();
        let lock = crate::maintenance::MaintenanceLock::acquire(temp.path()).unwrap();
        assert!(engine.import_file(&source, "project").await.is_err());
        drop(lock);
        assert!(
            !storage.is_file_imported(&source).unwrap(),
            "the file stays for the watcher"
        );
        assert!(engine.import_file(&source, "project").await.unwrap() > 0);
        assert!(storage.is_file_imported(&source).unwrap());
    }

    #[tokio::test]
    async fn an_active_conversation_is_deferred_unless_included() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(4)));
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings.clone(),
            search,
            projects.clone(),
        );
        // Written just now: a live session may still be appending to it.
        let fresh = projects.join("live-session.jsonl");
        std::fs::write(&fresh, user_line("Keep this fact.")).unwrap();
        let live = seed_source_chunk(
            &storage,
            &embeddings,
            "live-session",
            0,
            "Keep this fact.\n\n[[CSR:RECAP]] generated recap",
            "conversation",
        );
        storage
            .upsert_import_state_explicit(
                fresh.to_string_lossy().as_ref(),
                "live-session",
                1,
                "fixture",
            )
            .unwrap();
        // No source on disk, but a chunk stamped a minute ago.
        let recent = chunk_at(
            "recent-chunk",
            0,
            "Keep that fact.\n\n[[CSR:RECAP]] generated recap",
        );
        let recent = ConversationChunk {
            timestamp: (chrono::Utc::now() - chrono::Duration::minutes(1)).to_rfc3339(),
            ..recent
        };
        let vector = embeddings
            .embed(&[recent.content.as_str()])
            .unwrap()
            .remove(0);
        storage
            .insert_chunk_with_source(&recent, &vector, "conversation")
            .unwrap();
        // Quiet for an hour: scrubbed.
        let quiet = projects.join("quiet-session.jsonl");
        std::fs::write(&quiet, user_line("Keep the other fact.")).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&quiet)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
            .unwrap();
        seed_source_chunk(
            &storage,
            &embeddings,
            "quiet-session",
            0,
            "Keep the other fact.\n\n[[CSR:RECAP]] generated recap",
            "conversation",
        );
        storage
            .upsert_import_state_explicit(
                quiet.to_string_lossy().as_ref(),
                "quiet-session",
                1,
                "fixture",
            )
            .unwrap();

        let dry = super::dry_run_scrub(&storage, None, false, false).unwrap();
        assert_eq!((dry.deferred_active, dry.conversations_scrubbed), (2, 1));
        assert!(dry
            .actions
            .contains(&"active, deferred: live-session".to_string()));
        assert!(dry
            .actions
            .contains(&"active, deferred: recent-chunk".to_string()));
        assert!(dry.format_text(true).contains("deferred (active): 2\n"));

        let report = super::run_scrub(&engine, false, None, false, false)
            .await
            .unwrap();
        assert_eq!(
            (report.deferred_active, report.conversations_scrubbed),
            (2, 1)
        );
        assert_eq!(
            storage.get_chunk_content(&live).unwrap().as_deref(),
            Some("Keep this fact.\n\n[[CSR:RECAP]] generated recap"),
            "a deferred conversation is not written"
        );

        let included = super::run_scrub(&engine, false, None, false, true)
            .await
            .unwrap();
        assert_eq!(
            (included.deferred_active, included.conversations_scrubbed),
            (0, 2)
        );
        assert_eq!(
            storage.get_chunk_content(&live).unwrap().as_deref(),
            Some("Keep this fact.")
        );
        assert_eq!(
            storage.get_chunk_content(&recent.id).unwrap().as_deref(),
            Some("Keep that fact.")
        );
    }

    #[tokio::test]
    async fn astra_8_a_forward_past_byte_400_is_selected_and_rewritten() {
        let storage = Storage::open_memory().unwrap();
        let embeddings = EmbeddingEngine::new().unwrap();
        let content = format!(
            "{}\n\nCSR's compaction card (compacting): Summarizing 278k.",
            "Keep this user requirement. ".repeat(16)
        );
        seed_source_chunk(
            &storage,
            &embeddings,
            "late-card",
            0,
            &content,
            "conversation",
        );
        let dry = super::dry_run_scrub(&storage, Some("late-card"), false, true).unwrap();
        assert_eq!(
            (
                dry.conversations_scrubbed,
                dry.chunks_rewritten,
                dry.chunks_dropped
            ),
            (1, 1, 0),
            "{:?}",
            dry.actions
        );
    }

    use tokio::sync::RwLock;

    use crate::embeddings::EmbeddingEngine;
    use crate::engine::Engine;
    use crate::import::ConversationChunk;
    use crate::provenance::{ChunkProvenance, Speaker};
    use crate::search::SearchEngine;
    use crate::storage::Storage;

    #[tokio::test]
    async fn scrub_rewrites_stable_chunk_in_place_and_replaces_its_vector() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let source = projects.join("stable-conversation.jsonl");
        std::fs::write(
            &source,
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-09-01T00:00:00Z",
                "message": {"content": "Keep this user-authored fact.\n\n[[CSR:RECAP]] generated recap paragraph"}
            })
            .to_string(),
        )
        .unwrap();

        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(4)));
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings.clone(),
            search.clone(),
            projects.clone(),
        );
        let chunk_id = super::super::generate_chunk_id("stable-conversation", 0);
        let old_content =
            "Keep this user-authored fact.\n\n[[CSR:RECAP]] generated recap paragraph";
        let old_vector = embeddings.embed(&[old_content]).unwrap().remove(0);
        let chunk = ConversationChunk {
            id: chunk_id.clone(),
            conversation_id: "stable-conversation".into(),
            project_name: "project".into(),
            timestamp: "2026-09-01T00:00:00Z".into(),
            content: old_content.into(),
            message_count: 1,
            summary: None,
            author: Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage
            .insert_chunk_with_source(&chunk, &old_vector, "conversation")
            .unwrap();
        storage
            .upsert_import_state_explicit(
                source.to_string_lossy().as_ref(),
                "stable-conversation",
                1,
                "fixture",
            )
            .unwrap();
        search
            .write()
            .await
            .insert_chunk(chunk_id.clone(), old_vector.clone());
        let rowid_before: i64 = storage
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT rowid FROM chunks WHERE id = ?1",
                    [&chunk_id],
                    |row| row.get(0),
                )?)
            })
            .unwrap();

        let report = super::run_scrub(&engine, false, Some("stable-conversation"), false, true)
            .await
            .unwrap();

        assert_eq!(report.conversations_scrubbed, 1);
        assert_eq!(report.chunks_rewritten, 1);
        assert_eq!(report.chunks_dropped, 0);
        assert_eq!(report.vectors_replaced, 1);
        let rowid_after: i64 = storage
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT rowid FROM chunks WHERE id = ?1",
                    [&chunk_id],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(rowid_after, rowid_before, "stable IDs preserve rowids");
        let clean_content = storage.get_chunk_content(&chunk_id).unwrap().unwrap();
        assert_eq!(clean_content, "Keep this user-authored fact.");
        assert!(crate::import::contamination_reason(&clean_content).is_none());

        let index = search.read().await;
        assert!(
            index.search_chunks(&old_vector, 1, 0.999).is_empty(),
            "the blanked stale vector must no longer be searchable"
        );
        let new_vector = embeddings
            .embed(&[clean_content.as_str()])
            .unwrap()
            .remove(0);
        assert_eq!(index.search_chunks(&new_vector, 1, 0.999)[0].id, chunk_id);
    }

    struct Shifted {
        _temp: tempfile::TempDir,
        engine: Engine,
        storage: Arc<Storage>,
        search: Arc<RwLock<SearchEngine>>,
        first_id: String,
        stale_id: String,
    }

    /// A conversation stored as two chunks whose transcript now parses to one:
    /// the stored second chunk, `tail`, has no counterpart in the re-parse.
    async fn shifted_fixture(tail: &str) -> Shifted {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let source = projects.join("shifted-conversation.jsonl");
        std::fs::write(
            &source,
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-09-01T00:00:00Z",
                "message": {"content": "Clean replacement text."}
            })
            .to_string(),
        )
        .unwrap();

        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(4)));
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings.clone(),
            search.clone(),
            projects,
        );
        let first_id = super::super::generate_chunk_id("shifted-conversation", 0);
        let stale_id = super::super::generate_chunk_id("shifted-conversation", 1);
        for (id, seq, content) in [
            (
                first_id.clone(),
                0,
                "[[CSR:RECAP]] contaminated old content",
            ),
            (stale_id.clone(), 1, tail),
        ] {
            let vector = embeddings.embed(&[content]).unwrap().remove(0);
            let chunk = ConversationChunk {
                id: id.clone(),
                conversation_id: "shifted-conversation".into(),
                project_name: "project".into(),
                timestamp: "2026-09-01T00:00:00Z".into(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: Speaker::User,
                seq,
                is_sidechain: false,
            };
            storage
                .insert_chunk_with_source(&chunk, &vector, "conversation")
                .unwrap();
            storage
                .insert_chunk_provenance(
                    &id,
                    &ChunkProvenance {
                        author: Speaker::User,
                        source_conv_id: "parent-conversation".into(),
                        supersedes: (seq == 0).then(|| "prior-claim".into()),
                    },
                )
                .unwrap();
            search.write().await.insert_chunk(id, vector);
        }
        storage
            .upsert_import_state_explicit(
                source.to_string_lossy().as_ref(),
                "shifted-conversation",
                2,
                "fixture",
            )
            .unwrap();

        Shifted {
            _temp: temp,
            engine,
            storage,
            search,
            first_id,
            stale_id,
        }
    }

    #[tokio::test]
    async fn scrub_keeps_a_stored_tail_the_reparse_lacks() {
        // astra #3: the person's words in a stored chunk the transcript no longer
        // yields are not contamination. The scrub falls back to in place.
        let Shifted {
            engine,
            storage,
            search,
            first_id,
            stale_id,
            _temp: _hold,
        } = shifted_fixture("My only rollback plan.").await;

        let report = super::run_scrub(&engine, false, Some("shifted-conversation"), false, true)
            .await
            .unwrap();

        assert_eq!((report.chunks_rewritten, report.chunks_dropped), (0, 1));
        assert_eq!(
            storage.get_chunk_content(&stale_id).unwrap().as_deref(),
            Some("My only rollback plan.")
        );
        assert!(search.read().await.has_chunk(&stale_id));
        assert!(storage.get_chunk_content(&first_id).unwrap().is_none());
    }

    #[tokio::test]
    async fn scrub_rebuilds_shifted_ids_atomically_and_restores_provenance() {
        // A tail that is CSR's own words alone is dropped by the rebuild.
        let Shifted {
            engine,
            storage,
            search,
            first_id,
            stale_id,
            _temp: _hold,
        } = shifted_fixture("[[CSR:RECAP]] stale generated tail").await;

        let report = super::run_scrub(&engine, false, Some("shifted-conversation"), false, true)
            .await
            .unwrap();

        assert_eq!(report.chunks_rewritten, 1);
        assert_eq!(report.chunks_dropped, 1);
        assert_eq!(
            storage
                .get_chunk_ids_for_conversation("shifted-conversation")
                .unwrap(),
            vec![first_id.clone()]
        );
        assert_eq!(
            storage.get_chunk_content(&first_id).unwrap().as_deref(),
            Some("Clean replacement text.")
        );
        assert_eq!(
            storage.get_chunk_provenance(&first_id).unwrap().unwrap(),
            ChunkProvenance {
                author: Speaker::User,
                source_conv_id: "parent-conversation".into(),
                supersedes: Some("prior-claim".into()),
            }
        );
        assert!(storage.get_chunk_content(&stale_id).unwrap().is_none());
        assert!(!search.read().await.has_chunk(&stale_id));
    }

    #[tokio::test]
    async fn scrub_missing_source_rewrites_and_drops_in_place() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(4)));
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings.clone(),
            search.clone(),
            projects,
        );
        let rewrite_id = super::super::generate_chunk_id("missing-conversation", 0);
        let drop_id = super::super::generate_chunk_id("missing-conversation", 1);
        let mut rewrite_rowid = 0;
        for (id, seq, content) in [
            (
                rewrite_id.clone(),
                0,
                "Keep this fact.\n\n[[CSR:RECAP]] generated recap",
            ),
            (drop_id.clone(), 1, "[[CSR:RECAP]] generated only"),
        ] {
            let vector = embeddings.embed(&[content]).unwrap().remove(0);
            let chunk = ConversationChunk {
                id: id.clone(),
                conversation_id: "missing-conversation".into(),
                project_name: "project".into(),
                timestamp: "2026-09-01T00:00:00Z".into(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: Speaker::Assistant,
                seq,
                is_sidechain: false,
            };
            storage
                .insert_chunk_with_source(&chunk, &vector, "conversation")
                .unwrap();
            search.write().await.insert_chunk(id.clone(), vector);
            if seq == 0 {
                rewrite_rowid = storage
                    .with_connection(|conn| {
                        Ok(conn.query_row(
                            "SELECT rowid FROM chunks WHERE id = ?1",
                            [&id],
                            |row| row.get(0),
                        )?)
                    })
                    .unwrap();
            }
        }

        let report = super::run_scrub(&engine, false, Some("missing-conversation"), false, true)
            .await
            .unwrap();

        assert_eq!(report.chunks_rewritten, 1);
        assert_eq!(report.chunks_dropped, 1);
        assert_eq!(
            storage.get_chunk_content(&rewrite_id).unwrap().as_deref(),
            Some("Keep this fact.")
        );
        let rowid_after: i64 = storage
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT rowid FROM chunks WHERE id = ?1",
                    [&rewrite_id],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(rowid_after, rewrite_rowid);
        assert!(storage.get_chunk_content(&drop_id).unwrap().is_none());
        assert!(!search.read().await.has_chunk(&drop_id));
    }

    fn rollout_fixture(path: &std::path::Path) {
        std::fs::write(
            path,
            serde_json::json!({
                "timestamp": "2026-09-01T00:00:00Z",
                "type": "response_item",
                "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "codex rollout line"}]}
            })
            .to_string(),
        )
        .unwrap();
    }

    fn seed_source_chunk(
        storage: &Storage,
        embeddings: &EmbeddingEngine,
        conversation: &str,
        seq: usize,
        content: &str,
        source: &str,
    ) -> String {
        let id = super::super::generate_chunk_id(conversation, seq);
        let vector = embeddings.embed(&[content]).unwrap().remove(0);
        storage
            .insert_chunk_with_source(
                &ConversationChunk {
                    id: id.clone(),
                    conversation_id: conversation.into(),
                    project_name: "project".into(),
                    timestamp: "2026-09-01T00:00:00Z".into(),
                    content: content.into(),
                    message_count: 1,
                    summary: None,
                    author: Speaker::User,
                    seq,
                    is_sidechain: false,
                },
                &vector,
                source,
            )
            .unwrap();
        id
    }

    #[tokio::test]
    async fn scrub_codex_rollout_with_existing_file_is_scrubbed_in_place() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let rollout = temp.path().join("rollout-2026-09-01T00-00-00-abc.jsonl");
        rollout_fixture(&rollout);

        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(4)));
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings.clone(),
            search.clone(),
            projects,
        );
        let conversation = "codex:01a06d1b-0000-0000-0000-000000000000";
        let keep = seed_source_chunk(
            &storage,
            &embeddings,
            conversation,
            0,
            "Real codex work, no emission here.",
            "codex_rollout",
        );
        let rewrite = seed_source_chunk(
            &storage,
            &embeddings,
            conversation,
            1,
            "Keep this fact.\n\n[[CSR:RECAP]] generated recap",
            "codex_rollout",
        );
        let drop = seed_source_chunk(
            &storage,
            &embeddings,
            conversation,
            2,
            "[[CSR:RECAP]] generated only",
            "codex_rollout",
        );
        storage
            .upsert_import_state_explicit(
                rollout.to_string_lossy().as_ref(),
                conversation,
                3,
                "fixture",
            )
            .unwrap();

        let dry = super::dry_run_scrub(&storage, Some(conversation), false, true).unwrap();
        assert_eq!(
            (dry.chunks_rewritten, dry.chunks_dropped, dry.refused),
            (1, 1, 0)
        );

        let report = super::run_scrub(&engine, false, Some(conversation), false, true)
            .await
            .unwrap();
        assert_eq!(report.conversations_scrubbed, 1);
        assert_eq!(report.chunks_rewritten, 1);
        assert_eq!(report.chunks_dropped, 1);
        assert_eq!(report.refused, 0);
        assert_eq!(
            storage.get_chunk_content(&keep).unwrap().as_deref(),
            Some("Real codex work, no emission here.")
        );
        assert_eq!(
            storage.get_chunk_content(&rewrite).unwrap().as_deref(),
            Some("Keep this fact.")
        );
        assert!(storage.get_chunk_content(&drop).unwrap().is_none());
        assert_eq!(
            storage.get_chunk_source(&keep).unwrap().as_deref(),
            Some("codex_rollout")
        );
        assert_eq!(
            storage.get_chunk_source(&rewrite).unwrap().as_deref(),
            Some("codex_rollout")
        );
    }

    #[tokio::test]
    async fn scrub_refuses_a_reparse_that_would_rebuild_zero_chunks() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        // Parses to no chunks under the Claude parser.
        let source = projects.join("zero-rebuild.jsonl");
        std::fs::write(&source, "{\"type\":\"summary\",\"summary\":\"x\"}\n").unwrap();

        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(4)));
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings.clone(),
            search.clone(),
            projects,
        );
        let first = seed_source_chunk(
            &storage,
            &embeddings,
            "zero-rebuild",
            0,
            "Real work one.",
            "conversation",
        );
        let second = seed_source_chunk(
            &storage,
            &embeddings,
            "zero-rebuild",
            1,
            "Real work two.\n\n[[CSR:RECAP]] generated recap",
            "conversation",
        );
        storage
            .upsert_import_state_explicit(
                source.to_string_lossy().as_ref(),
                "zero-rebuild",
                2,
                "fixture",
            )
            .unwrap();

        let dry = super::dry_run_scrub(&storage, Some("zero-rebuild"), false, true).unwrap();
        assert_eq!(dry.refused, 1);
        assert_eq!(dry.conversations_scrubbed, 0);
        assert_eq!(dry.chunks_dropped, 0);
        assert!(dry.actions[0].contains("refused, re-parse would rebuild 0 chunks"));
        assert!(dry.format_text(true).contains("refused: 1"));

        let report = super::run_scrub(&engine, false, Some("zero-rebuild"), false, true)
            .await
            .unwrap();
        assert_eq!(report.refused, 1);
        assert_eq!(report.chunks_dropped, 0);
        assert_eq!(report.chunks_rewritten, 0);
        assert!(storage.get_chunk_content(&first).unwrap().is_some());
        assert_eq!(
            storage.get_chunk_content(&second).unwrap().as_deref(),
            Some("Real work two.\n\n[[CSR:RECAP]] generated recap"),
            "a refused conversation is not written at all"
        );
    }

    fn seed_chunk(
        storage: &Storage,
        embeddings: &EmbeddingEngine,
        conversation: &str,
        content: &str,
    ) -> (String, Vec<f32>) {
        let id = super::super::generate_chunk_id(conversation, 0);
        let vector = embeddings.embed(&[content]).unwrap().remove(0);
        storage
            .insert_chunk_with_source(
                &ConversationChunk {
                    id: id.clone(),
                    conversation_id: conversation.into(),
                    project_name: "project".into(),
                    timestamp: "2026-09-15T00:00:00Z".into(),
                    content: content.into(),
                    message_count: 1,
                    summary: None,
                    author: Speaker::User,
                    seq: 0,
                    is_sidechain: false,
                },
                &vector,
                "conversation",
            )
            .unwrap();
        (id, vector)
    }

    /// Imported while the skip was dead, the agent transcript's chunks no longer
    /// carry its signature (the sanitizer removed that paragraph), so only the
    /// source file identifies it. Everything derived from it goes; a real
    /// session, and a reflection that also cites one, stay.
    #[tokio::test]
    async fn scrub_purges_agent_transcripts_with_their_reflections() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let agent_source = projects.join("agent-child.jsonl");
        std::fs::write(
            &agent_source,
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-09-15T00:00:00Z",
                "message": {"content": "-\n# Ratification Dialog-Act Extraction\n\nUSER: keep the daemon"}
            })
            .to_string(),
        )
        .unwrap();
        let real_source = projects.join("real-session.jsonl");
        std::fs::write(
            &real_source,
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-09-15T00:00:00Z",
                "message": {"content": "Should we keep the daemon?"}
            })
            .to_string(),
        )
        .unwrap();

        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(4)));
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings.clone(),
            search.clone(),
            projects.clone(),
        );
        let (agent_chunk, agent_vector) = seed_chunk(
            &storage,
            &embeddings,
            "agent-child",
            "USER: keep the daemon",
        );
        let (real_chunk, real_vector) = seed_chunk(
            &storage,
            &embeddings,
            "real-session",
            "Should we keep the daemon?",
        );
        // Same id registered twice: an agent transcript on disk plus a source
        // that is gone and may have been a real session. Unreadable evidence
        // never deletes anything.
        let mixed_dir = projects.join("moved");
        std::fs::create_dir_all(&mixed_dir).unwrap();
        let mixed_source = mixed_dir.join("mixed-child.jsonl");
        std::fs::copy(&agent_source, &mixed_source).unwrap();
        let (mixed_chunk, _) = seed_chunk(&storage, &embeddings, "mixed-child", "a real question");
        for source in [
            mixed_source.to_string_lossy().to_string(),
            projects
                .join("gone/mixed-child.jsonl")
                .to_string_lossy()
                .to_string(),
        ] {
            storage
                .upsert_import_state_explicit(&source, "mixed-child", 1, "fixture")
                .unwrap();
        }
        for (source, conversation) in [
            (&agent_source, "agent-child"),
            (&real_source, "real-session"),
        ] {
            storage
                .upsert_import_state_explicit(
                    source.to_string_lossy().as_ref(),
                    conversation,
                    1,
                    "fixture",
                )
                .unwrap();
        }
        let tag = |conversation: &str| format!("conv_{conversation}");
        for (id, tags) in [
            (
                "agent-episode",
                vec![tag("agent-child"), "session_episode".to_string()],
            ),
            ("shared-fact", vec![tag("agent-child"), tag("real-session")]),
            ("cited-by-real", vec![tag("agent-child")]),
            ("real-episode", vec![tag("real-session")]),
        ] {
            storage
                .insert_reflection(id, "reflection body", &tags, &agent_vector)
                .unwrap();
            search
                .write()
                .await
                .insert_reflection(id.to_string(), agent_vector.clone());
        }
        storage
            .mark_enrichment_completed("agent-child", "extracted_v3", "agent-episode")
            .unwrap();
        storage
            .mark_enrichment_completed("real-session", "consolidated_fact", "cited-by-real")
            .unwrap();
        {
            let mut index = search.write().await;
            index.insert_chunk(agent_chunk.clone(), agent_vector);
            index.insert_chunk(real_chunk.clone(), real_vector);
        }

        let dry = super::dry_run_scrub(&storage, None, true, true).unwrap();
        assert_eq!(
            (
                dry.agent_transcripts_purged,
                dry.agent_chunks_purged,
                dry.agent_reflections_purged
            ),
            (1, 1, 1)
        );
        assert!(storage.get_chunk_content(&agent_chunk).unwrap().is_some());

        let report = super::run_scrub(&engine, false, None, true, true)
            .await
            .unwrap();
        assert_eq!(
            (
                report.agent_transcripts_purged,
                report.agent_chunks_purged,
                report.agent_reflections_purged,
                report.conversations_scrubbed
            ),
            (1, 1, 1, 0)
        );
        assert!(storage.get_chunk_content(&agent_chunk).unwrap().is_none());
        assert!(storage.get_chunk_content(&real_chunk).unwrap().is_some());
        assert!(storage.get_chunk_content(&mixed_chunk).unwrap().is_some());
        let remaining = storage
            .reflection_conversation_tags()
            .unwrap()
            .into_iter()
            .map(|(id, _)| id)
            .collect::<HashSet<_>>();
        assert_eq!(
            remaining,
            HashSet::from([
                "shared-fact".to_string(),
                "cited-by-real".to_string(),
                "real-episode".to_string()
            ])
        );
        let enrichment_rows: i64 = storage
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM enrichment_state WHERE conversation_id = 'agent-child'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(enrichment_rows, 0);
        assert_eq!(storage.get_imported_chunk_count(&agent_source).unwrap(), 0);
        let index = search.read().await;
        assert!(!index.has_chunk(&agent_chunk));
        assert!(index.has_chunk(&real_chunk));
        assert!(!index.has_reflection("agent-episode"));
        assert!(index.has_reflection("shared-fact"));
        drop(index);

        // Second run finds nothing: the purge is idempotent.
        let again = super::run_scrub(&engine, false, None, true, true)
            .await
            .unwrap();
        assert_eq!(again.agent_transcripts_purged, 0);
    }

    #[tokio::test]
    async fn busy_database_is_retried_and_other_errors_are_not() {
        let busy = || {
            anyhow::Error::from(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("database is locked".into()),
            ))
            .context("purging")
        };
        assert!(super::is_database_busy(&busy()));
        assert!(!super::is_database_busy(&anyhow::anyhow!("no such table")));

        let mut calls = 0;
        let result = super::retry_busy(|| {
            calls += 1;
            if calls < 3 {
                Err(busy())
            } else {
                Ok(calls)
            }
        })
        .await;
        assert_eq!(result.unwrap(), 3);

        let mut calls = 0;
        let failed: anyhow::Result<()> = super::retry_busy(|| {
            calls += 1;
            Err(anyhow::anyhow!("no such table"))
        })
        .await;
        assert!(failed.is_err());
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn scrub_flush_failure_does_not_refresh_contamination_cache() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let source = projects.join("flush-failure.jsonl");
        std::fs::write(
            &source,
            serde_json::json!({
                "type": "user",
                "timestamp": "2026-09-01T00:00:00Z",
                "message": {"content": "Clean replacement."}
            })
            .to_string(),
        )
        .unwrap();
        let index_dir = projects.join("index");
        std::fs::create_dir_all(&index_dir).unwrap();
        let manifest = index_dir.join("manifest.json");
        std::fs::write(&manifest, "stale manifest").unwrap();
        std::fs::create_dir(index_dir.join("index.lock")).unwrap();

        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(2)));
        let engine = Engine::from_parts(
            storage.clone(),
            embeddings.clone(),
            search.clone(),
            projects,
        );
        let chunk_id = super::super::generate_chunk_id("flush-failure", 0);
        let content = "[[CSR:RECAP]] contaminated old content";
        let vector = embeddings.embed(&[content]).unwrap().remove(0);
        storage
            .insert_chunk_with_source(
                &ConversationChunk {
                    id: chunk_id.clone(),
                    conversation_id: "flush-failure".into(),
                    project_name: "project".into(),
                    timestamp: "2026-09-01T00:00:00Z".into(),
                    content: content.into(),
                    message_count: 1,
                    summary: None,
                    author: Speaker::User,
                    seq: 0,
                    is_sidechain: false,
                },
                &vector,
                "conversation",
            )
            .unwrap();
        storage
            .upsert_import_state_explicit(
                source.to_string_lossy().as_ref(),
                "flush-failure",
                1,
                "fixture",
            )
            .unwrap();
        search.write().await.insert_chunk(chunk_id, vector);

        let error = super::run_scrub(&engine, false, Some("flush-failure"), false, true)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("persisting HNSW index"));
        assert!(
            !manifest.exists(),
            "failed scrub must invalidate stale cache"
        );
        assert!(storage.cached_contamination().unwrap().is_none());
    }
}
