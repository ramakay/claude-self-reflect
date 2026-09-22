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
use std::future::Future;
use std::rc::Rc;

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
        });
    });
}
