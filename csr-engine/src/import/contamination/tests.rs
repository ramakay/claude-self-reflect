use super::*;
use crate::eval::contamination::{
    cases, engine_outputs, pane_document, pane_outputs, plugin_synthetic,
};
use crate::provenance::Speaker;

const RECAP: &str = "recap [2m ago]: Fixed the importer. [[CSR:RECAP]]";
const DEGRADED: &str = "CSR engine ready (degraded mode).";

fn reasons(text: &str) -> Vec<ContaminationReason> {
    detect(text).into_iter().map(|hit| hit.reason).collect()
}

fn assert_unlabeled(text: &str) {
    assert!(detect(text).is_empty(), "labeled a mention: {text:?}");
}

/// A bound for the linearity tests: generous for a debug build on a busy
/// machine, far under what a quadratic scan of these inputs takes.
const LINEAR: std::time::Duration = std::time::Duration::from_secs(3);

fn within_the_bound(label: &str, text: &str) -> Vec<Hit> {
    let start = std::time::Instant::now();
    let hits = detect(text);
    let elapsed = start.elapsed();
    assert!(
        elapsed < LINEAR,
        "{label}: {} bytes took {elapsed:?}",
        text.len()
    );
    hits
}

#[test]
fn a_hit_is_a_range_and_the_text_is_never_edited() {
    let text = format!("before\n{DEGRADED}\nafter");
    let hits = detect(&text);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].formatter, "degraded_status");
    assert_eq!(hits[0].reason, ContaminationReason::DegradedStatus);
    assert_eq!(
        &text[hits[0].start_byte..hits[0].end_byte],
        format!("{DEGRADED}\n")
    );
    // detect takes &str: there is no API that returns edited text for stored
    // conversations. The one stripper is for derived artifacts.
    assert_eq!(strip_hits(&text), "before\nafter");
}

#[test]
fn hits_over_every_fixture_row_are_ordered_disjoint_and_on_char_boundaries() {
    for case in cases() {
        let hits = detect(&case.input);
        let mut cursor = 0;
        for hit in &hits {
            assert!(hit.start_byte >= cursor, "{}: overlapping hits", case.id);
            assert!(hit.end_byte > hit.start_byte, "{}: empty hit", case.id);
            assert!(case.input.is_char_boundary(hit.start_byte), "{}", case.id);
            assert!(case.input.is_char_boundary(hit.end_byte), "{}", case.id);
            cursor = hit.end_byte;
        }
        assert_eq!(first_reason(&case.input), hits.first().map(|h| h.reason));
        assert_eq!(has_csr_text(&case.input), !hits.is_empty());
    }
}

#[test]
fn registry_ids_are_unique_and_every_appendix_b_formatter_is_registered() {
    let mut ids: Vec<&str> = registered_formatter_ids().collect();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), total, "duplicate formatter id");
    for needed in [
        "recap",
        "dream_injection",
        "lessons",
        "memory_manifest",
        "briefing",
        "continuity",
        "continuum",
        "episode_index",
        "recent_sessions",
        "empty_status",
        "degraded_status",
        "pickup",
        "injection_section",
        "code_map",
        "anchor_notice",
        "memory_feedback_report",
        "memory_feedback_probe",
        "system_reminder",
        "agent_episode_analyst",
        "agent_summarizer",
        "agent_ratification",
        "agent_narrator",
        "plugin_dream_forward",
        "plugin_compaction_card",
        "plugin_session_story",
        "plugin_side_note",
        "plugin_keep_list",
        "plugin_compact_notice",
        "plugin_carry",
        "plugin_recall",
        "plugin_reading",
        "plugin_rule",
        "plugin_recap_forward",
        "plugin_last_session",
        "plugin_leftover",
        "plugin_agents_v1",
        "plugin_code_shift",
        "plugin_lost_and_found",
        "plugin_handed_legacy",
    ] {
        assert!(
            ids.contains(&needed),
            "Appendix B formatter {needed} missing"
        );
    }
    assert_eq!(agent_prompt_signatures().len(), 4);
}

