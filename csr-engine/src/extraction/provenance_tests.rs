use super::*;

#[test]
fn codex_r2_6_a_request_about_the_compaction_card_stays_the_persons() {
    let ask = probes::CARD_REQUEST;
    assert!(!is_csr_emission(ask));
    assert_eq!(extractable(ask).as_deref(), Some(ask));
    assert_eq!(strip_csr_emissions(ask), ask);
    // No known stage and no "):" after it: the person's words, whatever follows.
    let wrong = "CSR's compaction card (compacted by csr) is wrong; never drop my pending task.";
    assert!(!is_csr_emission(wrong));
    assert_eq!(extractable(wrong).as_deref(), Some(wrong));
    // The card's draft goes; the person's paragraphs before and after it stay.
    let card = "CSR's compaction card (compacted by csr): Context: 274k \u{2192} 70k tokens. Learnt: Never skip validation.";
    let around = format!("Look at this one.\n\n{card}\n\nWhy is the payback so slow?");
    let mine = "Look at this one.\n\nWhy is the payback so slow?";
    assert_eq!(extractable(&around).as_deref(), Some(mine));
    assert_eq!(strip_csr_emissions(&around), mine);
    // The removed `pane.py`'s one_line puts the draft on one line: the next line is the
    // person's.
    assert_eq!(
        extractable(&format!("{card}\nthat payback is wrong")).as_deref(),
        Some("that payback is wrong")
    );
    // A band hotkey that landed in the box before it is no word of the person's.
    let keyed = "2CSR's compaction card (compacting soon): 278k \u{b7} past your band.";
    assert!(is_csr_emission(keyed));
    assert!(extractable(keyed).is_none());
    assert_eq!(strip_csr_emissions(keyed), "");
}

#[test]
fn codex_r2_7_talk_that_opens_with_the_attribution_is_the_persons() {
    let talk = probes::attribution_talk();
    assert!(talk.find("CSR's dreamer").unwrap() > HEADER_WINDOW);
    assert_eq!(strip_dreamer_prompt(&talk), None);
    assert_eq!(extractable(&talk).as_deref(), Some(talk.as_str()));
    assert_eq!(strip_csr_emissions(&talk), talk);
    // Inside the header window too: a prefix of the attribution is not the forward.
    let short =
        "CSR's dreamer asked this while you worked yesterday, but I wrote today's question myself.";
    assert!(!is_csr_emission(short));
    assert_eq!(extractable(short).as_deref(), Some(short));
    assert_eq!(strip_csr_emissions(short), short);
    let asked = format!("Did you see it?\n\n{short}\n\n> my own quote");
    assert_eq!(strip_dreamer_prompt(&asked), None);
    assert_eq!(
        extractable(&asked).as_deref(),
        Some(format!("Did you see it?\n\n{short}").as_str())
    );
}

#[test]
fn codex_r2_8_every_forwarded_dream_goes_and_the_words_between_stay() {
    let f = probes::hindi_forward();
    let text = format!("{f}\n\nNo, keep the current palette.\n\n{f}");
    let mine = Some("No, keep the current palette.");
    assert_eq!(strip_dreamer_prompt(&text).as_deref(), mine);
    assert_eq!(extractable(&text).as_deref(), mine);
    assert_eq!(Some(strip_csr_emissions(&text).as_str()), mine);
    let after = format!("{text}\n\nAnd stop asking about colours.");
    let both = Some("No, keep the current palette.\n\nAnd stop asking about colours.");
    assert_eq!(extractable(&after).as_deref(), both);
    assert_eq!(Some(strip_csr_emissions(&after).as_str()), both);
    // The pane's one-line send and the band's forward in one prompt.
    let pane = "CSR's dreamer asked this while you worked: \u{201c}What if the recap led with the blocker?\u{201d} Its reasoning: Recap ordering.";
    let mixed = format!("{pane}\n\nNot now.\n\n{f}");
    assert_eq!(extractable(&mixed).as_deref(), Some("Not now."));
    assert_eq!(strip_csr_emissions(&mixed), "Not now.");
    // A band hotkey before the pane's send is not the person's; words typed before it on its line are.
    assert_eq!(
        extractable(&format!("2{pane}\nnot now")).as_deref(),
        Some("not now")
    );
    assert_eq!(
        extractable(&format!("Look at this: {pane}")).as_deref(),
        Some("Look at this:")
    );
}

fn labeled(text: &str) -> bool {
    crate::import::contamination::has_csr_text(text)
}

/// The readers that make derived artifacts agree on `input`: extraction and
/// the query-side emission stripper. (The stored conversation is not one of
/// them: import keeps every byte, and the detector only labels.)
fn own_words_agree(input: &str, mine: Option<&str>) {
    assert_eq!(extractable(input).as_deref(), mine, "extractable: {input}");
    assert_eq!(
        strip_csr_emissions(input),
        mine.unwrap_or(""),
        "strip_csr_emissions: {input}"
    );
}

/// An index item or a LAST line pasted alone is CSR's own line, with the
/// outcome label or without it: the label is withheld when it cannot be
/// trusted, and that must not make the line look like prose.
#[test]
fn a_pasted_index_item_or_last_line_is_an_emission_with_or_without_an_outcome() {
    for line in [
        "- [1h ago] Fix auth \u{2014} Investigated \u{2192} csr_reflect_on_past(\"conv_s\")",
        "- [1h ago] Fix auth \u{2014} Investigated (outcome=success) \u{2192} csr_reflect_on_past(\"conv_s\")",
        "LAST: Notes shipped. (trust=user_history)",
        "LAST: Notes shipped. (outcome=partial, trust=unknown)",
    ] {
        assert!(is_csr_emission(line), "{line}");
        assert!(extractable(line).is_none(), "{line}");
        // Behind a line of the person's own words it is still found.
        let mixed = format!("please look at this\n{line}");
        assert!(extractable(&mixed).is_none() || is_csr_emission(&mixed), "{line}");
    }
    // Prose that merely talks about the same words stays the person's.
    for prose in [
        "The last line, LAST: is a label we print.",
        "- [ ] write the test, then csr_reflect_on_past is what we call",
        "Run it and note that trust=user_history appears in status.",
    ] {
        assert!(!is_csr_emission(prose), "{prose}");
    }
}

