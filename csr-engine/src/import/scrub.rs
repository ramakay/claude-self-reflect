//! `backfill scrub`, as a plan and nothing else.
//!
//! CSR text in the conversation record is detected and labeled (search,
//! status, hook injection); it is never deleted or rewritten, because a pattern
//! cannot prove who typed a line. So in this release scrub deletes nothing: the
//! dry run still reads the stored conversations and reports what a re-parse of
//! their source transcripts would change, and a real run refuses with
//! [`SCRUB_REFUSAL`]. The agent-transcript purge is a deletion too, and refuses
//! the same way.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use crate::engine::Engine;
use crate::import::{
    classify_transcript, parse_jsonl_file_with_stats_and_parent, ConversationChunk, ParentContext,
    TranscriptKind,
};
use crate::provenance::TrustTier;
use crate::storage::Storage;

/// Why a real scrub does not run, in the words the command prints.
pub const SCRUB_REFUSAL: &str =
    "scrub deletes nothing in this release: CSR text is labeled in search instead";

/// A conversation whose source file or newest chunk changed this recently may
/// still have a live session writing it. The plan leaves such a conversation
/// out (`--include-active` overrides).
pub const ACTIVE_WINDOW: Duration = Duration::from_secs(15 * 60);

/// What a dry run found. Every count is a plan; nothing here was written.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScrubReport {
    /// Conversations holding CSR text the detector labels, whose source
    /// transcript was re-parsed.
    pub conversations_planned: usize,
    /// Chunks a re-parse would write differently than they are stored.
    pub chunks_differing: usize,
    /// Chunks a re-parse would no longer produce.
    pub chunks_missing_from_reparse: usize,
    /// Conversations whose re-parse would have rebuilt nothing, or lost most of
    /// what is stored: a parser mismatch, not a plan.
    pub refused: usize,
    /// Conversations with no readable single source transcript to re-parse.
    pub source_unavailable: usize,
    /// Conversations left out because they were active inside [`ACTIVE_WINDOW`].
    pub deferred_active: usize,
    /// The preflight's findings: other processes holding the database open.
    pub preflight: Vec<String>,
    /// Conversations a purge would remove whole because their transcript is a
    /// CSR agent prompt (structural, by first prompt). Not purged.
    pub agent_transcripts: usize,
    pub agent_chunks: usize,
    pub agent_reflections: usize,
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
            "{} summary (a plan; nothing was written)\nconversations planned: {}\nchunks a re-parse would write differently: {}\nchunks a re-parse would not produce: {}\nrefused: {}\nsource unavailable: {}\ndeferred (active): {}\nagent transcripts a purge would remove: {} ({} chunks, {} reflections)\n{SCRUB_REFUSAL}\n",
            if dry_run { "scrub dry-run" } else { "scrub" },
            self.conversations_planned,
            self.chunks_differing,
            self.chunks_missing_from_reparse,
            self.refused,
            self.source_unavailable,
            self.deferred_active,
            self.agent_transcripts,
            self.agent_chunks,
            self.agent_reflections,
        ));
        output
    }
}

#[derive(Debug)]
enum ScrubPlanKind {
    /// The re-parse yields the stored chunk ids; `changed` of them differ.
    Stable {
        source_path: PathBuf,
        changed: usize,
    },
    /// The re-parse yields other ids: `rebuilt` chunks, `missing` stored ones
    /// it does not produce.
    Shifted {
        source_path: PathBuf,
        rebuilt: usize,
        missing: usize,
    },
    /// No readable single source transcript: why is in `reason`.
    Missing { reason: String },
    /// A re-parse that would rebuild nothing, or drop most of what is stored,
    /// is a parser mismatch rather than a plan.
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

/// `Some(reason)` when a re-parse is not a plan: it rebuilds zero chunks, or
/// drops more than half of the conversation's stored chunks.
fn reparse_refusal(
    old_chunks: &[ConversationChunk],
    new_chunks: &[ConversationChunk],
    source_path: &Path,
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
            "refused, re-parse would rebuild {} chunks and lose {} of {} stored from {}",
            new_chunks.len(),
            dropped,
            old_chunks.len(),
            source_path.display()
        ))
    } else {
        None
    }
}