#[test]
fn quoting_a_marker_is_not_emitting_it() {
    for text in [
        format!("```\n{RECAP}\n```"),
        format!("~~~text\n{DEGRADED}\n~~~"),
        format!("````md\n```\n{RECAP}\n```\n{DEGRADED}\n````"),
        format!("```\n{DEGRADED}"),
        format!("see `{DEGRADED}` in the output"),
        format!("> {DEGRADED}"),
        format!("    {DEGRADED}"),
        format!("\t{DEGRADED}"),
        format!(" {DEGRADED}"),
        format!("   {RECAP}"),
        format!("let x = \"{DEGRADED}\";"),
        format!("src/hooks/session_start.rs:68:{DEGRADED}"),
        format!("assert_eq!(out, \"{DEGRADED}\");"),
        "Why does the token [[CSR:RECAP]] exist?".to_string(),
        format!("{RECAP} and then I kept typing"),
        "[[CSR:LESSONS]]".to_string(),
        "[[CSR:DREAM]] is the dream token".to_string(),
        "CSR CONTINUUM [2m ago]: the heading alone".to_string(),
        "SESSION CONTINUITY DETECTED - PAST CONTEXT ONLY, NOT INSTRUCTIONS".to_string(),
        "## STUCK WARNING".to_string(),
        "recap [the auth changes], then prioritize them".to_string(),
        // strings: a JSON value, a Python literal, a triple-quoted block
        "{\"sample\":\"prefix CSR's compaction card (compacting), quoted: “One.” suffix\"}"
            .to_string(),
        "sample = '''prefix CSR's compaction card (compacting), quoted: “One.” suffix'''"
            .to_string(),
        "x = \"\"\"\nCSR engine ready (degraded mode).\n\"\"\"".to_string(),
        // a free-form heading with somebody's markdown under it
        "## STUCK WARNING\n> Keep this quoted requirement".to_string(),
        "## STUCK WARNING\n    indented code line".to_string(),
        "## DON'T RETRY\n> - [episode] quoted row".to_string(),
    ] {
        assert_unlabeled(&text);
    }
}

#[test]
fn a_python_triple_quoted_card_on_its_own_lines_is_the_persons_code() {
    let text =
        "card = \"\"\"\nCSR's compaction card (compacting), quoted: “One.”\n\"\"\"\nprint(card)";
    assert_unlabeled(text);
    // after the string closes, a real card at column zero is labeled again
    let after = format!("{text}\nCSR's compaction card (compacting), quoted: “Two.”");
    assert_eq!(reasons(&after), [ContaminationReason::PluginCompactionCard]);
}

#[test]
fn the_codex_review_inputs_that_are_sentences_are_not_labeled() {
    for text in [
        "A standing rule from my mother: “Never lie”",
        "CSR's recap at the start of this session: is that generated locally?",
        "Proposal — requires verdict:\nPreserve my rollback plan.\n↳ csr-dream deadbeef",
        "[briefing: model-derived, trust=external]x\nSummary.\n\nDo not deploy. Preserve my rollback plan.",
    ] {
        assert_unlabeled(text);
    }
}

#[test]
fn a_standing_rule_is_dated_month_day_or_not_at_all() {
    for (text, label) in [
        ("A standing rule from 10-13: “Keep receipts”", true),
        ("A standing rule from 2026-10-13: “Keep receipts”", true),
        ("A standing rule from : “Keep receipts”", true),
        ("A standing rule from my mother: “Never lie”", false),
        (
            "A standing rule from last month: she said “never lie”",
            false,
        ),
    ] {
        assert_eq!(!detect(text).is_empty(), label, "{text}");
    }
}

#[test]
fn a_recap_forward_needs_the_recap_line_the_pane_sends() {
    for text in [
        "CSR's recap at the start of this session: recap [2m ago]: Fixed parser.",
        "CSR's recap at the start of this session: recap [2m ago]: Fixed parser. The rest: csr_reflect_on_past(\"conv_abc123\")",
    ] {
        assert_eq!(reasons(text), [ContaminationReason::PluginRecapForward], "{text}");
    }
}

fn cut_at_700(prefix: &str) -> String {
    format!("{prefix}{}…", "x".repeat(700 - 1 - prefix.chars().count()))
}