/// A person's sentence can have the same shape as a LAST line. Only the
/// trust tiers CSR writes make it CSR's.
#[test]
fn user_prose_shaped_like_a_last_line_with_an_unknown_tier_is_not_an_emission() {
    for prose in [
        "LAST: Review the certificate chain (trust=low)",
        "LAST: check the proxy (trust=none)",
    ] {
        assert!(!is_csr_emission(prose), "{prose}");
        assert!(extractable(prose).is_some(), "{prose}");
    }
    for tier in [
        "unknown",
        "external",
        "trusted_tool",
        "user_history",
        "user_confirmed",
        "system",
    ] {
        let line = format!("LAST: Review the certificate chain (trust={tier})");
        assert!(is_csr_emission(&line), "{line}");
    }
}

/// Extraction and import must not diverge silently on a mention: where
/// the text only quotes an emission, extraction learns nothing from it and
/// the import remover leaves every byte alone.
#[test]
fn protected_mentions_are_ignored_by_extraction_and_untouched_by_import() {
    let emissions = [
        "recap [2m ago]: Fixed the importer. [[CSR:RECAP]]\nSettled: a (abc1234).",
        "CSR engine ready (degraded mode).",
        "CSR CONTINUUM [4m ago]: Fix the importer\nLAST: Added checks (outcome=success, trust=user_history)\nFull state: csr_reflect_on_past(\"conv_abc123\")\n",
        "CSR's compaction card (compacting soon), quoted: \u{201c}Keep the plan.\u{201d}",
    ];
    for emission in emissions {
        let fenced = format!("```text\n{emission}\n```");
        let quoted: String = emission.lines().map(|line| format!("> {line}\n")).collect();
        let inline = format!("the hook wrote `{}` once", emission.lines().next().unwrap());
        for mention in [&fenced, &quoted, &inline] {
            // Import: a mention, every byte stays.
            assert!(
                !labeled(mention),
                "the detector labeled a mention: {mention:?}"
            );
            // Extraction: nothing of the quoted emission survives as content.
            if let Some(own) = extractable(mention) {
                assert!(
                    !own.contains("[[CSR:RECAP]]")
                        && !own.contains("degraded mode")
                        && !own.contains("CONTINUUM")
                        && !own.contains("compaction card"),
                    "extraction re-learned a quoted emission from {mention:?}: {own:?}"
                );
            }
        }
    }
}

#[test]
fn r5_ordered_forward_prefix_table_extraction_and_scrub() {
    for (raw, expected) in ordered_forward_prefix_cases() {
        own_words_agree(&raw, Some(&expected));
    }
    for forward in [
        "CSR's compaction card (compacting), quoted: “One.”",
        "CSR's story of this session, rev 1, quoted: “One.”",
        "CSR's dreamer asked this while you worked, quoted: “Q?” Its reasoning: “One.”",
    ] {
        let quoted = format!("> I vote 2 {forward} Keep the deployment frozen.");
        // Extraction excludes quoted personal lines, as before. The import
        // remover treats a blockquote line as a mention of the forward, not
        // an emission of it: every byte stays.
        assert_eq!(extractable(&quoted), None);
        assert!(!labeled(&quoted), "a quoted line is a mention");
        // A kept whitespace suffix supplies the join itself.
        own_words_agree(
            &format!("I vote\t2{forward} Keep the deployment frozen."),
            Some("I vote\t2 Keep the deployment frozen."),
        );
    }
}

/// The dreamer's attribution was renamed "CSR's dreamer" -> "Self-Reflect's dreamer". What is
/// written now and what old transcripts carry must both be suppressed, in every form.
#[test]
fn the_self_reflect_and_the_old_csr_dreamer_wordings_are_both_suppressed() {
    for who in ["Self-Reflect's dreamer", "CSR's dreamer"] {
        let band_v1 = format!(
            "What if we reuse the palette?\n\n{who} asked this while you worked. Its reasoning:\n\n> **Reuse**\n> Shared colours."
        );
        let band_v2 = format!(
            "\u{201c}What if we reuse the palette?\u{201d}\n\n{who} asked this while you worked, quoted. Its reasoning:\n\n> **Reuse**\n> Shared colours."
        );
        let pane_v1 = format!(
            "{who} asked this while you worked: \u{201c}What if?\u{201d} Its reasoning: Queue. Moved."
        );
        let pane_v2 = format!(
            "{who} asked this while you worked, quoted: \u{201c}What if?\u{201d} Its reasoning: \u{201c}Queue moved.\u{201d}"
        );
        for whole in [&band_v1, &band_v2] {
            assert!(is_dreamer_prompt(whole), "{who}: {whole}");
            assert!(is_csr_emission(whole), "{who}: {whole}");
            assert_eq!(strip_dreamer_prompt(whole).as_deref(), Some(""), "{who}");
            assert_eq!(extractable(whole), None, "{who}: {whole}");
            assert!(labeled(whole), "import labels {who}: {whole}");
        }
        // v1 never searches for a closing quote, so it runs to the end of its line; v2 ends at its
        // closing quote and the person's words after it stay.
        for (line, after) in [(&pane_v1, ""), (&pane_v2, " Thanks.")] {
            assert!(is_csr_emission(line), "{who}: {line}");
            assert_eq!(strip_csr_forwards(line).as_deref(), Some(""), "{who}");
            assert!(labeled(line), "import labels {who}: {line}");
            let mixed = format!("Keep it frozen. {line}{after}");
            assert_eq!(
                strip_csr_forwards(&mixed).as_deref(),
                Some(if after.is_empty() {
                    "Keep it frozen."
                } else {
                    "Keep it frozen. Thanks."
                }),
                "{who}"
            );
        }
        // The person talking ABOUT the dreamer is never suppressed, in either wording.
        let talk = format!(
            "{who} asked this while you worked yesterday, but I wrote today's question myself."
        );
        assert!(!is_dreamer_prompt(&talk));
        assert_eq!(strip_csr_forwards(&talk), None, "{who}");
    }
}

/// The band's forward as `askClaude` writes it.
const BAND: &str = "What if we reuse the palette?\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> **Reuse**\n> Shared colours.";

#[test]
fn astra_4_words_after_a_truncated_legacy_send_are_the_persons() {
    // The removed `pane.py` one_line(.., 700) cut the send at 699 characters and added an ellipsis: the send ended there.
    let head =
        "CSR's dreamer asked this while you worked: \u{201c}What if?\u{201d} Its reasoning: ";
    let sent = format!("{head}{}\u{2026}", "x".repeat(699 - head.chars().count()));
    assert_eq!(sent.chars().count(), 700);
    own_words_agree(
        &format!("{sent} Keep the deployment frozen."),
        Some("Keep the deployment frozen."),
    );
    own_words_agree(
        &format!("Look: {sent} Keep the deployment frozen."),
        Some("Look: Keep the deployment frozen."),
    );
    // The card's send is cut at 1,199 characters the same way.
    let card = "CSR's compaction card (compacted by csr): ";
    let sent = format!("{card}{}\u{2026}", "y".repeat(1199 - card.chars().count()));
    own_words_agree(
        &format!("{sent} Keep the deployment frozen."),
        Some("Keep the deployment frozen."),
    );
    // Untruncated legacy sends still run to the end of their line.
    own_words_agree(
        "CSR's compaction card (compacting): Summarizing 278k. ok\nKeep it.",
        Some("Keep it."),
    );
}

