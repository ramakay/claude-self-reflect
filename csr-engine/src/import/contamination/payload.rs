//! CSR tool payloads that reached the corpus outside a CSR tool block.
//!
//! The importer drops CSR's own MCP tool calls and their bound results by tool
//! name. That structure is missing wherever CSR's output travels as plain
//! text: a Bash or script run that calls the binary, a Codex `exec` that calls
//! the tool from JavaScript, a persisted tool-result file, a paste, and every
//! row imported before the structural rule existed.
//!
//! Search-side only. Import never calls this module: a tool result is dropped
//! at import by the tool's name and nothing else, because a grammar match that
//! is wrong there costs data. Search uses it to re-order, where a wrong verdict
//! costs a rank.
//!
//! A whitelist, and it abstains. A row is a payload only when the WHOLE row is
//! what an emitter writes (`format::format_search_results`,
//! `format_why_labeled`, or the importer's one-line rendering of a CSR tool
//! call), with nothing but whitespace around it. Text that holds a payload and
//! says more (a result followed by the analysis of it, a call marker with a
//! fix after it), a chunk cut out of the middle of an emission and prose that
//! quotes the format are not payloads: they keep the rank their text earns.

use std::sync::LazyLock;

use regex::Regex;

// A complete search-result envelope, header to closing tag. The excerpts in
// between can be any length and hold anything, quoted CSR output included.
static ENVELOPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)\A\x{1F3AF} RESULTS: \d+ match[^\n]*\n(?:\x{26A1} PERFORMANCE: \d+ms[^\n]*\n)?\n<search>\n.*</search>\z",
    )
    .expect("envelope regex")
});

// The importer's one-line rendering of a CSR tool call, and of any tool call.
static CSR_CALL_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[mcp__claude[-_]self[-_]reflect__[A-Za-z_]+(?::[^\]\n]*)?\]")
        .expect("csr call marker regex")
});
static ANY_TOOL_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[(?:mcp__[A-Za-z0-9_\-]+|ToolSearch)(?::[^\]\n]*)?\]").expect("tool marker regex")
});

// The lines of a `csr_why` result, one regex per shape the emitter writes.
const CONV: &str = r"conv_[A-Za-z0-9_\-]+";
static WHY_HEADER_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^WHY: [^\n]*$").expect("why header regex"));
static WHY_GROUP_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^{CONV}:$")).expect("why group regex"));
static WHY_ITEM_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"^\s+(?:best_via=[a-z_]+ reached_by=\S+ route_scores=\S* score=\d\.\d+(?: \[recent\x{{2191}}\])?(?: trust=\S+)?|via=\S+ score=\d\.\d+) \[[^\]\n]*\] {CONV}:(?: .*)?$"
    ))
    .expect("why item regex")
});
static WHY_NOTE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+\[CSR text[^\n]*$").expect("why note regex"));
static WHY_RECEIPT_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^receipt: scope=.*$").expect("why receipt regex"));
/// What a `csr_why` line a chunk boundary cut short can still be recognised by.
static WHY_CUT_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:best_via=|via=|receipt:|conv_)").expect("why cut regex"));

const WHY_EMPTY: &str = "No evidence chain found.";

/// Whether `text` is, whole, a `csr_why` result: its `WHY:` header, a blank
/// line, then only the lines the emitter writes (`conv_` group headers,
/// evidence items, `[CSR text]` notes, blank lines, the receipt) or the empty
/// answer. A chunk boundary may have cut the last line short; a last line that
/// still opens like an emitter line is accepted.
fn is_why_result(text: &str) -> bool {
    let mut lines = text.lines();
    let (Some(header), Some(blank)) = (lines.next(), lines.next()) else {
        return false;
    };
    if !WHY_HEADER_LINE.is_match(header) || !blank.trim().is_empty() {
        return false;
    }
    let rest: Vec<&str> = lines.collect();
    let mut content = 0usize;
    for (at, line) in rest.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let whole = line.trim_end() == WHY_EMPTY
            || WHY_GROUP_LINE.is_match(line)
            || WHY_ITEM_LINE.is_match(line)
            || WHY_NOTE_LINE.is_match(line)
            || WHY_RECEIPT_LINE.is_match(line);
        let cut = at + 1 == rest.len() && WHY_CUT_LINE.is_match(line);
        if !(whole || cut) {
            return false;
        }
        content += 1;
    }
    content > 0
}

