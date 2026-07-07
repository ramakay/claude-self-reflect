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

        // Relative time
        if let Some(ts) = parse_timestamp(&r.chunk.timestamp) {
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
        out.push_str(&format!(
            "      <excerpt><![CDATA[{}]]></excerpt>\n",
            excerpt
        ));

        // Conversation ID
        out.push_str(&format!("      <cid>{}</cid>\n", r.chunk.conversation_id));

        out.push_str("    </r>\n");
    }
    out.push_str("  </results>\n");
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
            top.chunk.timestamp
        ));
        let preview = if top.chunk.content.len() > 200 {
            format!("{}...", &top.chunk.content[..200])
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
                let preview = if most_recent.content.len() > 200 {
                    format!("{}...", &most_recent.content[..200])
                } else {
                    most_recent.content.clone()
                };
                let relative = relative_time_str(&most_recent.timestamp);
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
        let relative = relative_time_str(&r.chunk.timestamp);
        let preview = if r.chunk.content.len() > 200 {
            format!("{}...", &r.chunk.content[..200])
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
        let preview = if r.chunk.content.len() > 200 {
            format!("{}...", &r.chunk.content[..200])
        } else {
            r.chunk.content.clone()
        };
        out.push_str(&format!("  <result index=\"{}\">\n", offset + i + 1));
        out.push_str(&format!("    <score>{:.3}</score>\n", r.score));
        out.push_str(&format!(
            "    <timestamp>{}</timestamp>\n",
            xml_escape(&r.chunk.timestamp)
        ));
        out.push_str(&format!(
            "    <preview>{}</preview>\n",
            xml_escape(&preview)
        ));
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
        let relative = relative_time_str(&chunk.timestamp);
        let preview = if chunk.content.len() > 200 {
            format!("{}...", &chunk.content[..200])
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
                xml_escape(timestamp)
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
