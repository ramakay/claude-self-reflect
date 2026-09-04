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

type Reconstructed = super::provenance_matcher::MessageIndex;

pub fn backfill(
    storage: &Storage,
    projects_dir: &Path,
    batch_size: usize,
    dry_run: bool,
) -> Result<ProvenanceBackfillStats> {
    backfill_limited(
        storage,
        projects_dir,
        batch_size,
        dry_run,
        None,
        Retry::Never,
    )
}

pub fn backfill_incremental(
    storage: &Storage,
    projects_dir: &Path,
    batch_size: usize,
) -> Result<ProvenanceBackfillStats> {
    backfill_limited(
        storage,
        projects_dir,
        batch_size,
        false,
        Some(1),
        Retry::NewerSource,
    )
}

pub fn backfill_with_retry(
    storage: &Storage,
    projects_dir: &Path,
    batch_size: usize,
    dry_run: bool,
    retry_unknown: bool,
) -> Result<ProvenanceBackfillStats> {
    backfill_limited(
        storage,
        projects_dir,
        batch_size,
        dry_run,
        None,
        if retry_unknown {
            Retry::All
        } else {
            Retry::Never
        },
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Retry {
    Never,
    All,
    NewerSource,
}

fn backfill_limited(
    storage: &Storage,
    projects_dir: &Path,
    batch_size: usize,
    dry_run: bool,
    max_batches: Option<usize>,
    retry: Retry,
) -> Result<ProvenanceBackfillStats> {
    anyhow::ensure!(batch_size > 0, "provenance backfill batch must be positive");
    let mut stats = ProvenanceBackfillStats::default();
    let mut after: Option<(String, String)> = None;
    loop {
        let rows = storage.list_provenance_backfill_candidates(
            after.as_ref().map(|(c, i)| (c.as_str(), i.as_str())),
            batch_size,
            retry != Retry::Never,
        )?;
        if rows.is_empty() {
            break;
        }
        after = rows
            .last()
            .map(|row| (row.conversation_id.clone(), row.id.clone()));
        let rows = rows
            .into_iter()
            .filter(|row| {
                retry != Retry::NewerSource
                    || row.failure_observed_at.is_none()
                    || source_is_newer(projects_dir, row)
            })
            .collect::<Vec<_>>();
        if rows.is_empty() {
            continue;
        }
        stats.batches += 1;
        let mut cache: HashMap<PathBuf, std::result::Result<Reconstructed, &'static str>> =
            HashMap::new();
        let mut writes = Vec::with_capacity(rows.len());
        for row in &rows {
            stats.chunks_scanned += 1;
            let receipt_path =
                source_path(projects_dir, row).map(|path| path.to_string_lossy().into_owned());
            let reconstructed = reconstruct_chunk(storage, projects_dir, row, &rows, &mut cache);
            let evidence = match reconstructed {
                Ok(evidence) => {
                    stats.chunks_reconstructed += 1;
                    evidence
                }
                Err(reason) => {
                    stats.chunks_unreconstructible += 1;
                    failure_evidence(row, receipt_path, reason)
                }
            };
            writes.push(evidence);
        }
        if !dry_run {
            stats.chunks_written += storage.replace_backfill_evidence_batch(&writes)?;
        }
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

fn source_is_newer(projects_dir: &Path, row: &ProvenanceBackfillRow) -> bool {
    source_path(projects_dir, row)
        .as_deref()
        .is_some_and(|p| path_is_newer(p, row.failure_observed_at.as_deref()))
}

fn path_is_newer(path: &Path, observed: Option<&str>) -> bool {
    let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else {
        return false;
    };
    let modified: chrono::DateTime<chrono::Utc> = modified.into();
    observed
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .is_none_or(|seen| modified > seen)
}

fn reconstruct_chunk(
    storage: &Storage,
    projects_dir: &Path,
    row: &ProvenanceBackfillRow,
    rows: &[ProvenanceBackfillRow],
    cache: &mut HashMap<PathBuf, std::result::Result<Reconstructed, &'static str>>,
) -> std::result::Result<ChunkEvidence, &'static str> {
    let path = source_path(projects_dir, row).ok_or("source_missing")?;
    if !cache.contains_key(&path) {
        let needles = rows
            .iter()
            .filter(|r| source_path(projects_dir, r).as_ref() == Some(&path))
            .map(|r| r.content.as_str())
            .collect::<Vec<_>>();
        let reconstructed = match std::fs::metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err("source_missing"),
            Err(_) => Err("source_unparsed"),
            Ok(_) => reconstruct_file(storage, projects_dir, row, &path, &needles)
                .map_err(|_| "source_unparsed"),
        };
        cache.insert(path.clone(), reconstructed);
    }
    cache
        .get(&path)
        .expect("inserted source")
        .as_ref()
        .map_err(|e| *e)?
        .locate(&row.id, &row.content)
        .ok_or("source_unmatched")
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
    row.source_path
        .as_deref()
        .map(|p| PathBuf::from(p.split_once("#byte=").map_or(p, |(path, _)| path)))
}

fn reconstruct_file(
    storage: &Storage,
    projects_dir: &Path,
    row: &ProvenanceBackfillRow,
    path: &Path,
    needles: &[&str],
) -> Result<Reconstructed> {
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
    Reconstructed::read(
        path,
        &row.conversation_id,
        row.source == "codex_rollout",
        row.source == "plan" || row.conversation_id.starts_with("plan:"),
        parent.as_ref(),
        needles,
    )
}

fn failure_evidence(
    row: &ProvenanceBackfillRow,
    receipt_path: Option<String>,
    reason: &str,
) -> ChunkEvidence {
    let message_key = content_hash(&row.content);
    let event_id = blake3::hash(
        format!(
            "{reason}\0{}\0{}\0{}\0{}",
            row.conversation_id,
            row.id,
            message_key,
            receipt_path.as_deref().unwrap_or("")
        )
        .as_bytes(),
    )
    .to_hex()
    .to_string();
    let observed_at = if receipt_path
        .as_deref()
        .is_some_and(|p| path_is_newer(Path::new(p), row.failure_observed_at.as_deref()))
    {
        chrono::Utc::now().to_rfc3339()
    } else {
        row.failure_observed_at
            .clone()
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
    };
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
            receipt_kind: reason.into(),
            receipt_ref: receipt_path,
            observed_at,
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
    failure_evidence(
        &ProvenanceBackfillRow {
            id: chunk.id.clone(),
            conversation_id: chunk.conversation_id.clone(),
            project_name: chunk.project_name.clone(),
            timestamp: chunk.timestamp.clone(),
            content: chunk.content.clone(),
            source: "conversation".into(),
            is_sidechain: chunk.is_sidechain,
            source_path: source_path.clone(),
            failure_observed_at: None,
        },
        source_path,
        "source_unmatched",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(
        messages: &[serde_json::Value],
        contents: &[&str],
    ) -> (tempfile::TempDir, Storage, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        for message in messages {
            writeln!(file, "{message}").unwrap();
        }
        let storage = Storage::open_memory().unwrap();
        for (index, content) in contents.iter().enumerate() {
            storage
                .insert_chunk(
                    &crate::import::ConversationChunk {
                        id: format!("old-{index}"),
                        conversation_id: "session".into(),
                        project_name: "demo".into(),
                        timestamp: "2026-09-04T00:00:00Z".into(),
                        content: (*content).into(),
                        message_count: 1,
                        summary: None,
                        author: crate::provenance::Speaker::User,
                        seq: index,
                        is_sidechain: false,
                    },
                    &[0.0, 1.0],
                )
                .unwrap();
        }
        storage
            .mark_file_imported_with_suppression(&path, contents.len(), Default::default())
            .unwrap();
        (root, storage, path)
    }

    fn receipts(storage: &Storage, id: &str) -> Vec<(String, String, usize, usize)> {
        storage.with_connection(|conn| {
            let mut stmt = conn.prepare("SELECT e.receipt_kind, e.message_key, s.start_char, s.end_char FROM chunk_spans s JOIN provenance_events e USING(event_id) WHERE s.chunk_id=?1 ORDER BY e.seq,s.start_char")?;
            let rows = stmt.query_map([id], |r| Ok((r.get(0)?,r.get(1)?,r.get::<_,i64>(2)? as usize,r.get::<_,i64>(3)? as usize)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).unwrap()
    }

    #[test]
    fn historical_boundaries_match_message_coordinates_not_chunk_ids() {
        let (root, storage, _) = fixture(
            &[
                serde_json::json!({"type":"user","uuid":"u1","message":{"content":"αβ prefix middle suffix"}}),
                serde_json::json!({"type":"assistant","uuid":"a1","message":{"content":[{"type":"text","text":"next answer"},{"type":"tool_use","id":"fetch","name":"WebFetch","input":{}}]}}),
                serde_json::json!({"type":"user","uuid":"t1","message":{"content":[{"type":"tool_result","tool_use_id":"fetch","content":"external body"}]}}),
                serde_json::json!({"type":"assistant","uuid":"a2","message":{"content":"derived answer"}}),
            ],
            &[
                "middle",
                "suffix\n\nnext answer",
                "external body\n\nderived",
            ],
        );
        let stats = backfill(&storage, root.path(), 2000, false).unwrap();
        assert_eq!(stats.chunks_reconstructed, 3);
        assert_eq!(
            receipts(&storage, "old-0"),
            vec![("jsonl".into(), "u1".into(), 10, 16)]
        );
        assert_eq!(
            receipts(&storage, "old-1"),
            vec![
                ("jsonl".into(), "u1".into(), 17, 23),
                ("jsonl".into(), "a1".into(), 0, 11)
            ]
        );
        assert_eq!(
            storage.get_chunk_min_trust("old-1").unwrap(),
            TrustTier::UserHistory
        );
        assert_eq!(
            storage.get_chunk_min_trust("old-2").unwrap(),
            TrustTier::External
        );
    }

    #[test]
    fn pre_sanitizer_hook_wrapper_matches_raw_message() {
        let raw = "before <system-reminder>CSR ENDLESS MEMORY ACTIVE\nPAST CONTEXT</system-reminder> after";
        let (root, storage, _) = fixture(
            &[serde_json::json!({"type":"user","uuid":"raw-u","message":{"content":raw}})],
            &[raw],
        );
        assert!(!super::super::parse_jsonl_file_with_stats(
            &root.path().join("session.jsonl"),
            "demo"
        )
        .unwrap()
        .chunks
        .iter()
        .any(|c| c.content == raw));
        let stats = backfill(&storage, root.path(), 2000, false).unwrap();
        assert_eq!(stats.chunks_reconstructed, 1);
        assert_eq!(
            receipts(&storage, "old-0"),
            vec![("jsonl".into(), "raw-u".into(), 0, raw.chars().count())]
        );
    }

    #[test]
    fn sanitized_history_before_marker_suppression_reconstructs() {
        let (root, storage, _) = fixture(
            &[
                serde_json::json!({"type":"user","uuid":"u","message":{"content":"first"}}),
                serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"csr","name":"mcp__claude-self-reflect__csr_code_graph","input":{}}]}}),
                serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"csr","content":"removed memory"}]}}),
                serde_json::json!({"type":"assistant","uuid":"a","message":{"content":"later"}}),
                serde_json::json!({"type":"assistant","uuid":"bash","message":{"content":[{"type":"tool_use","name":"Bash","id":"b","input":{"command":"echo done"}}]}}),
            ],
            &["first\n\nlater\n\n[Bash: echo done]"],
        );
        assert_eq!(
            backfill(&storage, root.path(), 2000, false)
                .unwrap()
                .chunks_reconstructed,
            1
        );
        assert_eq!(
            receipts(&storage, "old-0")
                .iter()
                .map(|r| r.1.as_str())
                .collect::<Vec<_>>(),
            vec!["u", "a", "bash"]
        );
        assert_eq!(
            storage.get_chunk_min_trust("old-0").unwrap(),
            TrustTier::External
        );
    }

    #[test]
    fn plan_replay_preserves_file_receipt_and_mtime_and_is_noop() {
        let (root, storage, _) = fixture(&[], &["target"]);
        let projects = root.path().join("projects");
        let plans = root.path().join("plans");
        std::fs::create_dir(&plans).unwrap();
        let path = plans.join("safe-plan.md");
        std::fs::write(&path, "plan prefix target suffix").unwrap();
        storage
            .with_connection(|c| {
                c.execute(
                    "UPDATE chunks SET conversation_id='plan:safe-plan',source='plan'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            backfill(&storage, &projects, 1, false)
                .unwrap()
                .chunks_reconstructed,
            1
        );
        assert_eq!(receipts(&storage, "old-0")[0].0, "plan_file");
        assert_eq!(receipts(&storage, "old-0")[0].2, 12);
        assert_eq!(
            storage.get_chunk_min_trust("old-0").unwrap(),
            TrustTier::TrustedTool
        );
        let observed: String = storage
            .with_connection(|c| {
                Ok(
                    c.query_row("SELECT observed_at FROM provenance_events", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .unwrap();
        let mtime: chrono::DateTime<chrono::Utc> =
            std::fs::metadata(&path).unwrap().modified().unwrap().into();
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(&observed).unwrap(),
            mtime
        );
        let before = storage.with_connection(|c| Ok(c.total_changes())).unwrap();
        assert_eq!(
            backfill_with_retry(&storage, &projects, 1, false, true)
                .unwrap()
                .chunks_written,
            0
        );
        assert_eq!(
            backfill_incremental(&storage, &projects, 1)
                .unwrap()
                .chunks_written,
            0
        );
        assert_eq!(
            storage.with_connection(|c| Ok(c.total_changes())).unwrap(),
            before
        );
        let newer: std::time::SystemTime = (mtime + chrono::Duration::seconds(1)).into();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(newer)
            .unwrap();
        assert_eq!(
            backfill_incremental(&storage, &projects, 1)
                .unwrap()
                .chunks_written,
            1
        );
        assert_eq!(
            backfill_incremental(&storage, &projects, 1)
                .unwrap()
                .chunks_written,
            0
        );
    }

    #[test]
    fn partial_content_match_is_source_unmatched_and_unknown() {
        let (root, storage, _) = fixture(
            &[serde_json::json!({"type":"user","uuid":"u","message":{"content":"present"}})],
            &["present\n\ninvented"],
        );
        backfill(&storage, root.path(), 2000, false).unwrap();
        assert_eq!(receipts(&storage, "old-0")[0].0, "source_unmatched");
        assert_eq!(storage.provenance_failure_counts().unwrap(), [0, 0, 1]);
        assert_eq!(
            storage.get_chunk_min_trust("old-0").unwrap(),
            TrustTier::Unknown
        );
    }

    #[test]
    fn malformed_present_source_is_source_unparsed() {
        let (root, storage, path) = fixture(&[], &["content"]);
        std::fs::write(path, "{invalid json\n").unwrap();
        backfill(&storage, root.path(), 2000, false).unwrap();
        assert_eq!(receipts(&storage, "old-0")[0].0, "source_unparsed");
        assert_eq!(storage.provenance_failure_counts().unwrap(), [0, 1, 0]);
    }

    #[test]
    fn retry_missing_source_upgrades_once_and_never_revisits_jsonl() {
        let message =
            serde_json::json!({"type":"user","uuid":"u","message":{"content":"remember"}});
        let (root, storage, path) = fixture(std::slice::from_ref(&message), &["remember"]);
        std::fs::remove_file(&path).unwrap();
        backfill(&storage, root.path(), 1, false).unwrap();
        assert_eq!(receipts(&storage, "old-0")[0].0, "source_missing");
        assert_eq!(
            backfill_with_retry(&storage, root.path(), 1, false, true)
                .unwrap()
                .chunks_written,
            0
        );
        std::fs::write(&path, format!("{message}\n")).unwrap();
        let dry = backfill_with_retry(&storage, root.path(), 1, true, true).unwrap();
        assert_eq!(dry.chunks_reconstructed, 1);
        assert_eq!(
            storage.get_chunk_min_trust("old-0").unwrap(),
            TrustTier::Unknown
        );
        assert_eq!(
            backfill_with_retry(&storage, root.path(), 1, false, true)
                .unwrap()
                .chunks_written,
            1
        );
        assert_eq!(
            storage.get_chunk_min_trust("old-0").unwrap(),
            TrustTier::UserHistory
        );
        std::fs::write(&path, "invalid\n").unwrap();
        let before = storage.with_connection(|c| Ok(c.total_changes())).unwrap();
        assert_eq!(
            backfill_with_retry(&storage, root.path(), 1, false, true)
                .unwrap()
                .chunks_written,
            0
        );
        assert_eq!(
            storage.with_connection(|c| Ok(c.total_changes())).unwrap(),
            before
        );
        assert_eq!(
            storage.get_chunk_min_trust("old-0").unwrap(),
            TrustTier::UserHistory
        );
    }

    #[test]
    fn daemon_retry_requires_newer_source_and_advances_failed_observation() {
        let (root, storage, path) = fixture(
            &[serde_json::json!({"type":"user","message":{"content":"different"}})],
            &["needed"],
        );
        backfill(&storage, root.path(), 1, false).unwrap();
        assert_eq!(
            backfill_incremental(&storage, root.path(), 1)
                .unwrap()
                .chunks_written,
            0
        );
        storage
            .with_connection(|c| {
                c.execute(
                    "UPDATE provenance_events SET observed_at='2000-01-01T00:00:00Z'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        // Changed but still unmatched: record the new observation only once.
        assert_eq!(
            backfill_incremental(&storage, root.path(), 1)
                .unwrap()
                .chunks_written,
            1
        );
        let before = storage.with_connection(|c| Ok(c.total_changes())).unwrap();
        assert_eq!(
            backfill_incremental(&storage, root.path(), 1)
                .unwrap()
                .chunks_written,
            0
        );
        assert_eq!(
            storage.with_connection(|c| Ok(c.total_changes())).unwrap(),
            before
        );
        std::fs::write(
            &path,
            serde_json::json!({"type":"user","message":{"content":"needed"}}).to_string(),
        )
        .unwrap();
        storage
            .with_connection(|c| {
                c.execute(
                    "UPDATE provenance_events SET observed_at='2000-01-01T00:00:00Z'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            backfill_incremental(&storage, root.path(), 1)
                .unwrap()
                .chunks_reconstructed,
            1
        );
    }

    #[test]
    fn retry_legacy_unknown_receipt_is_reclassified_and_unchanged_run_writes_nothing() {
        let (root, storage, _) = fixture(
            &[serde_json::json!({"type":"user","message":{"content":"different"}})],
            &["absent"],
        );
        backfill(&storage, root.path(), 1, false).unwrap();
        storage
            .with_connection(|c| {
                c.execute(
                    "UPDATE provenance_events SET receipt_kind='unreconstructible'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        backfill_with_retry(&storage, root.path(), 1, false, true).unwrap();
        assert_eq!(receipts(&storage, "old-0")[0].0, "source_unmatched");
        let before = storage.with_connection(|c| Ok(c.total_changes())).unwrap();
        assert_eq!(
            backfill_with_retry(&storage, root.path(), 1, false, true)
                .unwrap()
                .chunks_written,
            0
        );
        assert_eq!(
            storage.with_connection(|c| Ok(c.total_changes())).unwrap(),
            before
        );
    }

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
        assert_eq!(receipt, "source_missing");
        assert_eq!(storage.provenance_failure_counts().unwrap(), [1, 0, 0]);
        assert_eq!(storage.provenance_coverage().unwrap().3, None);
    }
}
