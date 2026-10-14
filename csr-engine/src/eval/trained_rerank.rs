//! Trained re-ranker metrics, curated veto, and persisted gate reporting.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::mcp::tools::{reflect_for_curated_eval_with_vec, RecallRerankMode};
use crate::search::trained_rerank::LinearModel;
use crate::search::SearchEngine;
use crate::storage::Storage;

pub use crate::storage::trained_rerank::{CURATED_VETO_EPSILON, MIN_CURATED_CASES};

use super::EvalResult;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CuratedScores {
    pub baseline_mrr: f64,
    pub trained_mrr: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CuratedEvaluation {
    Scores {
        scores: CuratedScores,
        case_count: usize,
    },
    InsufficientData {
        case_count: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CuratedVetoDecision {
    Passed,
    Regression,
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CuratedCaseFloorDecision {
    Evaluate,
    InsufficientData { case_count: usize },
}

pub fn curated_case_floor_decision(case_count: usize) -> CuratedCaseFloorDecision {
    if case_count < MIN_CURATED_CASES {
        CuratedCaseFloorDecision::InsufficientData { case_count }
    } else {
        CuratedCaseFloorDecision::Evaluate
    }
}

pub fn decide_curated_veto(result: anyhow::Result<CuratedScores>) -> CuratedVetoDecision {
    match result {
        Ok(scores)
            if scores.baseline_mrr.is_finite()
                && scores.trained_mrr.is_finite()
                && scores.trained_mrr + CURATED_VETO_EPSILON >= scores.baseline_mrr =>
        {
            CuratedVetoDecision::Passed
        }
        Ok(scores) if scores.baseline_mrr.is_finite() && scores.trained_mrr.is_finite() => {
            CuratedVetoDecision::Regression
        }
        Ok(_) => CuratedVetoDecision::Error("curated scores are non-finite".into()),
        Err(error) => CuratedVetoDecision::Error(error.to_string()),
    }
}

fn rendered_conversation_ids(output: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    output
        .lines()
        .filter_map(|line| {
            let start = line.find("<cid>")? + "<cid>".len();
            let end = line[start..].find("</cid>")? + start;
            Some(line[start..end].to_string())
        })
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

async fn embed_query(embeddings: &Arc<EmbeddingEngine>, query: &str) -> Result<Vec<f32>> {
    let query = query.to_string();
    let embeddings = embeddings.clone();
    tokio::task::spawn_blocking(move || embeddings.embed_single(&query))
        .await
        .context("curated query embedding task failed")?
        .context("curated query embedding failed")
}

/// Derive independent cases from exact `(project, file_path)` history and run
/// them through production `reflect_on_past`, once with the deterministic
/// baseline and once with an explicit candidate model. No environment
/// activation or retrieval telemetry is involved.
pub async fn run_curated_veto(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    model: &LinearModel,
) -> Result<CuratedEvaluation> {
    const MAX_CURATED_CASES: usize = 20;
    let cases = storage.local_curated_rerank_cases(MAX_CURATED_CASES)?;
    if let CuratedCaseFloorDecision::InsufficientData { case_count } =
        curated_case_floor_decision(cases.len())
    {
        return Ok(CuratedEvaluation::InsufficientData { case_count });
    }
    let mut baseline_total = 0.0;
    let mut trained_total = 0.0;

    for (index, case) in cases.iter().enumerate() {
        let ground_truth: HashSet<_> = case.expected_sessions.iter().collect();
        let query_vec = embed_query(embeddings, &case.query_text)
            .await
            .with_context(|| format!("curated embedding unavailable for Q{}", index + 1))?;
        let baseline = reflect_for_curated_eval_with_vec(
            storage,
            search,
            &query_vec,
            &case.query_text,
            &case.project,
            RecallRerankMode::Baseline,
        )
        .await
        .with_context(|| format!("curated baseline execution failed for Q{}", index + 1))?;
        let trained = reflect_for_curated_eval_with_vec(
            storage,
            search,
            &query_vec,
            &case.query_text,
            &case.project,
            RecallRerankMode::Candidate(model),
        )
        .await
        .with_context(|| format!("curated trained execution failed for Q{}", index + 1))?;

        let reciprocal_rank = |output: &str| {
            rendered_conversation_ids(output)
                .iter()
                .position(|conversation_id| ground_truth.contains(conversation_id))
                .map_or(0.0, |rank| 1.0 / (rank + 1) as f64)
        };
        baseline_total += reciprocal_rank(&baseline);
        trained_total += reciprocal_rank(&trained);
    }

    let case_count = cases.len();
    Ok(CuratedEvaluation::Scores {
        scores: CuratedScores {
            baseline_mrr: baseline_total / case_count as f64,
            trained_mrr: trained_total / case_count as f64,
        },
        case_count,
    })
}

pub fn ndcg_at_k(relevances: &[f64], k: usize) -> f64 {
    fn dcg(values: &[f64], k: usize) -> f64 {
        values
            .iter()
            .take(k)
            .enumerate()
            .map(|(index, relevance)| {
                (2.0_f64.powf(*relevance) - 1.0) / ((index + 2) as f64).log2()
            })
            .sum()
    }
    let actual = dcg(relevances, k);
    let mut ideal = relevances.to_vec();
    ideal.sort_by(|left, right| right.partial_cmp(left).unwrap_or(std::cmp::Ordering::Equal));
    let ideal = dcg(&ideal, k);
    if ideal <= f64::EPSILON {
        0.0
    } else {
        actual / ideal
    }
}

pub fn mrr(relevances: &[f64]) -> f64 {
    relevances
        .iter()
        .position(|relevance| *relevance > 0.0)
        .map_or(0.0, |index| 1.0 / (index + 1) as f64)
}

pub fn latest_gate_result(storage: &Storage) -> EvalResult {
    let started = Instant::now();
    let attempt = match storage.latest_rerank_model_attempt() {
        Ok(Some(attempt)) => attempt,
        // No training has happened yet: nothing was tried, so nothing failed.
        // The line is printed; it does not gate the exit.
        Ok(None) => {
            return EvalResult::pass(
                "Trained Re-ranker Gate",
                "ranking",
                started.elapsed().as_secs_f64() * 1000.0,
                "ABSTAIN: never_run: no persisted training attempt".into(),
            );
        }
        Err(error) => {
            return EvalResult::fail(
                "Trained Re-ranker Gate",
                "ranking",
                started.elapsed().as_secs_f64() * 1000.0,
                format!("error reading gate: {error}"),
            );
        }
    };
    let detail = format!(
        "status={} baseline_ndcg5={} trained_ndcg5={} baseline_mrr={} trained_mrr={} curated_baseline={} curated_trained={} curated_cases={} curated_epsilon={:.9} clusters={}/{} (wins/losses/ties={}/{}/{}) train={}/{} eval={}/{} cutoff={} window={}..{}",
        attempt.gate_status,
        attempt
            .baseline_ndcg5
            .map_or_else(|| "n/a".into(), |value| format!("{value:.4}")),
        attempt
            .trained_ndcg5
            .map_or_else(|| "n/a".into(), |value| format!("{value:.4}")),
        attempt
            .baseline_mrr
            .map_or_else(|| "n/a".into(), |value| format!("{value:.4}")),
        attempt
            .trained_mrr
            .map_or_else(|| "n/a".into(), |value| format!("{value:.4}")),
        attempt
            .curated_baseline_score
            .map_or_else(|| "n/a".into(), |value| format!("{value:.4}")),
        attempt
            .curated_trained_score
            .map_or_else(|| "n/a".into(), |value| format!("{value:.4}")),
        attempt.curated_case_count,
        attempt.curated_veto_epsilon,
        attempt.eval_clusters,
        attempt.eval_impressions,
        attempt.cluster_wins,
        attempt.cluster_losses,
        attempt.cluster_ties,
        attempt.train_impressions,
        attempt.train_rows,
        attempt.eval_impressions,
        attempt.eval_rows,
        attempt.cutoff_ts.as_deref().unwrap_or("n/a"),
        attempt.eval_start_ts.as_deref().unwrap_or("n/a"),
        attempt.eval_end_ts.as_deref().unwrap_or("n/a"),
    );
    let compatibility_error = (attempt.gate_status == "passed")
        .then(|| {
            let classifier_hash = crate::hooks::reaction::classifier_hash();
            crate::search::trained_rerank::LinearModel::from_attempt(&attempt, &classifier_hash)
                .err()
        })
        .flatten();
    if attempt.gate_status == "passed" && compatibility_error.is_none() {
        EvalResult::pass(
            "Trained Re-ranker Gate",
            "ranking",
            started.elapsed().as_secs_f64() * 1000.0,
            detail,
        )
    } else {
        let reason = compatibility_error.map_or_else(
            || attempt.gate_reason.clone(),
            |error| format!("{}; model load failed: {error}", attempt.gate_reason),
        );
        // Too little labeled data to train or judge a model is an abstention,
        // like a recall probe with too few cases: printed, not gating. A model
        // that was tried and lost (`failed`) or broke (`error`) still gates.
        if attempt.gate_status == "insufficient_data" {
            return EvalResult::pass(
                "Trained Re-ranker Gate",
                "ranking",
                started.elapsed().as_secs_f64() * 1000.0,
                format!("ABSTAIN: insufficient_data; {detail}; reason={reason}"),
            );
        }
        EvalResult::fail(
            "Trained Re-ranker Gate",
            "ranking",
            started.elapsed().as_secs_f64() * 1000.0,
            format!("{detail}; reason={reason}"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- the gate's abstentions do not fail the full eval ---

    fn attempt(
        model_id: &str,
        status: &str,
        reason: &str,
    ) -> crate::storage::trained_rerank::ModelAttempt {
        crate::storage::trained_rerank::ModelAttempt {
            model_id: model_id.into(),
            feature_schema: crate::search::trained_rerank::FEATURE_SCHEMA,
            classifier_hash: "classifier".into(),
            seed: 1,
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
            excluded_contaminated: 0,
            abstained_reactions: 0,
            acceptance_labels: 0,
            correction_labels: 0,
            reask_labels: 0,
            redirect_labels: 0,
            near_miss_labels: 0,
            baseline_ndcg5: None,
            trained_ndcg5: None,
            baseline_mrr: None,
            trained_mrr: None,
            curated_baseline_score: None,
            curated_trained_score: None,
            curated_case_count: 0,
            curated_veto_epsilon: 0.0,
            gate_status: status.into(),
            gate_reason: reason.into(),
            weights_json: None,
            normalization_json: None,
            trained_at: "2026-10-13T00:00:00Z".into(),
        }
    }

    fn gate_after(status: Option<(&str, &str)>) -> EvalResult {
        let storage = Storage::open_memory().unwrap();
        if let Some((status, reason)) = status {
            storage
                .insert_rerank_model_attempt(&attempt("m1", status, reason), &[])
                .unwrap();
        }
        latest_gate_result(&storage)
    }

    #[test]
    fn never_run_and_insufficient_data_abstain_without_failing() {
        let never = gate_after(None);
        assert!(never.passed, "{}", never.detail);
        assert!(
            never.detail.starts_with("ABSTAIN: never_run"),
            "{}",
            never.detail
        );

        let thin = gate_after(Some(("insufficient_data", "only 3 labeled impressions")));
        assert!(thin.passed, "{}", thin.detail);
        assert!(
            thin.detail.starts_with("ABSTAIN: insufficient_data"),
            "{}",
            thin.detail
        );
        assert!(
            thin.detail.contains("only 3 labeled impressions"),
            "the reason stays printed"
        );
    }

    #[test]
    fn a_real_reranker_failure_still_gates() {
        for (status, reason) in [
            ("failed", "trained model lost to the baseline"),
            ("error", "training crashed"),
        ] {
            let result = gate_after(Some((status, reason)));
            assert!(!result.passed, "{status}: {}", result.detail);
            assert!(result.detail.contains(reason));
        }
    }

    #[test]
    fn has_gating_failure_ignores_the_abstentions_and_catches_a_real_failure() {
        let report = |gate: EvalResult| crate::eval::EvalReport {
            results: vec![
                EvalResult::pass("DB Connectivity", "c", 0.0, "ok".into()),
                gate,
            ],
            total_ms: 0.0,
        };
        assert!(!report(gate_after(None)).has_gating_failure());
        assert!(!report(gate_after(Some(("insufficient_data", "thin")))).has_gating_failure());
        assert!(report(gate_after(Some(("failed", "lost")))).has_gating_failure());
    }

    #[test]
    fn curated_veto_triggers_when_trained_mrr_regresses_beyond_epsilon() {
        let decision = decide_curated_veto(Ok(CuratedScores {
            baseline_mrr: 0.60,
            trained_mrr: 0.60 - (2.0 * CURATED_VETO_EPSILON),
        }));

        assert_eq!(decision, CuratedVetoDecision::Regression);
    }

    #[test]
    fn curated_veto_passes_when_trained_mrr_does_not_regress_beyond_epsilon() {
        let equal = decide_curated_veto(Ok(CuratedScores {
            baseline_mrr: 0.60,
            trained_mrr: 0.60,
        }));
        let within_epsilon = decide_curated_veto(Ok(CuratedScores {
            baseline_mrr: 0.60,
            trained_mrr: 0.60 - (CURATED_VETO_EPSILON / 2.0),
        }));

        assert_eq!(equal, CuratedVetoDecision::Passed);
        assert_eq!(within_epsilon, CuratedVetoDecision::Passed);
    }

    #[test]
    fn curated_veto_reports_unavailable_as_error_instead_of_passing() {
        let decision = decide_curated_veto(Err(anyhow::anyhow!("embedding unavailable")));

        assert_eq!(
            decision,
            CuratedVetoDecision::Error("embedding unavailable".into())
        );
    }

    #[test]
    fn fewer_than_five_local_curated_cases_is_insufficient_data_not_error() {
        assert_eq!(
            curated_case_floor_decision(4),
            CuratedCaseFloorDecision::InsufficientData { case_count: 4 }
        );
        assert_eq!(
            curated_case_floor_decision(5),
            CuratedCaseFloorDecision::Evaluate
        );
    }

    #[test]
    fn curated_conversation_ids_parse_in_rendered_rank_order() {
        let rendered = "<results>\n  <r rank=\"1\"><cid>session-a</cid></r>\n  <r rank=\"2\"><cid>session-b</cid></r>\n  <r rank=\"3\"><cid>session-a</cid></r>\n</results>";

        assert_eq!(
            rendered_conversation_ids(rendered),
            vec!["session-a", "session-b"]
        );
    }

    #[test]
    fn ndcg_at_five_is_one_for_ideal_order_and_lower_when_reversed() {
        let ideal = [3.0, 2.0, 0.0];
        assert!((ndcg_at_k(&ideal, 5) - 1.0).abs() < 1e-12);
        let reversed = [0.0, 2.0, 3.0];
        assert!(ndcg_at_k(&reversed, 5) < 1.0);
    }

    #[test]
    fn mrr_returns_reciprocal_rank_of_first_relevant_item() {
        assert_eq!(mrr(&[0.0, 0.0, 3.0]), 1.0 / 3.0);
        assert_eq!(mrr(&[0.0, 0.0]), 0.0);
    }
}
