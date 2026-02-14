use chrono::{DateTime, Utc};

use crate::import::ConversationChunk;

/// A search result enriched with chunk metadata.
pub struct EnrichedResult {
    pub score: f32,
    pub chunk: ConversationChunk,
}

/// Format search results as rich XML matching the Python server output.
pub fn format_search_results(
    results: &[EnrichedResult],
    query: &str,
    project_scope: &str,
    search_ms: u64,
    embed_ms: u64,
) -> String {
    let mut out = String::new();

    // Upfront summary
    if results.is_empty() {
        out.push_str(&format!(
            "❌ NO RESULTS: No conversations found matching '{}'\n",
            query
        ));
    } else {
        let top_score = results[0].score;
        let relevance = if top_score >= 0.85 {
            "high"
        } else if top_score >= 0.75 {
            "good"
        } else {
            "partial"
        };
        out.push_str(&format!(
            "🎯 RESULTS: {} matches ({} relevance, top score: {:.3})\n",
            results.len(),
            relevance,
            top_score,
        ));
        out.push_str(&format!(
            "⚡ PERFORMANCE: {}ms (1 collection searched)\n",
            search_ms + embed_ms,
        ));
    }

    out.push_str("\n<search>\n");

    // Summary
    if !results.is_empty() {
        let top_score = results[0].score;
        let relevance = if top_score >= 0.85 {
            "high"
        } else if top_score >= 0.75 {
            "good"
        } else {
            "partial"
        };

        let preview = &results[0].chunk.content;
        let preview_short = if preview.len() > 100 {
            format!("{}...", &preview[..100])
        } else {
            preview.clone()
        };

        out.push_str(&format!(
            "  <summary count=\"{}\" relevance=\"{}\" top-score=\"{:.3}\">\n",
            results.len(),
            relevance,
            top_score,
        ));
        out.push_str(&format!("    <preview>{}</preview>\n", preview_short));
        out.push_str("  </summary>\n");
    }

    // Meta
    out.push_str("  <meta>\n");
    out.push_str(&format!("    <q>{}</q>\n", query));
    out.push_str(&format!("    <scope>{}</scope>\n", project_scope));
    out.push_str(&format!("    <count>{}</count>\n", results.len()));
    if !results.is_empty() {
        let last_score = results.last().unwrap().score;
        out.push_str(&format!(
            "    <range>{:.3}-{:.3}</range>\n",
            last_score, results[0].score,
        ));
    }
    out.push_str("    <perf>\n");
    out.push_str(&format!("      <ttl>{}</ttl>\n", search_ms + embed_ms));
    out.push_str(&format!("      <emb>{}</emb>\n", embed_ms));
    out.push_str(&format!("      <srch>{}</srch>\n", search_ms));
    out.push_str("      <cols>1</cols>\n");
    out.push_str("    </perf>\n");
    out.push_str("  </meta>\n");

    // Results
    out.push_str("  <results>\n");
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!("    <r rank=\"{}\">\n", i + 1));
        out.push_str(&format!("      <s>{:.3}</s>\n", r.score));
        out.push_str(&format!("      <p>{}</p>\n", r.chunk.project_name));

        // Relative time
        if let Ok(ts) = r.chunk.timestamp.parse::<DateTime<Utc>>() {
            let now = Utc::now();
            let days_ago = (now - ts).num_days();
            let time_str = match days_ago {
                0 => "today".to_string(),
                1 => "yesterday".to_string(),
                d => format!("{}d", d),
            };
            out.push_str(&format!("      <t>{}</t>\n", time_str));
        }

        // Excerpt
        let excerpt = &r.chunk.content;
        out.push_str(&format!("      <excerpt><![CDATA[{}]]></excerpt>\n", excerpt));

        // Conversation ID
        out.push_str(&format!("      <cid>{}</cid>\n", r.chunk.conversation_id));

        out.push_str("    </r>\n");
    }
    out.push_str("  </results>\n");
    out.push_str("</search>\n");

    out
}

/// Format a quick check response matching the Python server output.
pub fn format_quick_check(
    results: &[EnrichedResult],
    _query: &str,
) -> String {
    let mut out = String::new();

    out.push_str("<quick_search>\n");
    out.push_str(&format!("  <count>{}</count>\n", results.len()));
    out.push_str("  <collections_with_matches>1</collections_with_matches>\n");

    if let Some(top) = results.first() {
        out.push_str("  <top_result>\n");
        out.push_str(&format!("    <score>{:.3}</score>\n", top.score));
        out.push_str(&format!("    <timestamp>{}</timestamp>\n", top.chunk.timestamp));
        let preview = if top.chunk.content.len() > 200 {
            format!("{}...", &top.chunk.content[..200])
        } else {
            top.chunk.content.clone()
        };
        out.push_str(&format!("    <preview>{}</preview>\n", preview));
        out.push_str("  </top_result>\n");
    }

    out.push_str("</quick_search>\n");
    out
}
