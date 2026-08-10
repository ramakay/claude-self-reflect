use chrono::Utc;

use crate::import::ConversationChunk;
use crate::temporal::parse_timestamp;

/// Escape special XML characters in user-generated content.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Preview truncation length (chars, not bytes) used across all result renderers.
pub const PREVIEW_CHARS: usize = 500;

/// Return the longest prefix of `s` with at most `max` chars, cut on a char
/// boundary (never panics on multi-byte UTF-8, unlike `&s[..max]` byte slicing).
pub fn truncate_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

/// Abstention floor for `csr_quick_check`: below this top score the tool makes a
/// negative existence claim instead of reporting a match.
///
/// Measured 2026-08-03 (`cargo run --release --example quick_check_floor`, exact
/// cosine over all 141,964 chunk embeddings): 8 fabricated topics (never
/// discussed on this machine) scored 0.308–0.605; 12 topics verified present in
/// `chunks.content` scored 0.468–0.816. **The distributions overlap** — there is
/// no floor that suppresses every fabricated probe without also suppressing real
/// ones (a floor at 0.605 would lose 4 of 12 genuine topics). 0.45 is therefore
/// the highest floor that misses no verified-genuine probe; it suppresses only
/// the clearly-hopeless tail. The overlap band above it is handled by the `weak`
/// label plus an honest calibration warning and a top1-top2 margin signal, not
/// by silent confidence.
pub const QUICK_CHECK_FLOOR: f32 = 0.45;

/// Top of the measured fabrication-overlap band (highest fabricated probe: 0.605,
/// rounded up). At or above this, a match is at least `partial`; below it a match
/// is `weak` — absolute cosine is weakly calibrated in this band on this corpus
/// and does not reliably separate genuine matches from noise by score alone.
/// `format_quick_check` surfaces a top1-top2 margin alongside the score as a
/// cheap discriminating signal instead of pretending the score settles it.
pub const WEAK_BAND_TOP: f32 = 0.62;

/// Band a similarity score into the relevance vocabulary shared by every result
/// renderer. Single source of truth — `format_search_results` and
/// `format_quick_check` must not drift apart on what a score means.
pub fn relevance_label(score: f32) -> &'static str {
    if score >= 0.85 {
        "high"
    } else if score >= 0.75 {
        "good"
    } else if score >= WEAK_BAND_TOP {
        "partial"
    } else {
        "weak"
    }
}

/// A search result enriched with chunk metadata.
pub struct EnrichedResult {
    pub score: f32,
    pub chunk: ConversationChunk,
    /// Resolution-ledger annotation, when a verdict has been recorded for this chunk.
    pub resolution: Option<String>,
    /// Set ONLY by `mcp::tools::apply_validity_partition` when the v10 dream
    /// verdict structurally sank this result (Demote channel). The footer
    /// count reads THIS flag, never a substring of `resolution` — ledger
    /// evidence text that happens to contain "[stale anchor]" must not fire
    /// the dream-verdict note, and with `CSR_NO_VALIDITY_PARTITION=1` the
    /// flag is never set, so output is byte-identical to pre-partition
    /// behavior.
    pub validity_demoted: bool,
}

/// Effective rerank score for a result whose position differs from raw-score
/// order. Kept separate from [`EnrichedResult`] so other search surfaces retain
/// their existing result model.
pub(crate) struct DisplayRankScore {
    pub chunk_id: String,
    pub adjusted_score: f32,
}

/// Drop multi-route and near-duplicate results, keeping first occurrence.
///
/// Input is assumed sorted by score descending. A result is dropped if either:
/// - its `chunk.id` was already seen, or
/// - the compound key `(conversation_id, normalized 200-char content prefix)`
///   was already seen.
///
/// Normalization: lowercase, collapse whitespace, take first 200 chars.
/// Cross-conversation collapsing is out of scope — identical content from
/// different conversation_ids both survive.
pub fn dedupe_results(results: &mut Vec<EnrichedResult>) {
    use std::collections::HashSet;

    let mut seen_ids = HashSet::new();
    let mut seen_keys = HashSet::new();

    results.retain(|r| {
        if !seen_ids.insert(r.chunk.id.clone()) {
            return false;
        }
        let normalized: String = r
            .chunk
            .content
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(200)
            .collect();
        let compound = (r.chunk.conversation_id.clone(), normalized);
        seen_keys.insert(compound)
    });
}

/// Drop plan-doc chunks whose origin conversation is already in the result set.
///
/// A `~/.claude/plans/*.md` chunk (conversation_id `plan:<slug>`) correlated to
/// conversation C carries the same decision text that C's ExitPlanMode turn does.
/// When both surface for one query, the origin conversation always survives and
/// the plan chunk is dropped — deterministic, no similarity threshold (Codex
/// adversarial review: content-shingle thresholds mis-collapse boilerplate and
/// can evict the authoritative side). `origin_of` maps a plan chunk id to its
/// correlated conversation id (from `chunk_provenance.source_conv_id`); plans
/// without a correlation are never dropped here.
pub fn dedupe_plan_origins(
    results: &mut Vec<EnrichedResult>,
    origin_of: &std::collections::HashMap<String, String>,
) {
    use std::collections::HashSet;
    let present_convs: HashSet<String> = results
        .iter()
        .filter(|r| !r.chunk.conversation_id.starts_with("plan:"))
        .map(|r| r.chunk.conversation_id.clone())
        .collect();
    results.retain(|r| {
        if !r.chunk.conversation_id.starts_with("plan:") {
            return true;
        }
        match origin_of.get(&r.chunk.id) {
            Some(conv) => !present_convs.contains(conv),
            None => true,
        }
    });
}

/// Drop sidechain chunks when their parent conversation is present. The parent
/// is the authoritative origin; a sidechain remains independently searchable
/// when the parent did not match.
pub fn dedupe_sidechain_origins(
    results: &mut Vec<EnrichedResult>,
    parent_of: &std::collections::HashMap<String, String>,
) {
    use std::collections::HashSet;
    let present_convs: HashSet<String> = results
        .iter()
        .filter(|result| !result.chunk.is_sidechain)
        .map(|result| result.chunk.conversation_id.clone())
        .collect();
    results.retain(|result| {
        if !result.chunk.is_sidechain {
            return true;
        }
        match parent_of.get(&result.chunk.id) {
            Some(parent) => !present_convs.contains(parent),
            None => true,
        }
    });
}

/// Format search results as rich XML matching the Python server output.
pub fn format_search_results(
    results: &[EnrichedResult],
    query: &str,
    project_scope: &str,
    search_ms: u64,
    embed_ms: u64,
) -> String {
    format_search_results_with_rank_scores(results, query, project_scope, search_ms, embed_ms, &[])
}

