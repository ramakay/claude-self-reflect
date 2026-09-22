//! Debug-only stage-instrumentation side channel consumed by
//! `examples/probe_codex_recall.rs` (WP-A). Lets the probe observe how far a
//! target conversation's best-ranked chunk travels through the REAL
//! `reflect_gather_pass` pipeline — HNSW fetch, decay/FTS enrichment,
//! provenance rerank, RRF fusion, dedupe, and the final page cut — without
//! reimplementing any of that logic in the probe itself (reimplementing it
//! risks silently drifting from production behavior).
//!
//! Zero cost in the production path: [`record_stage`] is a single
//! `try_with` lookup on a [`tokio::task_local!`] that is `Err` (and returns
//! immediately) unless a caller explicitly wrapped its call in [`scope`].
//! `reflect_on_past`'s normal callers (the MCP tool, hooks, CLI) never call
//! `scope`, so this never activates outside the probe binary.
//!
//! `tokio::task_local!` (not `thread_local!`) is required here: the search
//! path suspends at `.await` points (`RwLock::read().await`), and on a
//! multi-thread runtime the task can resume on a different OS thread after
//! an await — a plain `thread_local!` set before the call would silently
//! stop being visible. Task-local values follow the *task*, not the thread.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::rc::Rc;

use crate::search::rerank::{score_breakdown, RankCandidate, RankPolicy, ScoreBreakdown};

tokio::task_local! {
    static PROBE: RefCell<Option<ProbeState>>;
}

struct ProbeState {
    target_conversation_id: String,
    samples: Rc<RefCell<Vec<StageSample>>>,
}

/// One stage's snapshot of where the target conversation's best chunk sits.
#[derive(Debug, Clone)]
pub struct StageSample {
    pub stage: &'static str,
    pub total_candidates: usize,
    /// 0-based rank of the target conversation's best-placed candidate at
    /// this stage, or `None` if no candidate from that conversation survived
    /// to this stage.
    pub target_rank: Option<usize>,
    pub target_score: Option<f32>,
    /// The target conversation's rank within the FTS-side keyword ranking
    /// that fed RRF fusion at this stage — independent of `target_rank`,
    /// which is the position in the (already-fused) semantic ranking. Only
    /// ever populated by [`record_fts_rank`], currently called once, right
    /// after the `fuse_rrf` checkpoint.
    pub target_fts_rank: Option<usize>,
    /// Only populated for the `rerank` checkpoint: the target conversation's
    /// best-ranked candidate's per-term score decomposition, plus the same
    /// decomposition for every candidate now ranked above it (its
    /// "displacers"). Empty when the target had no surviving candidate at
    /// this stage, or on every other stage.
    pub rerank_breakdown: Vec<RerankBreakdownRow>,
}

/// One candidate's rerank score decomposition, captured for [`StageSample`]'s
/// `rerank_breakdown`. `breakdown.total` + `primacy_boost` is exactly the
/// score `search::rerank::rerank_with` would sort this candidate on under
/// [`RankPolicy::Recall`] — the policy `reflect_gather_pass` always uses for
/// this checkpoint. When trained-rerank residuals are active, the actual
/// `order` this row's rank was taken from may differ from this baseline
/// decomposition's own ordering; the breakdown still explains the baseline
/// term-by-term, which is what a human reading "why did X outrank Y" needs.
#[derive(Debug, Clone)]
pub struct RerankBreakdownRow {
    pub chunk_id: String,
    pub is_target: bool,
    pub breakdown: ScoreBreakdown,
    pub primacy_boost: f32,
}

/// Run `body` with stage recording active for `target_conversation_id`.
/// Returns the body's output plus every stage sample recorded during it, in
/// the order `record_stage` was called.
///
/// Must not be used across a `tokio::spawn` boundary — task-local scoping
/// does not propagate into spawned child tasks. The probe binary only calls
/// this around direct, in-task `.await`s, so that never applies here.
pub async fn scope<F, Fut, T>(target_conversation_id: &str, body: F) -> (T, Vec<StageSample>)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    let samples: Rc<RefCell<Vec<StageSample>>> = Rc::new(RefCell::new(Vec::new()));
    let state = ProbeState {
        target_conversation_id: target_conversation_id.to_string(),
        samples: samples.clone(),
    };
    let result = PROBE.scope(RefCell::new(Some(state)), body()).await;
    let collected = samples.borrow().clone();
    (result, collected)
}

