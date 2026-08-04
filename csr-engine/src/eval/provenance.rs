//! Provenance regression benchmark (Saga Phase 1 WS2) — `csr-engine eval --provenance`.
//!
//! Opt-in LOCAL gate only: never wired into default `eval`/`eval --full`, never CI.
//! Replicates the Phase 0 spike's 12 "why" queries through the production code path:
//! arm A = one-shot merged kNN (chunks + reflections, top-10), arm B =
//! `reinstatement::reinstate` with defaults. Ground truth = distinct
//! `code_evolution.session_id`s whose `file_path` matches the query's target file
//! suffix. Graceful on missing/empty DB (never a hard failure for a machine with no
//! saga history yet); regression (exit 1) only when both arms actually ran and B's
//! summed coverage is lower than A's.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::search::reinstatement::{reinstate, ReinstateConfig};
use crate::search::SearchEngine;
use crate::storage::Storage;

struct ProvenanceQuery {
    text: &'static str,
    /// Path suffix for ground-truth lookup in code_evolution ("" = judged only, no GT).
    target: &'static str,
}

// Lifted verbatim from examples/saga_spike.rs QUERIES (Phase 0 evidence set).
const QUERIES: &[ProvenanceQuery] = &[
    ProvenanceQuery { text: "why is the sqlite connection wrapped in a mutex for thread safety", target: "src/storage/mod.rs" },
    ProvenanceQuery { text: "why are tool mechanic scaffold chunks demoted in search ranking", target: "src/search/rerank.rs" },
    ProvenanceQuery { text: "why is integrity check cached in the meta table instead of running pragma integrity_check directly", target: "src/storage/mod.rs" },
    ProvenanceQuery { text: "why did AI narrative generation switch from a dated model pin to a model fallback chain", target: "src/narrative/mod.rs" },
    ProvenanceQuery { text: "why does import skip conversations that start with CSR agent prompts", target: "src/import/mod.rs" },
    ProvenanceQuery { text: "why were tool results dropped from import and how was chunking fixed to embed full conversations", target: "src/import/mod.rs" },
    ProvenanceQuery { text: "why does search fall back to exact scan for tiny hnsw indexes", target: "src/search/mod.rs" },
    ProvenanceQuery { text: "why is rmcp pinned to version 1.6 instead of upgrading to 1.7", target: "" },
    ProvenanceQuery { text: "why do hooks use catch-all wrappers so they never block claude code", target: "src/hooks/mod.rs" },
    ProvenanceQuery { text: "why does session start inject a memory manifest header capability claim", target: "src/hooks/session_start.rs" },
    ProvenanceQuery { text: "why does prompt submit classify intent with semantic exemplars instead of keywords", target: "src/hooks/intent.rs" },
    ProvenanceQuery { text: "why was fts5 keyword fallback added when semantic scores are low", target: "src/mcp/tools.rs" },
];

/// Result of a provenance eval run.
pub struct ProvenanceReport {
    pub text: String,
    /// True only when both arms actually ran on real data AND B underperformed A.
    pub regression: bool,
}

async fn embed(embeddings: &Arc<EmbeddingEngine>, text: &str) -> Result<Vec<f32>> {
    let q = text.to_string();
    let emb = embeddings.clone();
    tokio::task::spawn_blocking(move || emb.embed_single(&q)).await?
}

/// Arm A: one-shot merged kNN (chunks + reflections), top-10 by score, distinct conv ids.
async fn arm_a_convs(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
) -> Result<HashSet<String>> {
    const K: usize = 10;
    const MIN_SCORE: f32 = 0.20;

    let (chunks, reflections) = {
        let idx = search.read().await;
        (
            idx.search_chunks(query_vec, K, MIN_SCORE),
            idx.search_reflections(query_vec, K, MIN_SCORE),
        )
    };

    let chunk_ids: Vec<String> = chunks.iter().map(|r| r.id.clone()).collect();
    // Fail closed: a metadata fetch error would silently undercount arm A and
    // inflate B's relative win.
    let chunk_meta = storage
        .get_chunks_by_ids(&chunk_ids)
        .context("arm A chunk metadata fetch failed")?;

    let mut cands: Vec<(f32, String)> = Vec::new();
    for r in &chunks {
        if let Some(c) = chunk_meta.iter().find(|c| c.id == r.id) {
            cands.push((r.score, c.conversation_id.clone()));
        }
    }
    for r in &reflections {
        if let Ok(Some((_content, tags, _timestamp))) = storage.get_reflection_by_id(&r.id) {
            let conv = tags
                .iter()
                .find_map(|t| t.strip_prefix("conv_").map(str::to_string))
                .unwrap_or_else(|| format!("refl_{}", r.id));
            cands.push((r.score, conv));
        }
    }
    cands.sort_by(|a, b| b.0.total_cmp(&a.0));
    cands.truncate(K);
    Ok(cands.into_iter().map(|(_, c)| c).collect())
}