/// Whether `text` is nothing but rendered tool-call markers, at least one of
/// them a CSR tool.
fn is_bare_csr_call(text: &str) -> bool {
    CSR_CALL_MARKER.is_match(text) && ANY_TOOL_MARKER.replace_all(text, "").trim().is_empty()
}

/// Whether `text` is, whole, something CSR emitted: a complete search-result
/// envelope, a complete `csr_why` result, or the bare rendering of a CSR tool
/// call. Whitespace around it is ignored; any other text makes it not one.
pub fn is_csr_tool_payload(text: &str) -> bool {
    let text = text.trim();
    if text.len() < 12 {
        return false;
    }
    // A wrapper that serialises the result leaves its line breaks as `\n`.
    let unescaped;
    let text = if !text.contains('\n') && text.contains("\\n") {
        unescaped = text.replace("\\n", "\n").replace("\\\"", "\"");
        unescaped.trim()
    } else {
        text
    };
    if let Some(found) = ENVELOPE.find(text) {
        let body = found.as_str();
        if body.contains("<meta>") && body.contains("<results>") && body.contains("</results>") {
            return true;
        }
    }
    is_why_result(text) || is_bare_csr_call(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "3ebfc6f9-d73f-49b3-b29b-202ed5f35255";
    const SEARCH_HEAD: &str = "🎯 RESULTS: 5 matches (partial relevance, top score: 0.450)\n⚡ PERFORMANCE: 6ms (1 collection searched)\n\n<search>\n  <summary count=\"5\" relevance=\"partial\" top-score=\"0.450\">\n    <preview>[keyword] Memory has answer.</preview>\n  </summary>";

    fn envelope(excerpt: &str) -> String {
        format!(
            "\u{1F3AF} RESULTS: 1 matches (high relevance, top score: 0.900)\n\u{26A1} PERFORMANCE: 6ms (1 collection searched)\n\n<search>\n  <summary count=\"1\" relevance=\"high\" top-score=\"0.900\" rank1-score=\"0.900\" margin=\"n/a\">\n    <preview>Decision detail.</preview>\n  </summary>\n  <meta>\n    <q>why</q>\n  </meta>\n  <results>\n    <r rank=\"1\">\n      <s>0.900</s>\n      <excerpt><![CDATA[{excerpt}]]></excerpt>\n      <cid>c1</cid>\n      <id>i1</id>\n      <trust>unknown</trust>\n    </r>\n  </results>\n</search>\n"
        )
    }

    fn why_result(query: &str) -> String {
        format!(
            "WHY: {query}\n\nconv_{ID}:\n  best_via=blend reached_by=seed+blend route_scores=seed:0.900 score=0.832 [recent\u{2191}] trust=unknown [3mo ago] conv_{ID}: ## User Request {query}\n  via=reflection score=0.794 [2026-07-15T19:39:01Z] conv_{ID}: Qdrant was dropped.\n    [CSR text] this chunk quotes CSR output\n\nreceipt: scope=claude-self-reflect | seeds selected=3 conversations=2 surfaced=3 | probes excluded=0\n"
        )
    }

    #[test]
    fn a_whole_search_result_envelope_is_a_payload() {
        assert!(is_csr_tool_payload(&envelope("why did we drop it")));
        assert!(is_csr_tool_payload(&format!(
            "\n  {}  \n",
            envelope("why did we drop it")
        )));
        // Whatever the excerpt length.
        assert!(is_csr_tool_payload(&envelope(
            &"Decision detail. ".repeat(250)
        )));
        // The same envelope serialised with escaped line breaks and quotes.
        let escaped = envelope("x").replace('\n', "\\n").replace('"', "\\\"");
        assert!(is_csr_tool_payload(&escaped));
    }

    /// Codex finding 5: a payload is the whole row. A result with a line of
    /// analysis after it, a wrapper label before it, or a cut end is not one.
    #[test]
    fn text_around_an_envelope_keeps_it_from_being_a_payload() {
        let e = envelope("why did we drop it");
        assert!(!is_csr_tool_payload(&format!(
            "{e}\nRoot cause: the install burden."
        )));
        assert!(!is_csr_tool_payload(&format!("Here is what I saw:\n{e}")));
        assert!(!is_csr_tool_payload(&format!("Exit code 0\n{e}")));
        assert!(!is_csr_tool_payload(&format!(
            "===== csr_reflect_on_past {SEARCH_HEAD}"
        )));
        let wrapped = format!(
            "{}\n{e}\n{}",
            "Here is what I saw. ".repeat(40),
            "And so on. ".repeat(40)
        );
        assert!(!is_csr_tool_payload(&wrapped));
        assert!(!is_csr_tool_payload(&e[..e.len() - 300]));
    }

    #[test]
    fn a_fragment_of_an_emission_abstains() {
        assert!(!is_csr_tool_payload(SEARCH_HEAD));
        assert!(!is_csr_tool_payload(
            "=\"5\" relevance=\"partial\" top-score=\"0.450\">     <preview>[keyword] Memor"
        ));
        assert!(!is_csr_tool_payload("</meta>   <results>     <r rank=\"1\">       <s>0.683</s>       <excerpt><![CDATA[why did we drop it]]></excerpt>       <cid>story_c95f</cid>"));
        assert!(!is_csr_tool_payload(&format!(
            "ia=reflection score=0.794 [2026-07-15T19:39:01Z] conv_{ID}: ## User Request"
        )));
    }

    #[test]
    fn a_whole_csr_why_result_is_a_payload() {
        assert!(is_csr_tool_payload(&why_result("why did we drop Qdrant")));
        assert!(is_csr_tool_payload(
            "WHY: needle\n\nNo evidence chain found.\n"
        ));
        assert!(is_csr_tool_payload(&format!(
            "WHY: q\n\nconv_{ID}:\n  via=reflection score=0.794 [2026-07-15T19:39:01Z] conv_{ID}: ## User Request why"
        )));
        // A chunk boundary cut the last line short.
        let cut = why_result("why did we drop Qdrant");
        let cut = &cut[..cut.find("  via=reflection").unwrap() + 12];
        assert!(is_csr_tool_payload(cut), "{cut}");
    }

    /// Codex finding 5, the two inputs: real analysis after the result.
    #[test]
    fn analysis_after_a_csr_why_result_or_a_call_marker_is_not_a_payload() {
        assert!(!is_csr_tool_payload(
            "WHY: storage\n\nNo evidence chain found.\n\nRoot cause: migration 42 wrote the lock."
        ));
        assert!(!is_csr_tool_payload(
            "[mcp__claude_self_reflect__csr_why]\nFix: use SQLite."
        ));
        assert!(!is_csr_tool_payload(&format!(
            "{}\nRoot cause: the install burden.",
            why_result("why did we drop Qdrant")
        )));
        assert!(!is_csr_tool_payload(&format!(
            "I ran it and got:\n{}",
            why_result("why did we drop Qdrant")
        )));
        assert!(!is_csr_tool_payload(
            "[mcp__claude-self-reflect__csr_why: why did we drop Qdrant] Fix: use SQLite."
        ));
        assert!(!is_csr_tool_payload("WHY: only a header\n\n"));
    }

    #[test]
    fn a_bare_csr_tool_call_rendering_is_a_payload() {
        assert!(is_csr_tool_payload(
            "[mcp__claude-self-reflect__csr_why: why did we drop Qdrant]"
        ));
        assert!(is_csr_tool_payload("[ToolSearch: select:mcp__claude-self-reflect__csr_reflect_on_past]\n\n[mcp__claude-self-reflect__csr_reflect_on_past: why drop Qdrant]"));
        // A call to another tool is not CSR's.
        assert!(!is_csr_tool_payload("[ToolSearch: select:Read]"));
    }

    /// A `Read` of the emitter's source, and prose that quotes the format, must
    /// survive.
    #[test]
    fn source_and_prose_that_quote_the_format_are_not_payloads() {
        let source = r##"   520	        out.push_str(&format!(
   521	            "  <summary count=\"{}\" relevance=\"{}\" top-score=\"{:.3}\" rank1-score=\"{:.3}\" margin=\"{}\">\n",
   522	            results.len(),
   523	            relevance,
   524	        ));
   528	        out.push_str(&format!("    <preview>{}</preview>\n", preview));
"##;
        assert!(!is_csr_tool_payload(source));
        assert!(!is_csr_tool_payload(
            "The parser fails on <summary count=\"5\" relevance=\"partial\">.\nFix: escape attributes before decoding."
        ));
        assert!(!is_csr_tool_payload(
            "Fix: escape relevance=\"keyword\" before decoding."
        ));
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
