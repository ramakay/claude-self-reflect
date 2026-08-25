//! Nightly reaction harvesting, training, and chronological gate orchestration.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use tokio::sync::{RwLock, Semaphore};

use crate::embeddings::EmbeddingEngine;
use crate::hooks::intent::cosine_sim;
use crate::hooks::reaction::{self, ProbeSet};
use crate::search::trained_rerank::{
    self as learning, feature_vector, FeatureInput, LinearModel, TrainingSample,
};
use crate::search::SearchEngine;
use crate::storage::trained_rerank::{
    ExposureImpression, ExposureItem, GateClusterReceipt, LabeledExposure, LegacyRetrievalEvent,
    ModelAttempt, ReactionLabel, ReactionLabelCounts, LAST_ATTEMPT_META_KEY,
    MAX_REACTION_GAP_SECONDS,
};
use crate::storage::Storage;
use crate::transcript::{Entry, Role};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HarvestSummary {
    pub sources_seen: usize,
    pub sources_harvested: usize,
    pub sources_unchanged: usize,
    pub contaminated: usize,
    pub labels: usize,
}

const TRAIN_SEED: u64 = 0x4353_525F_4C54_5231;
const MIN_EVAL_SESSIONS: usize = 5;
#[derive(Debug, Clone)]
struct PreparedCandidate {
    memory_id: String,
    baseline_score: f64,
    features: learning::FeatureRow,
}