/// Run the provenance benchmark against the live engine. Graceful on an empty/near-
/// empty DB: returns a "skipped" report with `regression: false` rather than erroring.
pub async fn run_provenance(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
) -> Result<ProvenanceReport> {
    let total_chunks = storage
        .count_chunk_embeddings()
        .context("count_chunk_embeddings failed")?;
    if total_chunks == 0 {
        return Ok(ProvenanceReport {
            text: "provenance eval skipped: no chunks in database\n".to_string(),
            regression: false,
        });
    }

    let mut lines = String::new();
    let mut gt_possible = 0usize;
    let mut total_a = 0usize;
    let mut total_b = 0usize;
    let mut ran_any = false;

    for (qi, q) in QUERIES.iter().enumerate() {
        let gt = storage
            .ground_truth_sessions_for_target(q.target)
            .with_context(|| format!("ground truth lookup failed for Q{}", qi + 1))?;
        gt_possible += gt.len();

        let query_vec = match embed(embeddings, q.text).await {
            Ok(v) => v,
            Err(e) => {
                lines.push_str(&format!("Q{} embed error: {e}\n", qi + 1));
                continue;
            }
        };

        let a_convs = arm_a_convs(storage, search, &query_vec)
            .await
            .with_context(|| format!("arm A failed for Q{}", qi + 1))?;
        let a_cov = a_convs.iter().filter(|c| gt.contains(*c)).count();

        let cfg = ReinstateConfig::default();
        let b_items = reinstate(storage, embeddings, search, q.text, None, &cfg)
            .await
            .with_context(|| format!("reinstate() failed for Q{}", qi + 1))?;
        let b_convs: HashSet<String> = b_items.iter().map(|i| i.conversation_id.clone()).collect();
        let b_cov = b_convs.iter().filter(|c| gt.contains(*c)).count();

        ran_any = true;
        total_a += a_cov;
        total_b += b_cov;

        lines.push_str(&format!(
            "Q{} A={} B={} gt={}\n",
            qi + 1,
            a_cov,
            b_cov,
            gt.len()
        ));
    }

    if gt_possible == 0 {
        return Ok(ProvenanceReport {
            text: format!(
                "{lines}\nprovenance eval skipped: zero GT sessions reachable across {} queries\n",
                QUERIES.len()
            ),
            regression: false,
        });
    }

    let regression = ran_any && total_b < total_a;
    lines.push_str(&format!(
        "\n================ SUMMARY ================\nqueries: {} | total GT sessions reachable: {}\nGT coverage A={} B={}\n",
        QUERIES.len(),
        gt_possible,
        total_a,
        total_b
    ));

    Ok(ProvenanceReport {
        text: lines,
        regression,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ground_truth_query_finds_matching_suffix() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_code_evolution(
                "sess1",
                "proj",
                "/home/user/repo/src/storage/mod.rs",
                "rust",
                "Edit",
                "",
                "",
                "",
                "",
                "",
                "",
                None,
            )
            .unwrap();
        storage
            .insert_code_evolution(
                "sess2",
                "proj",
                "/home/user/repo/src/other.rs",
                "rust",
                "Edit",
                "",
                "",
                "",
                "",
                "",
                "",
                None,
            )
            .unwrap();
        let gt = storage
            .ground_truth_sessions_for_target("src/storage/mod.rs")
            .unwrap();
        assert_eq!(gt.len(), 1);
        assert!(gt.contains("sess1"));
    }

    #[test]
    fn empty_target_returns_empty_set() {
        let storage = Storage::open_memory().unwrap();
        let gt = storage.ground_truth_sessions_for_target("").unwrap();
        assert!(gt.is_empty());
    }

    /// Storage does not expose raw SQL (private `conn`), so we cannot DROP
    /// `code_evolution` after `Storage::open_memory()`. Instead we call the same
    /// `queries::ground_truth_sessions_for_target` path that Storage wraps, on a
    /// connection with no tables — missing table yields `Err` (not `Ok`/empty),
    /// which `run_provenance` now propagates via `?` instead of `unwrap_or_default()`.
    #[test]
    fn ground_truth_lookup_returns_err_when_code_evolution_missing() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let result =
            crate::storage::queries::ground_truth_sessions_for_target(&conn, "src/storage/mod.rs");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn graceful_skip_on_empty_db() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(16)));
        let report = run_provenance(&storage, &embeddings, &search)
            .await
            .unwrap();
        assert!(report.text.contains("provenance eval skipped"));
        assert!(!report.regression);
    }
}