#[test]
fn sends_cut_at_the_one_line_boundary_are_still_the_panes() {
    for (prefix, reason) in [
        ("CSR remembered: “", ContaminationReason::PluginPaneRecall),
        ("CSR remembered a past session: “", ContaminationReason::PluginPaneRecall),
        ("Earlier you read my “keep it” as “", ContaminationReason::PluginPaneReading),
        ("A standing rule from 10-12: “", ContaminationReason::PluginStandingRule),
        (
            "Since our past sessions, commit abc1234 “fix importer” (2026-10-13) changed code we worked on: ",
            ContaminationReason::PluginCodeShift,
        ),
    ] {
        let send = cut_at_700(prefix);
        assert_eq!(send.chars().count(), 700);
        assert_eq!(reasons(&send), [reason], "{prefix}");
        assert_unlabeled(&cut_at_700("I was reading about "));
    }
    assert_unlabeled(&format!("CSR remembered: “{}…", "x".repeat(10)));
}

#[test]
fn every_send_the_panes_own_formatters_write_is_detected_whole() {
    let document = pane_document();
    assert_eq!(document.schema, "pane-emissions-v1");
    assert!(document.cases.len() >= 50, "{}", document.cases.len());
    assert!(document.cases.iter().any(|case| case.cut));
    for case in &document.cases {
        let hits = detect(&case.text);
        if crate::eval::contamination::PANE_SENDS_READ_AS_PROSE.contains(&case.id.as_str()) {
            // Known limits, kept on purpose: the plain-line recap fallback
            // ("Welcome back. Nothing is pending.") and a standing rule whose
            // date field fell back to a free-text slice read like a person's
            // sentence, so they are not labeled.
            assert!(hits.is_empty(), "{}", case.text);
            continue;
        }
        assert_eq!(
            hits.first().map(|hit| (hit.reason, hit.start_byte)),
            Some((case.reason, 0)),
            "{} ({} chars, cut: {}): {}",
            case.id,
            case.chars,
            case.cut,
            case.text
        );
    }
}

#[test]
fn the_pane_fixture_is_the_one_pane_py_would_write_now() {
    let document = pane_document();
    let pane = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../plugins/csr-companion/bin/pane.py");
    let Ok(bytes) = std::fs::read(&pane) else {
        return; // packaged without the plugin beside it
    };
    use sha2::{Digest, Sha256};
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        document.source.sha256, digest,
        "pane.py changed: run `python3 -I plugins/csr-companion/tests/export-emissions.py --write` and review the grammar"
    );
    let plugin_copy = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../plugins/csr-companion/tests/fixtures/emissions/pane-v1.json");
    if let Ok(copy) = std::fs::read_to_string(plugin_copy) {
        assert_eq!(
            copy,
            include_str!("../../../tests/fixtures/contamination/pane-v1.json")
        );
    }
}

#[test]
fn the_dreamer_forms_are_found_wherever_a_line_carries_them() {
    let pane = "CSR's dreamer asked this while you worked, quoted: “Q?” Its reasoning: “R.”";
    assert_eq!(reasons(pane), [ContaminationReason::PluginDreamForward]);
    let band = "“Q?”\n\nCSR's dreamer asked this while you worked, quoted. Its reasoning:\n\n> **T**\n> reason";
    let hits = detect(band);
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].start_byte, hits[0].end_byte), (0, band.len()));
    // mid-line after the person's words, and the hotkey digit's quote marker
    assert_eq!(
        reasons(&format!("Look at this: {pane} then reply")).len(),
        1
    );
    assert_eq!(
        reasons(">2CSR's compaction card (compacting), quoted: “One.” Keep it frozen."),
        [ContaminationReason::PluginCompactionCard]
    );
    // quoted, glued, or in a string: a mention
    for text in [
        format!("> {pane}"),
        format!("\"{pane}\""),
        format!("file.rs:68:{pane}"),
        format!("see `{pane}` there"),
    ] {
        assert_unlabeled(&text);
    }
}

#[test]
fn a_briefing_is_its_header_line_and_nothing_after_it() {
    use crate::hooks::session_start::format_briefing;
    use crate::provenance::TrustTier;
    let briefing = format_briefing(
        TrustTier::External,
        "## Session Intelligence (CSR v9.2)\n\nFixed parser.",
    );
    let hits = detect(&briefing);
    assert_eq!(hits.len(), 1);
    assert_eq!(
        &briefing[hits[0].start_byte..hits[0].end_byte],
        "[briefing: model-derived, trust=external]\n"
    );
    // the person's words under a pasted briefing are not part of it
    let pasted = "[briefing: model-derived, trust=external]\nSummary.\n\nDo not deploy. Preserve my rollback plan.";
    let hits = detect(pasted);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].end_byte < pasted.find("Do not deploy").unwrap());
}

