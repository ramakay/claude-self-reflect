//! CSR tool payloads that reached the corpus outside a CSR tool block.
//!
//! The importer drops CSR's own MCP tool calls and their bound results by tool
//! name. That structure is missing wherever CSR's output travels as plain
//! text: a Bash or script run that calls the binary, a Codex `exec` that calls
//! the tool from JavaScript, a persisted tool-result file, a paste, and every
//! row imported before the structural rule existed. This module recognises the
//! output itself, from the grammar its two emitters write (`format::
//! format_search_results` and `format_why_labeled`), including a chunk-sized
//! fragment cut anywhere inside it and the JSON-escaped form a wrapper leaves.
//!
//! Search-side only. Import never calls this module: a tool result is dropped
//! at import by the tool's name and nothing else, because a grammar match that
//! is wrong there costs data. Search uses it to re-order, where a wrong verdict
//! costs a rank. [`is_csr_tool_payload`] sinks a whole emission (a complete
//! search-result envelope, a `csr_why` result in its emitted shape, a bare call
//! marker) and text the grammar dominates, such as a chunk cut from the middle
//! of an emission.
//!
//! A token can occur in prose, in source code and in a test that quotes the
//! format, and a whole tool result (a `Read` of `format/mod.rs`, a diff) must
//! not be taken for CSR's output because it holds one. So the grammar has to
//! dominate: every token is spelled the way the emitter writes it (the full
//! attribute sequence, never a bare attribute), and the text is a payload only
//! when the tokens are dense (at least one per [`DENSITY_CHARS`]), or when it
//! is a short fragment mostly made of one line of an emitter's grammar, or a
//! bare CSR tool-call marker standing alone.

use std::sync::LazyLock;

use regex::Regex;

macro_rules! token {
    ($name:ident, $pattern:literal) => {
        static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($pattern).expect(concat!(stringify!($name), " regex")));
    };
}

// Search-result grammar. `\\?"` accepts the quote both plain and JSON-escaped.
token!(RESULTS_HEADER, r"🎯 RESULTS: \d+ match");
token!(PERFORMANCE_LINE, r"⚡ PERFORMANCE: \d+ms \(");
// The summary element's attributes, instantiated: a number in `top-score`,
// the `keyword` relevance. A format string (`relevance=\"{}\"`) is source,
// not output, and does not match. Also matches the attribute run alone, for a
// chunk cut inside the element.
token!(
    SUMMARY_OPEN,
    r#"<summary count=\\?"\d+\\?" relevance=\\?"keyword\\?"|relevance=\\?"[a-z ]+\\?" top-score=\\?"\d"#
);
// A complete search-result envelope, header to closing tag. The excerpts in
// between can be any length and hold anything, quoted CSR output included.
token!(
    ENVELOPE,
    r"(?s)\x{1F3AF} RESULTS: \d+ match[^\n]*\n(?:\x{26A1} PERFORMANCE: \d+ms[^\n]*\n)?\n<search>\n.*</search>"
);
// A `csr_why` result from its first line: the header, a blank line, then
// either the empty answer or a conversation group whose first line is an item.
token!(
    WHY_COMPLETE,
    r"(?s)\A\s*WHY: [^\n]*\n\n(?:No evidence chain found\.|conv_[0-9a-f]{8}-[0-9a-f-]{27}:\n\s+(?:best_via=[a-z_]+ reached_by=\S+ route_scores=\S* score=\d|via=\S+ score=\d\.\d+ \[))"
);