#[test]
fn astra_9_a_card_after_the_persons_words_on_its_line_is_cut() {
    own_words_agree(
        "Please check this: CSR's compaction card (compacting): Summarizing 278k.",
        Some("Please check this:"),
    );
}

#[test]
fn astra_10_the_band_forward_is_cut_by_line_inside_the_persons_paragraph() {
    // A: the person's line directly above the question.
    let above = format!("Keep the deployment frozen.\n{BAND}");
    own_words_agree(&above, Some("Keep the deployment frozen."));
    assert_eq!(
        strip_dreamer_prompt(&above).as_deref(),
        Some("Keep the deployment frozen.")
    );
    // B: the person's reply directly under the reasoning.
    let below = format!("{BAND}\nNo, keep the current palette.");
    own_words_agree(&below, Some("No, keep the current palette."));
    assert_eq!(
        strip_dreamer_prompt(&below).as_deref(),
        Some("No, keep the current palette.")
    );
}

/// The v2 emitters' normalization: an inner ” becomes a straight quote.
fn straight(text: &str) -> String {
    text.replace('\u{201d}', "\"")
}

/// `<head>“<body>”` within `most` characters, the body cut with `…` (still closed) when it does not fit.
fn envelope(head: &str, body: &str, most: usize) -> String {
    let body = straight(body);
    let room = most - head.chars().count() - 2;
    let body = if body.chars().count() > room {
        format!(
            "{}\u{2026}",
            body.chars().take(room - 1).collect::<String>()
        )
    } else {
        body
    };
    format!("{head}\u{201c}{body}\u{201d}")
}

fn v2_pane(question: &str, reasoning: &str) -> String {
    let head = format!(
        "CSR's dreamer asked this while you worked, quoted: \u{201c}{}\u{201d} Its reasoning: ",
        straight(question)
    );
    envelope(&head, reasoning, 700)
}

fn v2_card(stage: &str, body: &str) -> String {
    envelope(
        &format!("CSR's compaction card ({stage}), quoted: "),
        body,
        1200,
    )
}

fn v2_story(rev: usize, body: &str) -> String {
    envelope(
        &format!("CSR's story of this session, rev {rev}, quoted: "),
        body,
        700,
    )
}

fn v2_band(question: &str, title: &str, reasoning: &str) -> String {
    format!(
        "\u{201c}{}\u{201d}\n\nCSR's dreamer asked this while you worked, quoted. Its reasoning:\n\n> **{title}**\n> {reasoning}",
        straight(question)
    )
}

#[test]
fn v2_forwards_round_trip_to_exactly_the_typed_reply() {
    let long = "Two surfaces already read the same inks, and the \u{201c}deck\u{201d} does too. "
        .repeat(30);
    let sends = [
        v2_pane("What if we cache it?", "Two callers read the same file."),
        v2_pane("What if we cache it?", &long),
        v2_card(
            "compacted by csr",
            "Context: 274k \u{2192} 70k tokens. Learnt: \u{201c}Never\u{201d} skip validation.",
        ),
        v2_card("compacting", &long),
        v2_story(3, "The session covered iOS fixes for Bold."),
        v2_story(3, &long),
    ];
    for send in &sends {
        own_words_agree(send, None);
        own_words_agree(
            &format!("Before I send this: {send} and after it, my reply."),
            Some("Before I send this: and after it, my reply."),
        );
        own_words_agree(&format!("2{send}\nmy reply"), Some("my reply"));
        // A quoted line is a mention to the import remover (every byte
        // stays); extraction and the query side still cut the forward.
        let quoted = format!("> {send} Keep it.");
        assert_eq!(extractable(&quoted).as_deref(), Some("Keep it."));
        assert_eq!(strip_csr_emissions(&quoted), "Keep it.");
        assert!(!labeled(&quoted), "a quoted line is a mention");
    }
    // The cut ones end on `…”` inside their limit.
    assert_eq!(sends[1].chars().count(), 700);
    assert!(sends[1].ends_with("\u{2026}\u{201d}"));
    assert_eq!(sends[3].chars().count(), 1200);
    // Two forwards on one line, the person's words between them.
    own_words_agree(&format!("{} then {}", sends[0], sends[2]), Some("then"));

    let band = v2_band("What if we reuse the palette?", "Reuse", "Shared colours.");
    own_words_agree(&band, None);
    own_words_agree(
        &format!("Earlier note.\n\n{band}\n\nNo, keep the current palette."),
        Some("Earlier note.\n\nNo, keep the current palette."),
    );
}

#[test]
fn v2_band_keeps_the_persons_words_before_the_question_on_its_line() {
    let band = v2_band("What if we reuse the palette?", "Reuse", "Shared colours.");
    let typed = format!("Keep the deployment frozen. {band}\nNo, keep the current palette.");
    own_words_agree(
        &typed,
        Some("Keep the deployment frozen.\nNo, keep the current palette."),
    );
    assert_eq!(
        strip_dreamer_prompt(&typed).as_deref(),
        Some("Keep the deployment frozen.\nNo, keep the current palette.")
    );
    // The whole v2 message is still the dreamer's prompt.
    assert!(is_dreamer_prompt(&band));
    assert!(is_csr_emission(&band));
}

#[test]
fn v1_legacy_forwards_never_look_for_a_closing_quote() {
    // A legacy body that opens with a quoted phrase goes whole (astra r3 #6).
    for legacy in [
        "CSR's compaction card (compacted by csr): \u{201c}Queue\u{201d} was moved. Never skip validation.",
        "CSR's dreamer asked this while you worked: \u{201c}What if?\u{201d} Its reasoning: \u{201c}Queue\u{201d} was moved. Never skip validation.",
        "CSR's story of this session, rev 2: \u{201c}Queue\u{201d} was moved. Never skip validation.",
    ] {
        own_words_agree(legacy, None);
        own_words_agree(&format!("{legacy}\nKeep it."), Some("Keep it."));
        own_words_agree(&format!("Look: {legacy}"), Some("Look:"));
    }
    // A 700-character legacy send ends at its cut; the reply after it, curly quotes and all, is the person's
    // (astra r3 #5: a quote search crossed the cut).
    let head =
        "CSR's dreamer asked this while you worked: \u{201c}What if?\u{201d} Its reasoning: ";
    let sent = format!("{head}{}\u{2026}", "x".repeat(699 - head.chars().count()));
    own_words_agree(
        &format!("{sent} Keep \u{201c}my words\u{201d} exactly."),
        Some("Keep \u{201c}my words\u{201d} exactly."),
    );
    // The legacy band, line by line; personal words before its question on that line cannot be told apart
    // and go with it (a documented legacy-only limit).
    own_words_agree(BAND, None);
    own_words_agree(&format!("Keep it. {BAND}"), None);
}