#[derive(Debug)]
struct ScrubPlan {
    conversation_id: String,
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
    storage.with_connection(|conn| {
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
    })
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
    let paths = source_paths(storage, conversation_id)?;
    let claude = is_claude_transcript_conversation(conversation_id, &sources);
    let kind = match paths.as_slice() {
        // More than one transcript recorded for one id: the one picked may be a
        // partial copy, so no re-parse is trusted to say what the rest held.
        [_, _, ..] if claude => ScrubPlanKind::Missing {
            reason: format!("{} source paths registered", paths.len()),
        },
        [path] if claude && path.exists() => {
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
            let parsed = parse_jsonl_file_with_stats_and_parent(path, project, parent.as_ref())
                .with_context(|| format!("re-parsing {}", path.display()))?;
            let chunks = parsed.chunks;
            let new_ids = chunks
                .iter()
                .map(|chunk| chunk.id.clone())
                .collect::<Vec<_>>();
            if let Some(reason) = reparse_refusal(&old_chunks, &chunks, path) {
                ScrubPlanKind::Refused { reason }
            } else if old_ids == new_ids {
                let old_content = old_chunks
                    .iter()
                    .map(|chunk| (chunk.id.as_str(), chunk.content.as_str()))
                    .collect::<HashMap<_, _>>();
                let changed = chunks
                    .iter()
                    .filter(|chunk| {
                        old_content.get(chunk.id.as_str()) != Some(&chunk.content.as_str())
                    })
                    .count();
                ScrubPlanKind::Stable {
                    source_path: path.clone(),
                    changed,
                }
            } else {
                let kept = new_ids.iter().collect::<HashSet<_>>();
                ScrubPlanKind::Shifted {
                    source_path: path.clone(),
                    rebuilt: chunks.len(),
                    missing: old_ids.iter().filter(|id| !kept.contains(id)).count(),
                }
            }
        }
        _ => ScrubPlanKind::Missing {
            reason: "source missing or not a Claude transcript".to_string(),
        },
    };
    Ok(ScrubPlan {
        conversation_id: conversation_id.to_string(),
        kind,
    })
}

fn summarize_plan(plan: &ScrubPlan, report: &mut ScrubReport) {
    let id = &plan.conversation_id;
    match &plan.kind {
        ScrubPlanKind::Refused { reason } => {
            report.refused += 1;
            report.actions.push(format!("{id}: {reason}"));
        }
        ScrubPlanKind::Missing { reason } => {
            report.source_unavailable += 1;
            report
                .actions
                .push(format!("{id}: no re-parse ({reason}); stored text kept"));
        }
        ScrubPlanKind::Stable {
            source_path,
            changed,
        } => {
            report.conversations_planned += 1;
            report.chunks_differing += changed;
            report.actions.push(format!(
                "{id}: a re-parse of {} writes {changed} chunks differently (not applied)",
                source_path.display()
            ));
        }
        ScrubPlanKind::Shifted {
            source_path,
            rebuilt,
            missing,
        } => {
            report.conversations_planned += 1;
            report.chunks_differing += rebuilt;
            report.chunks_missing_from_reparse += missing;
            report.actions.push(format!(
                "{id}: a re-parse of {} yields other chunk ids: {rebuilt} rebuilt, {missing} stored not produced (not applied)",
                source_path.display()
            ));
        }
    }
}