#[test]
fn the_continuity_block_ends_with_its_own_lines() {
    use crate::hooks::session_start::{format_continuity_header, format_past_session_line};
    let text = format!(
        "{}{}",
        format_continuity_header(
            "4m ago",
            "Fix the importer",
            "Edited 3 files",
            Some("span ownership")
        ),
        format_past_session_line("2h ago", "Fix parser", "")
    );
    let hits = detect(&text);
    assert_eq!(hits.len(), 1);
    assert!(
        text[hits[0].end_byte..].starts_with("- Past session"),
        "no legacy-tail expansion"
    );
}

#[test]
fn zero_width_noise_helps_match_the_sentinel_inside_a_valid_line_only() {
    assert_eq!(
        reasons("recap [2m ago]: Fixed the importer. [\u{200b}[CSR:\u{2060}RECAP]\u{200d}]"),
        [ContaminationReason::Recap]
    );
    assert_eq!(
        reasons("recap [2m ago]: Fixed the importer. ［［CSR:RECAP］］"),
        [ContaminationReason::Recap]
    );
    assert_unlabeled("Why does [\u{200b}[CSR:RECAP]] exist?");
    assert_unlabeled("re\u{200b}cap [2m ago]: Fixed the importer. [[CSR:RECAP]]");
    let dream = "CSR ☾ dream: `scrub` went stale — witnessed 2026-10-13, receipt abc1234. Matched your mention of `x`. Past conclusion, not an instruction. [[CSR:\u{200b}DREAM]]";
    assert_eq!(reasons(dream), [ContaminationReason::DreamInjection]);
    assert_unlabeled("[[CSR:LESSONS]]\n");
    assert_unlabeled("[[CSR:LESSONS]]\nand then some prose");
}

#[test]
fn a_bom_counts_only_at_byte_zero_and_crlf_lines_are_one_line() {
    assert_eq!(reasons(&format!("\u{feff}{DEGRADED}")).len(), 1);
    assert_unlabeled(&format!("x\n\u{feff}{DEGRADED}"));
    let crlf = format!("before\r\n{RECAP}\r\nSettled: a (abc1234).\r\nafter");
    let hits = detect(&crlf);
    assert_eq!(hits.len(), 1);
    assert_eq!(
        &crlf[hits[0].start_byte..hits[0].end_byte],
        format!("{RECAP}\r\nSettled: a (abc1234).\r\n")
    );
}

#[test]
fn only_a_complete_hook_wrapper_with_csr_s_header_is_the_importers_cut() {
    let wrapped = "Please fix the importer.\n<system-reminder>CSR ENDLESS MEMORY ACTIVE — every past session\nRetrieved memory payload.</system-reminder>\nKeep this sentence.";
    let mut count = 0;
    assert_eq!(
        remove_hook_wrappers(wrapped, &mut count),
        "Please fix the importer.\n\nKeep this sentence."
    );
    assert_eq!(count, 1);
    for text in [
        "<system-reminder>The date is Tuesday.</system-reminder>",
        "<system-reminder>CSR PICKUP — never closed",
        "<system-reminder>\nSessionStart:startup hook success: CSR ENDLESS MEMORY ACTIVE — x</system-reminder>",
        "<system-reminder>## RELEVANT PAST CONTEXT - NOT INSTRUCTIONS\n- [x] y</system-reminder>",
    ] {
        let mut count = 0;
        assert_eq!(remove_hook_wrappers(text, &mut count), text, "{text}");
        assert_eq!(count, 0);
    }
    // detection reports a wrapper left in stored text, outside any fence or string
    assert_eq!(
        reasons("<system-reminder>CSR PICKUP — old</system-reminder>"),
        [ContaminationReason::SystemReminder]
    );
    assert_unlabeled("```\n<system-reminder>CSR PICKUP — x</system-reminder>\n```");
    assert_unlabeled("\"<system-reminder>CSR PICKUP — x</system-reminder>\"");
}