/// True when a [`scope`] is active on the current task. Callers use this to
/// skip building the `(chunk_id, conversation_id, score)` vector (and any
/// supporting maps) they would otherwise pass to [`record_stage`] — that
/// build is real, unconditional work (String clones, HashMap construction)
/// that only matters once `record_stage` itself has somewhere to put it.
/// Outside an active probe scope this is a single `try_with` check.
pub(crate) fn is_active() -> bool {
    PROBE
        .try_with(|cell| cell.borrow().is_some())
        .unwrap_or(false)
}

/// Record one stage's ranked candidates: `(chunk_id, conversation_id, score)`
/// already in rank order (index 0 = best). No-op when not inside [`scope`].
pub(crate) fn record_stage(stage: &'static str, ranked: &[(String, String, f32)]) {
    let _ = PROBE.try_with(|cell| {
        let guard = cell.borrow();
        let Some(state) = guard.as_ref() else {
            return;
        };
        let hit = ranked
            .iter()
            .enumerate()
            .find(|(_, (_, conv, _))| conv == &state.target_conversation_id);
        state.samples.borrow_mut().push(StageSample {
            stage,
            total_candidates: ranked.len(),
            target_rank: hit.as_ref().map(|(rank, _)| *rank),
            target_score: hit.map(|(_, (_, _, score))| *score),
            target_fts_rank: None,
            rerank_breakdown: Vec::new(),
        });
    });
}

/// Records the per-term rerank score decomposition for the target's best
/// candidate and every candidate now ranked above it, onto the most recently
/// recorded [`StageSample`]. Call immediately after `record_stage("rerank",
/// ...)`. `order` is the post-rerank id ranking (best first); `conv_of` maps
/// candidate id -> conversation id; `origin` is `search::rerank::primacy_conv`
/// applied to the same candidate set. No-op when not inside [`scope`], when
/// nothing has been recorded yet, or when the target has no candidate left
/// at this stage.
pub(crate) fn record_rerank_breakdown(
    candidates: &[RankCandidate],
    order: &[String],
    conv_of: &HashMap<String, String>,
    origin: Option<&str>,
) {
    let _ = PROBE.try_with(|cell| {
        let guard = cell.borrow();
        let Some(state) = guard.as_ref() else {
            return;
        };
        let target = state.target_conversation_id.clone();
        let samples = state.samples.clone();
        drop(guard);

        let by_id: HashMap<&str, &RankCandidate> =
            candidates.iter().map(|c| (c.id.as_str(), c)).collect();
        let target_pos = order
            .iter()
            .position(|id| conv_of.get(id).map(String::as_str) == Some(target.as_str()));

        let Some(target_pos) = target_pos else {
            return; // Target absent from this stage's ranking — nothing to explain.
        };

        let mut rows = Vec::with_capacity(target_pos + 1);
        let row = |id: &str, is_target: bool| {
            by_id.get(id).map(|candidate| RerankBreakdownRow {
                chunk_id: id.to_string(),
                is_target,
                breakdown: score_breakdown(candidate, RankPolicy::Recall),
                primacy_boost: crate::search::rerank::primacy_boost(candidate, origin),
            })
        };
        rows.extend(order[..target_pos].iter().filter_map(|id| row(id, false)));
        rows.extend(row(&order[target_pos], true));

        if !rows.is_empty() {
            let mut samples = samples.borrow_mut();
            if let Some(last) = samples.last_mut() {
                last.rerank_breakdown = rows;
            }
        }
    });
}

/// Records the target conversation's rank within `fts_ranked`
/// (`(chunk_id, conversation_id)`, already in FTS/bm25 rank order) onto the
/// most recently recorded [`StageSample`]. Call immediately after
/// `record_stage` for whichever checkpoint the FTS-side ranking belongs to
/// (currently `fuse_rrf`, the only stage where both rankings are in scope at
/// once). No-op when not inside [`scope`] or when nothing has been recorded
/// yet.
pub(crate) fn record_fts_rank(fts_ranked: &[(String, String)]) {
    let _ = PROBE.try_with(|cell| {
        let guard = cell.borrow();
        let Some(state) = guard.as_ref() else {
            return;
        };
        let hit = fts_ranked
            .iter()
            .position(|(_, conv)| conv == &state.target_conversation_id);
        let samples = state.samples.clone();
        drop(guard);
        let mut samples = samples.borrow_mut();
        if let Some(last) = samples.last_mut() {
            last.target_fts_rank = hit;
        }
    });
}