#[derive(Debug, Clone)]
struct PreparedImpression {
    exposure: LabeledExposure,
    target: f64,
    reaction_weight: f64,
    candidates: Vec<PreparedCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReactionTurnPair {
    assistant_turn: usize,
    next_user_turn: usize,
    assistant_ts: Option<String>,
    next_user_ts: Option<String>,
    preceding_user_text: String,
    next_user_text: String,
}

fn substantive_user(entry: &Entry) -> bool {
    entry.role == Role::User
        && !entry.text.trim().is_empty()
        && !crate::extraction::provenance::is_csr_emission(&entry.text)
        && !crate::transcript::instrumentation::is_noisy_steer_text(&entry.text)
        && !entry.text.trim_start().starts_with("<local-command-")
}

fn substantive_reaction_user(entry: &Entry) -> bool {
    substantive_user(entry) && reaction::is_substantive_reaction_text(&entry.text)
}

fn canonical_timestamp(timestamp: Option<&str>) -> Option<String> {
    timestamp
        .and_then(crate::temporal::parse_timestamp)
        .map(|value| value.to_rfc3339())
}

fn reaction_turn_pairs(entries: &[Entry]) -> Vec<ReactionTurnPair> {
    let mut pairs = Vec::new();
    for (assistant_index, assistant) in entries.iter().enumerate() {
        if assistant.role != Role::Assistant || assistant.text.trim().is_empty() {
            continue;
        }
        let Some(assistant_time) = assistant
            .timestamp
            .as_deref()
            .and_then(crate::temporal::parse_timestamp)
        else {
            continue;
        };
        let Some(next_user) = entries[assistant_index + 1..].iter().find(|entry| {
            if !substantive_reaction_user(entry) {
                return false;
            }
            entry
                .timestamp
                .as_deref()
                .and_then(crate::temporal::parse_timestamp)
                .map(|timestamp| (timestamp - assistant_time).num_seconds())
                .is_some_and(|gap| (0..=MAX_REACTION_GAP_SECONDS).contains(&gap))
        }) else {
            continue;
        };
        let Some(assistant_ts) = canonical_timestamp(assistant.timestamp.as_deref()) else {
            continue;
        };
        let Some(next_user_ts) = canonical_timestamp(next_user.timestamp.as_deref()) else {
            continue;
        };
        let preceding_user_text = entries[..assistant_index]
            .iter()
            .rev()
            .find(|entry| substantive_user(entry))
            .map_or_else(String::new, |entry| entry.text.clone());
        pairs.push(ReactionTurnPair {
            assistant_turn: assistant.turn,
            next_user_turn: next_user.turn,
            assistant_ts: Some(assistant_ts),
            next_user_ts: Some(next_user_ts),
            preceding_user_text,
            next_user_text: next_user.text.clone(),
        });
    }
    pairs
}

fn conversation_is_contaminated(
    entries: &[Entry],
    csr_tool_blocks_suppressed: i64,
    csr_hook_wrappers_scrubbed: i64,
) -> bool {
    csr_tool_blocks_suppressed > 0
        || csr_hook_wrappers_scrubbed > 0
        || entries
            .iter()
            .any(|entry| crate::extraction::provenance::is_csr_emission(&entry.text))
}

fn transcript_mtime(path: &Path) -> Result<i64> {
    let modified = std::fs::metadata(path)?.modified()?;
    Ok(i64::try_from(modified.duration_since(UNIX_EPOCH)?.as_nanos()).unwrap_or(i64::MAX))
}

pub async fn harvest_reactions(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    shutdown: &AtomicBool,
) -> Result<HarvestSummary> {
    let Some(probes) = ProbeSet::load_or_build(embeddings).await else {
        anyhow::bail!("reaction exemplar probes could not be loaded or built");
    };
    let classifier_hash = reaction::classifier_hash();
    let mut summary = HarvestSummary::default();
    for source in storage.list_rerank_harvest_sources()? {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        summary.sources_seen += 1;
        let path = Path::new(&source.file_path);
        let Ok(mtime) = transcript_mtime(path) else {
            continue;
        };
        if storage.rerank_harvest_is_current(&source.session_id, &classifier_hash, mtime)? {
            summary.sources_unchanged += 1;
            continue;
        }
        let Ok(parsed) = crate::transcript::parse_transcript(path) else {
            continue;
        };
        let harvested_at = chrono::Utc::now().to_rfc3339();
        let contaminated = conversation_is_contaminated(
            &parsed.entries,
            source.csr_tool_blocks_suppressed,
            source.csr_hook_wrappers_scrubbed,
        );
        if contaminated {
            storage.replace_rerank_session_labels(
                &source.session_id,
                &classifier_hash,
                mtime,
                true,
                &harvested_at,
                &[],
            )?;
            summary.contaminated += 1;
            summary.sources_harvested += 1;
            continue;
        }
        let pairs = reaction_turn_pairs(&parsed.entries);
        let texts: Vec<String> = pairs
            .iter()
            .flat_map(|pair| {
                [
                    pair.next_user_text.clone(),
                    pair.preceding_user_text.clone(),
                ]
            })
            .collect();
        let vectors = if texts.is_empty() {
            Vec::new()
        } else {
            let engine = embeddings.clone();
            tokio::task::spawn_blocking(move || {
                let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                engine.embed(&refs)
            })
            .await??
        };
        if vectors.len() != pairs.len() * 2 {
            anyhow::bail!(
                "embedding count mismatch: expected {}, received {}",
                pairs.len() * 2,
                vectors.len()
            );
        }
        let mut labels = Vec::with_capacity(pairs.len());
        for (index, pair) in pairs.into_iter().enumerate() {
            let next_vector = &vectors[index * 2];
            let prompt_vector = &vectors[index * 2 + 1];
            let pickup_similarity =
                reaction::pickup_is_eligible(&pair.next_user_text, &pair.preceding_user_text)
                    .then(|| cosine_sim(next_vector, prompt_vector));
            let decision = probes.classify(
                &pair.next_user_text,
                &pair.preceding_user_text,
                next_vector,
                pickup_similarity,
            );
            labels.push(ReactionLabel {
                session_id: source.session_id.clone(),
                assistant_turn: pair.assistant_turn as i64,
                next_user_turn: pair.next_user_turn as i64,
                assistant_ts: pair.assistant_ts,
                next_user_ts: pair.next_user_ts,
                reaction: decision
                    .reaction
                    .map_or_else(|| "abstain".to_string(), |value| value.as_str().to_string()),
                proposed_reaction: decision
                    .proposed_reaction
                    .map(|value| value.as_str().to_string()),
                confidence: f64::from(decision.confidence),
                runner_up_score: f64::from(decision.runner_up_score),
                margin: f64::from(decision.margin),
                pickup_similarity: decision.pickup_similarity.map(f64::from),
                next_user_text: pair.next_user_text,
                near_miss: decision.near_miss,
                classifier_hash: classifier_hash.clone(),
                transcript_mtime: mtime,
                harvested_at: harvested_at.clone(),
            });
        }
        storage.replace_rerank_session_labels(
            &source.session_id,
            &classifier_hash,
            mtime,
            false,
            &harvested_at,
            &labels,
        )?;
        summary.labels += labels.len();
        summary.sources_harvested += 1;
    }
    Ok(summary)
}

fn prepare_impressions(exposures: &[LabeledExposure]) -> Vec<PreparedImpression> {
    let mut reaction_counts: std::collections::HashMap<(&str, i64), usize> =
        std::collections::HashMap::new();
    for exposure in exposures.iter().filter(|exposure| {
        !exposure.legacy
            && matches!(
                exposure.reaction.as_str(),
                "acceptance" | "correction" | "reask"
            )
    }) {
        *reaction_counts
            .entry((&exposure.session_id, exposure.reaction_turn))
            .or_default() += 1;
    }
    let mut prepared = Vec::new();
    for exposure in exposures {
        // Legacy retrieval events lack the exact scorer features required by
        // the current schema.
        if exposure.legacy {
            continue;
        }
        if exposure.reaction == "redirect" {
            continue;
        }
        let target = match exposure.reaction.as_str() {
            "acceptance" => 1.0,
            "correction" | "reask" => 0.0,
            _ => continue,
        };
        let item_count = exposure.items.len();
        let candidates: Vec<PreparedCandidate> = exposure
            .items
            .iter()
            .map(|item| {
                let baseline_score = item.baseline_score.unwrap_or(0.0);
                let rank_percentile = if item_count <= 1 {
                    0.0
                } else {
                    item.rank as f64 / (item_count - 1) as f64
                };
                let features = feature_vector(&FeatureInput {
                    cosine: item.cosine,
                    decayed_score: item.baseline_score,
                    recency: item.recency,
                    graph_proximity: item.graph_proximity,
                    baseline_score: item.baseline_score,
                    source_type: &item.source_type,
                    intent: &exposure.intent,
                    author: item.author.as_deref(),
                    is_scaffold: item.is_scaffold,
                    is_mechanic: item.is_mechanic,
                    supersedes: item.supersedes,
                    shown_rank_percentile: Some(rank_percentile),
                    impression_size: Some(item_count as f64),
                    legacy: Some(exposure.legacy),
                });
                PreparedCandidate {
                    memory_id: item.memory_id.clone(),
                    baseline_score,
                    features,
                }
            })
            .collect();
        if candidates.is_empty() {
            continue;
        }
        prepared.push(PreparedImpression {
            exposure: exposure.clone(),
            target,
            reaction_weight: 1.0
                / reaction_counts
                    .get(&(exposure.session_id.as_str(), exposure.reaction_turn))
                    .copied()
                    .unwrap_or(1) as f64,
            candidates,
        });
    }
    prepared
}

fn legacy_event_millis(event: &LegacyRetrievalEvent) -> Option<i64> {
    crate::temporal::parse_timestamp(&event.retrieved_at).map(|time| time.timestamp_millis())
}

fn backfill_legacy_exposures(storage: &Storage) -> Result<usize> {
    let events = storage.list_legacy_prompt_retrievals()?;
    let mut groups: Vec<Vec<LegacyRetrievalEvent>> = Vec::new();
    for event in events {
        let joins_last = groups.last().is_some_and(|group| {
            group.first().is_some_and(|first| {
                first.session_id == event.session_id
                    && legacy_event_millis(first)
                        .zip(legacy_event_millis(&event))
                        .is_some_and(|(left, right)| (right - left).abs() <= 100)
            })
        });
        if joins_last {
            groups.last_mut().expect("legacy group exists").push(event);
        } else {
            groups.push(vec![event]);
        }
    }
    let mut inserted = 0;
    for group in groups {
        let Some(first) = group.first() else {
            continue;
        };
        let key = format!("{}|{}", first.session_id, first.retrieved_at);
        let mut seen = std::collections::HashSet::new();
        let items: Vec<ExposureItem> = group
            .iter()
            .filter(|event| seen.insert((event.memory_id.clone(), event.memory_type.clone())))
            .enumerate()
            .map(|(rank, event)| ExposureItem {
                rank: rank as i64,
                memory_id: event.memory_id.clone(),
                conversation_id: event.conversation_id.clone(),
                source_type: event.memory_type.clone(),
                baseline_score: None,
                cosine: None,
                recency: None,
                graph_proximity: None,
                author: None,
                is_scaffold: false,
                is_mechanic: false,
                supersedes: false,
            })
            .collect();
        if items.is_empty() {
            continue;
        }
        storage.record_rerank_exposure(&ExposureImpression {
            impression_id: format!("legacy:{}", blake3::hash(key.as_bytes()).to_hex()),
            session_id: first.session_id.clone(),
            project: first.project.clone(),
            surface: "prompt_submit".into(),
            query_hash: None,
            query_embedding: None,
            intent: "other".into(),
            shown_at: first.retrieved_at.clone(),
            feature_schema: 0,
            legacy: true,
            items,
        })?;
        inserted += 1;
    }
    Ok(inserted)
}

fn training_samples(impressions: &[PreparedImpression]) -> Vec<TrainingSample> {
    impressions
        .iter()
        .flat_map(|impression| {
            let weight = impression.reaction_weight / impression.candidates.len() as f64;
            impression
                .candidates
                .iter()
                .map(move |candidate| TrainingSample {
                    features: candidate.features.values,
                    available: candidate.features.available,
                    target: impression.target,
                    weight,
                })
        })
        .collect()
}

#[derive(Debug)]
struct EvalCluster<'a> {
    anchor: &'a [f32],
    project: &'a str,
    intent: &'a str,
    impressions: Vec<&'a PreparedImpression>,
}