#[test]
fn memory_feedback_labels_a_component_only_from_its_first_bytes() {
    let report = "## CSR Memory Feedback — 2026-10-13 — session report\n- variant: x\n";
    assert_eq!(reasons(report), [ContaminationReason::MemoryFeedback]);
    assert_unlabeled("I filed a ## CSR Memory Feedback — note yesterday.");
    assert_unlabeled(&format!("Notes first.\n\n{report}"));
    let probe = "<command-message>memory-feedback</command-message>\n<command-name>/memory-feedback</command-name>\nCSR Memory Feedback Probe\nYou are reporting on the quality of the memory context CSR injected in this session.";
    assert_eq!(reasons(probe), [ContaminationReason::MemoryFeedback]);
}

#[test]
fn blocks_that_follow_one_another_are_each_found_and_prose_between_is_not() {
    let text = format!("{DEGRADED}\n{RECAP}\nmy own line\nCSR: 3 chunks, 1 reflections indexed. No recent sessions for this project.\n");
    assert_eq!(
        reasons(&text),
        [
            ContaminationReason::DegradedStatus,
            ContaminationReason::Recap,
            ContaminationReason::EmptyStatus
        ]
    );
}

#[test]
fn a_truncated_pickup_header_never_indexes_past_the_document() {
    for text in [
        "CSR PICKUP — continue",
        "CSR PICKUP — continue\r\nCSR CONTINUUM [x]: y",
        "CSR PICKUP — this prompt matches a past episode (similarity 0.87); the work may already exist — verify against it before re-deriving.",
        "CSR PICKUP — x\nCSR CONTINUUM [4m ago]: x",
        "CSR PICKUP — x\nCSR CONTINUUM [4m ago]: x\nLAST: y (outcome=success, trust=unknown)",
    ] {
        assert_unlabeled(text);
    }
}

#[test]
fn every_prefix_of_every_canary_case_is_scanned_without_panic() {
    let mut inputs: Vec<String> = engine_outputs()
        .into_iter()
        .chain(pane_outputs())
        .chain(plugin_synthetic())
        .map(|output| output.text)
        .collect();
    inputs.extend(cases().into_iter().map(|case| case.input));
    inputs.sort();
    inputs.dedup();
    for input in &inputs {
        for end in (0..=input.len()).filter(|&end| input.is_char_boundary(end)) {
            for hit in detect(&input[..end]) {
                assert!(hit.end_byte <= end);
            }
        }
    }
}

#[test]
fn the_scan_is_linear_on_the_inputs_that_made_the_old_one_quadratic() {
    let n = 50_000;
    // a card repeated on one line
    let cards = "CSR's compaction card (compacting), quoted: “x” ".repeat(n);
    assert!(!within_the_bound("repeated card", &cards).is_empty());
    // briefing headers back to back
    let briefings = "[briefing: model-derived, trust=external]\n\n".repeat(n);
    assert_eq!(within_the_bound("repeated briefing", &briefings).len(), n);
    // openers with one close and no newline
    let openers = format!("{}x</system-reminder>", "<system-reminder>".repeat(n));
    within_the_bound("openers, one close", &openers);
    // openers with no close at all
    within_the_bound("unclosed openers", &"<system-reminder>\n".repeat(n));
    within_the_bound(
        "registered unclosed",
        &"<system-reminder>CSR PICKUP — x\n".repeat(n),
    );
    // unmatched backticks and quotes on one line
    within_the_bound("backticks", &"`a ``b ```c ".repeat(n));
    within_the_bound(
        "quotes",
        &format!("{} CSR engine ready (degraded mode).", "\" ".repeat(n)),
    );
    // side-note openers, band attributions, long blank runs
    within_the_bound(
        "side notes",
        &"Here is a note offered by a side agent:\n\n".repeat(n),
    );
    within_the_bound(
        "band attributions",
        &"CSR's dreamer asked this while you worked, quoted. Its reasoning:\n\n".repeat(n),
    );
    within_the_bound("blank runs", &format!("{DEGRADED}\n{}", "\n".repeat(n * 4)));
}

