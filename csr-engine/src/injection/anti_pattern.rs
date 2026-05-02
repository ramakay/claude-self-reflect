//! Anti-pattern detector — extracts anti-patterns from past sessions for injection.
//!
//! Searches reflections tagged with `outcome_incomplete` or `outcome_abandoned`
//! for patterns relevant to the current prompt/task. "Don't retry this approach"
//! is the highest-value injection — prevents wasted iterations.
//!
//! Used by SessionStart and UserPromptSubmit hooks.

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::embeddings::EmbeddingEngine;
use crate::injection::InjectionItem;
use crate::search::SearchEngine;
use crate::storage::Storage;

/// Search for anti-patterns relevant to the current prompt.
///
/// Searches reflections tagged with `outcome_incomplete` or `outcome_abandoned`
/// and returns formatted anti-pattern warnings as InjectionItems.
pub async fn find_anti_patterns(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    prompt: &str,
    min_score: f32,
    limit: usize,
) -> Vec<InjectionItem> {
    let query = format!("failed approach don't retry: {}", prompt);
    let results = search_reflections_by_tag(
        storage,
        embeddings,
        search,
        &query,
        min_score,
        limit,
        &["outcome_incomplete", "outcome_abandoned"],
    )
    .await;

    results
        .into_iter()
        .map(|(content, score)| InjectionItem {
            content,
            score,
            source: "anti_pattern".to_string(),
        })
        .collect()
}

/// Search reflections filtered by any of the given tags.
///
/// Performs semantic search on reflections, then post-filters by tags.
/// Over-fetches by 3x to account for filtering.
pub async fn search_reflections_by_tag(
    storage: &Arc<Storage>,
    embeddings: &Arc<EmbeddingEngine>,
    search: &Arc<RwLock<SearchEngine>>,
    query: &str,
    min_score: f32,
    limit: usize,
    tags: &[&str],
) -> Vec<(String, f32)> {
    let query_vec = match embed_query(embeddings, query).await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let results = {
        let idx = search.read().await;
        idx.search_reflections(&query_vec, (limit * 3).min(50), min_score) // Over-fetch for filtering, capped at 50
    };

    let mut filtered = Vec::new();
    for result in &results {
        if let Ok(Some((_content, ref_tags, _ts))) = storage.get_reflection_by_id(&result.id) {
            let has_matching_tag = tags.iter().any(|t| ref_tags.iter().any(|rt| rt == t));
            if has_matching_tag {
                filtered.push((_content, result.score));
                if filtered.len() >= limit {
                    break;
                }
            }
        }
    }

    filtered
}

/// Embed a query string via spawn_blocking.
async fn embed_query(embeddings: &Arc<EmbeddingEngine>, query: &str) -> Result<Vec<f32>> {
    let q = query.to_string();
    let emb = embeddings.clone();
    tokio::task::spawn_blocking(move || emb.embed_single(&q)).await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_injection_item_from_anti_pattern() {
        let item = InjectionItem {
            content: "Don't use shared memory for hook communication".into(),
            score: 0.75,
            source: "anti_pattern".into(),
        };
        assert_eq!(item.source, "anti_pattern");
        assert!(item.score > 0.5);
    }
}
