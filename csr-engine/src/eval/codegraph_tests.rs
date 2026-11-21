use std::sync::Arc;

use super::*;
use crate::storage::Storage;

#[allow(clippy::too_many_arguments)]
fn resolve_stats(
    total: usize,
    bound: usize,
    external: usize,
    method: usize,
    stale: usize,
    internal_module: usize,
    drifted: usize,
    local: usize,
    unexplained: usize,
    ambiguous_remaining: usize,
    closure_rate: f64,
    internal_binding_rate: f64,
) -> ResolveStats {
    ResolveStats {
        total,
        resolved: bound,
        bound,
        external,
        method,
        stale,
        internal_module,
        drifted,
        local,
        unexplained,
        ambiguous_remaining,
        closure_rate,
        internal_binding_rate,
    }
}

// ─── Witness backfill (WCR Phase 5, TASK 2) ───

fn stale_calls_edge(src_id: &str, callee: &str, file: &str, callee_kind: &str) -> EdgeRow {
    EdgeRow {
        src_id: src_id.into(),
        dst_id: format!("name:{callee}"),
        kind: "calls".into(),
        src_file: file.into(),
        resolved: 0,
        weight: 1.0,
        callee_kind: callee_kind.into(),
        ..EdgeRow::default()
    }
}

fn seed_backfill_node(shadow: &Arc<Storage>, id: &str, file: &str, name: &str) {
    shadow
        .upsert_code_node(&NodeRow {
            id: id.into(),
            repo: "repo".into(),
            project: "proj".into(),
            file: file.into(),
            lang: "rust".into(),
            kind: "function".into(),
            name: name.into(),
            first_conv_id: "c".into(),
            last_conv_id: "c".into(),
            ..NodeRow::default()
        })
        .unwrap();
}

#[path = "codegraph_tests/backfill_tests.rs"]
mod backfill_tests;

#[path = "codegraph_tests/backfill_repoints_tests.rs"]
mod backfill_repoints_tests;