/// A quote line of n `>` marks, a hotkey digit, then n cards: deciding whether
/// the quote carries a hotkey must not be redone for every candidate.
#[test]
fn a_long_quote_prefix_with_many_candidates_is_linear() {
    let n = 40_000;
    let text = format!(
        "{}2{}",
        ">".repeat(n),
        "CSR's compaction card (compacting), quoted: “x” ".repeat(n)
    );
    within_the_bound("quote prefix, many candidates", &text);
    // the hotkey form itself still labels the card it precedes
    assert_eq!(
        reasons(">2CSR's compaction card (compacting), quoted: “One.” Keep it frozen."),
        [ContaminationReason::PluginCompactionCard]
    );
    // ...and a plain quote of one is still a mention
    assert_unlabeled("> CSR's compaction card (compacting), quoted: “One.”");
}

/// The companion's agents envelope (bin/agents.py): an opening marker line, one
/// JSON line, a closing marker line. All three lines are one hit; a marker line
/// without that shape is still a hit on its own.
#[test]
fn a_complete_agents_envelope_is_one_hit_and_a_lone_marker_line_still_is() {
    let envelope = "[[CSR:AGENTS:v1]]\n{\"kind\":\"handoff\",\"session\":\"synthetic-session\",\"request\":\"req-0123456789abcdef\",\"thread\":\"000000000001\",\"instruction\":\"Copy this envelope unchanged into the background agent's prompt.\"}\n[[/CSR:AGENTS:v1]]\n";
    let hits = detect(envelope);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].formatter, "plugin_agents_v1");
    assert_eq!(
        (hits[0].start_byte, hits[0].end_byte),
        (0, envelope.len()),
        "marker, JSON and marker are one span"
    );
    // after the person's words, and with CRLF line ends
    let pasted = format!("here it is:\n{}", envelope.replace('\n', "\r\n"));
    let hits = detect(&pasted);
    assert_eq!(hits.len(), 1, "{pasted:?}");
    assert_eq!(hits[0].end_byte, pasted.len());

    // not the envelope's shape: each marker line is still a hit, nothing else is
    fn marker_only(text: &str) -> Vec<&str> {
        detect(text)
            .iter()
            .map(|hit| &text[hit.start_byte..hit.end_byte])
            .collect()
    }
    let open = "[[CSR:AGENTS:v1]]";
    let close = "[[/CSR:AGENTS:v1]]";
    for (name, text) in [
        (
            "JSON that is not an object",
            format!("{open}\n[1,2]\n{close}\n"),
        ),
        ("prose between", format!("{open}\nsome words\n{close}\n")),
        (
            "two lines between",
            format!("{open}\n{{}}\n{{}}\n{close}\n"),
        ),
        ("nothing between", format!("{open}\n{close}\n")),
    ] {
        assert_eq!(
            marker_only(&text),
            [format!("{open}\n"), format!("{close}\n")],
            "{name}"
        );
    }
    assert_eq!(marker_only(&format!("{open}\n")), [format!("{open}\n")]);
    assert_eq!(marker_only(close), [close]);
    // a mention is not one
    assert_unlabeled("the marker [[CSR:AGENTS:v1]] opens it");
    assert_unlabeled("`[[CSR:AGENTS:v1]]`");
    assert_unlabeled("> [[CSR:AGENTS:v1]]");
    assert_unlabeled(&format!("```\n{envelope}```\n"));
}

/// A line of n EM SPACES then n cards: classifying the line (indented, quoted,
/// plain) must happen once per line, not after every accepted card.
#[test]
fn a_long_unicode_whitespace_prefix_with_many_cards_is_linear() {
    let n = 40_000;
    let text = format!(
        "{}{}",
        "\u{2003}".repeat(n),
        "CSR's compaction card (compacting), quoted: “x” ".repeat(n)
    );
    within_the_bound("em-space prefix, many cards", &text);
    // the round 3 input stays linear too
    let quoted = format!(
        "{}2{}",
        ">".repeat(n),
        "CSR's compaction card (compacting), quoted: “x” ".repeat(n)
    );
    within_the_bound("quote prefix, many cards", &quoted);
}