token!(RESULT_OPEN, r#"<r rank=\\?"\d+\\?">"#);
token!(EXCERPT_OPEN, r"<excerpt><!\[CDATA\[[^{]");
token!(TRUST_TAG, r"<trust>[a-z_]+</trust>");
token!(CID_TAG, r"<cid>[^<\n{]{1,80}</cid>");

// `csr_why` result lines: the legacy `via=<route> score=0.794 [<ts>] conv_<id>`
// and the current `best_via=<route> reached_by=... score=... [<age>] conv_<id>`.
token!(
    WHY_LEGACY_LINE,
    r"score=\d\.\d+ \[\d{4}-\d{2}-\d{2}T[^\]\n]*\] conv_[0-9a-f]{8}-"
);
token!(
    WHY_CURRENT_LINE,
    r"best_via=[a-z_]+ reached_by=\S+ route_scores=\S* score=\d"
);
token!(
    WHY_HEADER,
    r"(?m)^\s*WHY: [^\n]*\n\s*\n?\s*conv_[0-9a-f]{8}-"
);

// The importer's one-line rendering of a CSR tool call.
token!(
    CSR_CALL_MARKER,
    r"\[mcp__claude[-_]self[-_]reflect__[A-Za-z_]+(?::[^\]\n]*)?\]"
);
token!(
    ANY_TOOL_MARKER,
    r"\[(?:mcp__[A-Za-z0-9_\-]+|ToolSearch)(?::[^\]\n]*)?\]"
);

/// A call marker with at most this much other text is the call and nothing
/// else; with more, the text around it is the person's or Claude's own.
const CALL_ONLY_RESIDUE_CHARS: usize = 40;

/// Emitter tokens must come at least this densely for a long text to be a
/// payload. A real result block carries several tokens per result (a few
/// hundred characters each); a source file or a transcript that quotes the
/// format carries a handful in thousands of characters.
const DENSITY_CHARS: usize = 400;

/// Text allowed around a complete envelope (a shell's `Exit code` line, a
/// wrapper's label) before it is no longer the emission but something that
/// holds one.
const ENVELOPE_RESIDUE_BYTES: usize = 200;

/// A lone line of an emitter's grammar makes a short fragment a payload only
/// when it is at least this share (as 1/n) of the fragment: a chunk cut inside
/// the XML is mostly the grammar, a sentence that quotes it is not.
const FRAGMENT_SHARE_DENOM: usize = 4;

/// Whether `text` is, whole, something CSR emitted: a complete search-result
/// envelope, a `csr_why` result in its emitted shape, or the bare rendering of
/// a CSR tool call. Nothing short of the whole emission qualifies.
fn is_csr_emission(text: &str) -> bool {
    if text.len() < 12 {
        return false;
    }
    // A wrapper that serialises the result leaves its line breaks as `\n`.
    let unescaped;
    let text = if !text.contains('\n') && text.contains("\\n") {
        unescaped = text.replace("\\n", "\n").replace("\\\"", "\"");
        unescaped.as_str()
    } else {
        text
    };
    if let Some(found) = ENVELOPE.find(text) {
        let body = found.as_str();
        let residue = text.len() - found.len();
        if residue <= ENVELOPE_RESIDUE_BYTES
            && body.contains("<meta>")
            && body.contains("<results>")
            && body.contains("</results>")
        {
            return true;
        }
    }
    if WHY_COMPLETE.is_match(text) {
        return true;
    }
    CSR_CALL_MARKER.is_match(text) && {
        let residue = ANY_TOOL_MARKER.replace_all(text, "");
        residue.trim().chars().count() <= CALL_ONLY_RESIDUE_CHARS
    }
}

/// Whether `text` (or a fragment of a chunk) is CSR's own search-result or
/// `csr_why` output: a whole emission ([`is_csr_emission`]), or text the
/// emitter grammar dominates. For ordering only; import never drops on this.
pub fn is_csr_tool_payload(text: &str) -> bool {
    if is_csr_emission(text) {
        return true;
    }
    if text.len() < 12 {
        return false;
    }
    // Lines of an emitter's grammar that prose has no reason to reproduce.
    let strong: [&Regex; 4] = [
        &WHY_LEGACY_LINE,
        &WHY_CURRENT_LINE,
        &WHY_HEADER,
        &SUMMARY_OPEN,
    ];
    let loose: [&Regex; 6] = [
        &RESULTS_HEADER,
        &PERFORMANCE_LINE,
        &RESULT_OPEN,
        &EXCERPT_OPEN,
        &TRUST_TAG,
        &CID_TAG,
    ];
    let (mut strong_hits, mut strong_span) = (0usize, 0usize);
    for token in strong {
        for found in token.find_iter(text) {
            strong_hits += 1;
            strong_span += found.len();
        }
    }
    let loose_hits: usize = loose
        .iter()
        .map(|token| token.find_iter(text).count())
        .sum();
    let hits = strong_hits + loose_hits;
    (hits >= 2 && hits * DENSITY_CHARS >= text.len())
        || (strong_hits == 1 && loose_hits == 0 && strong_span * FRAGMENT_SHARE_DENOM >= text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH_HEAD: &str = "🎯 RESULTS: 5 matches (partial relevance, top score: 0.450)\n⚡ PERFORMANCE: 6ms (1 collection searched)\n\n<search>\n  <summary count=\"5\" relevance=\"partial\" top-score=\"0.450\">\n    <preview>[keyword] Memory has answer.</preview>\n  </summary>";
    const WHY_HEAD: &str = "WHY: why did we drop Qdrant\n\nconv_3ebfc6f9-d73f-49b3-b29b-202ed5f35255:\n  via=reflection score=0.794 [2026-07-15T19:39:01.638330+00:00] conv_3ebfc6f9-d73f-49b3-b29b-202ed5f35255: ## User Request why did we drop Qdrant";

    #[test]
    fn a_search_result_block_is_a_payload() {
        assert!(is_csr_tool_payload(SEARCH_HEAD));
        assert!(is_csr_tool_payload(&format!(
            "===== csr_reflect_on_past {SEARCH_HEAD}"
        )));
    }

    #[test]
    fn a_json_escaped_search_result_is_a_payload() {
        let escaped = r#"{"content":[{"type":"text","text":"🎯 RESULTS: 3 matches (weak relevance, top score: 0.527)\n⚡ PERFORMANCE: 1351ms (1 collection searched)\n\n<search>\n  <summary count=\"3\" relevance=\"weak\" top-score=\"0.527\" rank1-score=\"0.5\">"#;
        assert!(is_csr_tool_payload(escaped));
    }

    #[test]
    fn a_fragment_cut_inside_the_xml_is_a_payload() {
        assert!(is_csr_tool_payload(
            "=\"5\" relevance=\"partial\" top-score=\"0.450\">     <preview>[keyword] Memor"
        ));
        assert!(is_csr_tool_payload("</meta>   <results>     <r rank=\"1\">       <s>0.683</s>       <excerpt><![CDATA[why did we drop it]]></excerpt>       <cid>story_c95f</cid>       <id>story_c95f</id>       <trust>user_history</trust>"));
    }

    #[test]
    fn a_csr_why_result_is_a_payload_whole_or_cut() {
        assert!(is_csr_tool_payload(WHY_HEAD));
        assert!(is_csr_tool_payload("ia=reflection score=0.794 [2026-07-15T19:39:01.638330+00:00] conv_3ebfc6f9-d73f-49b3-b29b-202ed5f35255: ## User Request"));
        assert!(is_csr_tool_payload("  best_via=blend reached_by=seed+blend route_scores=seed:0.9 score=0.832 trust=unknown [3mo ago] conv_3ebfc6f9-d73f-49b3-b29b-202ed5f35255: text"));
        assert!(is_csr_tool_payload("ainers.    via=seed score=0.984 [2026-07-15T20:02:35.233Z] conv_3ebfc6f9-d73f-49b3-b29b-202ed5f35255: why did we drop"));
    }

    #[test]
    fn a_bare_csr_tool_call_rendering_is_a_payload() {
        assert!(is_csr_tool_payload(
            "[mcp__claude-self-reflect__csr_why: why did we drop Qdrant]"
        ));
        assert!(is_csr_tool_payload("[ToolSearch: select:mcp__claude-self-reflect__csr_reflect_on_past]\n\n[mcp__claude-self-reflect__csr_reflect_on_past: why drop Qdrant]"));
    }

    /// A `Read` of the emitter's source, and prose that quotes the format, must
    /// survive: the tokens are there but they are a sliver of the text.
    #[test]
    fn source_and_prose_that_quote_the_format_are_not_payloads() {
        let source = r##"   520	        out.push_str(&format!(
   521	            "  <summary count=\"{}\" relevance=\"{}\" top-score=\"{:.3}\" rank1-score=\"{:.3}\" margin=\"{}\">\n",
   522	            results.len(),
   523	            relevance,
   524	            top_score,
   525	            rank1_score,
   526	            margin,
   527	        ));
   528	        out.push_str(&format!("    <preview>{}</preview>\n", preview));
   529	        out.push_str("  </summary>\n");
   530	
   531	        // One result element per row, in rank order.
   532	        for (i, result) in results.iter().enumerate() {
   533	            out.push_str(&format!("    <r rank=\"{}\">\n", i + 1));
   534	            out.push_str(&format!("      <s>{:.3}</s>\n", result.score));
   535	            out.push_str(&format!("      <excerpt><![CDATA[{}]]></excerpt>\n", excerpt));
   536	            out.push_str(&format!("      <cid>{}</cid>\n", cid));
   537	            out.push_str(&format!("      <trust>{}</trust>\n", trust));
   538	        }
   539	    }
   540	
   541	    #[test]
   542	    fn the_summary_names_the_scope() {
   543	        let xml = format_search_results(&rows, "q", 1, "claude-self-reflect");
   544	        assert!(xml.contains("top-score=\"0.765\""));
   545	        // header: "🎯 RESULTS: 1 matches (high relevance, top score: 0.900)"
   546	        assert!(xml.contains("<summary count=\"1\" relevance=\"high\" top-score=\"0.900\""));
   547	    }
"##;
        assert!(!is_csr_tool_payload(source));
        assert!(!is_csr_tool_payload(
            "The parser fails on <summary count=\"5\" relevance=\"partial\">.\nFix: escape attributes before decoding."
        ));
        assert!(!is_csr_tool_payload(
            "The assertion on relevance=\"partial\" top-score=\"0.450\" failed because the margin attribute moved; I rewrote the helper so the order of attributes no longer matters to the test."
        ));
    }

    #[test]
    fn a_dense_search_result_block_is_still_a_payload() {
        let block = format!("{SEARCH_HEAD}\n  <meta><q>why did we drop Qdrant</q></meta>\n  <results>\n    <r rank=\"1\">\n      <s>0.683</s>\n      <excerpt><![CDATA[why did we drop Docker and Qdrant for a single Rust binary?]]></excerpt>\n      <cid>story_c95f</cid>\n      <trust>external</trust>\n    </r>\n    <r rank=\"2\">\n      <s>0.746</s>\n      <excerpt><![CDATA[Checking past sessions.]]></excerpt>\n      <cid>c95f</cid>\n      <trust>user_history</trust>\n    </r>");
        assert!(is_csr_tool_payload(&block));
    }

    /// Finding 5: one quoted attribute in a sentence is not output.
    #[test]
    fn a_quoted_keyword_attribute_in_prose_is_not_a_payload() {
        let fix = "Fix: escape relevance=\"keyword\" before decoding.";
        assert!(!is_csr_tool_payload(fix));
        assert!(!is_csr_emission(fix));
        assert!(!is_csr_emission(
            "The parser fails on <summary count=\"5\" relevance=\"partial\">.\nFix: escape attributes before decoding."
        ));
    }

    /// Finding 6: a complete envelope is recognised whatever the excerpt length.
    #[test]
    fn a_complete_envelope_with_a_long_excerpt_is_an_emission() {
        let excerpt = "Decision detail. ".repeat(250);
        let envelope = format!(
            "\u{1F3AF} RESULTS: 1 matches (high relevance, top score: 0.900)\n\u{26A1} PERFORMANCE: 6ms (1 collection searched)\n\n<search>\n  <summary count=\"1\" relevance=\"high\" top-score=\"0.900\" rank1-score=\"0.900\" margin=\"n/a\">\n    <preview>Decision detail.</preview>\n  </summary>\n  <meta>\n    <q>why</q>\n  </meta>\n  <results>\n    <r rank=\"1\">\n      <s>0.900</s>\n      <excerpt><![CDATA[{excerpt}]]></excerpt>\n      <cid>c1</cid>\n      <id>i1</id>\n      <trust>unknown</trust>\n    </r>\n  </results>\n</search>\n"
        );
        assert!(envelope.len() > 4000);
        assert!(is_csr_emission(&envelope));
        assert!(is_csr_tool_payload(&envelope));
        // The same envelope serialised with escaped line breaks and quotes.
        let escaped = envelope.replace('\n', "\\n").replace('"', "\\\"");
        assert!(is_csr_emission(&escaped));
        // An envelope quoted inside a long message is something that holds one.
        let wrapped = format!(
            "{}\n{envelope}\n{}",
            "Here is what I saw. ".repeat(40),
            "And so on. ".repeat(40)
        );
        assert!(!is_csr_emission(&wrapped));
        // A truncated envelope is not a whole emission.
        assert!(!is_csr_emission(&envelope[..envelope.len() - 300]));
    }

    #[test]
    fn a_csr_why_result_is_an_emission_only_from_its_first_line() {
        assert!(is_csr_emission(WHY_HEAD));
        assert!(is_csr_emission("WHY: needle\n\nNo evidence chain found.\n"));
        assert!(!is_csr_emission(&format!("I ran it and got:\n{WHY_HEAD}")));
    }

    #[test]
    fn prose_about_the_format_is_not_a_payload() {
        assert!(!is_csr_tool_payload("flagship query (\"why did we drop Docker and Qdrant\") returned 5/5 echoes of the question (story, reflection, terminal capture), zero decision content, top score 0.936."));
        assert!(!is_csr_tool_payload(
            "The result header reads 🎯 RESULTS: N matches and the footer ⚡ PERFORMANCE: Nms."
        ));
        assert!(!is_csr_tool_payload("Qdrant died with the whole v7 Python/Docker stack. Install friction was the reason; see [mcp__claude-self-reflect__csr_why: x] for the trail, which I read and then rewrote in full below with the actual decision text and receipts."));
        assert!(!is_csr_tool_payload("grep -rn \"via=\" src | head"));
        assert!(!is_csr_tool_payload(""));
    }
}
