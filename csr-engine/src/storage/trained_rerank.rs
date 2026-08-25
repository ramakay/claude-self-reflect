//! Persistence contracts for the trained re-ranker.
//!
//! This module owns every SQL statement for exposure impressions, reaction
//! labels, model attempts, and held-out cluster receipts. Reaction statistics
//! remain batch-only for audits; feature schema v2 never uses them for scoring.

use std::collections::HashMap;
use std::sync::TryLockError;
use std::time::Duration;

use anyhow::Result;
use rusqlite::{params, params_from_iter, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::Storage;

/// Current maximum tolerated curated-MRR loss. The value persisted on each
/// attempt is a receipt; model loading revalidates with this constant.
pub const CURATED_VETO_EPSILON: f64 = 1e-9;
pub const MIN_CURATED_CASES: usize = 5;
pub const MAX_REACTION_GAP_SECONDS: i64 = 4 * 60 * 60;
pub const LAST_ATTEMPT_META_KEY: &str = "trained_rerank_last_attempt";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExposureItem {
    pub rank: i64,
    pub memory_id: String,
    pub conversation_id: Option<String>,
    pub source_type: String,
    pub baseline_score: Option<f64>,
    pub cosine: Option<f64>,
    pub recency: Option<f64>,
    pub graph_proximity: Option<f64>,
    pub author: Option<String>,
    pub is_scaffold: bool,
    pub is_mechanic: bool,
    pub supersedes: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExposureImpression {
    pub impression_id: String,
    pub session_id: String,
    pub project: String,
    pub surface: String,
    pub query_hash: Option<String>,
    pub query_embedding: Option<Vec<f32>>,
    pub intent: String,
    pub shown_at: String,
    pub feature_schema: i64,
    pub legacy: bool,
    pub items: Vec<ExposureItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReactionLabel {
    pub session_id: String,
    pub assistant_turn: i64,
    pub next_user_turn: i64,
    pub assistant_ts: Option<String>,
    pub next_user_ts: Option<String>,
    pub reaction: String,
    pub proposed_reaction: Option<String>,
    pub confidence: f64,
    pub runner_up_score: f64,
    pub margin: f64,
    pub pickup_similarity: Option<f64>,
    pub next_user_text: String,
    pub near_miss: bool,
    pub classifier_hash: String,
    pub transcript_mtime: i64,
    pub harvested_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionLabelCounts {
    pub acceptance: i64,
    pub correction: i64,
    pub reask: i64,
    pub redirect: i64,
    pub abstain: i64,
    pub near_miss: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionStats {
    pub acceptance: i64,
    pub correction: i64,
    pub reask: i64,
    pub redirect: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarvestSource {
    pub file_path: String,
    pub session_id: String,
    pub project: String,
    pub csr_tool_blocks_suppressed: i64,
    pub csr_hook_wrappers_scrubbed: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabeledExposure {
    pub impression_id: String,
    pub session_id: String,
    pub project: String,
    pub intent: String,
    pub shown_at: String,
    pub reaction_at: String,
    pub reaction_turn: i64,
    pub reaction: String,
    pub query_embedding: Option<Vec<f32>>,
    pub legacy: bool,
    pub items: Vec<ExposureItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyRetrievalEvent {
    pub event_id: String,
    pub memory_id: String,
    pub memory_type: String,
    pub retrieved_at: String,
    pub session_id: String,
    pub project: String,
    pub conversation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelAttempt {
    pub model_id: String,
    pub feature_schema: i64,
    pub classifier_hash: String,
    pub seed: i64,
    pub cutoff_ts: Option<String>,
    pub train_start_ts: Option<String>,
    pub train_end_ts: Option<String>,
    pub eval_start_ts: Option<String>,
    pub eval_end_ts: Option<String>,
    pub train_impressions: i64,
    pub train_rows: i64,
    pub eval_impressions: i64,
    pub eval_rows: i64,
    pub eval_clusters: i64,
    pub cluster_wins: i64,
    pub cluster_losses: i64,
    pub cluster_ties: i64,
    pub excluded_contaminated: i64,
    pub abstained_reactions: i64,
    pub acceptance_labels: i64,
    pub correction_labels: i64,
    pub reask_labels: i64,
    pub redirect_labels: i64,
    pub near_miss_labels: i64,
    pub baseline_ndcg5: Option<f64>,
    pub trained_ndcg5: Option<f64>,
    pub baseline_mrr: Option<f64>,
    pub trained_mrr: Option<f64>,
    pub curated_baseline_score: Option<f64>,
    pub curated_trained_score: Option<f64>,
    pub curated_case_count: i64,
    pub curated_veto_epsilon: f64,
    pub gate_status: String,
    pub gate_reason: String,
    pub weights_json: Option<String>,
    pub normalization_json: Option<String>,
    pub trained_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateClusterReceipt {
    pub model_id: String,
    pub cluster_id: String,
    pub impression_count: i64,
    pub distinct_session_count: i64,
    pub candidate_count: i64,
    pub baseline_ndcg5: f64,
    pub trained_ndcg5: f64,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCuratedCase {
    pub project: String,
    pub file_path: String,
    pub query_text: String,
    pub expected_sessions: Vec<String>,
}

fn vec_to_bytes(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn bytes_to_vec(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(std::mem::size_of::<f32>())
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

fn row_to_reaction_label(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReactionLabel> {
    Ok(ReactionLabel {
        session_id: row.get(0)?,
        assistant_turn: row.get(1)?,
        next_user_turn: row.get(2)?,
        assistant_ts: row.get(3)?,
        next_user_ts: row.get(4)?,
        reaction: row.get(5)?,
        proposed_reaction: row.get(6)?,
        confidence: row.get(7)?,
        runner_up_score: row.get(8)?,
        margin: row.get(9)?,
        pickup_similarity: row.get(10)?,
        next_user_text: row.get(11)?,
        near_miss: row.get::<_, i64>(12)? != 0,
        classifier_hash: row.get(13)?,
        transcript_mtime: row.get(14)?,
        harvested_at: row.get(15)?,
    })
}

fn row_to_model_attempt(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelAttempt> {
    Ok(ModelAttempt {
        model_id: row.get(0)?,
        feature_schema: row.get(1)?,
        classifier_hash: row.get(2)?,
        seed: row.get(3)?,
        cutoff_ts: row.get(4)?,
        train_start_ts: row.get(5)?,
        train_end_ts: row.get(6)?,
        eval_start_ts: row.get(7)?,
        eval_end_ts: row.get(8)?,
        train_impressions: row.get(9)?,
        train_rows: row.get(10)?,
        eval_impressions: row.get(11)?,
        eval_rows: row.get(12)?,
        eval_clusters: row.get(13)?,
        cluster_wins: row.get(14)?,
        cluster_losses: row.get(15)?,
        cluster_ties: row.get(16)?,
        excluded_contaminated: row.get(17)?,
        abstained_reactions: row.get(18)?,
        acceptance_labels: row.get(19)?,
        correction_labels: row.get(20)?,
        reask_labels: row.get(21)?,
        redirect_labels: row.get(22)?,
        near_miss_labels: row.get(23)?,
        baseline_ndcg5: row.get(24)?,
        trained_ndcg5: row.get(25)?,
        baseline_mrr: row.get(26)?,
        trained_mrr: row.get(27)?,
        curated_baseline_score: row.get(28)?,
        curated_trained_score: row.get(29)?,
        curated_case_count: row.get(30)?,
        curated_veto_epsilon: row.get(31)?,
        gate_status: row.get(32)?,
        gate_reason: row.get(33)?,
        weights_json: row.get(34)?,
        normalization_json: row.get(35)?,
        trained_at: row.get(36)?,
    })
}

const MODEL_ATTEMPT_SELECT: &str =
    "SELECT model_id, feature_schema, classifier_hash, seed, cutoff_ts,
            train_start_ts, train_end_ts, eval_start_ts, eval_end_ts,
            train_impressions, train_rows, eval_impressions, eval_rows, eval_clusters,
            cluster_wins, cluster_losses, cluster_ties, excluded_contaminated,
            abstained_reactions, acceptance_labels, correction_labels, reask_labels,
            redirect_labels, near_miss_labels, baseline_ndcg5, trained_ndcg5,
            baseline_mrr, trained_mrr, curated_baseline_score,
            curated_trained_score, curated_case_count, curated_veto_epsilon, gate_status,
            gate_reason, weights_json, normalization_json, trained_at
     FROM rerank_models";

fn write_reaction_label(conn: &rusqlite::Connection, label: &ReactionLabel) -> Result<()> {
    conn.execute(
        "INSERT INTO rerank_reaction_labels
         (session_id, assistant_turn, next_user_turn, assistant_ts, next_user_ts,
          reaction, proposed_reaction, confidence, runner_up_score, margin,
          pickup_similarity, next_user_text, near_miss, classifier_hash,
          transcript_mtime, harvested_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
         ON CONFLICT(session_id, assistant_turn, classifier_hash) DO UPDATE SET
          next_user_turn=excluded.next_user_turn,
          assistant_ts=excluded.assistant_ts, next_user_ts=excluded.next_user_ts,
          reaction=excluded.reaction, proposed_reaction=excluded.proposed_reaction,
          confidence=excluded.confidence, runner_up_score=excluded.runner_up_score,
          margin=excluded.margin, pickup_similarity=excluded.pickup_similarity,
          next_user_text=excluded.next_user_text, near_miss=excluded.near_miss,
          transcript_mtime=excluded.transcript_mtime, harvested_at=excluded.harvested_at",
        params![
            label.session_id,
            label.assistant_turn,
            label.next_user_turn,
            label.assistant_ts,
            label.next_user_ts,
            label.reaction,
            label.proposed_reaction,
            label.confidence,
            label.runner_up_score,
            label.margin,
            label.pickup_similarity,
            label.next_user_text,
            i64::from(label.near_miss),
            label.classifier_hash,
            label.transcript_mtime,
            label.harvested_at,
        ],
    )?;
    Ok(())
}

fn write_model_attempt(
    tx: &rusqlite::Transaction<'_>,
    attempt: &ModelAttempt,
    clusters: &[GateClusterReceipt],
) -> Result<()> {
    tx.execute(
        "INSERT INTO rerank_models
         (model_id, feature_schema, classifier_hash, seed, cutoff_ts,
          train_start_ts, train_end_ts, eval_start_ts, eval_end_ts,
          train_impressions, train_rows, eval_impressions, eval_rows, eval_clusters,
          cluster_wins, cluster_losses, cluster_ties, excluded_contaminated,
          abstained_reactions, acceptance_labels, correction_labels, reask_labels,
          redirect_labels, near_miss_labels, baseline_ndcg5, trained_ndcg5,
          baseline_mrr, trained_mrr, curated_baseline_score,
          curated_trained_score, curated_case_count, curated_veto_epsilon, gate_status,
          gate_reason, weights_json, normalization_json, trained_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
                 ?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,
                 ?31,?32,?33,?34,?35,?36,?37)",
        params![
            attempt.model_id,
            attempt.feature_schema,
            attempt.classifier_hash,
            attempt.seed,
            attempt.cutoff_ts,
            attempt.train_start_ts,
            attempt.train_end_ts,
            attempt.eval_start_ts,
            attempt.eval_end_ts,
            attempt.train_impressions,
            attempt.train_rows,
            attempt.eval_impressions,
            attempt.eval_rows,
            attempt.eval_clusters,
            attempt.cluster_wins,
            attempt.cluster_losses,
            attempt.cluster_ties,
            attempt.excluded_contaminated,
            attempt.abstained_reactions,
            attempt.acceptance_labels,
            attempt.correction_labels,
            attempt.reask_labels,
            attempt.redirect_labels,
            attempt.near_miss_labels,
            attempt.baseline_ndcg5,
            attempt.trained_ndcg5,
            attempt.baseline_mrr,
            attempt.trained_mrr,
            attempt.curated_baseline_score,
            attempt.curated_trained_score,
            attempt.curated_case_count,
            attempt.curated_veto_epsilon,
            attempt.gate_status,
            attempt.gate_reason,
            attempt.weights_json,
            attempt.normalization_json,
            attempt.trained_at,
        ],
    )?;
    let mut stmt = tx.prepare(
        "INSERT INTO rerank_gate_clusters
         (model_id, cluster_id, impression_count, distinct_session_count,
          candidate_count, baseline_ndcg5, trained_ndcg5, outcome)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
    )?;
    for cluster in clusters {
        stmt.execute(params![
            cluster.model_id,
            cluster.cluster_id,
            cluster.impression_count,
            cluster.distinct_session_count,
            cluster.candidate_count,
            cluster.baseline_ndcg5,
            cluster.trained_ndcg5,
            cluster.outcome,
        ])?;
    }
    Ok(())
}

const REACTION_SELECT: &str =
    "SELECT session_id, assistant_turn, next_user_turn, assistant_ts, next_user_ts,
            reaction, proposed_reaction, confidence, runner_up_score, margin,
            pickup_similarity, next_user_text, near_miss, classifier_hash,
            transcript_mtime, harvested_at
     FROM rerank_reaction_labels";

fn write_exposure(conn: &rusqlite::Connection, impression: &ExposureImpression) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let embedding = impression.query_embedding.as_deref().map(vec_to_bytes);
    tx.execute(
        "INSERT OR IGNORE INTO rerank_exposure_impressions
         (impression_id, session_id, project, surface, query_hash, query_embedding,
          intent, shown_at, feature_schema, item_count, legacy)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            impression.impression_id,
            impression.session_id,
            impression.project,
            impression.surface,
            impression.query_hash,
            embedding,
            impression.intent,
            impression.shown_at,
            impression.feature_schema,
            impression.items.len() as i64,
            i64::from(impression.legacy),
        ],
    )?;
    let mut stmt = tx.prepare(
        "INSERT OR IGNORE INTO rerank_exposure_items
         (impression_id, rank, memory_id, conversation_id, source_type,
          baseline_score, cosine, recency, graph_proximity, author,
          is_scaffold, is_mechanic, supersedes)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
    )?;
    for item in &impression.items {
        stmt.execute(params![
            impression.impression_id,
            item.rank,
            item.memory_id,
            item.conversation_id,
            item.source_type,
            item.baseline_score,
            item.cosine,
            item.recency,
            item.graph_proximity,
            item.author,
            i64::from(item.is_scaffold),
            i64::from(item.is_mechanic),
            i64::from(item.supersedes),
        ])?;
    }
    drop(stmt);
    tx.commit()?;
    Ok(())
}

fn is_sqlite_busy(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<rusqlite::Error>()
        .and_then(|error| match error {
            rusqlite::Error::SqliteFailure(code, _) => Some(code.code),
            _ => None,
        })
        .is_some_and(|code| {
            matches!(
                code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
        })
}

impl Storage {
    /// Derive portable curated cases from the local corpus. Exact project/file
    /// identity avoids treating a common path in another repository as gold,
    /// and the chunks join guarantees every expected session is retrievable.
    pub fn local_curated_rerank_cases(&self, limit: usize) -> Result<Vec<LocalCuratedCase>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT evolution.project_name, evolution.file_path,
                        evolution.session_id
                 FROM code_evolution evolution
                 WHERE evolution.project_name != '' AND evolution.file_path != ''
                   AND EXISTS (
                       SELECT 1 FROM chunks chunk
                       WHERE chunk.conversation_id = evolution.session_id
                         AND chunk.project_name = evolution.project_name)
                 ORDER BY evolution.project_name, evolution.file_path,
                          evolution.session_id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            let mut grouped = std::collections::BTreeMap::<(String, String), Vec<String>>::new();
            for row in rows {
                let (project, file_path, session_id) = row?;
                grouped
                    .entry((project, file_path))
                    .or_default()
                    .push(session_id);
            }
            Ok(grouped
                .into_iter()
                .take(limit)
                .map(
                    |((project, file_path), expected_sessions)| LocalCuratedCase {
                        query_text: format!("history of changes to {file_path}"),
                        project,
                        file_path,
                        expected_sessions,
                    },
                )
                .collect())
        })
    }

    pub fn record_rerank_exposure(&self, impression: &ExposureImpression) -> Result<()> {
        self.with_connection(|conn| write_exposure(conn, impression))
    }

    /// Hook-safe exposure write. Never waits for the in-process mutex or a
    /// SQLite writer lock; `false` means telemetry was intentionally dropped.
    pub fn try_record_rerank_exposure(&self, impression: &ExposureImpression) -> Result<bool> {
        let conn = match self.conn.try_lock() {
            Ok(conn) => conn,
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Poisoned(error)) => {
                return Err(anyhow::anyhow!("lock: {error}"));
            }
        };
        let previous_timeout_ms: i64 =
            conn.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;
        conn.busy_timeout(Duration::ZERO)?;
        let result = write_exposure(&conn, impression);
        let restore = conn.busy_timeout(Duration::from_millis(
            u64::try_from(previous_timeout_ms).unwrap_or(5_000),
        ));
        restore?;
        match result {
            Ok(()) => Ok(true),
            Err(error) if is_sqlite_busy(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub fn upsert_rerank_reaction_label(&self, label: &ReactionLabel) -> Result<()> {
        self.with_connection(|conn| write_reaction_label(conn, label))
    }

    pub fn list_rerank_harvest_sources(&self) -> Result<Vec<HarvestSource>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT state.file_path, state.conversation_id,
                        COALESCE(MAX(chunks.project_name), 'unknown'),
                        COALESCE(state.csr_tool_blocks_suppressed, 0),
                        COALESCE(state.csr_hook_wrappers_scrubbed, 0)
                 FROM import_state state
                 LEFT JOIN chunks ON chunks.conversation_id = state.conversation_id
                 WHERE state.conversation_id IS NOT NULL
                   AND state.file_path LIKE '%.jsonl'
                 GROUP BY state.file_path, state.conversation_id
                 ORDER BY state.conversation_id, state.file_path",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(HarvestSource {
                    file_path: row.get(0)?,
                    session_id: row.get(1)?,
                    project: row.get(2)?,
                    csr_tool_blocks_suppressed: row.get(3)?,
                    csr_hook_wrappers_scrubbed: row.get(4)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub fn rerank_harvest_is_current(
        &self,
        session_id: &str,
        classifier_hash: &str,
        transcript_mtime: i64,
    ) -> Result<bool> {
        self.with_connection(|conn| {
            conn.query_row(
                "SELECT transcript_mtime = ?3 FROM rerank_harvest_state
                 WHERE session_id = ?1 AND classifier_hash = ?2",
                params![session_id, classifier_hash, transcript_mtime],
                |row| row.get(0),
            )
            .optional()
            .map(|value| value.unwrap_or(false))
            .map_err(Into::into)
        })
    }

    pub fn rerank_contaminated_session_count(&self, classifier_hash: &str) -> Result<i64> {
        self.with_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM rerank_harvest_state
                 WHERE classifier_hash = ?1 AND contaminated = 1",
                [classifier_hash],
                |row| row.get(0),
            )
            .map_err(Into::into)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn replace_rerank_session_labels(
        &self,
        session_id: &str,
        classifier_hash: &str,
        transcript_mtime: i64,
        contaminated: bool,
        harvested_at: &str,
        labels: &[ReactionLabel],
    ) -> Result<()> {
        self.with_connection(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "DELETE FROM rerank_reaction_labels
                 WHERE session_id = ?1 AND classifier_hash = ?2",
                params![session_id, classifier_hash],
            )?;
            for label in labels {
                write_reaction_label(&tx, label)?;
            }
            tx.execute(
                "INSERT INTO rerank_harvest_state
                 (session_id, classifier_hash, transcript_mtime, label_count,
                  contaminated, harvested_at)
                 VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(session_id, classifier_hash) DO UPDATE SET
                  transcript_mtime=excluded.transcript_mtime,
                  label_count=excluded.label_count,
                  contaminated=excluded.contaminated,
                  harvested_at=excluded.harvested_at",
                params![
                    session_id,
                    classifier_hash,
                    transcript_mtime,
                    labels.len() as i64,
                    i64::from(contaminated),
                    harvested_at,
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn rerank_reaction_label_counts(
        &self,
        classifier_hash: &str,
    ) -> Result<ReactionLabelCounts> {
        self.with_connection(|conn| {
            let mut counts = ReactionLabelCounts::default();
            let mut stmt = conn.prepare(
                "SELECT reaction, COUNT(*), COALESCE(SUM(near_miss), 0)
                 FROM rerank_reaction_labels WHERE classifier_hash = ?1 GROUP BY reaction",
            )?;
            let rows = stmt.query_map([classifier_hash], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (reaction, count, near_miss) = row?;
                match reaction.as_str() {
                    "acceptance" => counts.acceptance = count,
                    "correction" => counts.correction = count,
                    "reask" => counts.reask = count,
                    "redirect" => counts.redirect = count,
                    "abstain" => counts.abstain = count,
                    _ => {}
                }
                counts.near_miss += near_miss;
            }
            Ok(counts)
        })
    }

    pub fn audit_rerank_reaction_labels(
        &self,
        classifier_hash: &str,
        per_class: usize,
        near_misses: usize,
    ) -> Result<Vec<ReactionLabel>> {
        self.with_connection(|conn| {
            let mut out = Vec::new();
            let class_sql = format!(
                "{REACTION_SELECT} WHERE classifier_hash = ?1 AND reaction = ?2
                 ORDER BY session_id, assistant_turn LIMIT ?3"
            );
            for reaction in ["acceptance", "correction", "reask", "redirect"] {
                let mut stmt = conn.prepare(&class_sql)?;
                let rows = stmt.query_map(
                    params![classifier_hash, reaction, per_class as i64],
                    row_to_reaction_label,
                )?;
                out.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
            }
            let near_sql = format!(
                "{REACTION_SELECT} WHERE classifier_hash = ?1 AND reaction = 'abstain'
                 AND near_miss = 1 ORDER BY session_id, assistant_turn LIMIT ?2"
            );
            let mut stmt = conn.prepare(&near_sql)?;
            let rows = stmt.query_map(
                params![classifier_hash, near_misses as i64],
                row_to_reaction_label,
            )?;
            out.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
            Ok(out)
        })
    }

    pub fn latest_reaction_classifier_hash(&self) -> Result<Option<String>> {
        self.with_connection(|conn| {
            conn.query_row(
                "SELECT classifier_hash FROM rerank_harvest_state
                 ORDER BY harvested_at DESC, classifier_hash DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
        })
    }

    /// Fetch reaction summaries for an audit candidate set in one SQL query.
    pub fn get_rerank_reaction_stats_batch(
        &self,
        classifier_hash: &str,
        memory_ids: &[&str],
    ) -> Result<HashMap<String, ReactionStats>> {
        if memory_ids.is_empty() {
            return Ok(HashMap::new());
        }
        self.with_connection(|conn| {
            let placeholders = (0..memory_ids.len())
                .map(|index| format!("?{}", index + 3))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT item.memory_id,
                        SUM(CASE WHEN label.reaction='acceptance' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN label.reaction='correction' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN label.reaction='reask' THEN 1 ELSE 0 END),
                        SUM(CASE WHEN label.reaction='redirect' THEN 1 ELSE 0 END)
                 FROM rerank_exposure_items item
                 JOIN rerank_exposure_impressions impression
                   ON impression.impression_id = item.impression_id
                 JOIN rerank_reaction_labels label
                   ON label.session_id = impression.session_id
                  AND label.classifier_hash = ?1
                  AND label.assistant_turn = (
                      SELECT next_label.assistant_turn
                      FROM rerank_reaction_labels next_label
                      WHERE next_label.session_id = impression.session_id
                        AND next_label.classifier_hash = label.classifier_hash
                        AND next_label.assistant_ts >= impression.shown_at
                        AND unixepoch(next_label.assistant_ts)
                            <= unixepoch(impression.shown_at) + ?2
                      ORDER BY next_label.assistant_ts, next_label.assistant_turn
                      LIMIT 1)
                 WHERE item.memory_id IN ({placeholders}) AND label.reaction != 'abstain'
                   AND label.next_user_ts IS NOT NULL
                   AND unixepoch(label.next_user_ts) >= unixepoch(impression.shown_at)
                   AND unixepoch(label.next_user_ts)
                       <= unixepoch(impression.shown_at) + ?2
                 GROUP BY item.memory_id"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut parameters = vec![
                rusqlite::types::Value::Text(classifier_hash.to_owned()),
                rusqlite::types::Value::Integer(MAX_REACTION_GAP_SECONDS),
            ];
            parameters.extend(
                memory_ids
                    .iter()
                    .map(|memory_id| rusqlite::types::Value::Text((*memory_id).to_owned())),
            );
            let rows = stmt.query_map(params_from_iter(parameters.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    ReactionStats {
                        acceptance: row.get(1)?,
                        correction: row.get(2)?,
                        reask: row.get(3)?,
                        redirect: row.get(4)?,
                    },
                ))
            })?;
            Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
        })
    }

    pub fn load_labeled_rerank_exposures(
        &self,
        classifier_hash: &str,
    ) -> Result<Vec<LabeledExposure>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT impression.impression_id, impression.session_id,
                        impression.project, impression.intent, impression.shown_at,
                        label.next_user_ts, label.assistant_turn, label.reaction,
                        impression.query_embedding, impression.legacy, item.rank, item.memory_id,
                        item.conversation_id, item.source_type, item.baseline_score,
                        item.cosine, item.recency, item.graph_proximity, item.author,
                        item.is_scaffold, item.is_mechanic, item.supersedes
                 FROM rerank_exposure_impressions impression
                 JOIN rerank_reaction_labels label
                   ON label.session_id = impression.session_id
                  AND label.classifier_hash = ?1
                  AND label.assistant_turn = (
                      SELECT next_label.assistant_turn
                      FROM rerank_reaction_labels next_label
                      WHERE next_label.session_id = impression.session_id
                        AND next_label.classifier_hash = ?1
                        AND next_label.assistant_ts >= impression.shown_at
                        AND unixepoch(next_label.assistant_ts)
                            <= unixepoch(impression.shown_at) + ?2
                      ORDER BY next_label.assistant_ts, next_label.assistant_turn
                      LIMIT 1)
                 JOIN rerank_exposure_items item
                   ON item.impression_id = impression.impression_id
                 WHERE label.reaction != 'abstain'
                   AND label.next_user_ts IS NOT NULL
                   AND unixepoch(label.next_user_ts) >= unixepoch(impression.shown_at)
                   AND unixepoch(label.next_user_ts)
                       <= unixepoch(impression.shown_at) + ?2
                   AND impression.feature_schema = ?3
                 ORDER BY label.next_user_ts, impression.impression_id, item.rank",
            )?;
            let mut rows = stmt.query(rusqlite::params![
                classifier_hash,
                MAX_REACTION_GAP_SECONDS,
                crate::search::trained_rerank::FEATURE_SCHEMA,
            ])?;
            let mut out: Vec<LabeledExposure> = Vec::new();
            while let Some(row) = rows.next()? {
                let impression_id: String = row.get(0)?;
                let needs_new = out
                    .last()
                    .is_none_or(|impression| impression.impression_id != impression_id);
                if needs_new {
                    let embedding: Option<Vec<u8>> = row.get(8)?;
                    out.push(LabeledExposure {
                        impression_id: impression_id.clone(),
                        session_id: row.get(1)?,
                        project: row.get(2)?,
                        intent: row.get(3)?,
                        shown_at: row.get(4)?,
                        reaction_at: row.get(5)?,
                        reaction_turn: row.get(6)?,
                        reaction: row.get(7)?,
                        query_embedding: embedding.as_deref().map(bytes_to_vec),
                        legacy: row.get::<_, i64>(9)? != 0,
                        items: Vec::new(),
                    });
                }
                out.last_mut()
                    .expect("labeled exposure was inserted")
                    .items
                    .push(ExposureItem {
                        rank: row.get(10)?,
                        memory_id: row.get(11)?,
                        conversation_id: row.get(12)?,
                        source_type: row.get(13)?,
                        baseline_score: row.get(14)?,
                        cosine: row.get(15)?,
                        recency: row.get(16)?,
                        graph_proximity: row.get(17)?,
                        author: row.get(18)?,
                        is_scaffold: row.get::<_, i64>(19)? != 0,
                        is_mechanic: row.get::<_, i64>(20)? != 0,
                        supersedes: row.get::<_, i64>(21)? != 0,
                    });
            }
            Ok(out)
        })
    }

    pub fn list_legacy_prompt_retrievals(&self) -> Result<Vec<LegacyRetrievalEvent>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT event.id, event.memory_id, event.memory_type,
                        event.retrieved_at, event.session_id,
                        COALESCE(chunk.project_name, 'unknown'), chunk.conversation_id
                 FROM retrieval_events event
                 LEFT JOIN chunks chunk ON chunk.id = event.memory_id
                 WHERE event.hook_phase = 'prompt_submit'
                   AND event.session_id IS NOT NULL
                   AND event.session_id != 'mcp'
                   AND (chunk.id IS NOT NULL OR EXISTS (
                       SELECT 1 FROM reflections reflection
                       WHERE reflection.id = event.memory_id))
                   AND event.retrieved_at < COALESCE((
                       SELECT MIN(impression.shown_at)
                       FROM rerank_exposure_impressions impression
                       WHERE impression.legacy = 0), '9999-12-31T00:00:00Z')
                 ORDER BY event.session_id, event.retrieved_at, event.id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(LegacyRetrievalEvent {
                    event_id: row.get(0)?,
                    memory_id: row.get(1)?,
                    memory_type: row.get(2)?,
                    retrieved_at: row.get(3)?,
                    session_id: row.get(4)?,
                    project: row.get(5)?,
                    conversation_id: row.get(6)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }

    pub fn insert_rerank_model_attempt(
        &self,
        attempt: &ModelAttempt,
        clusters: &[GateClusterReceipt],
    ) -> Result<()> {
        self.with_connection(|conn| {
            let tx = conn.unchecked_transaction()?;
            write_model_attempt(&tx, attempt, clusters)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn insert_rerank_model_attempt_with_cadence(
        &self,
        attempt: &ModelAttempt,
        clusters: &[GateClusterReceipt],
        cadence_ts: &str,
    ) -> Result<()> {
        self.with_connection(|conn| {
            let tx = conn.unchecked_transaction()?;
            write_model_attempt(&tx, attempt, clusters)?;
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                params![LAST_ATTEMPT_META_KEY, cadence_ts],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn latest_rerank_model_attempt(&self) -> Result<Option<ModelAttempt>> {
        self.with_connection(|conn| {
            conn.query_row(
                &format!("{MODEL_ATTEMPT_SELECT} ORDER BY trained_at DESC, model_id DESC LIMIT 1"),
                [],
                row_to_model_attempt,
            )
            .optional()
            .map_err(Into::into)
        })
    }

    pub fn latest_passing_rerank_model_attempt(
        &self,
        feature_schema: i64,
    ) -> Result<Option<ModelAttempt>> {
        self.with_connection(|conn| {
            conn.query_row(
                &format!(
                    "{MODEL_ATTEMPT_SELECT}
                     WHERE gate_status = 'passed' AND feature_schema = ?1
                     ORDER BY trained_at DESC, model_id DESC LIMIT 1"
                ),
                [feature_schema],
                row_to_model_attempt,
            )
            .optional()
            .map_err(Into::into)
        })
    }

    pub fn rerank_gate_clusters(&self, model_id: &str) -> Result<Vec<GateClusterReceipt>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT model_id, cluster_id, impression_count, distinct_session_count,
                        candidate_count, baseline_ndcg5, trained_ndcg5, outcome
                 FROM rerank_gate_clusters WHERE model_id = ?1 ORDER BY cluster_id",
            )?;
            let rows = stmt.query_map([model_id], |row| {
                Ok(GateClusterReceipt {
                    model_id: row.get(0)?,
                    cluster_id: row.get(1)?,
                    impression_count: row.get(2)?,
                    distinct_session_count: row.get(3)?,
                    candidate_count: row.get(4)?,
                    baseline_ndcg5: row.get(5)?,
                    trained_ndcg5: row.get(6)?,
                    outcome: row.get(7)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn insert_local_curated_fixture(
        storage: &Storage,
        event_id: &str,
        session_id: &str,
        project: &str,
        file_path: &str,
    ) {
        storage
            .insert_chunk(
                &crate::import::ConversationChunk {
                    id: format!("chunk-{event_id}"),
                    conversation_id: session_id.into(),
                    project_name: project.into(),
                    timestamp: "2026-08-24T00:00:00Z".into(),
                    content: format!("work on {file_path}"),
                    message_count: 1,
                    summary: None,
                    author: crate::provenance::Speaker::User,
                    seq: 0,
                    is_sidechain: false,
                },
                &[1.0, 0.0],
            )
            .unwrap();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO code_evolution
                     (id, session_id, project_name, file_path, timestamp)
                     VALUES (?1, ?2, ?3, ?4, '2026-08-24T00:00:00Z')",
                    rusqlite::params![event_id, session_id, project, file_path],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn empty_impression() -> ExposureImpression {
        ExposureImpression {
            impression_id: "busy-impression".into(),
            session_id: "session".into(),
            project: "project".into(),
            surface: "prompt_submit".into(),
            query_hash: None,
            query_embedding: None,
            intent: "other".into(),
            shown_at: "2026-08-24T00:00:00Z".into(),
            feature_schema: crate::search::trained_rerank::FEATURE_SCHEMA,
            legacy: false,
            items: Vec::new(),
        }
    }

    fn test_model_attempt(model_id: &str, gate_status: &str, trained_at: &str) -> ModelAttempt {
        ModelAttempt {
            model_id: model_id.into(),
            feature_schema: crate::search::trained_rerank::FEATURE_SCHEMA,
            classifier_hash: "classifier".into(),
            seed: 7,
            cutoff_ts: None,
            train_start_ts: None,
            train_end_ts: None,
            eval_start_ts: None,
            eval_end_ts: None,
            train_impressions: 200,
            train_rows: 200,
            eval_impressions: 50,
            eval_rows: 50,
            eval_clusters: 10,
            cluster_wins: 6,
            cluster_losses: 4,
            cluster_ties: 0,
            excluded_contaminated: 0,
            abstained_reactions: 0,
            acceptance_labels: 1,
            correction_labels: 1,
            reask_labels: 0,
            redirect_labels: 0,
            near_miss_labels: 0,
            baseline_ndcg5: Some(0.5),
            trained_ndcg5: Some(0.6),
            baseline_mrr: Some(0.5),
            trained_mrr: Some(0.6),
            curated_baseline_score: Some(0.5),
            curated_trained_score: Some(0.5),
            curated_case_count: 5,
            curated_veto_epsilon: CURATED_VETO_EPSILON,
            gate_status: gate_status.into(),
            gate_reason: gate_status.into(),
            weights_json: None,
            normalization_json: None,
            trained_at: trained_at.into(),
        }
    }

    #[test]
    fn best_effort_exposure_write_drops_immediately_when_database_is_busy() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("busy.db");
        let storage = Storage::open(&path).unwrap();
        let blocker = rusqlite::Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();

        let started = Instant::now();
        let recorded = storage
            .try_record_rerank_exposure(&empty_impression())
            .unwrap();

        assert!(!recorded);
        assert!(started.elapsed() < Duration::from_millis(100));
        blocker.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn local_curated_cases_are_exactly_scoped_by_project_and_file() {
        let storage = Storage::open_memory().unwrap();
        insert_local_curated_fixture(
            &storage,
            "event-a",
            "session-a",
            "project-a",
            "src/storage/mod.rs",
        );
        insert_local_curated_fixture(
            &storage,
            "event-b",
            "session-b",
            "project-b",
            "src/storage/mod.rs",
        );
        insert_local_curated_fixture(
            &storage,
            "event-c",
            "session-c",
            "project-a",
            "src/search.rs",
        );

        let cases = storage.local_curated_rerank_cases(20).unwrap();

        assert_eq!(cases.len(), 3);
        assert_eq!(cases[0].project, "project-a");
        assert_eq!(cases[0].file_path, "src/search.rs");
        assert_eq!(cases[0].expected_sessions, vec!["session-c"]);
        assert_eq!(cases[1].project, "project-a");
        assert_eq!(cases[1].file_path, "src/storage/mod.rs");
        assert_eq!(cases[1].expected_sessions, vec!["session-a"]);
        assert_eq!(cases[2].project, "project-b");
        assert_eq!(cases[2].file_path, "src/storage/mod.rs");
        assert_eq!(cases[2].expected_sessions, vec!["session-b"]);
        assert!(cases
            .iter()
            .all(|case| case.query_text.contains(&case.file_path)));
    }

    #[test]
    fn reaction_join_rejects_labels_more_than_four_hours_after_exposure() {
        let storage = Storage::open_memory().unwrap();
        let mut impression = empty_impression();
        impression.feature_schema = crate::search::trained_rerank::FEATURE_SCHEMA;
        impression.items.push(ExposureItem {
            rank: 0,
            memory_id: "memory".into(),
            conversation_id: None,
            source_type: "chunk".into(),
            baseline_score: Some(0.5),
            cosine: Some(0.5),
            recency: Some(0.5),
            graph_proximity: None,
            author: Some("user".into()),
            is_scaffold: false,
            is_mechanic: false,
            supersedes: false,
        });
        storage.record_rerank_exposure(&impression).unwrap();
        storage
            .upsert_rerank_reaction_label(&ReactionLabel {
                session_id: "session".into(),
                assistant_turn: 2,
                next_user_turn: 3,
                assistant_ts: Some("2026-08-24T05:00:01Z".into()),
                next_user_ts: Some("2026-08-24T05:00:02Z".into()),
                reaction: "acceptance".into(),
                proposed_reaction: Some("acceptance".into()),
                confidence: 0.9,
                runner_up_score: 0.1,
                margin: 0.8,
                pickup_similarity: None,
                next_user_text: "yes".into(),
                near_miss: false,
                classifier_hash: "classifier".into(),
                transcript_mtime: 1,
                harvested_at: "2026-08-24T06:00:00Z".into(),
            })
            .unwrap();

        assert!(storage
            .load_labeled_rerank_exposures("classifier")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn reaction_join_bounds_the_complete_exposure_to_next_user_window_in_both_queries() {
        let storage = Storage::open_memory().unwrap();
        for (suffix, next_user_ts) in [
            ("late", "2026-08-24T05:00:00Z"),
            ("negative", "2026-08-23T23:59:59Z"),
        ] {
            let mut impression = empty_impression();
            impression.impression_id = format!("impression-{suffix}");
            impression.session_id = format!("session-{suffix}");
            impression.items.push(ExposureItem {
                rank: 0,
                memory_id: format!("memory-{suffix}"),
                conversation_id: None,
                source_type: "chunk".into(),
                baseline_score: Some(0.5),
                cosine: Some(0.5),
                recency: Some(0.5),
                graph_proximity: None,
                author: Some("user".into()),
                is_scaffold: false,
                is_mechanic: false,
                supersedes: false,
            });
            storage.record_rerank_exposure(&impression).unwrap();
            storage
                .upsert_rerank_reaction_label(&ReactionLabel {
                    session_id: impression.session_id,
                    assistant_turn: 2,
                    next_user_turn: 3,
                    assistant_ts: Some("2026-08-24T00:00:01Z".into()),
                    next_user_ts: Some(next_user_ts.into()),
                    reaction: "acceptance".into(),
                    proposed_reaction: Some("acceptance".into()),
                    confidence: 0.9,
                    runner_up_score: 0.1,
                    margin: 0.8,
                    pickup_similarity: None,
                    next_user_text: "yes".into(),
                    near_miss: false,
                    classifier_hash: "classifier".into(),
                    transcript_mtime: 1,
                    harvested_at: "2026-08-24T06:00:00Z".into(),
                })
                .unwrap();
        }

        assert!(storage
            .load_labeled_rerank_exposures("classifier")
            .unwrap()
            .is_empty());
        let stats = storage
            .get_rerank_reaction_stats_batch("classifier", &["memory-late", "memory-negative"])
            .unwrap();
        assert!(stats.is_empty());
    }

    #[test]
    fn latest_passing_attempt_survives_a_newer_error_receipt() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_rerank_model_attempt(
                &test_model_attempt("passed", "passed", "2026-08-24T00:00:00Z"),
                &[],
            )
            .unwrap();
        storage
            .insert_rerank_model_attempt(
                &test_model_attempt("error", "error", "2026-08-25T00:00:00Z"),
                &[],
            )
            .unwrap();

        assert_eq!(
            storage
                .latest_passing_rerank_model_attempt(crate::search::trained_rerank::FEATURE_SCHEMA)
                .unwrap()
                .unwrap()
                .model_id,
            "passed"
        );
        assert_eq!(
            storage
                .latest_rerank_model_attempt()
                .unwrap()
                .unwrap()
                .model_id,
            "error"
        );
    }

    #[test]
    fn model_attempt_and_nightly_cadence_are_one_atomic_write() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                conn.execute_batch(
                    "CREATE TRIGGER reject_trained_rerank_cadence
                     BEFORE INSERT ON meta
                     WHEN NEW.key = 'trained_rerank_last_attempt'
                     BEGIN
                         SELECT RAISE(ABORT, 'cadence write failed');
                     END;",
                )?;
                Ok(())
            })
            .unwrap();
        let attempt = test_model_attempt("atomic", "error", "2026-08-25T00:00:00Z");

        let result =
            storage.insert_rerank_model_attempt_with_cadence(&attempt, &[], "2026-08-25T00:00:00Z");

        assert!(result.is_err());
        assert!(storage.latest_rerank_model_attempt().unwrap().is_none());
        assert!(storage
            .get_meta("trained_rerank_last_attempt")
            .unwrap()
            .is_none());
    }
}