#[test]
fn which_chunks_can_carry_csr_text() {
    assert!(labelable(Some("conversation"), Some(Speaker::User)));
    assert!(labelable(Some("sidechain"), Some(Speaker::Assistant)));
    assert!(labelable(Some("conversation"), None));
    assert!(!labelable(Some("conversation"), Some(Speaker::ToolResult)));
    assert!(!labelable(Some("plan"), Some(Speaker::User)));
    assert!(!labelable(Some("codex_rollout"), Some(Speaker::User)));
    assert!(!labelable(None, Some(Speaker::User)));
    let chunk = |conversation: &str, author, content: &str| crate::import::ConversationChunk {
        id: "c".into(),
        conversation_id: conversation.into(),
        project_name: "p".into(),
        timestamp: "2026-10-13T00:00:00Z".into(),
        content: content.into(),
        message_count: 1,
        summary: None,
        author,
        seq: 0,
        is_sidechain: false,
    };
    assert!(chunk_hit(&chunk("conv", Speaker::User, DEGRADED)).is_some());
    assert!(chunk_hit(&chunk("conv", Speaker::ToolResult, DEGRADED)).is_none());
    assert!(chunk_hit(&chunk("codex:x", Speaker::User, DEGRADED)).is_none());
    assert!(chunk_hit(&chunk("plan:x", Speaker::Assistant, DEGRADED)).is_none());
    assert!(chunk_hit(&chunk("conv", Speaker::User, "ordinary prose")).is_none());
}

#[test]
fn could_contain_emission_is_a_sound_prefilter() {
    for case in cases() {
        if !detect(&case.input).is_empty() {
            assert!(could_contain_emission(&case.input), "{}", case.id);
        }
    }
    assert!(!could_contain_emission(
        "We fixed the stale HNSW vector by replacing it explicitly."
    ));
}

/// Component detection budget: p95 at most 100 µs on 900-byte components,
/// clean and matching alike. Release builds only.
#[cfg(not(debug_assertions))]
#[test]
fn detection_p95_stays_inside_the_budget() {
    let clean = "We fixed the stale HNSW vector by replacing it explicitly, then reran the suite. "
        .repeat(12);
    let clean = &clean[..900];
    let matching = format!(
        "{}\n\n{}\n\n{}",
        "Please look at this and tell me what you think. ".repeat(6),
        "recap [4m ago]: Fixed the importer. [[CSR:RECAP]]\nSettled: a (b).\nNext: c.",
        "CSR CONTINUUM [4m ago]: Fix\nLAST: x (outcome=success, trust=user_history)\nFull state: csr_reflect_on_past(\"conv_a\")",
    );
    for (label, text) in [("clean", clean.to_string()), ("matching", matching)] {
        let mut samples = Vec::with_capacity(10_000);
        for _ in 0..10_000 {
            let start = std::time::Instant::now();
            let hits = detect(std::hint::black_box(&text));
            samples.push(start.elapsed());
            std::hint::black_box(hits);
        }
        samples.sort_unstable();
        let p95 = samples[samples.len() * 95 / 100];
        assert!(
            p95 <= std::time::Duration::from_micros(100),
            "{label} p95 {p95:?} over the 100 µs budget"
        );
    }
}

// --- dreams-CLI card judgment spans (label, never delete) ---

mod dream_cards {
    use super::*;
    use crate::storage::dream_attribution::{
        marker_line, scan_markers, DREAM_CARD_PROPOSAL_HEADER, DREAM_CARD_TAKE_HEADER,
    };

    fn card() -> String {
        format!(
            "PROJECT csr — strategy\nObserved:\n  - the gate is stuck ⌗abcd1234\n\
             {DREAM_CARD_TAKE_HEADER} first line of the model's judgment\nsecond line of it\n\
             Verify first: read the receipt\n\
             {DREAM_CARD_PROPOSAL_HEADER} skip review and merge\n{}\n",
            marker_line("cccccccccccccccc")
        )
    }