fn cluster_heldout(impressions: &[PreparedImpression]) -> Vec<EvalCluster<'_>> {
    let mut clusters: Vec<EvalCluster<'_>> = Vec::new();
    let mut ordered: Vec<_> = impressions.iter().collect();
    ordered.sort_by(|left, right| {
        left.exposure
            .project
            .cmp(&right.exposure.project)
            .then_with(|| left.exposure.intent.cmp(&right.exposure.intent))
            .then_with(|| {
                left.exposure
                    .query_embedding
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .zip(
                        right
                            .exposure
                            .query_embedding
                            .as_deref()
                            .unwrap_or_default(),
                    )
                    .map(|(left, right)| left.total_cmp(right))
                    .find(|ordering| !ordering.is_eq())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                left.exposure
                    .impression_id
                    .cmp(&right.exposure.impression_id)
            })
    });
    for impression in ordered {
        let Some(query_embedding) = impression.exposure.query_embedding.as_deref() else {
            continue;
        };
        if let Some(cluster) = clusters.iter_mut().find(|cluster| {
            cluster.project == impression.exposure.project
                && cluster.intent == impression.exposure.intent
                && cosine_sim(cluster.anchor, query_embedding) >= 0.82
        }) {
            cluster.impressions.push(impression);
        } else {
            clusters.push(EvalCluster {
                anchor: query_embedding,
                project: &impression.exposure.project,
                intent: &impression.exposure.intent,
                impressions: vec![impression],
            });
        }
    }
    clusters
}

struct CandidateAggregate {
    relevance_sum: f64,
    baseline_sum: f64,
    features_sum: [f64; learning::FEATURE_COUNT],
    feature_count: [usize; learning::FEATURE_COUNT],
    count: usize,
}

impl Default for CandidateAggregate {
    fn default() -> Self {
        Self {
            relevance_sum: 0.0,
            baseline_sum: 0.0,
            features_sum: [0.0; learning::FEATURE_COUNT],
            feature_count: [0; learning::FEATURE_COUNT],
            count: 0,
        }
    }
}

