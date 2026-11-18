//! What a result page says about its own rows: which are weak matches, and why
//! the printed scores may not descend. Labels only: no row is dropped, moved or
//! re-scored here, so the order, the row count and `csr_get_more` paging are
//! exactly what the ranking produced. Split out of `format/mod.rs`.

use super::*;
use crate::search::identifiers::{analyze_query, exact_id_tokens, find_exact_id, IdentKind};

/// Time decay never lowers a score below this share of the raw cosine
/// (`search::decay`: weight 0.3, so the factor is at least 0.7). A displayed
/// score under `QUICK_CHECK_FLOOR * 0.7` therefore came from a cosine under the
/// floor whatever the row's age; a score between the two could be an old row
/// that clears it, and is not called weak.
const MIN_DECAY_FACTOR: f32 = 0.7;

/// Labels for the rows of one page, in page order.
pub(super) struct PageLabels {
    weak: Vec<bool>,
    order_disagrees: bool,
}

impl PageLabels {
    /// A row is a *weak match* when it carries no evidence of its own:
    /// it is a keyword-only hit (a full-text word match with no similarity
    /// score), or its score is certainly under the abstention floor
    /// ([`QUICK_CHECK_FLOOR`]); and it holds no identifier of the query (an
    /// exact identifier, a path or a symbol). The first row of a page is never
    /// weak: it is what the ranking chose, and the summary's own `caveat`
    /// speaks for it. A page that is keyword-only throughout (the outage
    /// fallback) is labelled as such in its header and has no weak rows.
    pub(super) fn new(results: &[EnrichedResult], query: &str, keyword_only: bool) -> Self {
        Self::at_offset(results, query, keyword_only, 0)
    }

    /// [`Self::new`] for a `csr_get_more` page that starts `offset` rows into
    /// the pool: only the pool's first row (offset 0) is exempt, and the
    /// order note speaks for the rows of this page.
    pub(super) fn at_offset(
        results: &[EnrichedResult],
        query: &str,
        keyword_only: bool,
        offset: usize,
    ) -> Self {
        let order_disagrees = results.iter().enumerate().any(|(i, later)| {
            results[..i]
                .iter()
                .any(|earlier| later.score > earlier.score)
        });
        if keyword_only {
            return Self {
                weak: vec![false; results.len()],
                order_disagrees,
            };
        }
        let ids = exact_id_tokens(query);
        let names: Vec<String> = analyze_query(query)
            .into_iter()
            .filter(|ident| ident.kind != IdentKind::Acronym)
            .map(|ident| ident.term)
            .collect();
        let weak = results
            .iter()
            .enumerate()
            .map(|(i, row)| {
                if i == 0 && offset == 0 {
                    return false;
                }
                let keyword = row.chunk.content.starts_with("[keyword] ");
                let under_floor = !keyword && row.score < QUICK_CHECK_FLOOR * MIN_DECAY_FACTOR;
                if !(keyword || under_floor) {
                    return false;
                }
                let holds_identifier = ids
                    .iter()
                    .chain(names.iter())
                    .any(|id| find_exact_id(&row.chunk.content, id).is_some());
                !holds_identifier
            })
            .collect();
        Self {
            weak,
            order_disagrees,
        }
    }

    pub(super) fn is_weak(&self, index: usize) -> bool {
        self.weak.get(index).copied().unwrap_or(false)
    }

    /// The `<meta>` lines this page adds, none when there is nothing to say.
    pub(super) fn meta_lines(&self) -> String {
        let mut out = String::new();
        let weak = self.weak.iter().filter(|w| **w).count();
        if weak > 0 {
            out.push_str(&format!(
                "    <weak-matches>{weak} of {} rows are weak matches, marked tier weak: a keyword-only hit or a score under the relevance floor, holding no identifier of the query. They keep their place so paging holds. Read them as leads, not answers.</weak-matches>\n",
                self.weak.len()
            ));
        }
        if self.order_disagrees {
            out.push_str(
                "    <order>Rows are in ranked order, not score order. The score s shown on a row is that row's own similarity (aged by decay) and is not the key that sorts the page, so it need not fall from row to row.</order>\n",
            );
        }
        out
    }
}