#[test]
fn a_question_with_curly_quotes_of_its_own_is_cut_whole() {
    let send = v2_pane(
        "What if the \u{201c}Queue\u{201d} view moved?",
        "It sits beside \u{201c}Lost and found\u{201d}.",
    );
    assert!(send.contains("\u{201c}Queue\" view"));
    own_words_agree(&format!("{send} No."), Some("No."));
}

#[test]
fn cjk_forwards_are_cut_by_the_same_grammar() {
    let card = v2_card(
        "compacted by csr",
        "\u{30b3}\u{30f3}\u{30c6}\u{30ad}\u{30b9}\u{30c8}\u{ff1a}274k \u{2192} 70k\u{3002}\u{691c}\u{8a3c}\u{3092}\u{98db}\u{3070}\u{3055}\u{306a}\u{3044}\u{3002}",
    );
    own_words_agree(&card, None);
    own_words_agree(
        &format!("\u{898b}\u{3066}\u{ff1a}{card}\u{8fd4}\u{4e8b}\u{3067}\u{3059}\u{3002}"),
        Some("\u{898b}\u{3066}\u{ff1a} \u{8fd4}\u{4e8b}\u{3067}\u{3059}\u{3002}"),
    );
    let question = "\u{30ad}\u{30e3}\u{30c3}\u{30b7}\u{30e5}\u{3092}\u{5171}\u{6709}\u{3057}\u{305f}\u{3089}\u{3069}\u{3046}\u{3067}\u{3057}\u{3087}\u{3046}\u{304b}\u{ff1f}";
    let dream = v2_pane(question, &"\u{4e8c}\u{3064}\u{306e}\u{753b}\u{9762}\u{304c}\u{540c}\u{3058}\u{8272}\u{3092}\u{8aad}\u{3080}\u{3002}".repeat(80));
    assert_eq!(dream.chars().count(), 700);
    own_words_agree(
        &format!("{dream}\u{3044}\u{3044}\u{3048}\u{3002}"),
        Some("\u{3044}\u{3044}\u{3048}\u{3002}"),
    );
    own_words_agree(
        &v2_band(question, "\u{8272}", "\u{540c}\u{3058}\u{8272}\u{3002}"),
        None,
    );
    // A legacy card cut at 1,200 CJK characters, then the person's reply.
    let head = "CSR's compaction card (compacting): ";
    let legacy = format!(
        "{head}{}\u{2026}",
        "\u{8981}".repeat(1199 - head.chars().count())
    );
    own_words_agree(
        &format!("{legacy}\u{3044}\u{3044}\u{3048}"),
        Some("\u{3044}\u{3044}\u{3048}"),
    );
}

#[test]
fn astra_r2_2_feedback_about_a_story_is_the_persons() {
    // The broad line pass cut from the story's opening words to the end of the line, without its
    // revision delimiter, and took the person's sentence with it.
    let own = "CSR's story of this session, rev 3 is wrong; keep my rollback plan.";
    let input = format!(
        "{own}\n\nCSR's compaction card (compacting), quoted: \u{201c}Summarizing.\u{201d}"
    );
    own_words_agree(&input, Some(own));
    own_words_agree(own, Some(own));
}

#[test]
fn astra_r2_8_a_number_between_two_forwards_is_the_persons() {
    let card =
        |body: &str| format!("CSR's compaction card (compacting), quoted: \u{201c}{body}\u{201d}");
    own_words_agree(&format!("{} 42 {}", card("One."), card("Two.")), Some("42"));
    // The single hotkey digit the band leaves before a send still goes with it.
    own_words_agree(&format!("2{}\nmy reply", card("One.")), Some("my reply"));
    own_words_agree(&format!("42{}", card("One.")), Some("42"));
}

#[test]
fn astra_5_a_sentinel_line_goes_and_the_line_beside_it_stays() {
    let input = "Keep the deployment frozen.\n[[CSR:RECAP]] generated recap";
    // Extraction and the query side treat a sentinel line as the machine's.
    assert_eq!(strip_csr_emissions(input), "Keep the deployment frozen.");
    // The detector labels a sentinel only as the end of a valid recap line,
    // so a line that merely carries the token is the person's.
    assert!(!labeled(input));
    // A recap line without its sentinel is no complete recap either.
    let recap = "Keep the deployment frozen.\nrecap [2h ago]: shipped the cap. Next: tag it.";
    assert!(!labeled(recap));
    // With its sentinel it is found (and kept: nothing is cut from the record).
    let valid = "Keep the deployment frozen.\nrecap [2h ago]: shipped the cap. [[CSR:RECAP]]\n";
    assert!(labeled(valid));
}

#[test]
fn astra_8_a_forward_past_the_header_window_marks_the_text_contaminated() {
    let input = format!(
        "{}\n\nCSR's compaction card (compacting): Summarizing 278k.",
        "Keep this user requirement. ".repeat(16)
    );
    assert!(input.find("CSR's compaction card").unwrap() > HEADER_WINDOW);
    assert!(crate::import::contamination::has_csr_text(&input));
}

#[test]
fn strip_forwarded_note_cuts_opener_and_quote_keeps_prose() {
    let note = "Here is a note offered by a side agent:\n\n> **T**\n>\n> body [commit f154ead]\n\n> second quote";
    assert_eq!(strip_forwarded_note(note).as_deref(), Some(""));
    let with_prose = format!("{note}\n\nwhat now?\nand more");
    assert_eq!(
        strip_forwarded_note(&with_prose).as_deref(),
        Some("what now?\nand more")
    );
    let csr = "Here is a note offered by CSR (claude-self-reflect):\n> x";
    assert_eq!(strip_forwarded_note(csr).as_deref(), Some(""));
}

#[test]
fn strip_forwarded_note_is_none_for_non_forwards() {
    assert_eq!(strip_forwarded_note("plain prose"), None);
    assert_eq!(
        strip_forwarded_note("I wonder why here is a note offered by YSK shows up"),
        None
    );
    // opener line does not end with a colon: not a forward
    assert_eq!(
        strip_forwarded_note("Here is a note offered by a side agent\n> x"),
        None
    );
}

