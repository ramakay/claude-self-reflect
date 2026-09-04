use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::engine::Engine;
use crate::import::{
    derive_conversation_attribution, parse_jsonl_file_with_stats_and_parent,
    scrub_contaminated_text, ConversationChunk, ParentContext,
};
use crate::provenance::{ChunkEvidence, ChunkProvenance, TrustTier};
use crate::storage::Storage;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScrubReport {
    pub conversations_scrubbed: usize,
    pub chunks_rewritten: usize,
    pub chunks_dropped: usize,
    pub vectors_replaced: usize,
    /// `(chunks, reflections)` in the compact index persisted after a real run.
    pub index_rebuilt: Option<(usize, usize)>,
    pub actions: Vec<String>,
}

impl ScrubReport {
    pub fn format_text(&self, dry_run: bool) -> String {
        let mut output = String::new();
        for action in &self.actions {
            output.push_str(action);
            output.push('\n');
        }
        output.push_str(&format!(
            "{} summary\nconversations scrubbed: {}\nchunks rewritten: {}\nchunks dropped: {}\nvectors replaced: {}\n",
            if dry_run { "scrub dry-run" } else { "scrub" },
            self.conversations_scrubbed,
            self.chunks_rewritten,
            self.chunks_dropped,
            self.vectors_replaced
        ));
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
    Missing {
        rewritten: Vec<ConversationChunk>,
        dropped_ids: Vec<String>,
    },
}

#[derive(Debug)]
struct ScrubPlan {
    conversation_id: String,
    old_chunks: Vec<ConversationChunk>,
    sources: HashMap<String, String>,
    provenance: HashMap<String, ChunkProvenance>,
    structural_evidence: HashMap<String, ChunkEvidence>,
    kind: ScrubPlanKind,
}

fn source_path(storage: &Storage, conversation_id: &str) -> Result<Option<PathBuf>> {
    storage.with_connection(|conn| {
        let mut statement = conn.prepare(
            "SELECT file_path FROM import_state WHERE conversation_id = ?1 ORDER BY file_path LIMIT 1",
        )?;
        let mut rows = statement.query([conversation_id])?;
        Ok(rows
            .next()?
            .map(|row| row.get::<_, String>(0).map(PathBuf::from))
            .transpose()?)
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
    let provenance = old_ids
        .iter()
        .filter_map(|id| {
            storage
                .get_chunk_provenance(id)
                .transpose()
                .map(|result| result.map(|value| (id.clone(), value)))
        })
        .collect::<Result<HashMap<_, _>>>()?;

    let path = source_path(storage, conversation_id)?;
    let mut structural_evidence = HashMap::new();
    let kind = if let Some(path) = path.filter(|path| path.exists()) {
        let project = old_chunks
            .first()
            .map(|chunk| chunk.project_name.as_str())
            .unwrap_or_default();
        let parent = old_chunks
            .iter()
            .any(|chunk| chunk.is_sidechain)
            .then_some(ParentContext {
                floor: TrustTier::Unknown,
                event_id: None,
            });
        let parsed = parse_jsonl_file_with_stats_and_parent(&path, project, parent.as_ref())
            .with_context(|| format!("re-parsing {}", path.display()))?;
        let chunks = parsed.chunks;
        structural_evidence = parsed.evidence;
        let new_ids = chunks
            .iter()
            .map(|chunk| chunk.id.clone())
            .collect::<Vec<_>>();
        if old_ids == new_ids {
            let old_content = old_chunks
                .iter()
                .map(|chunk| (chunk.id.as_str(), chunk.content.as_str()))
                .collect::<HashMap<_, _>>();
            let changed_ids = chunks
                .iter()
                .filter(|chunk| old_content.get(chunk.id.as_str()) != Some(&chunk.content.as_str()))
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
    } else {
        let mut rewritten = Vec::new();
        let mut dropped_ids = Vec::new();
        for chunk in &old_chunks {
            match scrub_contaminated_text(&chunk.content) {
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
        }
    };

    Ok(ScrubPlan {
        conversation_id: conversation_id.to_string(),
        old_chunks,
        sources,
        provenance,
        structural_evidence,
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

fn summarize_plan(plan: &ScrubPlan, report: &mut ScrubReport) {
    report.conversations_scrubbed += 1;
    match &plan.kind {
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
        } => {
            report.chunks_rewritten += rewritten.len();
            report.chunks_dropped += dropped_ids.len();
            report.vectors_replaced += rewritten.len();
            report.actions.push(format!(
                "{}: source missing, rewrite {} chunks, drop {}, and replace {} vectors in place",
                plan.conversation_id,
                rewritten.len(),
                dropped_ids.len(),
                rewritten.len()
            ));
        }
    }
}

fn selected_plans(storage: &Storage, conversation: Option<&str>) -> Result<Vec<ScrubPlan>> {
    let contaminated = storage.contaminated_conversations()?;
    contaminated
        .into_iter()
        .filter(|(id, _)| conversation.is_none_or(|wanted| wanted == id))
        .map(|(id, _)| load_plan(storage, &id))
        .collect()
}

pub fn dry_run_scrub(storage: &Storage, conversation: Option<&str>) -> Result<ScrubReport> {
    let mut report = ScrubReport::default();
    for plan in selected_plans(storage, conversation)? {
        summarize_plan(&plan, &mut report);
    }
    Ok(report)
}

async fn replace_vector(engine: &Engine, chunk: &ConversationChunk, embedding: Vec<f32>) {
    let mut index = engine.search().write().await;
    index.remove_chunk(&chunk.id);
    index.insert_chunk(chunk.id.clone(), embedding);
}

pub async fn run_scrub(
    engine: &Engine,
    dry_run: bool,
    conversation: Option<&str>,
) -> Result<ScrubReport> {
    if dry_run {
        return dry_run_scrub(engine.storage(), conversation);
    }

    let plans = selected_plans(engine.storage(), conversation)?;
    let mut report = ScrubReport::default();
    if !plans.is_empty() {
        engine.invalidate_index_manifest()?;
    }
    for plan in plans {
        summarize_plan(&plan, &mut report);
        match &plan.kind {
            ScrubPlanKind::Stable {
                source_path,
                chunks,
                changed_ids,
            } => {
                let attribution =
                    derive_conversation_attribution(engine.projects_dir(), source_path);
                let default_source_conv_id = attribution
                    .parent_conversation_id
                    .as_deref()
                    .unwrap_or(&plan.conversation_id);
                let old_vectors = engine
                    .storage()
                    .get_chunk_vectors_by_ids(
                        &chunks
                            .iter()
                            .map(|chunk| chunk.id.clone())
                            .collect::<Vec<_>>(),
                    )?
                    .into_iter()
                    .collect::<HashMap<_, _>>();
                for chunk in chunks {
                    let source = plan
                        .sources
                        .get(&chunk.id)
                        .map(String::as_str)
                        .unwrap_or("conversation");
                    let embedding = if changed_ids.contains(&chunk.id) {
                        engine
                            .embeddings()
                            .embed(&[chunk.content.as_str()])?
                            .remove(0)
                    } else {
                        old_vectors
                            .get(&chunk.id)
                            .cloned()
                            .with_context(|| format!("missing stored vector for {}", chunk.id))?
                    };
                    engine
                        .storage()
                        .insert_chunk_with_source(chunk, &embedding, source)?;
                    engine.storage().insert_chunk_provenance(
                        &chunk.id,
                        &provenance_for(&plan, chunk, default_source_conv_id),
                    )?;
                    if let Some(evidence) = plan.structural_evidence.get(&chunk.id) {
                        engine.storage().replace_chunk_evidence(evidence)?;
                    }
                    if changed_ids.contains(&chunk.id) {
                        replace_vector(engine, chunk, embedding).await;
                    }
                }
                engine
                    .storage()
                    .mark_file_imported(source_path, chunks.len())?;
            }
            ScrubPlanKind::Shifted {
                source_path,
                chunks,
            } => {
                let old_ids = plan
                    .old_chunks
                    .iter()
                    .map(|chunk| chunk.id.clone())
                    .collect::<Vec<_>>();
                let texts = chunks
                    .iter()
                    .map(|chunk| chunk.content.as_str())
                    .collect::<Vec<_>>();
                let embeddings = if texts.is_empty() {
                    Vec::new()
                } else {
                    engine.embeddings().embed(&texts)?
                };
                let attribution =
                    derive_conversation_attribution(engine.projects_dir(), source_path);
                let default_source_conv_id = attribution
                    .parent_conversation_id
                    .as_deref()
                    .unwrap_or(&plan.conversation_id);
                let source = old_ids
                    .first()
                    .and_then(|id| plan.sources.get(id))
                    .map(String::as_str)
                    .unwrap_or("conversation");
                let rows = chunks
                    .iter()
                    .zip(embeddings)
                    .map(|(chunk, embedding)| {
                        (
                            chunk.clone(),
                            embedding,
                            provenance_for(&plan, chunk, default_source_conv_id),
                            source.to_string(),
                        )
                    })
                    .collect::<Vec<_>>();
                engine
                    .storage()
                    .replace_conversation_chunks_atomic(&plan.conversation_id, &rows)?;
                let evidence = chunks
                    .iter()
                    .filter_map(|chunk| plan.structural_evidence.get(&chunk.id).cloned())
                    .collect::<Vec<_>>();
                engine.storage().replace_chunk_evidence_batch(&evidence)?;
                {
                    let mut index = engine.search().write().await;
                    for id in &old_ids {
                        index.remove_chunk(id);
                    }
                    for (chunk, embedding, _, _) in &rows {
                        index.insert_chunk(chunk.id.clone(), embedding.clone());
                    }
                }
                engine
                    .storage()
                    .mark_file_imported(source_path, chunks.len())?;
            }
            ScrubPlanKind::Missing {
                rewritten,
                dropped_ids,
            } => {
                for chunk in rewritten {
                    let embedding = engine
                        .embeddings()
                        .embed(&[chunk.content.as_str()])?
                        .remove(0);
                    let source = plan
                        .sources
                        .get(&chunk.id)
                        .map(String::as_str)
                        .unwrap_or("conversation");
                    engine
                        .storage()
                        .insert_chunk_with_source(chunk, &embedding, source)?;
                    engine.storage().insert_chunk_provenance(
                        &chunk.id,
                        &provenance_for(&plan, chunk, &plan.conversation_id),
                    )?;
                    engine.storage().replace_chunk_evidence(
                        &super::provenance_backfill::unreconstructible_chunk_evidence(chunk, None),
                    )?;
                    replace_vector(engine, chunk, embedding).await;
                }
                for id in dropped_ids {
                    engine.storage().delete_chunk(id)?;
                    engine.search().write().await.remove_chunk(id);
                }
            }
        }
    }
    if report.conversations_scrubbed > 0 {
        // Replacing vectors leaves blank tombstones in the in-memory HNSW.
        // Rebuild a compact index from SQLite and persist it so queries never
        // have to over-fetch past tombstones and the next start loads from
        // cache. Runs of zero conversations touch neither index nor manifest.
        report.index_rebuilt = Some(engine.rebuild_search_index().await?);
    }
    engine.storage().refresh_contamination_cache()?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

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

        let report = super::run_scrub(&engine, false, Some("stable-conversation"))
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

    #[tokio::test]
    async fn scrub_rebuilds_shifted_ids_atomically_and_restores_provenance() {
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
            (stale_id.clone(), 1, "stale tail"),
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

        let report = super::run_scrub(&engine, false, Some("shifted-conversation"))
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

        let report = super::run_scrub(&engine, false, Some("missing-conversation"))
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

        let error = super::run_scrub(&engine, false, Some("flush-failure"))
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