/// Format search results with the effective rerank scores for candidates whose
/// position differs from pure raw-score order.
pub(crate) fn format_search_results_with_rank_scores(
    results: &[EnrichedResult],
    query: &str,
    project_scope: &str,
    search_ms: u64,
    embed_ms: u64,
    display_rank_scores: &[DisplayRankScore],
) -> String {
    let mut out = String::new();

    // Upfront summary
    if results.is_empty() {
        out.push_str(&format!(
            "❌ NO RESULTS: No conversations found matching '{}'\n",
            query
        ));
    } else {
        let top_score = results
            .iter()
            .map(|result| result.score)
            .fold(f32::NEG_INFINITY, f32::max);
        let relevance = relevance_label(top_score);
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
        let top_score = results
            .iter()
            .map(|result| result.score)
            .fold(f32::NEG_INFINITY, f32::max);
        let relevance = relevance_label(top_score);

        let preview = &results[0].chunk.content;
        let preview_short = if preview.chars().count() > 100 {
            format!("{}...", truncate_chars(preview, 100))
        } else {
            preview.clone()
        };

        out.push_str(&format!(
            "  <summary count=\"{}\" relevance=\"{}\" top-score=\"{:.3}\">\n",
            results.len(),
            relevance,
            top_score,
        ));
        out.push_str(&format!(
            "    <preview>{}</preview>\n",
            xml_escape(&preview_short)
        ));
        out.push_str("  </summary>\n");
    }

    // Meta
    out.push_str("  <meta>\n");
    out.push_str(&format!("    <q>{}</q>\n", xml_escape(query)));
    out.push_str(&format!(
        "    <scope>{}</scope>\n",
        xml_escape(project_scope)
    ));
    out.push_str(&format!("    <count>{}</count>\n", results.len()));
    if !results.is_empty() {
        let min_score = results
            .iter()
            .map(|result| result.score)
            .fold(f32::INFINITY, f32::min);
        let max_score = results
            .iter()
            .map(|result| result.score)
            .fold(f32::NEG_INFINITY, f32::max);
        out.push_str(&format!(
            "    <range>{:.3}-{:.3}</range>\n",
            min_score, max_score,
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
        if let Some(rank_score) = display_rank_scores
            .iter()
            .find(|rank_score| rank_score.chunk_id == r.chunk.id)
        {
            out.push_str(&format!(
                "      <s adj=\"{:.3}\">{:.3}</s>\n",
                rank_score.adjusted_score, r.score
            ));
        } else {
            out.push_str(&format!("      <s>{:.3}</s>\n", r.score));
        }
        out.push_str(&format!(
            "      <p>{}</p>\n",
            xml_escape(&r.chunk.project_name)
        ));

        // Age stamp
        out.push_str(&format!("      <t>{}</t>\n", age_stamp(&r.chunk.timestamp)));

        // Excerpt
        let excerpt = &r.chunk.content;
        out.push_str(&format!(
            "      <excerpt><![CDATA[{}]]></excerpt>\n",
            excerpt
        ));

        // Conversation ID
        out.push_str(&format!("      <cid>{}</cid>\n", r.chunk.conversation_id));
        out.push_str(&format!("      <id>{}</id>\n", r.chunk.id));

        if let Some(ref note) = r.resolution {
            out.push_str(&format!(
                "      <resolution>{}</resolution>\n",
                xml_escape(note)
            ));
        }

        out.push_str("    </r>\n");
    }
    out.push_str("  </results>\n");

    let resolved_count = results
        .iter()
        .filter(|r| {
            r.resolution
                .as_deref()
                .is_some_and(|s| s.starts_with("resolved"))
        })
        .count();
    if resolved_count > 0 {
        out.push_str(&format!(
            "  <note>{} resolved item(s) demoted within page — matched but verified addressed</note>\n",
            resolved_count
        ));
    }

    // v10 dream verdicts: chunks whose bound code symbol is gone/fully stale
    // at the observed HEAD are sunk below every non-demoted result (never
    // dropped) by the search validity partition — see
    // `mcp::tools::apply_validity_partition`, the only writer of
    // `validity_demoted`. The flag (not a substring of `resolution`) drives
    // this count: ledger evidence text containing "[stale anchor]" must not
    // fire it, and with the kill switch on the flag is never set, so this
    // block contributes nothing (byte-identical pre-partition output).
    let demoted_count = results.iter().filter(|r| r.validity_demoted).count();
    if demoted_count > 0 {
        out.push_str(&format!(
            "  <note>{} item(s) demoted within page — bound code anchor no longer current (dream verdict)</note>\n",
            demoted_count
        ));
    }

    out.push_str("</search>\n");

    out
}

/// Format a quick check response.
///
/// This surface must be able to say *no*. A nearest neighbour always exists, so
/// reporting `count=1` plus a preview for any top score at all turned every probe
/// — including topics that were never discussed — into an apparent confirmation.
/// Below [`QUICK_CHECK_FLOOR`] the answer is a negative existence claim with no
/// preview (weak-match preview text is exactly how fabrication presents); in the
/// measured overlap band it is labelled `weak` and carries an honest calibration
/// warning — absolute cosine is weakly calibrated on this corpus, not that the
/// topic was probably never discussed (genuine topics measure inside the same
/// band). Every `found=true` response also carries a top1-top2 margin alongside
/// the score: a cheap, already-computed signal for whether the top hit actually
/// beat the field, since the score alone cannot be trusted to say so here.
pub fn format_quick_check(results: &[EnrichedResult], _query: &str) -> String {
    let mut out = String::new();

    out.push_str("<quick_search>\n");

    let top = results.first().filter(|t| t.score >= QUICK_CHECK_FLOOR);

    let Some(top) = top else {
        // Nothing found, or nothing above the floor: negative existence claim.
        out.push_str("  <found>false</found>\n");
        out.push_str("  <count>0</count>\n");
        out.push_str("  <collections_with_matches>0</collections_with_matches>\n");
        out.push_str(
            "  <message>no sufficiently similar past discussion — topic likely not discussed before</message>\n",
        );
        if let Some(rejected) = results.first() {
            out.push_str(&format!(
                "  <best_rejected_score>{:.3}</best_rejected_score>\n",
                rejected.score
            ));
            out.push_str(&format!(
                "  <floor>{:.2}</floor>\n  <note>nearest neighbour scored below the abstention floor; preview withheld because weak-match text reads as confirmation</note>\n",
                QUICK_CHECK_FLOOR
            ));
        }
        out.push_str("</quick_search>\n");
        return out;
    };

    let relevance = relevance_label(top.score);

    // Cheap discriminating signal: how far the top hit sits above the runner-up.
    // A wide margin says "this beat the field"; a near-zero margin says "this is
    // indistinguishable from the next candidate" — useful whether or not the top
    // score itself lands in the weakly-calibrated band. `results` is assumed
    // sorted descending by score (same assumption `dedupe_results` documents),
    // so `results[1]` is the runner-up whenever it exists.
    let margin = results.get(1).map(|second| top.score - second.score);

    out.push_str("  <found>true</found>\n");
    // The runner-up is fetched only to calculate the margin. Keep the public
    // quick-check surface at one rendered match, as promised by the tool.
    out.push_str("  <count>1</count>\n");
    out.push_str(&format!("  <relevance>{}</relevance>\n", relevance));
    out.push_str("  <collections_with_matches>1</collections_with_matches>\n");

    if relevance == "weak" {
        out.push_str(&format!(
            "  <warning>weak match — may be spurious. Absolute cosine similarity is weakly calibrated in the {:.2}-{:.2} range on this corpus: genuine and fabricated-probe scores overlap here and the score alone cannot tell them apart. Check the margin below and read the preview before treating this as evidence the topic came up.</warning>\n",
            QUICK_CHECK_FLOOR, WEAK_BAND_TOP,
        ));
    }

    out.push_str("  <top_result>\n");
    out.push_str(&format!("    <score>{:.3}</score>\n", top.score));
    match margin {
        Some(m) => out.push_str(&format!("    <margin>{:.3}</margin>\n", m)),
        None => out.push_str("    <margin>n/a</margin>\n"),
    }
    out.push_str(&format!(
        "    <timestamp>{}</timestamp>\n",
        age_stamp(&top.chunk.timestamp)
    ));
    let preview = if top.chunk.content.chars().count() > PREVIEW_CHARS {
        format!("{}...", truncate_chars(&top.chunk.content, PREVIEW_CHARS))
    } else {
        top.chunk.content.clone()
    };
    out.push_str(&format!(
        "    <preview>{}</preview>\n",
        xml_escape(&preview)
    ));
    out.push_str("  </top_result>\n");

    out.push_str("</quick_search>\n");
    out
}

/// Format search insights/summary as XML.
pub fn format_search_insights(results: &[EnrichedResult], query: &str) -> String {
    let mut out = String::new();

    if results.is_empty() {
        return "<search_summary><message>No matches found</message></search_summary>".into();
    }

    let avg_score: f32 = results.iter().map(|r| r.score).sum::<f32>() / results.len() as f32;

    out.push_str("<search_summary>\n");
    out.push_str(&format!("  <query>{}</query>\n", xml_escape(query)));
    out.push_str(&format!(
        "  <total_matches>{}</total_matches>\n",
        results.len()
    ));
    out.push_str(&format!(
        "  <average_score>{:.3}</average_score>\n",
        avg_score
    ));
    out.push_str("  <collections_matched>1</collections_matched>\n");
    out.push_str(&format!(
        "  <insight>Found {} matches with average relevance of {:.3}</insight>\n",
        results.len(),
        avg_score,
    ));
    out.push_str("</search_summary>\n");
    out
}

/// Format recent work results as XML.
pub fn format_recent_work(chunks: &[ConversationChunk], group_by: &str) -> String {
    let mut out = String::new();

    if chunks.is_empty() {
        return "<no_results>No recent conversations found.</no_results>".into();
    }

    match group_by {
        "day" => {
            // Group by day using BTreeMap for ordering
            let mut groups: std::collections::BTreeMap<String, Vec<&ConversationChunk>> =
                std::collections::BTreeMap::new();
            for chunk in chunks {
                let key = if let Some(ts) = parse_timestamp(&chunk.timestamp) {
                    ts.format("%Y-%m-%d").to_string()
                } else {
                    "unknown".into()
                };
                groups.entry(key).or_default().push(chunk);
            }

            out.push_str(&format!("<recent_work days='{}'>\n", groups.len()));
            for (day, day_chunks) in groups.iter().rev() {
                let projects: std::collections::HashSet<&str> =
                    day_chunks.iter().map(|c| c.project_name.as_str()).collect();
                out.push_str(&format!(
                    "  <day date='{}' conversations='{}'>\n",
                    day,
                    day_chunks.len()
                ));
                out.push_str(&format!(
                    "    <projects>{}</projects>\n",
                    projects.into_iter().collect::<Vec<_>>().join(", ")
                ));
                out.push_str("  </day>\n");
            }
            out.push_str("</recent_work>");
        }
        _ => {
            // Default: group by conversation
            let mut convs: std::collections::BTreeMap<String, Vec<&ConversationChunk>> =
                std::collections::BTreeMap::new();
            for chunk in chunks {
                convs
                    .entry(chunk.conversation_id.clone())
                    .or_default()
                    .push(chunk);
            }

            out.push_str(&format!("<recent_work conversations='{}'>\n", convs.len()));
            for (conv_id, conv_chunks) in convs.iter().rev() {
                let most_recent = conv_chunks.iter().max_by_key(|c| &c.timestamp).unwrap();
                let preview = if most_recent.content.chars().count() > PREVIEW_CHARS {
                    format!("{}...", truncate_chars(&most_recent.content, PREVIEW_CHARS))
                } else {
                    most_recent.content.clone()
                };
                let relative = age_stamp(&most_recent.timestamp);
                out.push_str(&format!(
                    "  <conversation id='{}' time='{}' project='{}'>\n",
                    conv_id, relative, most_recent.project_name
                ));
                out.push_str(&format!(
                    "    <preview>{}</preview>\n",
                    xml_escape(&preview)
                ));
                out.push_str("  </conversation>\n");
            }
            out.push_str("</recent_work>");
        }
    }

    out
}

/// Format time-constrained search results.
pub fn format_recency_results(
    results: &[EnrichedResult],
    query: &str,
    time_range_desc: &str,
) -> String {
    let mut out = String::new();

    if results.is_empty() {
        return format!(
            "<no_results>No results found for '{}' in the specified time range.</no_results>",
            query
        );
    }

    out.push_str(&format!(
        "<search_results query='{}' time_range='{}' count='{}'>\n",
        xml_escape(query),
        xml_escape(time_range_desc),
        results.len()
    ));

    for (i, r) in results.iter().enumerate() {
        let relative = age_stamp(&r.chunk.timestamp);
        let preview = if r.chunk.content.chars().count() > PREVIEW_CHARS {
            format!("{}...", truncate_chars(&r.chunk.content, PREVIEW_CHARS))
        } else {
            r.chunk.content.clone()
        };
        out.push_str(&format!(
            "  <result rank='{}' score='{:.3}' time='{}'>\n",
            i + 1,
            r.score,
            relative
        ));
        out.push_str(&format!(
            "    <preview>{}</preview>\n",
            xml_escape(&preview)
        ));
        out.push_str(&format!(
            "    <conversation_id>{}</conversation_id>\n",
            xml_escape(&r.chunk.conversation_id)
        ));
        if let Some(ref note) = r.resolution {
            out.push_str(&format!(
                "    <resolution>{}</resolution>\n",
                xml_escape(note)
            ));
        }
        out.push_str("  </result>\n");
    }

    out.push_str("</search_results>");
    out
}

/// Format timeline grouped by period.
pub fn format_timeline(
    groups: &std::collections::BTreeMap<String, Vec<&ConversationChunk>>,
    time_range_desc: &str,
) -> String {
    let mut out = String::new();

    if groups.is_empty() {
        return "<timeline>No activity found in the specified time range.</timeline>".into();
    }

    out.push_str(&format!(
        "<timeline range='{}' periods='{}'>\n",
        time_range_desc,
        groups.len()
    ));

    for (period_key, chunks) in groups {
        let msg_count: usize = chunks.iter().map(|c| c.message_count).sum();
        out.push_str(&format!(
            "  <period key='{}' conversations='{}'>\n",
            period_key,
            chunks.len()
        ));
        out.push_str(&format!("    <stats messages='{}'/>\n", msg_count));
        out.push_str("  </period>\n");
    }

    out.push_str("</timeline>");
    out
}

/// Format paginated "more results".
pub fn format_more_results(
    results: &[EnrichedResult],
    query: &str,
    offset: usize,
    total_available: usize,
) -> String {
    let mut out = String::new();

    if results.is_empty() {
        return format!(
            "<more_results><message>No results at offset {}</message></more_results>",
            offset
        );
    }

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

    for (i, r) in results.iter().enumerate() {
        let preview = if r.chunk.content.chars().count() > PREVIEW_CHARS {
            format!("{}...", truncate_chars(&r.chunk.content, PREVIEW_CHARS))
        } else {
            r.chunk.content.clone()
        };
        out.push_str(&format!("  <result index=\"{}\">\n", offset + i + 1));
        out.push_str(&format!("    <score>{:.3}</score>\n", r.score));
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
        out.push_str("  </result>\n");
    }

    out.push_str("</more_results>");
    out
}

/// Format file-based search results. `indexed` reports whether the code
/// graph has any extraction record for this file at all — false here means
/// this is a pure keyword fallback with no graph backing whatsoever.
pub fn format_file_results(chunks: &[ConversationChunk], file_path: &str, indexed: bool) -> String {
    let mut out = String::new();

    if chunks.is_empty() {
        return format!(
            "<file_search indexed='{}'><message>No conversations found analyzing {}</message></file_search>",
            indexed,
            xml_escape(file_path)
        );
    }

    out.push_str(&format!(
        "<file_search file='{}' count='{}' indexed='{}'>\n",
        xml_escape(file_path),
        chunks.len(),
        indexed
    ));

    for (i, chunk) in chunks.iter().enumerate() {
        let relative = age_stamp(&chunk.timestamp);
        let preview = if chunk.content.chars().count() > PREVIEW_CHARS {
            format!("{}...", truncate_chars(&chunk.content, PREVIEW_CHARS))
        } else {
            chunk.content.clone()
        };
        out.push_str(&format!(
            "  <result rank='{}' time='{}'>\n",
            i + 1,
            relative
        ));
        out.push_str(&format!(
            "    <preview>{}</preview>\n",
            xml_escape(&preview)
        ));
        out.push_str(&format!(
            "    <conversation_id>{}</conversation_id>\n",
            xml_escape(&chunk.conversation_id)
        ));
        out.push_str("  </result>\n");
    }

    out.push_str("</file_search>");
    out
}

/// Format get_full_conversation response.
pub fn format_full_conversation(
    conversation_id: &str,
    file_path: Option<&str>,
    project: Option<&str>,
) -> String {
    if let Some(path) = file_path {
        format!(
            "<conversation_file>\n<conversation_id>{}</conversation_id>\n<file_path>{}</file_path>\n<project>{}</project>\n<message>Use the Read tool with this file path to read the complete conversation. For structured facts (stats/prompts/tools/files/errors/slice/grep) without reading the raw file, use csr_transcript or `csr-engine transcript {} <view>` instead.</message>\n</conversation_file>",
            conversation_id,
            path,
            project.unwrap_or("unknown"),
            conversation_id,
        )
    } else {
        format!(
            "<conversation_file>\n<error>Conversation ID '{}' not found in any project.</error>\n<suggestion>The conversation may not have been imported yet, or the ID may be incorrect.</suggestion>\n</conversation_file>",
            conversation_id
        )
    }
}

/// Format session learnings response.
pub fn format_session_learnings(
    session_id: &str,
    learnings: &[(String, Vec<String>, String)], // (content, tags, timestamp)
) -> String {
    let mut out = String::new();

    out.push_str("<session_learnings>\n");
    out.push_str(&format!("  <session_id>{}</session_id>\n", session_id));
    out.push_str(&format!("  <count>{}</count>\n", learnings.len()));

    if learnings.is_empty() {
        out.push_str(&format!(
            "  <message>No learnings stored yet for this session. Use store_reflection() with tags=['session_{}'] to store iteration learnings.</message>\n",
            session_id
        ));
    } else {
        out.push_str("  <learnings>\n");
        for (content, tags, timestamp) in learnings {
            // Extract iteration from tags
            let iteration = tags
                .iter()
                .find(|t| t.starts_with("iteration_"))
                .map(|t| t.strip_prefix("iteration_").unwrap_or("unknown"))
                .unwrap_or("unknown");

            out.push_str(&format!("    <learning iteration=\"{}\">\n", iteration));
            out.push_str(&format!(
                "      <timestamp>{}</timestamp>\n",
                xml_escape(&age_stamp(timestamp))
            ));
            out.push_str(&format!(
                "      <content>{}</content>\n",
                xml_escape(content)
            ));
            out.push_str(&format!(
                "      <tags>{}</tags>\n",
                xml_escape(&tags.join(", "))
            ));
            out.push_str("    </learning>\n");
        }
        out.push_str("  </learnings>\n");
    }

    out.push_str("</session_learnings>\n");
    out
}

/// Helper: compute relative time string from an ISO timestamp.
fn relative_time_str(timestamp: &str) -> String {
    if let Some(ts) = parse_timestamp(timestamp) {
        let days_ago = (Utc::now() - ts).num_days();
        match days_ago {
            0 => "today".into(),
            1 => "yesterday".into(),
            d if d < 7 => format!("{}d ago", d),
            d if d < 30 => format!("{}w ago", d / 7),
            d => format!("{}mo ago", d / 30),
        }
    } else {
        "unknown".into()
    }
}

/// Render an absolute-date + relative-age stamp: `as of 2026-07-03 (3w ago)`.
/// Falls back to the raw string unchanged if the timestamp cannot be parsed.
pub fn age_stamp(timestamp: &str) -> String {
    match parse_timestamp(timestamp) {
        Some(ts) => format!(
            "as of {} ({})",
            ts.format("%Y-%m-%d"),
            relative_time_str(timestamp)
        ),
        None => timestamp.to_string(),
    }
}

/// Render a resolution-ledger entry as a short annotation string.
/// `created_at` is an RFC3339 timestamp; only the date portion (first 10
/// chars, `YYYY-MM-DD`) is shown — falls back to the full string if shorter
/// than 10 chars.
pub fn resolution_note(entry_status: &str, evidence: &str, created_at: &str) -> String {
    let date = if created_at.len() >= 10 {
        &created_at[..10]
    } else {
        created_at
    };
    match entry_status {
        "resolved" => format!("resolved — {} (verified {})", evidence, date),
        "still_open" => format!("still open — verified {}", date),
        "regressed" => format!("regressed — {} ({})", evidence, date),
        other => format!("{} — {} ({})", other, evidence, date),
    }
}

// ─── v9.4 code property graph formatters ───

/// Format a file ledger (§8b): deterministic, immutable per-file dossier.
pub fn format_file_ledger(ledger: &crate::storage::codegraph::FileLedger) -> String {
    use std::fmt::Write as _;

    if ledger.symbols.is_empty() && ledger.timeline.is_empty() {
        return format!(
            "<file_ledger file='{}' indexed='{}'><message>No graph or evolution history for {}</message></file_ledger>",
            xml_escape(&ledger.file),
            ledger.indexed,
            xml_escape(&ledger.file)
        );
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "<file_ledger file='{}' indexed='{}'>",
        xml_escape(&ledger.file),
        ledger.indexed
    );

    // Symbols now (with conversation provenance).
    out.push_str("  <symbols_now>\n");
    for s in &ledger.symbols {
        // span_start/span_end are 0-based line numbers; the module-node
        // sentinel hardcodes both to 0, which is not a real span — omit
        // `lines=` rather than print the misleading '1-1'.
        let lines_attr = if s.span_start == 0 && s.span_end == 0 {
            String::new()
        } else {
            format!(" lines='{}-{}'", s.span_start + 1, s.span_end + 1)
        };
        // WP2 Stage 2: `attribution` (code_node_attribution, two channels)
        // replaces `first_conv_id` as introduction evidence here —
        // first_conv_id is a file-level projection (H4, receipt R2), never
        // a per-symbol fact. `last_conv` stays: it is a "last touched"
        // signal, not an introduction claim.
        let attribution = if s.attribution.is_empty() {
            "unattributed"
        } else {
            s.attribution.as_str()
        };
        let _ = writeln!(
            out,
            "    <symbol kind='{}' name='{}' attribution='{}' last_conv='{}' body_hash='{}'{}/>",
            xml_escape(&s.kind),
            xml_escape(&s.name),
            xml_escape(attribution),
            xml_escape(&s.last_conv_id),
            xml_escape(&s.body_hash),
            lines_attr,
        );
    }
    out.push_str("  </symbols_now>\n");

    // Timeline (code_evolution).
    out.push_str("  <timeline>\n");
    for t in &ledger.timeline {
        let added = parse_json_names(&t.functions_added);
        let removed = parse_json_names(&t.functions_removed);
        let rel = relative_time_str(&t.timestamp);
        let _ = writeln!(
            out,
            "    <change time='{}' session='{}' tool='{}' fns_added='{}' fns_removed='{}'/>",
            rel,
            xml_escape(&t.session_id),
            xml_escape(&t.tool_name),
            xml_escape(&added.join(",")),
            xml_escape(&removed.join(",")),
        );
    }
    out.push_str("  </timeline>\n");

    // Callers (who depends on this file).
    out.push_str("  <callers>\n");
    for (name, file) in &ledger.callers {
        let _ = writeln!(
            out,
            "    <caller name='{}' file='{}'/>",
            xml_escape(name),
            xml_escape(file),
        );
    }
    out.push_str("  </callers>\n");

    out.push_str("</file_ledger>");
    out
}

/// Format a code-graph query result (neighbors | callers | callees).
pub fn format_code_graph(
    mode: &str,
    target: &str,
    nodes: &[crate::storage::codegraph::NodeRow],
    neighbors: &[crate::storage::codegraph::NeighborEdge],
) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "<code_graph mode='{}' target='{}'>",
        xml_escape(mode),
        xml_escape(target)
    );

    if mode == "neighbors" {
        if neighbors.is_empty() {
            out.push_str("  <message>No neighbors found</message>\n");
        }
        for ne in neighbors {
            let attribution = if ne.node.attribution.is_empty() {
                "unattributed"
            } else {
                ne.node.attribution.as_str()
            };
            let _ = writeln!(
                out,
                "  <edge dir='{}' kind='{}' resolved='{}' name='{}' node_kind='{}' file='{}' last_conv='{}' attribution='{}'/>",
                xml_escape(&ne.direction),
                xml_escape(&ne.edge_kind),
                ne.resolved,
                xml_escape(&ne.node.name),
                xml_escape(&ne.node.kind),
                xml_escape(&ne.node.file),
                xml_escape(&ne.node.last_conv_id),
                xml_escape(attribution),
            );
        }
    } else {
        if nodes.is_empty() {
            let _ = writeln!(out, "  <message>No {} found</message>", xml_escape(mode));
        }
        for n in nodes {
            let match_attr = if n.name_only {
                "name-only"
            } else {
                "definition"
            };
            // span_start/span_end are 0-based line numbers; the module-node
            // sentinel hardcodes both to 0 — omit `lines=` rather than print '1-1'.
            let lines_attr = if n.span_start == 0 && n.span_end == 0 {
                String::new()
            } else {
                format!(" lines='{}-{}'", n.span_start + 1, n.span_end + 1)
            };
            let attribution = if n.attribution.is_empty() {
                "unattributed"
            } else {
                n.attribution.as_str()
            };
            let _ = writeln!(
                out,
                "  <node name='{}' kind='{}' file='{}' last_conv='{}' match='{}' attribution='{}'{}/>",
                xml_escape(&n.name),
                xml_escape(&n.kind),
                xml_escape(&n.file),
                xml_escape(&n.last_conv_id),
                match_attr,
                xml_escape(attribution),
                lines_attr,
            );
        }
    }

    out.push_str("</code_graph>");
    out
}