// --- strip_plumbing ---

#[test]
fn plumbing_strips_command_wrappers_keeps_nothing() {
    let text = "<command-message>memory-feedback</command-message>\n\
                    <command-name>/memory-feedback</command-name>";
    assert!(strip_plumbing(text).trim().is_empty());
}

#[test]
fn plumbing_strips_unclosed_caveat_to_end() {
    // Round-3 regression: CONTINUUM request showed the raw caveat wrapper.
    let text = "<local-command-caveat>Caveat: The messages below were generated \
                    by the user while running local commands. DO NOT respond";
    assert!(strip_plumbing(text).trim().is_empty());
}

#[test]
fn plumbing_keeps_real_prompt_around_reminder() {
    let text = "fix the auth bug <system-reminder>hook noise</system-reminder> in login.rs";
    let out = strip_plumbing(text);
    assert!(out.contains("fix the auth bug"));
    assert!(out.contains("in login.rs"));
    assert!(!out.contains("hook noise"));
}

// --- strip_quoted ---

#[test]
fn quoted_strips_fences_inline_and_blockquotes() {
    let text =
        "Fixed the parser.\n```\nNEXT: garbage\n```\nSee `LAST: token` and\n> TODOS: quoted\nDone.";
    let out = strip_quoted(text);
    assert!(out.contains("Fixed the parser."));
    assert!(out.contains("Done."));
    assert!(!out.contains("garbage"));
    assert!(!out.contains("token"));
    assert!(!out.contains("quoted"));
}

#[test]
fn quoted_drops_unclosed_fence_tail() {
    let out = strip_quoted("Real prose.\n```\nNEXT: never closed");
    assert!(out.contains("Real prose."));
    assert!(!out.contains("never closed"));
}

// --- is_csr_emission ---

#[test]
fn emission_detects_headers() {
    assert!(is_csr_emission("CSR CONTINUUM [2m ago]: stuff"));
    assert!(is_csr_emission("## Session Intelligence (CSR v9.2)\n..."));
    assert!(is_csr_emission("## CSR Memory Feedback — 2026-06-10"));
    assert!(is_csr_emission("You are CSR Episode Analyst. Summarize..."));
}

#[test]
fn emission_detects_the_dreamers_prompt_sent_as_a_user_turn() {
    // The dreamer's question leads; its marker sentence follows within the header window.
    let sent = "What if the pane took its colours from pigment's nyt inks?\n\n\
                    CSR's dreamer asked this while you worked. Its reasoning:\n\n> **Share the palette**";
    assert!(is_csr_emission(sent));
    assert!(!is_csr_emission(
        "what if the dreamer asked fewer questions while I work?"
    ));
}

#[test]
fn dreamer_prompt_is_recognised_only_in_its_exact_whole_message_grammar() {
    let sent = "What if the pane took its colours from pigment's nyt inks?\n\n\
                    CSR's dreamer asked this while you worked. Its reasoning:\n\n\
                    > **Share the palette**\n>\n> Two surfaces already read the same inks.";
    assert!(is_dreamer_prompt(sent));
    assert!(is_dreamer_prompt(&sent.replace('\n', "\r\n")));
    // the question may be absent
    assert!(is_dreamer_prompt(
        "CSR's dreamer asked this while you worked. Its reasoning:\n\n> **t**\n> body"
    ));
    // prose that mentions it, extra person-typed prose, or an unquoted tail is not the grammar
    assert!(!is_dreamer_prompt(
        "what if the dreamer asked fewer questions while I work?"
    ));
    assert!(!is_dreamer_prompt(
        "I wrote this myself.\n\nSecond thought of mine.\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> q"
    ));
    assert!(!is_dreamer_prompt(
        "Q?\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\nand then I typed more"
    ));
    assert!(!is_dreamer_prompt(
        "Q?\n\nCSR's dreamer asked this while you worked. Its reasoning:"
    ));
}

#[test]
fn a_reply_under_a_long_hindi_dream_keeps_only_the_persons_words() {
    // Codex review 10-12, #14: the person's reply under a forwarded dream breaks the whole-message grammar, and
    // a 223-character Devanagari question (553 bytes) puts the attribution past HEADER_WINDOW: the question was
    // kept as the person's prose, and strip_csr_emissions cut at the attribution, dropping the reply instead.
    let question = format!(
        "\u{915}\u{94d}\u{92f}\u{93e} \u{939}\u{94b} \u{905}\u{917}\u{930} {}?",
        "\u{939}\u{92e} \u{92a}\u{947}\u{928} \u{915}\u{947} \u{930}\u{902}\u{917} \u{921}\u{947}\u{915} \u{915}\u{940} \u{938}\u{94d}\u{92f}\u{93e}\u{939}\u{940} \u{938}\u{947} \u{932}\u{947}\u{902} "
            .repeat(6)
    );
    assert!(question.len() > HEADER_WINDOW);
    let forward = format!(
        "{question}\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> **Share the palette**\n> * Two surfaces read the same inks."
    );
    let reply = format!("{forward}\n\nNo, keep the current palette.");
    let mine = Some("No, keep the current palette.");
    assert_eq!(strip_dreamer_prompt(&reply).as_deref(), mine);
    assert_eq!(extractable(&reply).as_deref(), mine);
    assert_eq!(Some(strip_csr_emissions(&reply).trim()), mine);
    assert!(labeled(&reply));
    assert_eq!(strip_dreamer_prompt(&forward).as_deref(), Some(""));
    assert!(extractable(&forward).is_none());
    assert_eq!(strip_csr_emissions(&forward), "");
    // the pane's one-line send, words above and below it
    let pane = "Look.\n\nCSR's dreamer asked this while you worked: \u{201c}What if?\u{201d} Its reasoning: x.\n\nNot now.";
    assert_eq!(
        strip_dreamer_prompt(pane).as_deref(),
        Some("Look.\n\nNot now.")
    );
    // no dreamer message at all: None, and the text is left to the other checks
    assert_eq!(
        strip_dreamer_prompt("what if the dreamer asked less?"),
        None
    );
}