fn evaluate_clusters(
    model_id: &str,
    model: &LinearModel,
    heldout: &[PreparedImpression],
) -> (Vec<GateClusterReceipt>, f64, f64, f64, f64, usize) {
    let mut receipts = Vec::new();
    let mut baseline_mrrs = Vec::new();
    let mut trained_mrrs = Vec::new();
    let mut valid_sessions = std::collections::HashSet::new();
    for cluster in cluster_heldout(heldout) {
        let distinct_reactions = cluster
            .impressions
            .iter()
            .map(|impression| {
                (
                    impression.exposure.session_id.as_str(),
                    impression.exposure.reaction_turn,
                )
            })
            .collect::<std::collections::HashSet<_>>()
            .len();
        let distinct_sessions = cluster
            .impressions
            .iter()
            .map(|impression| impression.exposure.session_id.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len();
        if distinct_reactions < 2 || distinct_sessions < 2 {
            continue;
        }
        let mut candidates: std::collections::BTreeMap<String, CandidateAggregate> =
            std::collections::BTreeMap::new();
        for impression in &cluster.impressions {
            let relevance = if impression.target == 1.0 { 3.0 } else { 0.0 };
            for candidate in &impression.candidates {
                let aggregate = candidates.entry(candidate.memory_id.clone()).or_default();
                aggregate.relevance_sum += relevance;
                aggregate.baseline_sum += candidate.baseline_score;
                for (index, value) in candidate.features.values.iter().enumerate() {
                    if candidate.features.available[index] {
                        aggregate.features_sum[index] += value;
                        aggregate.feature_count[index] += 1;
                    }
                }
                aggregate.count += 1;
            }
        }
        let has_relevant = candidates
            .values()
            .any(|candidate| candidate.relevance_sum > 0.0);
        let has_non_relevant = candidates
            .values()
            .any(|candidate| candidate.relevance_sum <= f64::EPSILON);
        if candidates.len() < learning::MIN_CLUSTER_CANDIDATES || !has_relevant || !has_non_relevant
        {
            continue;
        }
        let mut rows: Vec<(String, f64, f64, f64)> = candidates
            .into_iter()
            .filter_map(|(memory_id, aggregate)| {
                let count = aggregate.count as f64;
                let relevance = aggregate.relevance_sum / count;
                let baseline = aggregate.baseline_sum / count;
                let mut features = learning::FeatureRow {
                    values: aggregate.features_sum,
                    available: [false; learning::FEATURE_COUNT],
                };
                for index in 0..learning::FEATURE_COUNT {
                    if aggregate.feature_count[index] > 0 {
                        features.values[index] /= aggregate.feature_count[index] as f64;
                        features.available[index] = true;
                    }
                }
                features.available[learning::NUISANCE_FEATURES].fill(false);
                let trained = baseline + learning::bounded_residual(model.probability(&features)?);
                Some((memory_id, relevance, baseline, trained))
            })
            .collect();
        let candidate_count = rows.len();
        rows.sort_by(|left, right| {
            right
                .2
                .partial_cmp(&left.2)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let baseline_relevance: Vec<f64> = rows.iter().map(|row| row.1).collect();
        let baseline_ndcg = crate::eval::trained_rerank::ndcg_at_k(&baseline_relevance, 5);
        let baseline_mrr = crate::eval::trained_rerank::mrr(&baseline_relevance);
        rows.sort_by(|left, right| {
            right
                .3
                .partial_cmp(&left.3)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let trained_relevance: Vec<f64> = rows.iter().map(|row| row.1).collect();
        let trained_ndcg = crate::eval::trained_rerank::ndcg_at_k(&trained_relevance, 5);
        let trained_mrr = crate::eval::trained_rerank::mrr(&trained_relevance);
        let outcome = if trained_ndcg > baseline_ndcg + f64::EPSILON {
            "win"
        } else if baseline_ndcg > trained_ndcg + f64::EPSILON {
            "loss"
        } else {
            "tie"
        };
        receipts.push(GateClusterReceipt {
            model_id: model_id.to_string(),
            cluster_id: cluster.impressions[0].exposure.impression_id.clone(),
            impression_count: cluster.impressions.len() as i64,
            distinct_session_count: distinct_sessions as i64,
            candidate_count: candidate_count as i64,
            baseline_ndcg5: baseline_ndcg,
            trained_ndcg5: trained_ndcg,
            outcome: outcome.into(),
        });
        valid_sessions.extend(
            cluster
                .impressions
                .iter()
                .map(|impression| impression.exposure.session_id.clone()),
        );
        baseline_mrrs.push(baseline_mrr);
        trained_mrrs.push(trained_mrr);
    }
    let mean = |values: &[f64]| {
        if values.is_empty() {
            0.0
        } else {
            values.iter().sum::<f64>() / values.len() as f64
        }
    };
    let baseline_ndcgs: Vec<f64> = receipts.iter().map(|row| row.baseline_ndcg5).collect();
    let trained_ndcgs: Vec<f64> = receipts.iter().map(|row| row.trained_ndcg5).collect();
    (
        receipts,
        mean(&baseline_ndcgs),
        mean(&trained_ndcgs),
        mean(&baseline_mrrs),
        mean(&trained_mrrs),
        valid_sessions.len(),
    )
}

fn evaluation_floor_is_met(valid_clusters: usize, distinct_sessions: usize) -> bool {
    valid_clusters >= learning::MIN_EVAL_CLUSTERS && distinct_sessions >= MIN_EVAL_SESSIONS
}

#[allow(clippy::too_many_arguments)]
fn insufficient_attempt(
    model_id: String,
    classifier_hash: String,
    counts: &ReactionLabelCounts,
    contaminated: i64,
    train: &[PreparedImpression],
    heldout: &[PreparedImpression],
    cutoff: Option<String>,
    reason: String,
) -> ModelAttempt {
    ModelAttempt {
        model_id,
        feature_schema: learning::FEATURE_SCHEMA,
        classifier_hash,
        seed: TRAIN_SEED as i64,
        cutoff_ts: cutoff,
        train_start_ts: train.first().map(|row| row.exposure.reaction_at.clone()),
        train_end_ts: train.last().map(|row| row.exposure.reaction_at.clone()),
        eval_start_ts: heldout.first().map(|row| row.exposure.reaction_at.clone()),
        eval_end_ts: heldout.last().map(|row| row.exposure.reaction_at.clone()),
        train_impressions: train.len() as i64,
        train_rows: train.iter().map(|row| row.candidates.len() as i64).sum(),
        eval_impressions: heldout.len() as i64,
        eval_rows: heldout.iter().map(|row| row.candidates.len() as i64).sum(),
        eval_clusters: 0,
        cluster_wins: 0,
        cluster_losses: 0,
        cluster_ties: 0,
        excluded_contaminated: contaminated,
        abstained_reactions: counts.abstain,
        acceptance_labels: counts.acceptance,
        correction_labels: counts.correction,
        reask_labels: counts.reask,
        redirect_labels: counts.redirect,
        near_miss_labels: counts.near_miss,
        baseline_ndcg5: None,
        trained_ndcg5: None,
        baseline_mrr: None,
        trained_mrr: None,
        curated_baseline_score: None,
        curated_trained_score: None,
        curated_case_count: 0,
        curated_veto_epsilon: crate::storage::trained_rerank::CURATED_VETO_EPSILON,
        gate_status: "insufficient_data".into(),
        gate_reason: reason,
        weights_json: None,
        normalization_json: None,
        trained_at: chrono::Utc::now().to_rfc3339(),
    }
}

fn persist_cycle_attempt(
    storage: &Storage,
    attempt: &ModelAttempt,
    receipts: &[GateClusterReceipt],
) -> Result<()> {
    storage.insert_rerank_model_attempt_with_cadence(attempt, receipts, &attempt.trained_at)
}

pub async fn run_training_cycle(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    shutdown: &AtomicBool,
) -> Result<ModelAttempt> {
    let _harvest = harvest_reactions(storage, embeddings, shutdown).await?;
    let _legacy_impressions = backfill_legacy_exposures(storage)?;
    let classifier_hash = reaction::classifier_hash();
    let counts = storage.rerank_reaction_label_counts(&classifier_hash)?;
    let contaminated = storage.rerank_contaminated_session_count(&classifier_hash)?;
    let mut exposures = storage.load_labeled_rerank_exposures(&classifier_hash)?;
    exposures.sort_by(|left, right| {
        left.reaction_at
            .cmp(&right.reaction_at)
            .then_with(|| left.impression_id.cmp(&right.impression_id))
    });
    let model_id = uuid::Uuid::new_v4().to_string();
    if exposures.is_empty() {
        let attempt = insufficient_attempt(
            model_id,
            classifier_hash,
            &counts,
            contaminated,
            &[],
            &[],
            None,
            format!(
                "need at least {} labeled impressions; found 0",
                learning::MIN_TRAIN_IMPRESSIONS + learning::MIN_EVAL_IMPRESSIONS
            ),
        );
        persist_cycle_attempt(storage, &attempt, &[])?;
        return Ok(attempt);
    }
    let cutoff_index = exposures.len() * 4 / 5;
    let cutoff = exposures[cutoff_index].reaction_at.clone();
    let (train_exposures, heldout_exposures): (Vec<_>, Vec<_>) = exposures
        .into_iter()
        .partition(|exposure| exposure.reaction_at < cutoff);
    let train = prepare_impressions(&train_exposures);
    let heldout = prepare_impressions(&heldout_exposures);
    let train_classes = train.iter().fold([false; 2], |mut classes, impression| {
        classes[impression.target as usize] = true;
        classes
    });
    let eval_classes = heldout.iter().fold([false; 2], |mut classes, impression| {
        classes[impression.target as usize] = true;
        classes
    });
    if train.len() < learning::MIN_TRAIN_IMPRESSIONS
        || heldout.len() < learning::MIN_EVAL_IMPRESSIONS
        || !train_classes.into_iter().all(|present| present)
        || !eval_classes.into_iter().all(|present| present)
    {
        let attempt = insufficient_attempt(
            model_id,
            classifier_hash,
            &counts,
            contaminated,
            &train,
            &heldout,
            Some(cutoff),
            "chronological split lacks impression floors or both target classes".into(),
        );
        persist_cycle_attempt(storage, &attempt, &[])?;
        return Ok(attempt);
    }
    let samples = training_samples(&train);
    let model = learning::train(&samples, TRAIN_SEED)?;
    let (receipts, baseline_ndcg, trained_ndcg, baseline_mrr, trained_mrr, eval_distinct_sessions) =
        evaluate_clusters(&model_id, &model, &heldout);
    let wins = receipts.iter().filter(|row| row.outcome == "win").count();
    let losses = receipts.iter().filter(|row| row.outcome == "loss").count();
    let ties = receipts.len() - wins - losses;
    let (weights_json, normalization_json) = model.persistence_json()?;
    if !evaluation_floor_is_met(receipts.len(), eval_distinct_sessions) {
        let mut attempt = insufficient_attempt(
            model_id,
            classifier_hash,
            &counts,
            contaminated,
            &train,
            &heldout,
            Some(cutoff),
            format!(
                "need at least {} evaluable held-out clusters spanning {} sessions; found {} clusters spanning {} sessions",
                learning::MIN_EVAL_CLUSTERS,
                MIN_EVAL_SESSIONS,
                receipts.len(),
                eval_distinct_sessions,
            ),
        );
        attempt.eval_clusters = receipts.len() as i64;
        attempt.cluster_wins = wins as i64;
        attempt.cluster_losses = losses as i64;
        attempt.cluster_ties = ties as i64;
        attempt.baseline_ndcg5 = Some(baseline_ndcg);
        attempt.trained_ndcg5 = Some(trained_ndcg);
        attempt.baseline_mrr = Some(baseline_mrr);
        attempt.trained_mrr = Some(trained_mrr);
        attempt.weights_json = Some(weights_json);
        attempt.normalization_json = Some(normalization_json);
        persist_cycle_attempt(storage, &attempt, &receipts)?;
        return Ok(attempt);
    }
    let chronological_passed = learning::gate_passes(baseline_ndcg, trained_ndcg, wins, losses);
    let mut attempt = insufficient_attempt(
        model_id,
        classifier_hash,
        &counts,
        contaminated,
        &train,
        &heldout,
        Some(cutoff),
        String::new(),
    );
    attempt.eval_clusters = receipts.len() as i64;
    attempt.cluster_wins = wins as i64;
    attempt.cluster_losses = losses as i64;
    attempt.cluster_ties = ties as i64;
    attempt.baseline_ndcg5 = Some(baseline_ndcg);
    attempt.trained_ndcg5 = Some(trained_ndcg);
    attempt.baseline_mrr = Some(baseline_mrr);
    attempt.trained_mrr = Some(trained_mrr);
    attempt.weights_json = Some(weights_json);
    attempt.normalization_json = Some(normalization_json);
    if !chronological_passed {
        attempt.gate_status = "failed".into();
        attempt.gate_reason =
            "trained model did not strictly win both mean NDCG@5 and cluster majority".into();
    } else {
        let curated_result =
            crate::eval::trained_rerank::run_curated_veto(storage, embeddings, search, &model)
                .await;
        match curated_result {
            Ok(crate::eval::trained_rerank::CuratedEvaluation::Scores { scores, case_count }) => {
                attempt.curated_case_count = case_count as i64;
                attempt.curated_baseline_score = Some(scores.baseline_mrr);
                attempt.curated_trained_score = Some(scores.trained_mrr);
                match crate::eval::trained_rerank::decide_curated_veto(Ok(scores)) {
                    crate::eval::trained_rerank::CuratedVetoDecision::Passed => {
                        attempt.gate_status = "passed".into();
                        attempt.gate_reason =
                            "chronological gate passed and curated MRR did not regress".into();
                    }
                    crate::eval::trained_rerank::CuratedVetoDecision::Regression => {
                        attempt.gate_status = "failed".into();
                        attempt.gate_reason = "curated_regression".into();
                    }
                    crate::eval::trained_rerank::CuratedVetoDecision::Error(reason) => {
                        attempt.curated_baseline_score = None;
                        attempt.curated_trained_score = None;
                        attempt.gate_status = "error".into();
                        attempt.gate_reason = reason;
                    }
                }
            }
            Ok(crate::eval::trained_rerank::CuratedEvaluation::InsufficientData { case_count }) => {
                attempt.curated_case_count = case_count as i64;
                attempt.gate_status = "insufficient_data".into();
                attempt.gate_reason = format!(
                    "curated eval needs at least {} corpus-local cases; found {case_count}",
                    crate::eval::trained_rerank::MIN_CURATED_CASES
                );
            }
            Err(error) => {
                attempt.gate_status = "error".into();
                attempt.gate_reason =
                    match crate::eval::trained_rerank::decide_curated_veto(Err(error)) {
                        crate::eval::trained_rerank::CuratedVetoDecision::Error(reason) => reason,
                        _ => unreachable!("an unavailable curated eval must produce an error"),
                    };
            }
        }
    }
    persist_cycle_attempt(storage, &attempt, &receipts)?;
    Ok(attempt)
}

fn training_due(last_attempt: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> bool {
    let Some(last_attempt) = last_attempt.and_then(crate::temporal::parse_timestamp) else {
        return true;
    };
    now.signed_duration_since(last_attempt) >= chrono::Duration::hours(24)
}

fn persist_error_attempt(storage: &Storage, error: &anyhow::Error) -> Result<()> {
    let classifier_hash = reaction::classifier_hash();
    let counts = storage
        .rerank_reaction_label_counts(&classifier_hash)
        .unwrap_or_default();
    let contaminated = storage
        .rerank_contaminated_session_count(&classifier_hash)
        .unwrap_or(0);
    let attempt = ModelAttempt {
        model_id: uuid::Uuid::new_v4().to_string(),
        feature_schema: learning::FEATURE_SCHEMA,
        classifier_hash,
        seed: TRAIN_SEED as i64,
        cutoff_ts: None,
        train_start_ts: None,
        train_end_ts: None,
        eval_start_ts: None,
        eval_end_ts: None,
        train_impressions: 0,
        train_rows: 0,
        eval_impressions: 0,
        eval_rows: 0,
        eval_clusters: 0,
        cluster_wins: 0,
        cluster_losses: 0,
        cluster_ties: 0,
        excluded_contaminated: contaminated,
        abstained_reactions: counts.abstain,
        acceptance_labels: counts.acceptance,
        correction_labels: counts.correction,
        reask_labels: counts.reask,
        redirect_labels: counts.redirect,
        near_miss_labels: counts.near_miss,
        baseline_ndcg5: None,
        trained_ndcg5: None,
        baseline_mrr: None,
        trained_mrr: None,
        curated_baseline_score: None,
        curated_trained_score: None,
        curated_case_count: 0,
        curated_veto_epsilon: crate::storage::trained_rerank::CURATED_VETO_EPSILON,
        gate_status: "error".into(),
        gate_reason: error.to_string(),
        weights_json: None,
        normalization_json: None,
        trained_at: chrono::Utc::now().to_rfc3339(),
    };
    persist_cycle_attempt(storage, &attempt, &[])
}

pub async fn nightly_loop(
    storage: Arc<Storage>,
    embeddings: Arc<EmbeddingEngine>,
    search: Arc<RwLock<SearchEngine>>,
    heavy_work: Arc<Semaphore>,
    shutdown: Arc<AtomicBool>,
) {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        let last = storage.get_meta(LAST_ATTEMPT_META_KEY).ok().flatten();
        if training_due(last.as_deref(), chrono::Utc::now()) {
            let permit = match heavy_work.clone().acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => return,
            };
            if shutdown.load(Ordering::SeqCst) {
                drop(permit);
                return;
            }
            let cycle_result = run_training_cycle(&storage, &embeddings, &search, &shutdown).await;
            match cycle_result {
                Ok(attempt) => {
                    tracing::info!(
                        model_id = %attempt.model_id,
                        gate_status = %attempt.gate_status,
                        train_impressions = attempt.train_impressions,
                        eval_impressions = attempt.eval_impressions,
                        eval_clusters = attempt.eval_clusters,
                        cluster_wins = attempt.cluster_wins,
                        cluster_losses = attempt.cluster_losses,
                        cluster_ties = attempt.cluster_ties,
                        "trained re-ranker nightly attempt persisted"
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, "trained re-ranker cycle failed");
                    if let Err(persist_error) = persist_error_attempt(&storage, &error) {
                        tracing::warn!(%persist_error, "trained re-ranker error receipt failed");
                    }
                }
            }
            drop(permit);
        }
        for _ in 0..60 {
            if shutdown.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::transcript::{Entry, Role, ToolResult};

    use super::*;

    fn prepared_for_cluster(
        id: &str,
        embedding: Vec<f32>,
        target: f64,
        reaction_turn: i64,
        candidate_count: usize,
    ) -> PreparedImpression {
        PreparedImpression {
            exposure: LabeledExposure {
                impression_id: id.into(),
                session_id: format!("session-{id}"),
                project: "project".into(),
                intent: "explore".into(),
                shown_at: "2026-08-24T00:00:00Z".into(),
                reaction_at: "2026-08-24T00:01:00Z".into(),
                reaction_turn,
                reaction: if target == 1.0 {
                    "acceptance".into()
                } else {
                    "correction".into()
                },
                query_embedding: Some(embedding),
                legacy: false,
                items: Vec::new(),
            },
            target,
            reaction_weight: 1.0,
            candidates: (0..candidate_count)
                .map(|index| PreparedCandidate {
                    memory_id: format!("memory-{index}"),
                    baseline_score: 1.0 - index as f64 * 0.05,
                    features: feature_vector(&FeatureInput {
                        cosine: Some(0.8),
                        decayed_score: Some(0.8),
                        recency: Some(0.8),
                        graph_proximity: None,
                        baseline_score: Some(0.8),
                        source_type: "chunk",
                        intent: "explore",
                        author: Some("user"),
                        is_scaffold: false,
                        is_mechanic: false,
                        supersedes: false,
                        shown_rank_percentile: Some(0.0),
                        impression_size: Some(candidate_count as f64),
                        legacy: Some(false),
                    }),
                })
                .collect(),
        }
    }

    fn entry(turn: usize, role: Role, text: &str) -> Entry {
        Entry {
            turn,
            role,
            timestamp: Some(format!("2026-08-24T00:00:{turn:02}Z")),
            uuid: None,
            is_sidechain: false,
            text: text.into(),
            tool_uses: Vec::new(),
            tool_results: Vec::new(),
        }
    }

    #[test]
    fn reaction_alignment_skips_tool_result_only_user_entries() {
        let mut tool_result = entry(3, Role::User, "");
        tool_result.tool_results.push(ToolResult {
            tool_use_id: Some("tool-1".into()),
            is_error: false,
            byte_size: 2,
            preview: "ok".into(),
        });
        let entries = vec![
            entry(1, Role::User, "original request"),
            entry(2, Role::Assistant, "answer"),
            tool_result,
            entry(4, Role::Assistant, "tool follow-up"),
            entry(5, Role::User, "yes, that solved it"),
        ];

        let pairs = reaction_turn_pairs(&entries);

        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].assistant_turn, 2);
        assert_eq!(pairs[0].next_user_turn, 5);
        assert_eq!(pairs[0].preceding_user_text, "original request");
    }

    #[test]
    fn contamination_is_a_predicate_not_a_hard_coded_session_list() {
        let clean = vec![entry(1, Role::User, "normal request")];
        assert!(!conversation_is_contaminated(&clean, 0, 0));
        assert!(conversation_is_contaminated(&clean, 1, 0));
        let emitted = vec![entry(
            1,
            Role::Assistant,
            "CSR ENDLESS MEMORY ACTIVE — prior context",
        )];
        assert!(conversation_is_contaminated(&emitted, 0, 0));
    }

    #[test]
    fn reaction_alignment_skips_harness_notifications() {
        let entries = vec![
            entry(1, Role::User, "original request"),
            entry(2, Role::Assistant, "answer"),
            entry(
                3,
                Role::User,
                "<task-notification>background job completed</task-notification>",
            ),
            entry(4, Role::User, "no, that is the wrong behavior"),
        ];
        let pairs = reaction_turn_pairs(&entries);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].next_user_turn, 4);
    }

    #[test]
    fn reaction_alignment_walks_past_non_reaction_artifacts() {
        let entries = vec![
            entry(1, Role::User, "Can you fix the failing server?"),
            entry(2, Role::Assistant, "I restarted it."),
            entry(
                3,
                Role::User,
                "[Image: original 1200x800, displayed at 800x533]",
            ),
            entry(
                4,
                Role::User,
                "Base directory for this skill: /Users/example/.agents/skills/test",
            ),
            entry(5, Role::User, "/compact"),
            entry(6, Role::User, "[Request interrupted by user]"),
            entry(7, Role::User, "[queued] check the logs too"),
            entry(8, Role::User, "No, the server is still down."),
        ];

        let pairs = reaction_turn_pairs(&entries);

        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].next_user_turn, 8);
        assert_eq!(pairs[0].next_user_text, "No, the server is still down.");
    }

    #[test]
    fn reaction_alignment_canonicalizes_offsets_before_sql_time_matching() {
        let mut assistant = entry(2, Role::Assistant, "answer");
        assistant.timestamp = Some("2026-08-23T17:00:02-07:00".into());
        let mut user = entry(3, Role::User, "yes, that worked");
        user.timestamp = Some("2026-08-23T17:00:03-07:00".into());

        let pairs = reaction_turn_pairs(&[entry(1, Role::User, "request"), assistant, user]);

        assert_eq!(
            pairs[0].assistant_ts.as_deref(),
            Some("2026-08-24T00:00:02+00:00")
        );
        assert_eq!(
            pairs[0].next_user_ts.as_deref(),
            Some("2026-08-24T00:00:03+00:00")
        );
    }

    #[test]
    fn nightly_cadence_is_immediately_due_then_waits_twenty_four_hours() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-24T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(training_due(None, now));
        assert!(!training_due(Some("2026-08-24T11:59:00Z"), now));
        assert!(training_due(Some("2026-08-23T11:59:00Z"), now));
    }

    #[test]
    fn one_reaction_has_total_weight_one_across_shared_impressions() {
        let exposure = |impression_id: &str| LabeledExposure {
            impression_id: impression_id.into(),
            session_id: "session".into(),
            project: "project".into(),
            intent: "other".into(),
            shown_at: "2026-08-24T00:00:00Z".into(),
            reaction_at: "2026-08-24T00:01:00Z".into(),
            reaction_turn: 7,
            reaction: "acceptance".into(),
            query_embedding: Some(vec![1.0, 0.0]),
            legacy: false,
            items: vec![ExposureItem {
                rank: 0,
                memory_id: format!("memory-{impression_id}"),
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
            }],
        };
        let prepared = prepare_impressions(&[exposure("a"), exposure("b")]);
        let total_weight: f64 = training_samples(&prepared)
            .iter()
            .map(|sample| sample.weight)
            .sum();
        assert_eq!(total_weight, 1.0);
    }

    #[test]
    fn cluster_anchors_are_stable_under_input_reordering() {
        let a = prepared_for_cluster("a", vec![1.0, 0.0], 1.0, 1, 5);
        let b = prepared_for_cluster("b", vec![0.99, 0.01], 0.0, 2, 5);
        let c = prepared_for_cluster("c", vec![0.0, 1.0], 1.0, 3, 5);
        let cluster_ids = |rows: &[PreparedImpression]| {
            cluster_heldout(rows)
                .into_iter()
                .map(|cluster| cluster.impressions[0].exposure.impression_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            cluster_ids(&[b.clone(), c.clone(), a.clone()]),
            cluster_ids(&[c, a, b])
        );
    }

    #[test]
    fn heldout_cluster_needs_at_least_five_distinct_candidates() {
        let heldout = vec![
            prepared_for_cluster("a", vec![1.0, 0.0], 1.0, 1, 4),
            prepared_for_cluster("b", vec![0.99, 0.01], 0.0, 2, 4),
        ];
        let model = LinearModel {
            weights: vec![0.0; learning::FEATURE_COUNT],
            bias: 0.0,
            normalization: learning::Normalization {
                means: vec![0.0; learning::FEATURE_COUNT],
                scales: vec![1.0; learning::FEATURE_COUNT],
            },
            seed: 7,
        };
        assert!(evaluate_clusters("model", &model, &heldout).0.is_empty());
    }

    #[test]
    fn heldout_cluster_needs_reactions_from_two_distinct_sessions() {
        let first = prepared_for_cluster("a", vec![1.0, 0.0], 1.0, 1, 5);
        let mut second = prepared_for_cluster("b", vec![0.99, 0.01], 0.0, 2, 5);
        second.exposure.session_id = first.exposure.session_id.clone();
        for (index, candidate) in second.candidates.iter_mut().enumerate() {
            candidate.memory_id = format!("negative-memory-{index}");
        }
        let heldout = vec![first, second];
        let model = LinearModel {
            weights: vec![0.0; learning::FEATURE_COUNT],
            bias: 0.0,
            normalization: learning::Normalization {
                means: vec![0.0; learning::FEATURE_COUNT],
                scales: vec![1.0; learning::FEATURE_COUNT],
            },
            seed: 7,
        };

        assert!(evaluate_clusters("model", &model, &heldout).0.is_empty());
    }

    #[test]
    fn chronological_gate_needs_ten_valid_clusters_spanning_five_sessions() {
        assert!(!evaluation_floor_is_met(learning::MIN_EVAL_CLUSTERS, 4));
        assert!(evaluation_floor_is_met(learning::MIN_EVAL_CLUSTERS, 5));
    }
}