/// Parse a JSON string array of names; empty vec on any error.
fn parse_json_names(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::Speaker;

    fn make_chunk(id: &str, conversation_id: &str, content: &str) -> ConversationChunk {
        ConversationChunk {
            id: id.into(),
            conversation_id: conversation_id.into(),
            project_name: "test-project".into(),
            timestamp: "2026-01-15T10:00:00Z".into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult,
            seq: 0,
            is_sidechain: false,
        }
    }

    #[test]
    fn dedupe_plan_origins_drops_plan_when_origin_present() {
        use std::collections::HashMap;
        let mk = |id: &str, conv: &str| EnrichedResult {
            score: 0.9,
            chunk: make_chunk(id, conv, "shared decision text"),
            resolution: None,
            validity_demoted: false,
        };
        // Correlated plan + its origin conversation both matched: plan drops.
        let mut results = vec![mk("p1", "plan:witty"), mk("c1", "conv-a")];
        let origin_of: HashMap<String, String> = [("p1".to_string(), "conv-a".to_string())].into();
        dedupe_plan_origins(&mut results, &origin_of);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk.id, "c1");

        // Origin NOT in the result set: correlated plan survives.
        let mut results = vec![mk("p1", "plan:witty"), mk("c2", "conv-b")];
        dedupe_plan_origins(&mut results, &origin_of);
        assert_eq!(results.len(), 2);

        // Uncorrelated plan (no provenance edge) never drops.
        let mut results = vec![mk("p2", "plan:crispy"), mk("c1", "conv-a")];
        dedupe_plan_origins(&mut results, &HashMap::new());
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn dedupe_sidechain_origins_prefers_parent_conversation() {
        use std::collections::HashMap;
        let mk = |id: &str, conv: &str, sidechain: bool| {
            let mut chunk = make_chunk(id, conv, "shared evidence text");
            chunk.is_sidechain = sidechain;
            EnrichedResult {
                score: 0.9,
                chunk,
                resolution: None,
                validity_demoted: false,
            }
        };
        let parent_of: HashMap<String, String> =
            [("side-1".to_string(), "parent-1".to_string())].into();

        let mut tied = vec![
            mk("side-1", "agent-child", true),
            mk("origin-1", "parent-1", false),
        ];
        dedupe_sidechain_origins(&mut tied, &parent_of);
        assert_eq!(tied.len(), 1);
        assert_eq!(tied[0].chunk.id, "origin-1");

        let mut parent_absent = vec![mk("side-1", "agent-child", true)];
        dedupe_sidechain_origins(&mut parent_absent, &parent_of);
        assert_eq!(parent_absent.len(), 1);
    }

    #[test]
    fn truncate_chars_multi_byte_boundary() {
        // Chinese chars are 3 bytes each; truncating by byte count would panic.
        let s = "你好世界你好世界"; // 8 chars
        let result = truncate_chars(s, 4);
        assert_eq!(result.chars().count(), 4);
        assert_eq!(result, "你好世界");
    }

    #[test]
    fn truncate_chars_max_larger_than_string() {
        let s = "café";
        assert_eq!(truncate_chars(s, 100), s);
        assert_eq!(truncate_chars(s, 4), s);
    }

    #[test]
    fn age_stamp_valid_iso() {
        let stamp = age_stamp("2026-01-01T00:00:00Z");
        assert!(stamp.starts_with("as of 2026-01-01 ("), "got: {stamp}");
        assert!(stamp.contains(')'), "got: {stamp}");
    }

    #[test]
    fn age_stamp_unparseable_unchanged() {
        assert_eq!(age_stamp("not-a-timestamp"), "not-a-timestamp");
    }

    #[test]
    fn dedupe_drops_duplicate_chunk_id() {
        let mut results = vec![
            EnrichedResult {
                score: 0.9,
                chunk: make_chunk("same-id", "conv-a", "content A"),
                resolution: None,
                validity_demoted: false,
            },
            EnrichedResult {
                score: 0.5,
                chunk: make_chunk("same-id", "conv-b", "content B"),
                resolution: None,
                validity_demoted: false,
            },
        ];
        dedupe_results(&mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].score, 0.9);
        assert_eq!(results[0].chunk.content, "content A");
    }

    #[test]
    fn dedupe_drops_same_conversation_same_prefix_different_ids() {
        let content = "shared prefix content that is long enough for testing";
        let mut results = vec![
            EnrichedResult {
                score: 0.9,
                chunk: make_chunk("id-1", "conv-1", content),
                resolution: None,
                validity_demoted: false,
            },
            EnrichedResult {
                score: 0.7,
                chunk: make_chunk("id-2", "conv-1", content),
                resolution: None,
                validity_demoted: false,
            },
        ];
        dedupe_results(&mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk.id, "id-1");
    }

    #[test]
    fn dedupe_keeps_different_conversations_same_content() {
        let content = "identical content across conversations";
        let mut results = vec![
            EnrichedResult {
                score: 0.9,
                chunk: make_chunk("id-1", "conv-1", content),
                resolution: None,
                validity_demoted: false,
            },
            EnrichedResult {
                score: 0.8,
                chunk: make_chunk("id-2", "conv-2", content),
                resolution: None,
                validity_demoted: false,
            },
        ];
        dedupe_results(&mut results);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn dedupe_normalizes_whitespace_and_case_within_conversation() {
        let mut results = vec![
            EnrichedResult {
                score: 0.9,
                chunk: make_chunk("id-1", "conv-1", "Hello   World"),
                resolution: None,
                validity_demoted: false,
            },
            EnrichedResult {
                score: 0.7,
                chunk: make_chunk("id-2", "conv-1", "hello world"),
                resolution: None,
                validity_demoted: false,
            },
        ];
        dedupe_results(&mut results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk.id, "id-1");
    }

    #[test]
    fn resolution_note_formats_resolved() {
        let note = resolution_note("resolved", "shipped commit abc123", "2026-07-20T10:00:00Z");
        assert!(note.starts_with("resolved —"), "got: {note}");
        assert!(note.contains("shipped commit abc123"));
        assert!(note.contains("2026-07-20"));
    }

    #[test]
    fn resolution_note_formats_still_open() {
        let note = resolution_note("still_open", "unused evidence", "2026-07-20T10:00:00Z");
        assert!(note.starts_with("still open —"), "got: {note}");
        assert!(note.contains("2026-07-20"));
    }

    #[test]
    fn resolution_note_formats_regressed() {
        let note = resolution_note("regressed", "broke again in v9.4", "2026-07-20T10:00:00Z");
        assert!(note.starts_with("regressed —"), "got: {note}");
        assert!(note.contains("broke again in v9.4"));
        assert!(note.contains("2026-07-20"));
    }

    #[test]
    fn format_search_results_renders_resolution_and_footer_count() {
        let results = vec![
            EnrichedResult {
                score: 0.9,
                chunk: make_chunk("id-open", "conv-1", "open item"),
                resolution: None,
                validity_demoted: false,
            },
            EnrichedResult {
                score: 0.8,
                chunk: make_chunk("id-resolved", "conv-1", "resolved item"),
                resolution: Some(resolution_note(
                    "resolved",
                    "verified in prod",
                    "2026-07-20T10:00:00Z",
                )),
                validity_demoted: false,
            },
        ];
        let xml = format_search_results(&results, "q", "all", 1, 1);
        assert!(xml.contains("<resolution>resolved"), "got: {xml}");
        assert!(
            xml.contains("<note>1 resolved item(s) demoted"),
            "got: {xml}"
        );
        // The unresolved result must have no <resolution> tag rendered for it —
        // check there is exactly one <resolution> element in total.
        assert_eq!(xml.matches("<resolution>").count(), 1);
    }

    #[test]
    fn format_search_results_no_resolution_tag_when_none() {
        let results = vec![EnrichedResult {
            score: 0.9,
            chunk: make_chunk("id-1", "conv-1", "plain item"),
            resolution: None,
            validity_demoted: false,
        }];
        let xml = format_search_results(&results, "q", "all", 1, 1);
        assert!(!xml.contains("<resolution>"), "got: {xml}");
        assert!(!xml.contains("<note>"), "got: {xml}");
    }

    #[test]
    fn format_search_results_explains_reranked_order_with_raw_extrema() {
        let results = vec![
            EnrichedResult {
                score: 0.474,
                chunk: make_chunk("boosted", "conv-1", "boosted result"),
                resolution: None,
                validity_demoted: false,
            },
            EnrichedResult {
                score: 0.765,
                chunk: make_chunk("demoted", "conv-2", "demoted result"),
                resolution: None,
                validity_demoted: false,
            },
            EnrichedResult {
                score: 0.200,
                chunk: make_chunk("validity-tail", "conv-3", "validity-demoted tail"),
                resolution: None,
                validity_demoted: true,
            },
        ];

        let display_rank_scores = vec![
            DisplayRankScore {
                chunk_id: "boosted".to_string(),
                adjusted_score: 1.124,
            },
            DisplayRankScore {
                chunk_id: "demoted".to_string(),
                adjusted_score: 0.265,
            },
        ];
        let xml = format_search_results_with_rank_scores(
            &results,
            "rerank",
            "all",
            5,
            3,
            &display_rank_scores,
        );

        assert!(
            xml.contains("top score: 0.765"),
            "upfront summary must use the maximum raw score: {xml}"
        );
        assert!(
            xml.contains("top-score=\"0.765\""),
            "XML summary must use the maximum raw score: {xml}"
        );
        assert!(
            xml.contains("<range>0.200-0.765</range>"),
            "range must use raw min..max even after partitioning: {xml}"
        );
        let boosted = xml.find("<id>boosted</id>").unwrap();
        let demoted = xml.find("<id>demoted</id>").unwrap();
        let validity_tail = xml.find("<id>validity-tail</id>").unwrap();
        assert!(boosted < demoted && demoted < validity_tail, "{xml}");
        assert!(xml.contains("<s adj=\"1.124\">0.474</s>"), "got: {xml}");
        assert!(xml.contains("<s adj=\"0.265\">0.765</s>"), "got: {xml}");
        assert!(
            xml.contains("<s>0.200</s>"),
            "validity-only tail movement must not gain an adjusted score: {xml}"
        );
    }

    #[test]
    fn kill_switch_output_is_byte_identical_to_pre_partition_behavior() {
        // With CSR_NO_VALIDITY_PARTITION=1 the partition never sets
        // `validity_demoted` and never appends a note — so the formatter must
        // produce the exact pre-lane output, byte for byte, even when a
        // resolution-ledger evidence string happens to CONTAIN the literal
        // "[stale anchor]". The old substring sniff fired the dream-verdict
        // footer on exactly this fixture; the flag must not.
        let chunk = ConversationChunk {
            id: "c-1".into(),
            conversation_id: "conv-1".into(),
            project_name: "test-project".into(),
            // Deliberately unparseable so age_stamp echoes it verbatim —
            // keeps the expected string deterministic across runs.
            timestamp: "ts-fixed".into(),
            content: "hello world".into(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult,
            seq: 0,
            is_sidechain: false,
        };
        let results = vec![EnrichedResult {
            score: 0.9,
            chunk,
            resolution: Some(
                "resolved — evidence cites [stale anchor] old_fn wording (verified 2026-01-01)"
                    .to_string(),
            ),
            validity_demoted: false, // kill switch on: the partition never set it
        }];
        let xml = format_search_results(&results, "q", "all", 5, 3);
        let expected = "\u{1f3af} RESULTS: 1 matches (high relevance, top score: 0.900)\n\
\u{26a1} PERFORMANCE: 8ms (1 collection searched)\n\
\n\
<search>\n\
\x20 <summary count=\"1\" relevance=\"high\" top-score=\"0.900\">\n\
\x20   <preview>hello world</preview>\n\
\x20 </summary>\n\
\x20 <meta>\n\
\x20   <q>q</q>\n\
\x20   <scope>all</scope>\n\
\x20   <count>1</count>\n\
\x20   <range>0.900-0.900</range>\n\
\x20   <perf>\n\
\x20     <ttl>8</ttl>\n\
\x20     <emb>3</emb>\n\
\x20     <srch>5</srch>\n\
\x20     <cols>1</cols>\n\
\x20   </perf>\n\
\x20 </meta>\n\
\x20 <results>\n\
\x20   <r rank=\"1\">\n\
\x20     <s>0.900</s>\n\
\x20     <p>test-project</p>\n\
\x20     <t>ts-fixed</t>\n\
\x20     <excerpt><![CDATA[hello world]]></excerpt>\n\
\x20     <cid>conv-1</cid>\n\
\x20     <id>c-1</id>\n\
\x20     <resolution>resolved \u{2014} evidence cites [stale anchor] old_fn wording (verified 2026-01-01)</resolution>\n\
\x20   </r>\n\
\x20 </results>\n\
\x20 <note>1 resolved item(s) demoted within page \u{2014} matched but verified addressed</note>\n\
</search>\n";
        assert_eq!(xml, expected, "kill-switch output must be byte-identical");
        assert!(
            !xml.contains("dream verdict"),
            "'[stale anchor]' inside ledger evidence must not fire the dream-verdict footer"
        );
    }

    #[test]
    fn format_code_graph_marks_name_only_vs_definition_with_lines() {
        use crate::storage::codegraph::NodeRow;
        let name_only_node = NodeRow {
            name: "resolve_edges".into(),
            kind: "function".into(),
            file: "src/other_project/resolver.rs".into(),
            last_conv_id: "conv_1".into(),
            name_only: true,
            ..NodeRow::default()
        };
        let def_node = NodeRow {
            name: "resolve_edges".into(),
            kind: "function".into(),
            file: "src/extraction/resolver.rs".into(),
            last_conv_id: "conv_2".into(),
            span_start: 9,
            span_end: 20,
            name_only: false,
            ..NodeRow::default()
        };
        let xml = format_code_graph("callers", "resolve_edges", &[name_only_node, def_node], &[]);
        assert!(xml.contains("match='name-only'"), "got: {xml}");
        assert!(xml.contains("match='definition'"), "got: {xml}");
        // span_start/span_end are 0-based; rendered as 1-based lines.
        assert!(xml.contains("lines='10-21'"), "got: {xml}");
    }

    #[test]
    fn format_code_graph_omits_lines_for_zero_span() {
        use crate::storage::codegraph::NodeRow;
        let module_node = NodeRow {
            name: "src/demo.rs".into(),
            kind: "module".into(),
            file: "src/demo.rs".into(),
            last_conv_id: "conv_1".into(),
            span_start: 0,
            span_end: 0,
            name_only: false,
            ..NodeRow::default()
        };
        let xml = format_code_graph("callers", "demo", &[module_node], &[]);
        assert!(!xml.contains("lines="), "got: {xml}");
    }

    #[test]
    fn format_code_graph_renders_attribution_never_first_conv_id() {
        // WP2 Stage 2: consumer surfaces must render the two-channel
        // `attribution` field and must never present `first_conv_id` as
        // introduction evidence.
        use crate::storage::codegraph::NodeRow;
        let attributed = NodeRow {
            name: "foo".into(),
            kind: "function".into(),
            file: "a.rs".into(),
            first_conv_id: "conv_should_not_appear".into(),
            attribution: "transcript:70690eeb".into(),
            ..NodeRow::default()
        };
        let unattributed = NodeRow {
            name: "bar".into(),
            kind: "function".into(),
            file: "a.rs".into(),
            first_conv_id: "conv_should_not_appear_either".into(),
            attribution: String::new(),
            ..NodeRow::default()
        };
        let xml = format_code_graph("callers", "foo", &[attributed, unattributed], &[]);
        assert!(
            xml.contains("attribution='transcript:70690eeb'"),
            "got: {xml}"
        );
        assert!(
            xml.contains("attribution='unattributed'"),
            "empty attribution must render as the literal 'unattributed' state: {xml}"
        );
        assert!(
            !xml.contains("conv_should_not_appear"),
            "first_conv_id must never be presented as introduction evidence: {xml}"
        );
    }

    #[test]
    fn format_file_ledger_renders_attribution_never_first_conv() {
        use crate::storage::codegraph::{FileLedger, NodeRow};
        let ledger = FileLedger {
            file: "a.rs".into(),
            symbols: vec![NodeRow {
                name: "foo".into(),
                kind: "function".into(),
                file: "a.rs".into(),
                first_conv_id: "conv_should_not_appear".into(),
                attribution: "git:624e7229".into(),
                ..NodeRow::default()
            }],
            timeline: vec![],
            callers: vec![],
            indexed: true,
        };
        let xml = format_file_ledger(&ledger);
        assert!(xml.contains("attribution='git:624e7229'"), "got: {xml}");
        assert!(
            !xml.contains("first_conv="),
            "first_conv must no longer be rendered as introduction evidence: {xml}"
        );
        assert!(!xml.contains("conv_should_not_appear"), "got: {xml}");
    }

    // ─── quick_check abstention (fabrication defect, measured 2026-08-03) ───

    fn quick_result(score: f32, content: &str) -> EnrichedResult {
        EnrichedResult {
            score,
            chunk: make_chunk("qc1", "conv-qc", content),
            resolution: None,
            validity_demoted: false,
        }
    }

    #[test]
    fn quick_check_empty_reports_not_found() {
        let xml = format_quick_check(&[], "never discussed anything");
        assert!(xml.contains("<found>false</found>"), "got: {xml}");
        assert!(xml.contains("<count>0</count>"), "got: {xml}");
        assert!(
            xml.contains("topic likely not discussed before"),
            "got: {xml}"
        );
        assert!(!xml.contains("<preview>"), "got: {xml}");
        // No candidate at all: nothing to disclose as rejected.
        assert!(!xml.contains("<best_rejected_score>"), "got: {xml}");
    }

    #[test]
    fn quick_check_below_floor_suppresses_preview_and_count() {
        // Literal score of the weakest measured fabricated probe:
        // "onboarding call with the new backend contractor about payroll access"
        // matched Clerk SMS OTP cost discussion at 0.456 — pre-fix this rendered
        // count=1 with a confident preview.
        let results = vec![quick_result(
            0.44,
            "Clerk SMS OTP costs per verification in India",
        )];
        let xml = format_quick_check(&results, "onboarding call about payroll access");
        assert!(xml.contains("<found>false</found>"), "got: {xml}");
        assert!(xml.contains("<count>0</count>"), "got: {xml}");
        assert!(!xml.contains("<count>1</count>"), "got: {xml}");
        assert!(!xml.contains("<preview>"), "got: {xml}");
        assert!(!xml.contains("Clerk SMS OTP"), "got: {xml}");
        assert!(
            xml.contains("<best_rejected_score>0.440</best_rejected_score>"),
            "got: {xml}"
        );
    }

    #[test]
    fn quick_check_weak_band_labels_and_warns() {
        // The four fabricated probes measured 2026-08-03 all land in the weak
        // band: no floor separates them from genuine topics (genuine minimum was
        // 0.468), so they are labelled and warned about rather than suppressed.
        for score in [0.456_f32, 0.461, 0.583, 0.588] {
            let results = vec![quick_result(
                score,
                "unrelated content that merely looks close",
            )];
            let xml = format_quick_check(&results, "fabricated topic");
            assert!(
                xml.contains("<relevance>weak</relevance>"),
                "{score}: {xml}"
            );
            assert!(xml.contains("may be spurious"), "{score}: {xml}");
            assert!(xml.contains("<found>true</found>"), "{score}: {xml}");
            assert!((QUICK_CHECK_FLOOR..WEAK_BAND_TOP).contains(&score));
        }
    }

    #[test]
    fn quick_check_warning_no_longer_claims_indistinguishable_from_never_discussed() {
        // D4: the old wording ("not distinguishable from topics that were never
        // discussed") told users a weak score meant "probably never happened",
        // but real matches land in this exact band too. The honest claim is that
        // absolute cosine is weakly calibrated here — not a verdict on the topic.
        let results = vec![quick_result(0.50, "borderline content")];
        let xml = format_quick_check(&results, "borderline probe");
        assert!(xml.contains("<warning>"), "got: {xml}");
        assert!(
            !xml.contains("never discussed"),
            "warning must not claim indistinguishability from never-discussed topics: {xml}"
        );
        assert!(
            xml.contains("weakly calibrated"),
            "warning should honestly describe weak calibration instead: {xml}"
        );
    }

    #[test]
    fn quick_check_margin_signal_differs_with_top1_top2_gap() {
        // D4 part 2: a cheap discriminating signal (top1-top2 margin) must render
        // differently for a decisive win vs. a near-tie, even when both top scores
        // land in the same relevance band.
        let decisive = vec![
            quick_result(0.70, "clear top match"),
            quick_result(0.20, "distant runner-up"),
        ];
        let near_tie = vec![
            quick_result(0.70, "clear top match"),
            quick_result(0.69, "near-tied runner-up"),
        ];

        let xml_decisive = format_quick_check(&decisive, "probe");
        let xml_tie = format_quick_check(&near_tie, "probe");

        assert!(
            xml_decisive.contains("<margin>0.500</margin>"),
            "got: {xml_decisive}"
        );
        assert!(xml_tie.contains("<margin>0.010</margin>"), "got: {xml_tie}");
        assert!(
            xml_decisive.contains("<count>1</count>"),
            "the runner-up is margin evidence, not a rendered result: {xml_decisive}"
        );
        assert!(
            !xml_decisive.contains("distant runner-up"),
            "the runner-up preview must remain hidden: {xml_decisive}"
        );
    }

    #[test]
    fn quick_check_margin_is_na_with_no_second_candidate() {
        // This slice represents the complete candidate corpus: n/a is valid
        // only because the corpus genuinely contains no runner-up.
        let results = vec![quick_result(0.80, "only candidate")];
        assert_eq!(results.len(), 1);
        let xml = format_quick_check(&results, "probe");
        assert!(xml.contains("<margin>n/a</margin>"), "got: {xml}");
    }

    #[test]
    fn quick_check_mid_band_is_labelled_partial() {
        let results = vec![quick_result(0.673, "npm OIDC trusted publisher workflow")];
        let xml = format_quick_check(&results, "npm OIDC trusted publisher");
        assert!(xml.contains("<found>true</found>"), "got: {xml}");
        assert!(xml.contains("<relevance>partial</relevance>"), "got: {xml}");
        assert!(!xml.contains("may be spurious"), "got: {xml}");
        assert!(
            xml.contains("npm OIDC trusted publisher workflow"),
            "got: {xml}"
        );
    }

    #[test]
    fn quick_check_strong_match_keeps_preview_and_count() {
        let results = vec![quick_result(
            0.816,
            "SessionStart memory manifest injection fix",
        )];
        let xml = format_quick_check(&results, "SessionStart memory manifest injection");
        assert!(xml.contains("<found>true</found>"), "got: {xml}");
        assert!(xml.contains("<count>1</count>"), "got: {xml}");
        assert!(xml.contains("<score>0.816</score>"), "got: {xml}");
        assert!(xml.contains("<relevance>good</relevance>"), "got: {xml}");
        assert!(
            xml.contains("SessionStart memory manifest injection fix"),
            "got: {xml}"
        );
        assert!(!xml.contains("may be spurious"), "got: {xml}");
    }

    #[test]
    fn relevance_label_bands_are_shared_with_search() {
        assert_eq!(relevance_label(0.90), "high");
        assert_eq!(relevance_label(0.80), "good");
        assert_eq!(relevance_label(0.62), "partial");
        assert_eq!(relevance_label(0.619), "weak");
        assert_eq!(relevance_label(0.30), "weak");
        // Same banding drives the search renderer's summary attribute.
        let xml = format_search_results(
            &[quick_result(0.50, "borderline content")],
            "q",
            "proj",
            1,
            1,
        );
        assert!(xml.contains("relevance=\"weak\""), "got: {xml}");
    }
}
