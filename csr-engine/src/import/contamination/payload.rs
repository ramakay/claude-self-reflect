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
//! A single loose token can occur in prose that discusses the format; a payload
//! needs two distinct loose search-result tokens, or one line of an emitter's
//! own grammar (a `csr_why` result line, the summary element), or a bare CSR
//! tool-call marker standing alone.

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
token!(SUMMARY_OPEN, r#"<summary count=\\?"\d+\\?" relevance="#);
token!(
    RELEVANCE_ATTR,
    r#"relevance=\\?"[a-z ]+\\?" top-score=\\?""#
);
token!(RESULT_OPEN, r#"<r rank=\\?"\d+\\?">"#);
token!(EXCERPT_OPEN, r"<excerpt><!\[CDATA\[");
token!(TRUST_TAG, r"<trust>[a-z_]+</trust>");
token!(CID_TAG, r"<cid>[^<\n]{1,80}</cid>");

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

/// Whether `text` (or a fragment of a chunk) is CSR's own search-result or
/// `csr_why` output, or the bare rendering of a CSR tool call.
pub fn is_csr_tool_payload(text: &str) -> bool {
    if text.len() < 12 {
        return false;
    }
    // Each of these is one line of an emitter's grammar that prose has no
    // reason to reproduce, so one is enough.
    if WHY_LEGACY_LINE.is_match(text)
        || WHY_CURRENT_LINE.is_match(text)
        || WHY_HEADER.is_match(text)
        || SUMMARY_OPEN.is_match(text)
        || RELEVANCE_ATTR.is_match(text)
    {
        return true;
    }
    let search_tokens = [
        &*RESULTS_HEADER,
        &*PERFORMANCE_LINE,
        &*RESULT_OPEN,
        &*EXCERPT_OPEN,
        &*TRUST_TAG,
        &*CID_TAG,
    ]
    .iter()
    .filter(|token| token.is_match(text))
    .count();
    if search_tokens >= 2 {
        return true;
    }
    CSR_CALL_MARKER.is_match(text) && {
        let residue = ANY_TOOL_MARKER.replace_all(text, "");
        residue.trim().chars().count() <= CALL_ONLY_RESIDUE_CHARS
    }
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
