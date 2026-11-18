//! The health probe's search: the production recall pipeline with a fixed
//! page shape and no ambient scope.

use super::*;
use crate::health::page_repeats::{ProbePage, ProbeRow};

/// The probe's scope: explicit all-project, with no current-project boost and
/// no client-directory default, so nothing in the process environment (cwd,
/// `MCP_CLIENT_CWD`, `CLAUDE_PROJECT_DIR`) reaches the measurement.
pub(super) fn health_probe_scope(storage: &Storage) -> Result<SearchProjectScope> {
    SearchProjectScope::resolve_with(storage, Some("all"), None, None, None)
}

/// The health probe's production search: limit 5, minimum score 0.3, explicit
/// all-project scope built without the ambient cwd or client-project boost (so
/// the daemon's cwd cannot move the measurement), the runtime reranker and the
/// production validity, forgetting and search-mode settings. Retrieval
/// telemetry is suppressed through the same `record_retrievals=false` seam the
/// curated eval uses. Returns the rows of the actual final page, in result
/// order (each row's conversation id and the key the same-text collapse groups
/// its text by), and how many copies the collapse left out of the pool.
pub(crate) async fn health_probe_page_with_vec(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    rerank_intent: &str,
) -> Result<ProbePage> {
    const LIMIT: usize = 5;
    const MIN_SCORE: f32 = 0.3;

    let scope = health_probe_scope(storage)?;
    let search_mode = search_mode_from(std::env::var("CSR_SEARCH_MODE").ok().as_deref());
    let partition_enabled = validity_partition_enabled();
    let consumption_mode = consumption_mode_for_partition(partition_enabled);
    let active_forgetting = active_forgetting_enabled();
    let found = reflect_final_page(
        storage,
        search,
        query_vec,
        query,
        LIMIT,
        MIN_SCORE,
        &scope,
        partition_enabled,
        consumption_mode,
        active_forgetting,
        rerank_intent,
        search_mode,
        RecallRerankMode::Runtime,
        false,
    )
    .await?;
    Ok(ProbePage {
        same_text_collapsed: found.cross_source_collapsed.same_text,
        rows: found
            .enriched
            .into_iter()
            .take(LIMIT)
            .map(|result| ProbeRow {
                same_text: page::same_text_key(&result.chunk.content),
                conversation_id: result.chunk.conversation_id,
            })
            .collect(),
    })
}

/// The conversation ids of [`health_probe_page_with_vec`]'s page, in order.
#[cfg(test)]
pub(crate) async fn reflect_for_health_probe_with_vec(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    query_vec: &[f32],
    query: &str,
    rerank_intent: &str,
) -> Result<Vec<String>> {
    let found =
        health_probe_page_with_vec(storage, search, query_vec, query, rerank_intent).await?;
    Ok(found
        .rows
        .into_iter()
        .map(|row| row.conversation_id)
        .collect())
}

/// [`health_probe_page_with_vec`] after local query embedding and the shared
/// intent resolution. No MCP client, subprocess or model call.
pub(crate) async fn reflect_for_health_probe(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
) -> Result<ProbePage> {
    let query_vec = embed_query(embeddings, query).await?;
    let rerank_intent = resolve_rerank_intent(embeddings, &query_vec).await;
    health_probe_page_with_vec(storage, search, &query_vec, query, rerank_intent).await
}