/// A conversation the importer skips whole: every registered source transcript
/// opens with a CSR agent prompt (the first-prompt signature), so nothing
/// derived from it is corpus. Known limit: it is identified by that one
/// signature.
#[derive(Debug)]
struct AgentTranscript {
    conversation_id: String,
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
            Some(last) if last.conversation_id == conversation_id => {}
            _ => agents.push(AgentTranscript {
                conversation_id,
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

    // A reflection counts when every conversation it is tagged with is an agent
    // transcript and no conversation outside THIS run's set cites it.
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
    report.agent_transcripts += 1;
    report.agent_chunks += agent.chunks;
    report.agent_reflections += agent.reflection_ids.len();
    report.actions.push(format!(
        "{}: agent transcript, a purge would remove {} chunks and {} reflections (not applied)",
        agent.conversation_id,
        agent.chunks,
        agent.reflection_ids.len()
    ));
}

/// Shared by every writer that has to survive another csr-engine process
/// holding the SQLite write lock (Codex rollout import, the watcher).
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

/// Plans for every conversation holding labeled CSR text that `conversation`
/// selects, minus the ones in `skip` and, unless `include_active`, the ones a
/// live session may still be writing ([`is_active`]).
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

/// The plan: read-only. Opens no writer and changes nothing.
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
    let listed = agents
        .iter()
        .map(|agent| agent.conversation_id.clone())
        .collect::<HashSet<_>>();
    for plan in selected_plans(storage, conversation, include_active, &listed, &mut report)? {
        summarize_plan(&plan, &mut report);
    }
    Ok(report)
}

/// A dry run is the plan above. A real run refuses before it reads or opens
/// anything for writing: scrub deletes nothing in this release.
pub async fn run_scrub(
    engine: &Engine,
    dry_run: bool,
    conversation: Option<&str>,
    agent_transcripts_only: bool,
    include_active: bool,
) -> Result<ScrubReport> {
    if !dry_run {
        anyhow::bail!(SCRUB_REFUSAL);
    }
    dry_run_scrub(
        engine.storage(),
        conversation,
        agent_transcripts_only,
        include_active,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::RwLock;

    use super::*;
    use crate::embeddings::EmbeddingEngine;
    use crate::provenance::Speaker;
    use crate::search::SearchEngine;

    const RECAP: &str = "recap [2h ago]: generated recap. [[CSR:RECAP]]";

    fn user_line(text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "timestamp": "2026-09-01T00:00:00Z",
            "message": {"content": text}
        })
        .to_string()
    }

