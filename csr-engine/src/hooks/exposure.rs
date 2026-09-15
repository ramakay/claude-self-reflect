//! Best-effort exposure logging shared by injection hooks.

use crate::injection::formatter::{FormattedInjection, InjectionCategory};
use crate::storage::trained_rerank::{ExposureImpression, ExposureItem};
use crate::storage::Storage;

pub const FEATURE_SCHEMA: i64 = crate::search::trained_rerank::FEATURE_SCHEMA;

/// Map a formatter receipt back to the parallel stable-memory metadata kept by
/// the hook. Items without a stable ID are omitted, and persisted ranks are
/// dense output ranks rather than pre-budget candidate positions.
pub fn rendered_exposure_items(
    receipt: &FormattedInjection,
    anti_patterns: &[Option<ExposureItem>],
    relevant_context: &[Option<ExposureItem>],
    winning_strategies: &[Option<ExposureItem>],
) -> Vec<ExposureItem> {
    receipt
        .rendered_items
        .iter()
        .filter_map(|rendered| {
            let source = match rendered.category {
                InjectionCategory::AntiPattern => anti_patterns,
                InjectionCategory::RelevantContext => relevant_context,
                InjectionCategory::WinningStrategy => winning_strategies,
                InjectionCategory::ErrorMatch | InjectionCategory::IterationLearning => &[],
            };
            source.get(rendered.index).and_then(Clone::clone)
        })
        .enumerate()
        .map(|(rank, mut item)| {
            item.rank = rank as i64;
            item
        })
        .collect()
}

pub fn query_hash(query: &str) -> String {
    blake3::hash(query.as_bytes()).to_hex().to_string()
}

/// Record a whole stdout emission without ever waiting on database contention.
/// Hooks call this only after they know the corresponding text was emitted.
#[allow(clippy::too_many_arguments)]
pub fn record_impression(
    storage: &Storage,
    session_id: Option<&str>,
    project: &str,
    surface: &str,
    query: Option<&str>,
    query_embedding: Option<&[f32]>,
    intent: &str,
    items: Vec<ExposureItem>,
) {
    let Some(session_id) = session_id else {
        return;
    };
    if items.is_empty() {
        return;
    }
    let impression = ExposureImpression {
        impression_id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        project: project.to_string(),
        surface: surface.to_string(),
        query_hash: query.map(query_hash),
        query_embedding: query_embedding.map(ToOwned::to_owned),
        intent: intent.to_string(),
        shown_at: chrono::Utc::now().to_rfc3339(),
        feature_schema: FEATURE_SCHEMA,
        legacy: false,
        items,
    };
    match storage.try_record_rerank_exposure(&impression) {
        Ok(true) => {}
        Ok(false) => eprintln!("CSR: exposure telemetry dropped (database busy)"),
        Err(error) => eprintln!("CSR: exposure telemetry error (non-fatal): {error}"),
    }
}

#[cfg(test)]
mod tests {
    use crate::injection::formatter::{
        FormattedInjection, InjectionCategory, RenderedInjectionItem,
    };
    use crate::storage::trained_rerank::ExposureItem;

    use super::rendered_exposure_items;

    fn item(id: &str) -> ExposureItem {
        ExposureItem {
            rank: -1,
            memory_id: id.into(),
            conversation_id: None,
            source_type: "reflection".into(),
            baseline_score: None,
            cosine: None,
            recency: None,
            graph_proximity: None,
            author: None,
            is_scaffold: false,
            is_mechanic: false,
            supersedes: false,
        }
    }

    #[test]
    fn receipt_maps_only_rendered_memory_items_and_assigns_dense_ranks() {
        let receipt = FormattedInjection {
            text: "shown".into(),
            rendered_items: vec![
                RenderedInjectionItem {
                    category: InjectionCategory::AntiPattern,
                    index: 0,
                },
                RenderedInjectionItem {
                    category: InjectionCategory::RelevantContext,
                    index: 0,
                },
                RenderedInjectionItem {
                    category: InjectionCategory::WinningStrategy,
                    index: 1,
                },
            ],
        };
        let anti = vec![Some(item("anti"))];
        let relevant = vec![None];
        let winning = vec![Some(item("truncated-out")), Some(item("winner"))];

        let selected = rendered_exposure_items(&receipt, &anti, &relevant, &winning);

        assert_eq!(
            selected
                .iter()
                .map(|entry| (entry.rank, entry.memory_id.as_str()))
                .collect::<Vec<_>>(),
            vec![(0, "anti"), (1, "winner")]
        );
    }
}