#[test]
fn emission_detects_the_compaction_cards_draft_a_click_puts_in_the_prompt() {
    // Codex review 10-12, #13: a double click on the companion's done compaction card puts this draft in the
    // prompt box; sent, it was imported as the person's words, its "Learnt:" lines as their own.
    let card = "CSR's compaction card (compacted by csr): Context: 274k \u{2192} 70k tokens. \
                    Each API call now sends 204k fewer tokens. Learnt: Never skip validation.";
    assert!(is_csr_emission(card));
    assert!(extractable(card).is_none());
    for stage in ["compacting soon", "compacting", "compaction skipped"] {
        assert!(is_csr_emission(&format!(
            "CSR's compaction card ({stage}): 278k"
        )));
    }
    // the person's own words about the card stay theirs
    assert!(!is_csr_emission(
        "the compaction card should say it in prompts"
    ));
}

#[test]
fn dreamer_prompt_in_any_language_is_held_past_the_header_window() {
    // The dreamer writes in the person's language. Devanagari is 3 bytes a character, so a long Hindi question
    // pushes the attribution past HEADER_WINDOW; the whole-message grammar must still hold it as CSR's own.
    let hindi = "क्या होगा अगर पैन अपने रंग पिगमेंट की स्याही से ले, ताकि दोनों सतहें एक ही रंग पढ़ें और हर बार नया रंग चुनने की ज़रूरत न पड़े, और पुराने सत्रों में जो रंग तय हुए थे वे भी वहीं से आएँ?";
    let japanese = "ペインの色をピグメントのインクから取ったら、二つの画面が同じ色を読み、毎回新しい色を選ぶ必要がなくなり、過去のセッションで決めた色もそこから来て、ステータス行や情報ページの図も同じインクで描かれ、利用者が以前に一度だけ選んだ配色がすべての画面で保たれるのではないでしょうか？";
    let mixed = "क्या होगा अगर pane अपने colours pigment की nyt inks से ले, ताकि दोनों surfaces एक ही palette पढ़ें?";
    for question in [hindi, japanese, mixed] {
        let sent = format!(
            "{question}\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> **रंग साझा करें**\n>\n> दो सतहें पहले से वही स्याही पढ़ती हैं।"
        );
        assert!(
            question.len() > HEADER_WINDOW || question == mixed,
            "{}",
            question.len()
        );
        assert!(is_dreamer_prompt(&sent), "{question}");
        assert!(is_csr_emission(&sent), "{question}");
        assert!(labeled(&sent), "{question}");
    }
    // The person's own non-English question is still theirs.
    assert!(!is_csr_emission(hindi));
    assert!(!is_dreamer_prompt(japanese));
}

#[test]
fn emission_detects_the_companions_compaction_keep_list_and_notice() {
    assert!(is_csr_emission(
        "CSR COMPACT KEEP — in the summary, keep these, in this order, ids and SHAs word for word:\n\
             1. Running in the background: subagent a427c126 (Explore CapCut)."
    ));
    assert!(is_csr_emission(
        "CSR compacted at 312k → 66k · pays back in 3 turns · kept: 2 running agents, 1 open decision"
    ));
    // The owner's own words about compacting stay theirs.
    assert!(!is_csr_emission(
        "compact at 250k from now on, and keep the agents running"
    ));
}

#[test]
fn emission_detects_field_token_echo() {
    // Round-2 regression: probe instruction text extracted as next_steps.
    assert!(is_csr_emission(
        "NEXT: NEXT:/TODOS:/ANCHORS: lines) - briefing block - Do NOT count CLAUDE.md"
    ));
}

#[test]
fn emission_allows_single_token_in_prose() {
    assert!(!is_csr_emission(
        "next: fix the TODOS: counter display in the statusline"
    ));
    assert!(!is_csr_emission("next step: deploy to production"));
    assert!(!is_csr_emission(
        "Fixed token validation, added regression test"
    ));
}

#[test]
fn emission_header_only_matters_in_leading_window() {
    let mut text = "a".repeat(500);
    text.push_str("CSR CONTINUUM [old]");
    assert!(!is_csr_emission(&text));
}

// --- extractable (full pipeline, probe-round regressions) ---

#[test]
fn extractable_rejects_pasted_probe_report() {
    let report = "## CSR Memory Feedback — 2026-06-10 — session in claude-self-reflect\n\
                      - **variant**: unknown\n- **noise**: CSR CONTINUUM block garbled";
    assert_eq!(extractable(report), None);
}

#[test]
fn extractable_rejects_caveat_only_message() {
    assert_eq!(
        extractable("<local-command-caveat>Caveat: The messages below were generated"),
        None
    );
}

#[test]
fn extractable_keeps_prose_drops_quoted_tokens() {
    // Round-3 regression: assistant summary quoting injected fields in
    // backticks. The quoted tokens go; the genuine prose stays.
    let text = "`NEXT: none recorded` — polluted boilerplate filtered, \
                    including episodes already in the DB. Commit:";
    let out = extractable(text).expect("prose should survive");
    assert!(!out.contains("NEXT:"));
    assert!(out.contains("polluted boilerplate filtered"));
}

#[test]
fn quoted_keeps_short_identifier_spans() {
    // Round-4 regression: "commit `4343b50`, deployed" rendered as
    // "commit , deployed". Short identifier spans keep their content.
    let out = strip_quoted("One-shot fix done — commit `4343b50`, deployed to `/usr/local`.");
    assert!(out.contains("commit 4343b50, deployed"));
    // ...but spans carrying emission tokens stay stripped regardless.
    let out = strip_quoted("shows `NEXT: garbage` now");
    assert!(!out.contains("NEXT:"));
    // ...and long spans are mini quoted blocks, stripped.
    let long = format!("see `{}` for details", "x".repeat(80));
    assert!(!strip_quoted(&long).contains("xxx"));
}

#[test]
fn extractable_rejects_probe_command_text() {
    // The /memory-feedback command body after plumbing-tag stripping.
    let probe = "CSR Memory Feedback Probe You are reporting on the quality \
                     of the memory context CSR injected into THIS session.";
    assert_eq!(extractable(probe), None);
}

#[test]
fn substantive_rejects_metadata_and_keeps_prose() {
    // Round-6: bare timestamp request and command-only rolling summary.
    assert!(!is_substantive("20260611-122252"));
    assert!(!is_substantive("memory-feedback"));
    assert!(!is_substantive(""));
    assert!(!is_substantive(
        "<command-message>memory-feedback</command-message>"
    ));
    // Real work descriptions pass.
    assert!(is_substantive("fix the auth bug in login.rs"));
    assert!(is_substantive(
        "Refactored self-reference filtering into provenance"
    ));
}

#[test]
fn extractable_keeps_normal_session_text() {
    let out = extractable("Fix the authentication bug in the login handler");
    assert_eq!(
        out.as_deref(),
        Some("Fix the authentication bug in the login handler")
    );
}

