//! `csr_get_more` pages. Split out of `format/mod.rs`; carries the same weak
//! and order labels as the first page (labels only: no row is dropped or moved).

use super::*;

/// Format paginated "more results".
pub fn format_more_results(
    results: &[EnrichedResult],
    query: &str,
    offset: usize,
    total_available: usize,
) -> String {
    more_results(results, query, offset, total_available, false)
}

/// [`format_more_results`] for the keyword fallback, whose rows are all
/// keyword hits: none is a weak match, the page says so by saying nothing.
pub fn format_more_keyword(
    results: &[EnrichedResult],
    query: &str,
    offset: usize,
    total_available: usize,
) -> String {
    more_results(results, query, offset, total_available, true)
}

fn more_results(
    results: &[EnrichedResult],
    query: &str,
    offset: usize,
    total_available: usize,
    keyword_only: bool,
) -> String {
    let mut out = String::new();

    if results.is_empty() {
        return format!(
            "<more_results><message>No results at offset {}</message></more_results>",
            offset
        );
    }

    let labels = page_labels::PageLabels::at_offset(results, query, keyword_only, offset);
    out.push_str("<more_results>\n");
    out.push_str(&format!("  <query>{}</query>\n", xml_escape(query)));
    out.push_str(&format!("  <offset>{}</offset>\n", offset));
    out.push_str(&format!(
        "  <results_returned>{}</results_returned>\n",
        results.len()
    ));
    out.push_str(&format!(
        "  <total_available>{}</total_available>\n",
        total_available
    ));
    out.push_str(&labels.meta_lines());

    for (i, r) in results.iter().enumerate() {
        let preview = if r.chunk.content.chars().count() > PREVIEW_CHARS {
            format!("{}...", truncate_chars(&r.chunk.content, PREVIEW_CHARS))
        } else {
            r.chunk.content.clone()
        };
        out.push_str(&format!("  <result index=\"{}\">\n", offset + i + 1));
        if r.chunk.content.starts_with("[keyword] ") {
            out.push_str("    <relevance>keyword</relevance>\n");
        } else {
            out.push_str(&format!("    <score>{:.3}</score>\n", r.score));
        }
        out.push_str(&format!(
            "    <timestamp>{}</timestamp>\n",
            xml_escape(&age_stamp(&r.chunk.timestamp))
        ));
        out.push_str(&format!(
            "    <preview>{}</preview>\n",
            xml_escape(&preview)
        ));
        if let Some(ref note) = r.resolution {
            out.push_str(&format!(
                "    <resolution>{}</resolution>\n",
                xml_escape(note)
            ));
        }
        out.push_str(&format!("    <trust>{}</trust>\n", r.trust));
        if labels.is_weak(i) {
            out.push_str("    <tier>weak</tier>\n");
        }
        out.push_str("  </result>\n");
    }

    out.push_str("</more_results>");
    out
}