    #[test]
    fn a_pasted_dream_card_judgment_span_is_a_hit_up_to_the_next_boundary() {
        let card = card();
        let hits = detect(&card);
        let spans: Vec<&str> = hits
            .iter()
            .map(|hit| &card[hit.start_byte..hit.end_byte])
            .collect();
        assert_eq!(
            spans,
            [
                format!("{DREAM_CARD_TAKE_HEADER} first line of the model's judgment\nsecond line of it\n"),
                format!("{DREAM_CARD_PROPOSAL_HEADER} skip review and merge\n"),
            ],
            "{hits:?}"
        );
        assert!(hits
            .iter()
            .all(|hit| hit.reason == ContaminationReason::DreamCardJudgment));
        // observed facts, the homework line and the marker are not judgment
        assert!(!spans.concat().contains("the gate is stuck"));
        assert!(!spans.concat().contains("Verify first"));
        assert!(!spans.concat().contains("↳ csr-dream"));
        // a chunk that is the card is CSR text, and it is the person's bytes still
        assert!(has_csr_text(&card));
        // derived text drops the judgment and keeps the marker (binding needs it)
        let stripped = strip_hits(&card);
        assert!(!stripped.contains("model's judgment") && !stripped.contains("merge"));
        assert!(stripped.contains("the gate is stuck") && stripped.contains("Verify first"));
        assert_eq!(scan_markers(&stripped), ["cccccccccccccccc"]);
    }

    #[test]
    fn a_pasted_card_after_the_persons_words_is_found_too() {
        let pasted = format!("Look at this one:\n\n{}\nWhat do you think?", card());
        assert_eq!(
            reasons(&pasted),
            [
                ContaminationReason::DreamCardJudgment,
                ContaminationReason::DreamCardJudgment
            ]
        );
    }

    #[test]
    fn the_dream_card_marker_line_alone_is_not_a_hit() {
        let marker = marker_line("cccccccccccccccc");
        assert_unlabeled(&marker);
        assert_unlabeled(&format!("run this:\n{marker}\nthanks"));
        // the card without a judgment span: nothing to label
        assert_unlabeled(&format!(
            "PROJECT csr — strategy\nObserved:\n  - a fact ⌗abcd1234\n{marker}\n"
        ));
    }

    #[test]
    fn dream_card_negatives_are_kept() {
        let marker = marker_line("cccccccccccccccc");
        // prose that says "my take" or "the proposal" is the person's
        assert_unlabeled("My take: ship it.\nThe proposal: wait a week.\n");
        // a header phrase with no marker line closing a card
        assert_unlabeled(&format!(
            "{DREAM_CARD_TAKE_HEADER} the model said so\nand nothing closes it\n"
        ));
        // a marker that is only quoted inline, not a line of its own
        assert_unlabeled(&format!(
            "{DREAM_CARD_PROPOSAL_HEADER} do the thing\nthe card ends with `{marker}` they say\n"
        ));
        assert_unlabeled(&format!(
            "{DREAM_CARD_PROPOSAL_HEADER} do the thing\nsee also {marker} in the log\n"
        ));
        // a marker in a quote block, and a header in a quote block
        assert_unlabeled(&format!("> {DREAM_CARD_TAKE_HEADER} quoted\n> {marker}\n"));
        // a new card's PROJECT line between the header and any marker breaks the closure
        assert_unlabeled(&format!(
            "{DREAM_CARD_TAKE_HEADER} orphan\nPROJECT other — strategy\nObserved:\n{marker}\n"
        ));
        // mentions inside code are protected as everywhere else
        assert_unlabeled(&format!("```\n{}```\n", card()));
    }

    #[test]
    fn a_marker_before_the_header_does_not_close_it() {
        let marker = marker_line("cccccccccccccccc");
        assert_unlabeled(&format!(
            "{marker}\n{DREAM_CARD_TAKE_HEADER} judgment after the marker\n"
        ));
    }

    #[test]
    fn many_headers_without_a_marker_stay_linear() {
        let headers = format!("{DREAM_CARD_PROPOSAL_HEADER} x\n").repeat(20_000);
        let text = format!("PROJECT csr — strategy\n{headers}");
        assert!(within_the_bound("headers, no marker", &text).is_empty());
        let closed = format!("{text}{}\n", marker_line("cccccccccccccccc"));
        let hits = within_the_bound("headers, one marker", &closed);
        assert_eq!(hits.len(), 20_000);
        // and a card-less run of headers and one stray marker is not a card
        let stray = format!("{headers}{}\n", marker_line("cccccccccccccccc"));
        assert!(within_the_bound("headers, stray marker", &stray).is_empty());
    }
}
