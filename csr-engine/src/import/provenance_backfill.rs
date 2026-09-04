use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::provenance::{content_hash, ChunkEvidence, ChunkSpan, ProvenanceEvent, TrustTier};
use crate::storage::queries::ProvenanceBackfillRow;
use crate::storage::Storage;

pub const DEFAULT_BATCH_SIZE: usize = 2_000;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ProvenanceBackfillStats {
    pub chunks_scanned: usize,
    pub chunks_reconstructed: usize,
    pub chunks_unreconstructible: usize,
    pub chunks_written: usize,
    pub batches: usize,
}

type Reconstructed = HashMap<String, (String, ChunkEvidence)>;

pub fn backfill(
    storage: &Storage,
    projects_dir: &Path,
    batch_size: usize,
    dry_run: bool,
) -> Result<ProvenanceBackfillStats> {
    backfill_limited(storage, projects_dir, batch_size, dry_run, None)
}

pub fn backfill_incremental(
    storage: &Storage,
    projects_dir: &Path,
    batch_size: usize,
) -> Result<ProvenanceBackfillStats> {
    backfill_limited(storage, projects_dir, batch_size, false, Some(1))
}

fn backfill_limited(
    storage: &Storage,
    projects_dir: &Path,
    batch_size: usize,
    dry_run: bool,
    max_batches: Option<usize>,
) -> Result<ProvenanceBackfillStats> {
    anyhow::ensure!(batch_size > 0, "provenance backfill batch must be positive");
    let mut stats = ProvenanceBackfillStats::default();
    let mut after_id: Option<String> = None;
    loop {
        let rows = storage.list_chunks_missing_spans(after_id.as_deref(), batch_size)?;
        if rows.is_empty() {
            break;
        }
        stats.batches += 1;
        let mut cache: HashMap<PathBuf, Option<Reconstructed>> = HashMap::new();
        let mut writes = Vec::with_capacity(rows.len());
        for row in &rows {
            stats.chunks_scanned += 1;
            let receipt_path =
                source_path(projects_dir, row).map(|path| path.to_string_lossy().into_owned());
            let reconstructed = reconstruct_chunk(storage, projects_dir, row, &mut cache);
            let evidence = match reconstructed {
                Some(evidence) => {
                    stats.chunks_reconstructed += 1;
                    evidence
                }
                None => {
                    stats.chunks_unreconstructible += 1;
                    unreconstructible_evidence(row, receipt_path)
                }
            };
            writes.push(evidence);
        }
        if !dry_run {
            storage.replace_chunk_evidence_batch(&writes)?;
            stats.chunks_written += writes.len();
        }
        after_id = rows.last().map(|row| row.id.clone());
        eprintln!(
            "provenance backfill: scanned={} reconstructed={} unknown={} written={}",
            stats.chunks_scanned,
            stats.chunks_reconstructed,
            stats.chunks_unreconstructible,
            stats.chunks_written,
        );
        if max_batches.is_some_and(|limit| stats.batches >= limit) {
            break;
        }
    }
    Ok(stats)
}

fn reconstruct_chunk(
    storage: &Storage,
    projects_dir: &Path,
    row: &ProvenanceBackfillRow,
    cache: &mut HashMap<PathBuf, Option<Reconstructed>>,
) -> Option<ChunkEvidence> {
    let path = source_path(projects_dir, row)?;
    if !cache.contains_key(&path) {
        let reconstructed = reconstruct_file(storage, projects_dir, row, &path).ok();
        cache.insert(path.clone(), reconstructed);
    }
    let (content, evidence) = cache.get(&path)?.as_ref()?.get(&row.id)?;
    (content == &row.content).then(|| evidence.clone())
}

fn source_path(projects_dir: &Path, row: &ProvenanceBackfillRow) -> Option<PathBuf> {
    if row.source == "plan" || row.conversation_id.starts_with("plan:") {
        let slug = row.conversation_id.strip_prefix("plan:")?;
        return Some(
            projects_dir
                .parent()
                .unwrap_or(projects_dir)
                .join("plans")
                .join(format!("{slug}.md")),
        );
    }
    row.source_path.as_deref().map(PathBuf::from)
}

fn reconstruct_file(
    storage: &Storage,
    projects_dir: &Path,
    row: &ProvenanceBackfillRow,
    path: &Path,
) -> Result<Reconstructed> {
    anyhow::ensure!(path.is_file(), "source file is unavailable");
    if row.source == "plan" || row.conversation_id.starts_with("plan:") {
        return super::plans::reconstruct_plan_evidence(
            path,
            &row.conversation_id,
            &row.project_name,
            &row.timestamp,
        );
    }
    if row.source == "codex_rollout" {
        return super::codex_rollout::reconstruct_rollout_evidence(path);
    }

    let parent = if row.is_sidechain {
        let attribution = super::derive_conversation_attribution(projects_dir, path);
        if let Some(parent_id) = attribution.parent_conversation_id {
            let parent_key = super::sidechain_parent_message_key(path);
            Some(storage.parent_provenance_context(&parent_id, parent_key.as_deref())?)
        } else {
            Some(super::ParentContext {
                floor: TrustTier::Unknown,
                event_id: None,
            })
        }
    } else {
        None
    };
    let parsed =
        super::parse_jsonl_file_with_stats_and_parent(path, &row.project_name, parent.as_ref())?;
    Ok(parsed
        .chunks
        .into_iter()
        .filter_map(|chunk| {
            parsed
                .evidence
                .get(&chunk.id)
                .cloned()
                .map(|evidence| (chunk.id, (chunk.content, evidence)))
        })
        .collect())
}

