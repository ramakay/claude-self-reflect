//! The `csr_quick_check` answers: the similarity answer, the exact-identifier
//! answers and the keyword-outage answer. Split out of `format/mod.rs`.
//!
//! Every positive answer carries the conversation id, the chunk id and the
//! project of the evidence, so the caller can open it with the other tools.

use super::*;
use crate::search::identifiers::{exact_id_tokens, find_exact_id};

/// The row that holds a query identifier literally, with what was and was not
/// found. Built by `mcp::tools` only after [`find_exact_id`] confirmed the
/// identifier in `row.chunk.content`.
pub struct ExactHit {
    pub row: EnrichedResult,
    /// Identifiers of the query that `row` holds literally.
    pub matched: Vec<String>,
    /// Identifiers of the query that no stored chunk held.
    pub missing: Vec<String>,
    /// How many stored chunks hold a matched identifier (the row shown is one).
    pub literal_rows: usize,
}

/// `conversation_id`, `chunk_id` and `project` of the evidence row.
fn push_handles(out: &mut String, row: &EnrichedResult) {
    out.push_str(&format!(
        "    <conversation_id>{}</conversation_id>\n    <chunk_id>{}</chunk_id>\n    <project>{}</project>\n",
        xml_escape(&row.chunk.conversation_id),
        xml_escape(&row.chunk.id),
        xml_escape(&row.chunk.project_name)
    ));
}

fn plain_preview(content: &str) -> String {
    if content.chars().count() > PREVIEW_CHARS {
        format!("{}...", truncate_chars(content, PREVIEW_CHARS))
    } else {
        content.to_string()
    }
}

