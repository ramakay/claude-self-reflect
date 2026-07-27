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

/// A search result enriched with chunk metadata.
pub struct EnrichedResult {
    pub score: f32,
    pub chunk: ConversationChunk,
    /// Resolution-ledger annotation, when a verdict has been recorded for this chunk.
    pub resolution: Option<String>,
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

    out.push_str("</search>\n");

    out
}

/// Format a quick check response matching the Python server output.
pub fn format_quick_check(results: &[EnrichedResult], _query: &str) -> String {
    let mut out = String::new();

    out.push_str("<quick_search>\n");
    out.push_str(&format!("  <count>{}</count>\n", results.len()));
    out.push_str("  <collections_with_matches>1</collections_with_matches>\n");

    if let Some(top) = results.first() {
        out.push_str("  <top_result>\n");
        out.push_str(&format!("    <score>{:.3}</score>\n", top.score));
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
    }

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

/// Format file-based search results.
pub fn format_file_results(chunks: &[ConversationChunk], file_path: &str) -> String {
    let mut out = String::new();

    if chunks.is_empty() {
        return format!(
            "<file_search><message>No conversations found analyzing {}</message></file_search>",
            file_path
        );
    }

    out.push_str(&format!(
        "<file_search file='{}' count='{}'>\n",
        xml_escape(file_path),
        chunks.len()
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
            "<conversation_file>\n<conversation_id>{}</conversation_id>\n<file_path>{}</file_path>\n<project>{}</project>\n<message>Use the Read tool with this file path to read the complete conversation.</message>\n</conversation_file>",
            conversation_id,
            path,
            project.unwrap_or("unknown"),
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
            "<file_ledger file='{}'><message>No graph or evolution history for {}</message></file_ledger>",
            xml_escape(&ledger.file),
            xml_escape(&ledger.file)
        );
    }

    let mut out = String::new();
    let _ = writeln!(out, "<file_ledger file='{}'>", xml_escape(&ledger.file));

    // Symbols now (with conversation provenance).
    out.push_str("  <symbols_now>\n");
    for s in &ledger.symbols {
        let _ = writeln!(
            out,
            "    <symbol kind='{}' name='{}' first_conv='{}' last_conv='{}' body_hash='{}'/>",
            xml_escape(&s.kind),
            xml_escape(&s.name),
            xml_escape(&s.first_conv_id),
            xml_escape(&s.last_conv_id),
            xml_escape(&s.body_hash),
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
            let _ = writeln!(
                out,
                "  <edge dir='{}' kind='{}' resolved='{}' name='{}' node_kind='{}' file='{}' last_conv='{}'/>",
                xml_escape(&ne.direction),
                xml_escape(&ne.edge_kind),
                ne.resolved,
                xml_escape(&ne.node.name),
                xml_escape(&ne.node.kind),
                xml_escape(&ne.node.file),
                xml_escape(&ne.node.last_conv_id),
            );
        }
    } else {
        if nodes.is_empty() {
            let _ = writeln!(out, "  <message>No {} found</message>", xml_escape(mode));
        }
        for n in nodes {
            let _ = writeln!(
                out,
                "  <node name='{}' kind='{}' file='{}' last_conv='{}'/>",
                xml_escape(&n.name),
                xml_escape(&n.kind),
                xml_escape(&n.file),
                xml_escape(&n.last_conv_id),
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
            },
            EnrichedResult {
                score: 0.5,
                chunk: make_chunk("same-id", "conv-b", "content B"),
                resolution: None,
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
            },
            EnrichedResult {
                score: 0.7,
                chunk: make_chunk("id-2", "conv-1", content),
                resolution: None,
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
            },
            EnrichedResult {
                score: 0.8,
                chunk: make_chunk("id-2", "conv-2", content),
                resolution: None,
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
            },
            EnrichedResult {
                score: 0.7,
                chunk: make_chunk("id-2", "conv-1", "hello world"),
                resolution: None,
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
            },
            EnrichedResult {
                score: 0.8,
                chunk: make_chunk("id-resolved", "conv-1", "resolved item"),
                resolution: Some(resolution_note(
                    "resolved",
                    "verified in prod",
                    "2026-07-20T10:00:00Z",
                )),
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
        }];
        let xml = format_search_results(&results, "q", "all", 1, 1);
        assert!(!xml.contains("<resolution>"), "got: {xml}");
        assert!(!xml.contains("<note>"), "got: {xml}");
    }
}