fn unreconstructible_evidence(
    row: &ProvenanceBackfillRow,
    receipt_path: Option<String>,
) -> ChunkEvidence {
    let message_key = content_hash(&row.content);
    let event_id = blake3::hash(
        format!(
            "unreconstructible\0{}\0{}\0{}",
            row.conversation_id, row.id, message_key
        )
        .as_bytes(),
    )
    .to_hex()
    .to_string();
    ChunkEvidence {
        chunk_id: row.id.clone(),
        events: vec![ProvenanceEvent {
            event_id: event_id.clone(),
            conversation_id: row.conversation_id.clone(),
            message_key,
            seq: 0,
            channel: "unclassified".into(),
            trust_tier: TrustTier::Unknown,
            parent_event_id: None,
            receipt_kind: "unreconstructible".into(),
            receipt_ref: receipt_path,
            observed_at: row.timestamp.clone(),
        }],
        spans: vec![ChunkSpan {
            chunk_id: row.id.clone(),
            event_id,
            start_char: 0,
            end_char: row.content.chars().count(),
            content_hash: content_hash(&row.content),
        }],
        min_trust: TrustTier::Unknown,
        tool_result_share: None,
    }
}

pub(crate) fn unreconstructible_chunk_evidence(
    chunk: &super::ConversationChunk,
    source_path: Option<String>,
) -> ChunkEvidence {
    unreconstructible_evidence(
        &ProvenanceBackfillRow {
            id: chunk.id.clone(),
            conversation_id: chunk.conversation_id.clone(),
            project_name: chunk.project_name.clone(),
            timestamp: chunk.timestamp.clone(),
            content: chunk.content.clone(),
            source: "conversation".into(),
            is_sidechain: chunk.is_sidechain,
            source_path: source_path.clone(),
        },
        source_path,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn structural_backfill_is_resumable_and_second_run_is_noop() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("-Users-test-projects-demo");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join("session-1.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"type":"user","uuid":"message-1","timestamp":"2026-09-04T00:00:00Z","message":{{"role":"user","content":"remember this"}}}}"#
        )
        .unwrap();

        let parsed = super::super::parse_jsonl_file_with_stats(&path, "demo").unwrap();
        let storage = Storage::open_memory().unwrap();
        for chunk in &parsed.chunks {
            storage.insert_chunk(chunk, &[0.0, 1.0]).unwrap();
        }
        storage
            .mark_file_imported_with_suppression(&path, parsed.chunks.len(), Default::default())
            .unwrap();

        let dry_run = backfill(&storage, root.path(), 1, true).unwrap();
        assert_eq!(dry_run.chunks_scanned, parsed.chunks.len());
        assert_eq!(dry_run.chunks_written, 0);
        assert_eq!(storage.provenance_coverage().unwrap().0, 0);

        let first = backfill(&storage, root.path(), 1, false).unwrap();
        assert_eq!(first.chunks_reconstructed, parsed.chunks.len());
        assert_eq!(first.chunks_unreconstructible, 0);
        assert_eq!(first.chunks_written, parsed.chunks.len());
        assert_eq!(
            storage.get_chunk_min_trust(&parsed.chunks[0].id).unwrap(),
            TrustTier::UserHistory
        );

        let second = backfill(&storage, root.path(), 1, false).unwrap();
        assert_eq!(second, ProvenanceBackfillStats::default());
    }

    #[test]
    fn missing_source_is_counted_and_remains_unknown() {
        let storage = Storage::open_memory().unwrap();
        let chunk = crate::import::ConversationChunk {
            id: "missing-source".into(),
            conversation_id: "missing-conv".into(),
            project_name: "demo".into(),
            timestamp: "2026-09-04T00:00:00Z".into(),
            content: "orphaned content".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[0.0, 1.0]).unwrap();

        let stats = backfill(&storage, Path::new("/nonexistent"), 2_000, false).unwrap();
        assert_eq!(stats.chunks_unreconstructible, 1);
        assert_eq!(
            storage.get_chunk_min_trust(&chunk.id).unwrap(),
            TrustTier::Unknown
        );
        let receipt: String = storage
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT receipt_kind FROM provenance_events WHERE conversation_id='missing-conv'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(receipt, "unreconstructible");
        assert_eq!(storage.provenance_coverage().unwrap().3, None);
    }
}