/// The preview of an exact answer: the usual first [`PREVIEW_CHARS`] when they
/// hold the identifier, else a window of that size around it, so the text the
/// caller reads is the text that matched.
fn evidence_preview(content: &str, id: &str) -> String {
    let Some(at) = find_exact_id(content, id) else {
        return plain_preview(content);
    };
    let before = content[..at].chars().count();
    if before + id.chars().count() <= PREVIEW_CHARS {
        return plain_preview(content);
    }
    let start = before.saturating_sub(150);
    let window: String = content.chars().skip(start).take(PREVIEW_CHARS).collect();
    let more = content.chars().count() > start + PREVIEW_CHARS;
    format!("...{window}{}", if more { "..." } else { "" })
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
///
/// A query that holds an exact identifier does not reach this function as a
/// `found=true` unless the identifier is in the evidence; see
/// [`format_quick_check_exact`] and [`format_quick_check_exact_miss`].
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

    // Cheap discriminating signal: how far the top hit sits above the runner-up.
    // A wide margin says "this beat the field"; a near-zero margin says "this is
    // close to the next candidate" — useful whether or not the top score itself
    // lands in the weakly-calibrated band. `results` is assumed sorted
    // descending by score (same assumption `dedupe_results` documents), so
    // `results[1]` is the runner-up whenever it exists.
    let margin = results.get(1).map(|second| top.score - second.score);
    // `quick_check_with_vec` only ever populates results from the vector
    // index (`search_chunks`/`search_chunks_filtered`) — no FTS fallback, so
    // `synthetic_top` is always false here; still passed explicitly so this
    // call site does not silently drift if that changes.
    //
    // `margin_validated_for_path: true` — this margin is exact-cosine top1-
    // top2 over a single, uniform vector-index page, exactly the scale
    // `WEAK_BAND_DECISIVE_MARGIN` was measured against.
    let assessment =
        relevance_assessment(top.score, results.get(1).map(|r| r.score), 1, false, true);

    out.push_str("  <found>true</found>\n");
    // The runner-up is fetched only to calculate the margin. Keep the public
    // quick-check surface at one rendered match, as promised by the tool.
    out.push_str("  <count>1</count>\n");
    out.push_str(&format!("  <relevance>{}</relevance>\n", assessment.band));
    out.push_str("  <collections_with_matches>1</collections_with_matches>\n");

    if let Some(warning) = &assessment.warning {
        out.push_str(&format!("  <warning>{}</warning>\n", warning));
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
    push_handles(&mut out, top);
    out.push_str(&format!(
        "    <preview>{}</preview>\n",
        xml_escape(&plain_preview(&top.chunk.content))
    ));
    if let Some(note) = &top.resolution {
        out.push_str(&format!(
            "    <resolution>{}</resolution>\n",
            xml_escape(note)
        ));
    }
    out.push_str("  </top_result>\n");

    out.push_str("</quick_search>\n");
    out
}

/// A quick check whose query holds an exact identifier that a stored chunk
/// holds literally. `found=true` here means exactly that: the identifier is in
/// the preview's chunk. No cosine is invented, so the score is `n/a`.
pub fn format_quick_check_exact(hit: &ExactHit) -> String {
    let mut out = String::from("<quick_search>\n  <found>true</found>\n  <exact>true</exact>\n");
    out.push_str(&format!(
        "  <matched_identifiers>{}</matched_identifiers>\n",
        xml_escape(&hit.matched.join(" "))
    ));
    if !hit.missing.is_empty() {
        out.push_str(&format!(
            "  <missing_identifiers>{}</missing_identifiers>\n",
            xml_escape(&hit.missing.join(" "))
        ));
    }
    out.push_str("  <count>1</count>\n  <relevance>exact</relevance>\n");
    out.push_str(&format!(
        "  <literal_match_count>{}</literal_match_count>\n",
        hit.literal_rows
    ));
    out.push_str("  <collections_with_matches>1</collections_with_matches>\n  <top_result>\n");
    out.push_str("    <score>n/a</score>\n    <margin>n/a</margin>\n");
    out.push_str(&format!(
        "    <timestamp>{}</timestamp>\n",
        age_stamp(&hit.row.chunk.timestamp)
    ));
    push_handles(&mut out, &hit.row);
    let id = hit.matched.first().map(String::as_str).unwrap_or_default();
    out.push_str(&format!(
        "    <preview>{}</preview>\n",
        xml_escape(&evidence_preview(&hit.row.chunk.content, id))
    ));
    if let Some(note) = &hit.row.resolution {
        out.push_str(&format!(
            "    <resolution>{}</resolution>\n",
            xml_escape(note)
        ));
    }
    out.push_str("  </top_result>\n</quick_search>\n");
    out
}

/// A quick check whose query holds an exact identifier that no stored chunk
/// holds. The answer is `found=false` with `exact=false`. The nearest similar
/// chunk, when it clears [`QUICK_CHECK_FLOOR`], follows under its own label so
/// it cannot be read as the identifier's record; a chunk that does hold the
/// identifier (a restatement of the query, which the answer does not count) is
/// never listed as related. `window_full` says the full-text lookup read as many
/// matches as it can, so a record may lie beyond them: the answer then does not
/// claim there is none.
pub fn format_quick_check_exact_miss(
    results: &[EnrichedResult],
    ids: &[String],
    window_full: bool,
) -> String {
    let mut out = String::from("<quick_search>\n  <found>false</found>\n  <exact>false</exact>\n");
    out.push_str(&format!(
        "  <identifiers>{}</identifiers>\n  <count>0</count>\n  <collections_with_matches>0</collections_with_matches>\n",
        xml_escape(&ids.join(" "))
    ));
    if window_full {
        out.push_str(
            "  <message>none of the first matches for the identifier holds it literally, and there are more matches than were read, so this is not proof that it was never written; a similar chunk, if listed below, is related text and not a record of this identifier</message>\n",
        );
    } else {
        out.push_str(
            "  <message>no past text holds the identifier literally (a restatement of this query does not count); a similar chunk, if listed below, is related text and not a record of this identifier</message>\n",
        );
    }
    let near = results.first().filter(|row| {
        !ids.iter()
            .any(|id| find_exact_id(&row.chunk.content, id).is_some())
    });
    if let Some(near) = near.filter(|r| r.score >= QUICK_CHECK_FLOOR) {
        out.push_str(&format!(
            "  <nearest_related>\n    <label>related by similarity only; does not contain the identifier</label>\n    <score>{:.3}</score>\n    <timestamp>{}</timestamp>\n",
            near.score,
            age_stamp(&near.chunk.timestamp)
        ));
        push_handles(&mut out, near);
        out.push_str(&format!(
            "    <preview>{}</preview>\n  </nearest_related>\n",
            xml_escape(&plain_preview(&near.chunk.content))
        ));
    } else if let Some(rejected) = near {
        out.push_str(&format!(
            "  <best_rejected_score>{:.3}</best_rejected_score>\n  <floor>{:.2}</floor>\n",
            rejected.score, QUICK_CHECK_FLOOR
        ));
    }
    out.push_str("</quick_search>\n");
    out
}

/// Keyword quick checks report lexical matches without inventing cosine
/// confidence. A query that holds an exact identifier is answered from the rows
/// that hold it literally, or `found=false` with `exact=false`.
pub fn format_keyword_quick_check(results: &[EnrichedResult], query: &str) -> String {
    let ids = exact_id_tokens(query);
    let hit = |row: &EnrichedResult| -> Vec<String> {
        ids.iter()
            .filter(|id| find_exact_id(&row.chunk.content, id).is_some())
            .cloned()
            .collect()
    };
    if !ids.is_empty() {
        let literal: Vec<(&EnrichedResult, Vec<String>)> = results
            .iter()
            .map(|row| (row, hit(row)))
            .filter(|(_, matched)| !matched.is_empty())
            .collect();
        let Some((row, matched)) = literal.first() else {
            return format_quick_check_exact_miss(&[], &ids, false);
        };
        let missing = ids
            .iter()
            .filter(|id| !matched.contains(id))
            .cloned()
            .collect();
        let mut clean = (*row).clone();
        clean.chunk.content = clean
            .chunk
            .content
            .strip_prefix("[keyword] ")
            .unwrap_or(&clean.chunk.content)
            .to_string();
        return format_quick_check_exact(&ExactHit {
            row: clean,
            matched: matched.clone(),
            missing,
            literal_rows: literal.len(),
        });
    }
    let Some(top) = results.first() else {
        return "<quick_search>\n  <found>false</found>\n  <count>0</count>\n  <collections_with_matches>0</collections_with_matches>\n  <message>No keyword matches found</message>\n</quick_search>\n".into();
    };
    let resolution = top
        .resolution
        .as_deref()
        .map(|note| format!("    <resolution>{}</resolution>\n", xml_escape(note)))
        .unwrap_or_default();
    let mut handles = String::new();
    push_handles(&mut handles, top);
    format!("<quick_search>\n  <found>true</found>\n  <count>1</count>\n  <relevance>keyword</relevance>\n  <collections_with_matches>1</collections_with_matches>\n  <top_result>\n    <score>n/a</score>\n    <margin>n/a</margin>\n    <timestamp>{}</timestamp>\n{handles}    <preview>{}</preview>\n{resolution}  </top_result>\n</quick_search>\n",
        age_stamp(&top.chunk.timestamp), xml_escape(truncate_chars(&top.chunk.content, PREVIEW_CHARS)))
}