    fn seed(storage: &Storage, conversation: &str, seq: usize, content: &str) -> String {
        let id = super::super::generate_chunk_id(conversation, seq);
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
                &[0.1; 384],
                "conversation",
            )
            .unwrap();
        id
    }

    fn register_source(storage: &Storage, path: &Path, conversation: &str) {
        storage
            .upsert_import_state_explicit(
                path.to_string_lossy().as_ref(),
                conversation,
                1,
                "fixture",
            )
            .unwrap();
    }

    fn all_text(storage: &Storage) -> Vec<(String, String)> {
        storage
            .with_connection(|conn| {
                let mut statement = conn.prepare("SELECT id, content FROM chunks ORDER BY id")?;
                let rows = statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .unwrap()
    }

    /// Every destructive path refuses: a real run errors with the refusal and
    /// the stored conversation is exactly as it was.
    #[tokio::test]
    async fn a_real_scrub_refuses_and_deletes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let storage = Arc::new(Storage::open_memory().unwrap());
        let engine = Engine::from_parts(
            storage.clone(),
            Arc::new(EmbeddingEngine::new().unwrap()),
            Arc::new(RwLock::new(SearchEngine::new(4))),
            projects.clone(),
        );
        // CSR text in the record, and an agent transcript the old scrub purged.
        seed(
            &storage,
            "labeled",
            0,
            &format!("Keep this fact.\n\n{RECAP}"),
        );
        let agent = projects.join("agent-child.jsonl");
        std::fs::write(
            &agent,
            user_line("-\n# Ratification Dialog-Act Extraction\n\nUSER: keep the daemon"),
        )
        .unwrap();
        seed(&storage, "agent-child", 0, "agent residue");
        register_source(&storage, &agent, "agent-child");
        let before = all_text(&storage);

        for (conversation, agents_only, include_active) in [
            (None, false, false),
            (None, false, true),
            (Some("labeled"), false, true),
            (None, true, true),
        ] {
            let error = run_scrub(&engine, false, conversation, agents_only, include_active)
                .await
                .unwrap_err();
            assert_eq!(error.to_string(), SCRUB_REFUSAL);
        }
        assert_eq!(all_text(&storage), before, "scrub deletes nothing");
    }

    /// The dry run keeps its plan: it re-parses the source and says what would
    /// change, and writes nothing.
    #[test]
    fn the_dry_run_reports_a_plan_and_writes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("labeled.jsonl");
        // The record holds a wrapper the importer cuts and CSR text it keeps.
        let wrapped = format!(
            "Keep this fact.\n\n{RECAP}\n<system-reminder>CSR PICKUP — x</system-reminder>\nand this."
        );
        std::fs::write(&source, user_line(&wrapped)).unwrap();
        let storage = Storage::open_memory().unwrap();
        seed(&storage, "labeled", 0, &wrapped);
        register_source(&storage, &source, "labeled");
        let before = all_text(&storage);

        let report = dry_run_scrub(&storage, None, false, true).unwrap();
        assert_eq!(report.conversations_planned, 1, "{:?}", report.actions);
        assert_eq!(report.chunks_differing, 1);
        let text = report.format_text(true);
        assert!(text.contains("a plan; nothing was written"), "{text}");
        assert!(text.contains(SCRUB_REFUSAL), "{text}");
        assert_eq!(all_text(&storage), before);
    }

    #[test]
    fn a_clean_reparse_plans_no_change() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("same.jsonl");
        let text = format!("Keep this fact.\n\n{RECAP}");
        std::fs::write(&source, user_line(&text)).unwrap();
        let storage = Storage::open_memory().unwrap();
        seed(&storage, "same", 0, &text);
        register_source(&storage, &source, "same");
        let report = dry_run_scrub(&storage, None, false, true).unwrap();
        // The importer keeps CSR text: the re-parse is the stored chunk.
        assert_eq!(
            (report.conversations_planned, report.chunks_differing),
            (1, 0)
        );
        assert!(
            report.actions[0].contains("writes 0 chunks differently"),
            "{:?}",
            report.actions
        );
    }

    #[test]
    fn a_missing_source_is_reported_and_its_text_kept() {
        let storage = Storage::open_memory().unwrap();
        seed(&storage, "gone", 0, RECAP);
        let before = all_text(&storage);
        let report = dry_run_scrub(&storage, None, false, true).unwrap();
        assert_eq!(report.source_unavailable, 1);
        assert!(
            report.actions[0].contains("stored text kept"),
            "{:?}",
            report.actions
        );
        assert_eq!(all_text(&storage), before);
    }

    #[test]
    fn a_reparse_that_would_rebuild_zero_chunks_is_a_refused_plan() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("empty.jsonl");
        std::fs::write(&source, "").unwrap();
        let storage = Storage::open_memory().unwrap();
        seed(&storage, "empty", 0, RECAP);
        register_source(&storage, &source, "empty");
        let report = dry_run_scrub(&storage, None, false, true).unwrap();
        assert_eq!(report.refused, 1);
        assert!(
            report.actions[0].contains("refused"),
            "{:?}",
            report.actions
        );
    }

    #[test]
    fn an_active_conversation_is_left_out_of_the_plan_unless_included() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("live.jsonl");
        // Written just now: a live session may still be appending to it.
        std::fs::write(&source, user_line(RECAP)).unwrap();
        let storage = Storage::open_memory().unwrap();
        seed(&storage, "live", 0, RECAP);
        register_source(&storage, &source, "live");
        let report = dry_run_scrub(&storage, None, false, false).unwrap();
        assert_eq!(
            (report.deferred_active, report.conversations_planned),
            (1, 0)
        );
        let report = dry_run_scrub(&storage, None, false, true).unwrap();
        assert_eq!(
            (report.deferred_active, report.conversations_planned),
            (0, 1)
        );
    }

    #[test]
    fn the_dry_run_lists_agent_transcripts_it_would_purge_and_purges_none() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("agent-child.jsonl");
        std::fs::write(
            &source,
            user_line("-\n# Ratification Dialog-Act Extraction\n\nUSER: keep the daemon"),
        )
        .unwrap();
        let storage = Storage::open_memory().unwrap();
        seed(&storage, "agent-child", 0, "agent residue");
        register_source(&storage, &source, "agent-child");
        let before = all_text(&storage);
        let report = dry_run_scrub(&storage, None, true, true).unwrap();
        assert_eq!((report.agent_transcripts, report.agent_chunks), (1, 1));
        assert!(
            report.actions[0].contains("not applied"),
            "{:?}",
            report.actions
        );
        assert_eq!(all_text(&storage), before);
    }

    /// More than one transcript recorded for one id: no re-parse is trusted to
    /// say what the rest held, so the plan says so and the text stays.
    #[test]
    fn a_conversation_with_several_registered_sources_gets_no_reparse() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::open_memory().unwrap();
        seed(&storage, "twice", 0, RECAP);
        for name in ["twice-a.jsonl", "twice-b.jsonl"] {
            let source = temp.path().join(name);
            std::fs::write(&source, user_line(RECAP)).unwrap();
            register_source(&storage, &source, "twice");
        }
        let before = all_text(&storage);
        let report = dry_run_scrub(&storage, None, false, true).unwrap();
        assert_eq!(
            (report.source_unavailable, report.conversations_planned),
            (1, 0)
        );
        assert!(
            report.actions[0].contains("2 source paths registered"),
            "{:?}",
            report.actions
        );
        assert_eq!(all_text(&storage), before);
    }

    /// A conversation whose chunks include another adapter's source (a plan, a
    /// Codex rollout) is never re-parsed as a Claude transcript, even with a
    /// readable file registered; the conversation beside it is planned as usual.
    #[test]
    fn another_sources_chunks_are_never_reparsed_as_a_claude_transcript() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::open_memory().unwrap();
        let text = format!("Keep this fact.\n\n{RECAP}");

        let claude = temp.path().join("claude.jsonl");
        std::fs::write(&claude, user_line(&text)).unwrap();
        seed(&storage, "claude", 0, &text);
        register_source(&storage, &claude, "claude");

        let mixed = temp.path().join("mixed.jsonl");
        std::fs::write(&mixed, user_line(&text)).unwrap();
        seed(&storage, "mixed", 0, &text);
        storage
            .insert_chunk_with_source(
                &ConversationChunk {
                    id: super::super::generate_chunk_id("mixed", 1),
                    conversation_id: "mixed".into(),
                    project_name: "project".into(),
                    timestamp: "2026-09-01T00:00:00Z".into(),
                    content: "a plan document".into(),
                    message_count: 1,
                    summary: None,
                    author: Speaker::User,
                    seq: 1,
                    is_sidechain: false,
                },
                &[0.1; 384],
                "plan",
            )
            .unwrap();
        register_source(&storage, &mixed, "mixed");

        let before = all_text(&storage);
        let report = dry_run_scrub(&storage, None, false, true).unwrap();
        assert_eq!(
            (report.conversations_planned, report.source_unavailable),
            (1, 1),
            "{:?}",
            report.actions
        );
        let mixed_line = report
            .actions
            .iter()
            .find(|line| line.starts_with("mixed:"))
            .expect("the mixed conversation is reported");
        assert!(
            mixed_line.contains("not a Claude transcript"),
            "{mixed_line}"
        );
        assert!(report
            .actions
            .iter()
            .any(|line| line.starts_with("claude:") && line.contains("writes 0 chunks")));
        assert_eq!(all_text(&storage), before);
    }

    /// Only reflections that belong to the agent transcripts alone are counted:
    /// one shared with a real conversation, and one a kept conversation's
    /// enrichment cites, are not.
    #[test]
    fn agent_reflections_shared_with_or_cited_by_a_kept_conversation_are_not_counted() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("agent-a.jsonl");
        std::fs::write(
            &source,
            user_line("-\n# Ratification Dialog-Act Extraction\n\nUSER: keep the daemon"),
        )
        .unwrap();
        let storage = Storage::open_memory().unwrap();
        seed(&storage, "agent-a", 0, "agent residue");
        register_source(&storage, &source, "agent-a");
        let reflect = |id: &str, tags: &[&str]| {
            let tags: Vec<String> = tags.iter().map(|tag| tag.to_string()).collect();
            storage
                .insert_reflection(id, &format!("reflection {id}"), &tags, &[0.1; 384])
                .unwrap();
        };
        reflect("own", &["conv_agent-a"]);
        reflect("shared", &["conv_agent-a", "conv_real-session"]);
        reflect("cited", &["conv_agent-a"]);
        // a kept conversation's enrichment cites "cited"
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO enrichment_state (conversation_id, enrichment_type, status, reflection_id)
                     VALUES ('real-session', 'heuristic', 'done', 'cited')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let before = all_text(&storage);

        let report = dry_run_scrub(&storage, None, true, true).unwrap();
        assert_eq!(
            (
                report.agent_transcripts,
                report.agent_chunks,
                report.agent_reflections
            ),
            (1, 1, 1),
            "{:?}",
            report.actions
        );
        assert_eq!(all_text(&storage), before);
        // the reflections themselves are all still there
        for id in ["own", "shared", "cited"] {
            assert!(storage.get_reflection_by_id(id).unwrap().is_some(), "{id}");
        }
    }

    #[test]
    fn busy_database_errors_are_recognised_and_other_errors_are_not() {
        let busy = anyhow::Error::from(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".into()),
        ))
        .context("importing");
        assert!(is_database_busy(&busy));
        assert!(!is_database_busy(&anyhow::anyhow!("no such table")));
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
}