// --- RECAP_SENTINEL ---

#[test]
fn sentinel_detected_regardless_of_position_or_preamble() {
    let long_preamble = "a".repeat(HEADER_WINDOW + 200);
    let text = format!("{long_preamble} trailing note {RECAP_SENTINEL} more text");
    assert!(is_csr_emission(&text));
}

#[test]
fn lessons_sentinel_rejects_machine_generated_candidates() {
    let text = "human-looking preamble\n[[CSR:LESSONS]]\n- Never bypass checks";
    assert!(is_csr_emission(text));
    assert!(extractable(text).is_none());
}

#[test]
fn learnt_sentinel_rejects_machine_generated_line() {
    let line = format!(
        "learnt [yesterday]: 17 memory notes: a-note, +16 more (6ca2a4d5, 2026-10-16). {LEARNT_SENTINEL}"
    );
    assert!(MACHINE_SENTINELS.contains(&LEARNT_SENTINEL));
    assert!(contains_machine_sentinel(&line));
    let zero_width: String = line.chars().flat_map(|c| [c, '\u{200B}']).collect();
    let confusable = line
        .replace("[[CSR", "\u{FF3B}\u{FF3B}CSR")
        .replace("]]", "\u{FF3D}\u{FF3D}");
    let long_preamble = format!("{}\n{line}", "a preamble sentence. ".repeat(40));
    assert!(long_preamble.find(LEARNT_SENTINEL).unwrap() > HEADER_WINDOW);
    for text in [
        line.clone(),
        format!("> {line}"),
        format!("- {line}"),
        zero_width,
        confusable,
        long_preamble,
    ] {
        assert!(is_csr_emission(&text), "{text:?}");
        assert!(extractable(&text).is_none(), "{text:?}");
    }
    // Prose that merely starts with the word is not an emission.
    assert!(!is_csr_emission(
        "learnt [the hard way], I now back up first."
    ));
}

#[test]
fn sentinel_survives_zero_width_noise() {
    let noisy: String = format!("preamble {RECAP_SENTINEL} tail")
        .chars()
        .flat_map(|c| [c, '\u{200B}'])
        .collect();
    assert!(is_csr_emission(&noisy));
}

#[test]
fn sentinel_survives_confusable_bracket_substitution() {
    let swapped = RECAP_SENTINEL
        .replace('[', "\u{FF3B}")
        .replace(']', "\u{FF3D}");
    let text = format!("some prose then {swapped} more prose");
    assert!(is_csr_emission(&text));
}

#[test]
fn sentinel_absent_from_genuine_prose_about_recaps() {
    assert!(!is_csr_emission(
        "Let's recap what we discussed: the login bug is fixed and deployed."
    ));
    assert!(!is_csr_emission(
        "The recap feature works great, though sometimes it's a bit verbose."
    ));
    assert!(!is_csr_emission(
        "I want to recap [the auth changes], then prioritize them."
    ));
}

// --- extractable ordering: sentinel must be seen before strip_quoted ---

#[test]
fn extractable_rejects_blockquoted_recap_alongside_unquoted_preamble() {
    // Regression: if strip_quoted ran first, it would erase every
    // blockquoted line (including the sentinel), leaving the unquoted
    // preamble looking like genuine session content and leaking it
    // through as Some(..).
    let quoted_recap =
        format!("> recap [2h ago]: fixed the bug: shipped it. {RECAP_SENTINEL}\n> Next: ship it.");
    let text = format!(
        "some unrelated preamble text that reads like genuine session content\n{quoted_recap}"
    );
    assert!(extractable(&text).is_none());
}

#[test]
fn extractable_keeps_unquoted_prose_alongside_quoted_historical_header() {
    // Regression: full is_csr_emission before strip_quoted over-rejected
    // genuine unquoted content that merely co-occurred with a quoted
    // historical EMISSION_HEADERS echo (no sentinel present).
    let text = "> SESSION CONTINUITY DETECTED: old injected context\n\
                    I actually fixed the login bug today, deployed and verified.";
    let got = extractable(text).expect("unquoted genuine prose must survive");
    assert_eq!(
        got.trim(),
        "I actually fixed the login bug today, deployed and verified."
    );
}

// --- strip_csr_emissions (query side of the self-echo guard) ---

/// The prompt pasted live on 2026-10-10 (the pane's hand-off send text plus
/// the person's question); the session id inside is replaced by a dummy.
const PASTED_HANDOFF: &str = "CSR handed you these this session: Corpora ingestion and watcher setup · 2 weeks ago · csr-engine · 5×; Hairline skill retrieval and agent proposal · 2 days ago · csr-engine · 3×; Implement the following plan: · 7 months ago · 2×; Implement the following plan: · 7 months ago; When claude is killed or closed it generates a session e.g: claude --resume 00000000-0000-4000-8000-000000000000 <- the session is generated, can CSR retreive it - what enrichment is needed to catch \" · 5 months ago <<< This Handed to Claude seems not useful thoughts?";

#[test]
fn pasted_handoff_list_keeps_only_the_persons_question() {
    assert_eq!(
        strip_csr_emissions(PASTED_HANDOFF),
        "<<< This Handed to Claude seems not useful thoughts?"
    );
    // Words before the paste are the person's too.
    let both = strip_csr_emissions(&format!("why did you get these? {PASTED_HANDOFF}"));
    assert_eq!(
        both.split_whitespace().collect::<Vec<_>>().join(" "),
        "why did you get these? <<< This Handed to Claude seems not useful thoughts?"
    );
}

#[test]
fn truncated_send_text_ends_at_the_panes_ellipsis() {
    // one_line(text, 700) cuts at 699 characters and appends an ellipsis.
    let item = "Assembled a seven-track channel slate from samples · 8 weeks ago · Campaigns; ";
    let body: String = format!("{PANE_HANDED}{}", item.repeat(20))
        .chars()
        .take(PANE_SEND_WIDTH)
        .collect();
    let sent = format!("{body}… are any of these useful for the reel?");
    assert_eq!(
        strip_csr_emissions(&sent),
        "are any of these useful for the reel?"
    );
}

/// The removed `pane.py` `lost_asks`: both send forms, as typed into the prompt. The
/// thread text is from session 3002ab28 (anukriti-website).
const LOST_SIDE: &str = "Lost and found: “The IG post time is a soft follow-up. Then ask for the release go-ahead (allowed, since publishing needs approval). For example: on \"ship\", I'll pull the coda from the hub (or keep it if the app is updated first), merge PR #30, run IndexNo…” went quiet after 19:38. Revisit it as a side task: finish or close it in a few steps without derailing the current work, then tell me in one line.";

#[test]
fn lost_and_found_sends_are_cut_out() {
    assert_eq!(strip_csr_emissions(LOST_SIDE), "");
    let sub = "Lost and found: “Retry the upload” went quiet after 09:05. Hand it to a background subagent with a five-part spec (objective, files, interfaces, constraints, verification), keep working on the current task, and report its result when it lands.";
    assert_eq!(strip_csr_emissions(sub), "");
    // Words the person adds after the send text are theirs.
    assert_eq!(
        strip_csr_emissions(&format!("{LOST_SIDE} and check the hub first")),
        "and check the hub first"
    );
    assert_eq!(
        strip_csr_emissions(&format!("{LOST_SIDE}\nalso check the hub")),
        "also check the hub"
    );
    // Prose that only mentions the section is left alone.
    let prose = "Lost and found is the pane section I like most.";
    assert_eq!(strip_csr_emissions(prose), prose);
}

#[test]
fn lost_and_found_sends_with_a_conversation_pointer_are_cut_out_too() {
    let pointer = "Its conversation is conv_3002ab28-7ba1-4c61-9d2e-0a1b2c3d4e5f: read it with csr_transcript(session=\"3002ab28-7ba1-4c61-9d2e-0a1b2c3d4e5f\", view=\"stats\") first, not jq or Python over the raw JSONL.";
    let side = format!("Lost and found: “Retry the upload” went quiet after 09:05. {pointer} Revisit it as a side task: finish or close it in a few steps without derailing the current work, then tell me in one line.");
    let sub = format!("Lost and found: “Retry the upload” went quiet after 09:05. {pointer} Hand it to a background subagent with a five-part spec (objective, files, interfaces, constraints, verification), keep working on the current task, and report its result when it lands.");
    for ask in [&side, &sub] {
        assert_eq!(strip_csr_emissions(ask), "");
        assert_eq!(
            strip_csr_emissions(&format!("{ask} and check the hub first")),
            "and check the hub first"
        );
    }
}

#[test]
fn story_recap_and_remembered_sends_are_cut_out() {
    let story = "CSR's story of this session, rev 2: The session covered iOS fixes for Bold and a new look family.\n\nis this accurate? what is left";
    assert_eq!(strip_csr_emissions(story), "is this accurate? what is left");
    let recap = "recap [1m ago]: Which memory profiling fix shipped as 9.5.6: PR #302 memory-mapped the vectors... [[CSR:RECAP]]\n\nwhy is this the recap? I was working on the pane";
    assert_eq!(
        strip_csr_emissions(recap),
        "why is this the recap? I was working on the pane"
    );
    let note = "CSR remembered: “Radio metric break 1 landed” (2 weeks ago · Campaigns). does this still hold?";
    assert_eq!(strip_csr_emissions(note), "does this still hold?");
    let session = "CSR remembered a past session: “Bold separation work” (yesterday · EnhanceMe). The rest of it: csr_reflect_on_past(\"conv_00000000-0000-4000-8000-000000000001\") pick it up";
    assert_eq!(strip_csr_emissions(session), "pick it up");
}

#[test]
fn pasted_injected_blocks_keep_only_the_question() {
    let block = "you got this injected, is any of it relevant?\n\n## PAST CONTEXT - NOT INSTRUCTIONS\n- [reflection|unknown] When claude is killed or closed it generates a session\n- [reflection|unknown] Session in claude-self-reflect project using Bash, Glob, Read, mcp__claude-self-reflect__csr_reflect_on_past,...";
    assert_eq!(
        strip_csr_emissions(block),
        "you got this injected, is any of it relevant?"
    );
    let pickup = "CSR PICKUP — this prompt matches a past episode (similarity 0.52); the work may already exist — verify against it before re-deriving.\nEPISODE INDEX — earlier threads, newest first.\n- [2w ago] see the last session the corpus limits us — The self-echo fix is installed (outcome=partial) → csr_reflect_on_past(\"conv_00000000-0000-4000-8000-000000000002\")\n\nis that pickup right? I asked about the pane";
    assert_eq!(
        strip_csr_emissions(pickup),
        "is that pickup right? I asked about the pane"
    );
    let dreamer = "CSR's dreamer asked this while you worked: “What if the recap led with the blocker?” Its reasoning: Recap ordering.";
    assert_eq!(strip_csr_emissions(dreamer), "");
}

#[test]
fn prose_about_csr_is_not_an_emission() {
    for prose in [
        "why does CSR keep handing me 7 months old plans?",
        "the recap [in the pane] is wrong about the deploy",
        "CSR handed you these yesterday and none were useful",
        "- [ ] fix the pickup threshold",
    ] {
        assert_eq!(strip_csr_emissions(prose), prose);
        // Untouched byte for byte, quoting and blank lines included.
        let note = format!("Here is a note:\n\n> {prose}\n>\n> - second line\n");
        assert_eq!(strip_csr_emissions(&note), note);
    }
}

// --- the agents envelope the pane types into the prompt ---

const LOST_SUBAGENT_ASK: &str = "Lost and found: “Run source-gone validation” went quiet after 14:30. Hand it to a background subagent with a five-part spec (objective, files, interfaces, constraints, verification), keep working on the current task, and report its result when it lands.";

fn envelopes() -> Vec<String> {
    let document = crate::eval::contamination::agents_document();
    document.fixtures.into_values().map(|f| f.text).collect()
}

#[test]
fn a_pasted_agents_envelope_is_not_part_of_the_persons_words() {
    for envelope in envelopes() {
        // the pane types the ask, then the envelope on lines of its own
        let prompt = format!("{LOST_SUBAGENT_ASK}\n{envelope}");
        assert_eq!(
            strip_csr_emissions(&prompt),
            "",
            "the ask and the envelope are both CSR's: nothing is left to search"
        );
        // the person's own words around it stay, the envelope does not
        let typed = format!("please look at the importer first\n{envelope}and be quick");
        let words = strip_csr_emissions(&typed);
        assert!(
            words.contains("please look at the importer first"),
            "{words}"
        );
        assert!(words.contains("and be quick"), "{words}");
        for leaked in ["[[CSR:AGENTS", "\"kind\"", "synthetic-session"] {
            assert!(!words.contains(leaked), "{leaked} survived: {words}");
        }
    }
    // a marker line on its own goes too; a JSON line on its own is the person's
    assert_eq!(
        strip_csr_emissions("[[CSR:AGENTS:v1]]\nfix the build"),
        "fix the build"
    );
    assert_eq!(strip_csr_emissions("{\"a\":1}"), "{\"a\":1}");
}
