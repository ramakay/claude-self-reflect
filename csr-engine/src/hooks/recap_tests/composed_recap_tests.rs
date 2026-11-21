use super::*;

#[test]
fn structured_deliveries_exclude_a_dropped_correction_even_when_body_echoes_marker() {
    let mut ep = episode();
    ep.anchor_request = Some("Corrected: note (abcdef12:9, 2026-09-01)".into());
    let feeds = RecapFeeds {
        settled: vec![SettledFact {
            claim: "s".repeat(383),
            receipt: "abc1234".into(),
            status: "resolved".into(),
            source: "user_confirmed".into(),
        }],
        corrections: vec![CorrectionLine {
            quote: "q".repeat(90),
            session8: "abcdef12".into(),
            turn: 9,
            date: "2026-09-01".into(),
        }],
        ..RecapFeeds::empty()
    };

    let composed = compose_recap_with_deliveries(&ep, &feeds, "now").unwrap();

    assert!(composed
        .text
        .contains("Corrected: note (abcdef12:9, 2026-09-01)"));
    assert!(!composed.text.contains("\nCorrected: "));
    assert!(composed.corrections.is_empty());
}

/// What is recorded as delivered is what was shown: a correction that is CSR
/// text is skipped in both.
#[test]
fn the_delivered_correction_is_the_one_displayed_when_another_is_csr_text() {
    let correction = |quote: &str, turn: u32| CorrectionLine {
        quote: quote.into(),
        session8: "abcdef12".into(),
        turn,
        date: "2026-09-01".into(),
    };
    let feeds = RecapFeeds {
        corrections: vec![
            correction("A standing rule from 10-12: “Keep receipts”", 1),
            correction("Use the staging endpoint", 2),
        ],
        ..RecapFeeds::empty()
    };
    let composed = compose_recap_with_deliveries(&episode(), &feeds, "now").unwrap();
    assert!(
        composed.text.contains("Use the staging endpoint"),
        "{}",
        composed.text
    );
    assert!(
        !composed.text.contains("Keep receipts"),
        "{}",
        composed.text
    );
    let recorded: Vec<u32> = composed.corrections.iter().map(|c| c.turn).collect();
    assert_eq!(recorded, [2], "the identity recorded is the one shown");
}

/// A completion claim that is CSR text is not rendered; a clean one beside
/// it is.
#[test]
fn a_completion_claim_that_is_csr_text_is_not_injected() {
    let mut ep = episode();
    ep.completions = vec![
        CompletionReceipt {
            claim: "Implemented deterministic recap output".into(),
            receipt: "abc1234".into(),
        },
        CompletionReceipt {
            claim: "A standing rule from 10-12: “Keep receipts”".into(),
            receipt: "def5678".into(),
        },
    ];
    let recap = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert!(!recap.contains("Keep receipts"), "{recap}");
    assert!(
        recap.contains("Implemented deterministic recap output"),
        "{recap}"
    );
}

/// The detector reads the claim as it was stored, before extraction trims
/// it: an indented claim is code the detector leaves alone and still renders;
/// the same claim unindented is CSR text and does not.
#[test]
fn an_indented_claim_still_renders_and_the_unindented_one_does_not() {
    let rule = "A standing rule from 10-12: “Keep receipts”";
    let recap_with = |claim: String| {
        let mut ep = episode();
        ep.completions = vec![CompletionReceipt {
            claim,
            receipt: "abc1234".into(),
        }];
        compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap()
    };
    let indented = recap_with(format!("    {rule}"));
    assert!(indented.contains("Keep receipts"), "{indented}");
    let plain = recap_with(rule.to_string());
    assert!(!plain.contains("Keep receipts"), "{plain}");
}

#[test]
fn dream_clause_drops_entirely_without_a_feed() {
    let got = compose_recap(&episode(), &RecapFeeds::empty(), "now").unwrap();
    assert!(
        !got.contains(DREAM_CLAUSE_PREFIX),
        "no evidence must mean no clause: {got}"
    );
}

#[test]
fn dream_clause_drops_when_the_receipt_is_missing() {
    let feeds = RecapFeeds {
        top_dream: Some(DreamClause {
            receipt: "   ".into(),
            ..dream()
        }),
        ..RecapFeeds::empty()
    };
    let got = compose_recap(&episode(), &feeds, "now").unwrap();
    assert!(
        !got.contains(DREAM_CLAUSE_PREFIX),
        "a receiptless conclusion must never be named: {got}"
    );
}

#[test]
fn dream_clause_drops_when_the_label_carries_no_substance() {
    let feeds = RecapFeeds {
        top_dream: Some(DreamClause {
            label: "\u{2026}".into(),
            ..dream()
        }),
        ..RecapFeeds::empty()
    };
    let got = compose_recap(&episode(), &feeds, "now").unwrap();
    assert!(!got.contains(DREAM_CLAUSE_PREFIX), "{got}");
}

#[test]
fn dream_clause_obeys_the_same_character_budget_as_every_other_clause() {
    let mut ep = episode();
    ep.anchor_request = Some("r".repeat(INTENT_MAX_CHARS));
    ep.completions = vec![CompletionReceipt {
        claim: "c".repeat(100),
        receipt: "abc1234".into(),
    }];
    let feeds = RecapFeeds {
        settled: (0..4)
            .map(|n| SettledFact {
                claim: format!("{}-{n}", "settled".repeat(30)),
                receipt: format!("receipt{n}"),
                status: "resolved".into(),
                source: "user_confirmed".into(),
            })
            .collect(),
        top_dream: Some(dream()),
        ..RecapFeeds::empty()
    };
    let got = compose_recap(&ep, &feeds, "now").unwrap();
    assert!(got.chars().count() <= 700);
}

#[test]
fn composed_recap_survives_bullet_wrapping() {
    let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
    assert!(extractable(&wrap_bullet(&recap)).is_none());
}

#[test]
fn composed_recap_survives_blockquote_wrapping() {
    let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
    assert!(extractable(&wrap_blockquote(&recap)).is_none());
}

#[test]
fn composed_recap_survives_emphasis_wrapping() {
    let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
    assert!(extractable(&wrap_emphasis(&recap)).is_none());
}

#[test]
fn composed_recap_survives_html_wrapping() {
    let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
    assert!(extractable(&wrap_html(&recap)).is_none());
}

#[test]
fn composed_recap_survives_zero_width_noise() {
    let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
    assert!(extractable(&wrap_zero_width(&recap)).is_none());
}

#[test]
fn composed_recap_survives_confusable_bracket_substitution() {
    let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
    assert!(extractable(&wrap_confusable_brackets(&recap)).is_none());
}

#[test]
fn composed_recap_survives_preamble_past_header_window() {
    let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
    let wrapped = wrap_long_preamble(&recap);
    assert!(wrapped.len() > 400, "preamble must exceed HEADER_WINDOW");
    assert!(extractable(&wrapped).is_none());
}

#[test]
fn composed_recap_survives_stacked_wrapping() {
    // Bullet + blockquote + zero-width noise + a long preamble, combined
    // — the sentinel must survive all of it at once, not just one
    // transformation at a time.
    let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
    let stacked = wrap_long_preamble(&wrap_zero_width(&wrap_blockquote(&wrap_bullet(&recap))));
    assert!(extractable(&stacked).is_none());
}

#[test]
fn composed_recap_always_carries_the_sentinel_within_budget() {
    let recap = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
    assert!(recap.contains(RECAP_SENTINEL));
    assert!(recap.chars().count() <= 700);
}

// ---- intent: whole sentences of the latest task, or abstain ----------

#[test]
fn clean_intent_joins_soft_wraps_and_never_truncates() {
    // Live garble: this prompt was cut at 77 chars ("…can") and the
    // truncation ellipsis then trimmed away as trailing junk.
    let got = clean_intent(
        "anthropics codemod, when github links are bought up or disccussed, can\n  \
             present a side pane (enhanced for herdr users) - thoughts?",
    );
    assert_eq!(
        got.as_deref(),
        Some(
            "anthropics codemod, when github links are bought up or disccussed, can \
                 present a side pane (enhanced for herdr users) - thoughts?"
        )
    );
}

#[test]
fn clean_intent_keeps_leading_whole_sentences_within_budget() {
    let first = "Fix the recap composer.";
    let long = format!("{} end.", "word ".repeat(40));
    let got = clean_intent(&format!("{first} {long}")).unwrap();
    assert_eq!(got, first);
}

#[test]
fn clean_intent_abstains_when_the_first_sentence_is_too_long() {
    let text = format!("{} end", "word ".repeat(40));
    assert!(text.chars().count() > INTENT_MAX_CHARS);
    assert_eq!(clean_intent(&text), None);
}

#[test]
fn clean_intent_abstains_on_an_introducer_and_stops_at_lists() {
    assert_eq!(
        clean_intent("Here is a note offered by a side agent:"),
        None
    );
    assert_eq!(clean_intent("Fix these:\n- recap\n- pane"), None);
    assert_eq!(
        clean_intent("Fix the recap\n- it garbles\n\nlater text").as_deref(),
        Some("Fix the recap")
    );
}

#[test]
fn acknowledgements_are_not_tasks() {
    for ack in [
        "go",
        "ok, do it please",
        "Yes go ahead",
        "lgtm!",
        "carry on",
    ] {
        assert!(is_acknowledgement(ack), "{ack}");
    }
    for task in ["fix the tests", "go fix the recap", "ok now ship 10.1"] {
        assert!(!is_acknowledgement(task), "{task}");
    }
}

#[test]
fn legacy_long_session_abstains_from_its_first_prompt() {
    // Episodes written before the evidence fields only carry the first
    // prompt; in a long session it is a stale topic.
    let mut ep = episode();
    ep.recap_evidence_version = None;
    ep.anchor_request = None;
    ep.completions.clear();
    ep.request = "anthropics codemod, when github links are bought up".into();
    ep.completed = String::new();
    ep.message_count = 30_393;
    ep.next_steps = None;
    ep.todos.clear();
    ep.blockers = None;
    assert_eq!(compose_recap(&ep, &RecapFeeds::empty(), "3h ago"), None);

    ep.message_count = 6;
    assert_eq!(
        compose_recap(&ep, &RecapFeeds::empty(), "3h ago").as_deref(),
        Some("recap [3h ago]: anthropics codemod, when github links are bought up. [[CSR:RECAP]]")
    );
}

#[test]
fn legacy_completed_prose_is_never_rendered() {
    let mut ep = episode();
    ep.recap_evidence_version = None;
    ep.anchor_request = None;
    ep.completions.clear();
    ep.request = "Fix the recap composer".into();
    ep.completed = "I'll recreate it headless: a real claude session runs in tmux.".into();
    ep.next_steps = None;
    ep.todos.clear();
    ep.blockers = None;
    let got = compose_recap(&ep, &RecapFeeds::empty(), "2m ago").unwrap();
    assert_eq!(got, "recap [2m ago]: Fix the recap composer. [[CSR:RECAP]]");
}

#[test]
fn completed_clause_prefers_commits_and_renders_entries_whole() {
    let mut ep = episode();
    ep.completions = vec![
        CompletionReceipt {
            claim: "oldest".into(),
            receipt: "0000000".into(),
        },
        CompletionReceipt {
            claim: "x".repeat(101),
            receipt: "1111111".into(),
        },
        CompletionReceipt {
            claim: "unreceipted".into(),
            receipt: " ".into(),
        },
        CompletionReceipt {
            claim: "Write regression tests".into(),
            receipt: "task #3 completed".into(),
        },
    ];
    ep.next_steps = None;
    ep.todos.clear();
    ep.blockers = None;
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    // Commits come first: the only renderable commit (the 101-char claim
    // is dropped, never cut) outranks the newer task receipt.
    assert_eq!(
        got,
        "recap [now]: Fix the recap composer. Latest: oldest (0000000). [[CSR:RECAP]]"
    );
    // With no commit at all, the latest task receipts show.
    ep.completions
        .retain(|done| !is_commit_receipt(&done.receipt));
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert_eq!(
        got,
        "recap [now]: Fix the recap composer: Write regression tests (task #3 completed). [[CSR:RECAP]]"
    );
}

#[test]
fn only_receipts_made_under_the_anchor_are_shown() {
    let done = |claim: &str, receipt: &str| CompletionReceipt {
        claim: claim.into(),
        receipt: receipt.into(),
    };
    // The head line is the intent and what was made under it; what the
    // session made under other asks is `Settled:`, never beside the intent.
    let head = |text: &str| text.lines().next().unwrap().to_string();
    let mut ep = episode();
    ep.todos.clear();
    ep.next_steps = None;
    ep.blockers = None;
    ep.completions = vec![
        done("fix(a): under an earlier ask", "aaaaaaa"),
        done("fix(b): under the anchor", "bbbbbbb"),
        done("fix(c): under a later ask", "ccccccc"),
    ];
    // Only the anchor's own commit stands beside its intent.
    ep.anchor_receipts = Some(vec![1]);
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert_eq!(
        head(&got),
        "recap [now]: Fix the recap composer. Latest: fix(b): under the anchor (bbbbbbb). [[CSR:RECAP]]"
    );
    assert_eq!(
        got.lines().nth(1),
        Some("Settled: fix(c): under a later ask (ccccccc).")
    );
    // The anchor made none: the head says what was asked and nothing more.
    ep.anchor_receipts = Some(vec![]);
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert_eq!(
        head(&got),
        "recap [now]: Fix the recap composer. [[CSR:RECAP]]"
    );
    assert_eq!(
        got.lines().nth(1),
        Some("Settled: fix(b): under the anchor (bbbbbbb); fix(c): under a later ask (ccccccc).")
    );
    // An episode stored before the tie was recorded: its receipts are the
    // anchor's own, or they are not shown beside the intent at all.
    ep.anchor_receipts = None;
    ep.completions_untied = true;
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert_eq!(
        head(&got),
        "recap [now]: Fix the recap composer. [[CSR:RECAP]]"
    );
    ep.completions_untied = false;
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert!(got.contains("(bbbbbbb); fix(c): under a later ask (ccccccc)"));
    assert!(!got.contains("Settled:"), "{got}");
    // A task the session marked completed reads as the anchor's result.
    ep.completions = vec![done("Write regression tests", "task #3 completed")];
    ep.anchor_receipts = Some(vec![0]);
    assert_eq!(
        compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap(),
        "recap [now]: Fix the recap composer: Write regression tests (task #3 completed). [[CSR:RECAP]]"
    );
    assert_eq!(receipt_kind("todo completed"), ReceiptKind::Task);
    assert_eq!(receipt_kind("task #4 completed"), ReceiptKind::Task);
    assert_eq!(receipt_kind("deadbee"), ReceiptKind::Commit);
}

#[test]
fn abbreviations_do_not_end_a_sentence() {
    let long = "the recap composer so that it never cuts a prompt in the middle of a \
                    sentence, keeps soft wraps joined, reads only receipted completions, and \
                    abstains when nothing clean is left to say.";
    let text = format!("Please update e.g. {long}");
    assert!(text.chars().count() > INTENT_MAX_CHARS);
    assert_eq!(clean_intent(&text), None);

    let sentences = |text: &str| -> Vec<String> {
        split_preview_sentences(text)
            .into_iter()
            .map(|(start, end)| text[start..end].to_string())
            .collect()
    };
    assert_eq!(
        sentences("Use a tool, e.g. grep, i.e. fast, etc. vs. sed. Done."),
        ["Use a tool, e.g. grep, i.e. fast, etc. vs. sed.", "Done."]
    );
    assert_eq!(
        sentences("Ask Dr. Who and Mr. X, cf. approx. N. Smith. Done."),
        ["Ask Dr. Who and Mr. X, cf. approx. N. Smith.", "Done."]
    );
    assert_eq!(
        sentences("Ship 10.1 and fix stop.rs today. Then more."),
        ["Ship 10.1 and fix stop.rs today.", "Then more."]
    );
}

#[test]
fn legacy_untied_receipts_are_not_shown_beside_the_intent() {
    // An episode stored before the tie was recorded, whose receipts were
    // not the anchor's own: they stand beside the intent never, and are
    // only the commits the session made, under `Settled:`.
    let mut ep = episode();
    ep.anchor_request = Some("Fix the recap composer".into());
    ep.completions_untied = true;
    ep.anchor_receipts = None;
    ep.next_steps = None;
    ep.todos.clear();
    ep.blockers = None;
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert_eq!(
        got,
        "recap [now]: Fix the recap composer. [[CSR:RECAP]]\nSettled: Implemented deterministic recap output (abc1234)."
    );
    assert!(crate::extraction::provenance::is_csr_emission(&got));
    // Without an intent there is nothing left to say.
    ep.anchor_request = None;
    assert_eq!(compose_recap(&ep, &RecapFeeds::empty(), "now"), None);
}

#[test]
fn anchor_intent_cuts_an_over_long_first_sentence_at_a_clause() {
    assert_eq!(clean_intent(REYCITAL_1), None);
    assert_eq!(
        anchor_intent(REYCITAL_1).as_deref(),
        Some("This ia new Reycital challenge")
    );
    assert_eq!(clean_intent(SELF_BRANDING_1), None);
    assert_eq!(
        anchor_intent(SELF_BRANDING_1).as_deref(),
        Some(
            "I am visualizing a atlantic style essay about how ai usage trends have changed \
                 and why even though things look different they are thes same"
        )
    );
    // A whole sentence that fits still wins over any cut.
    assert_eq!(
        anchor_intent("Fix the recap composer, please. Then more.").as_deref(),
        Some("Fix the recap composer, please. Then more.")
    );
}

#[test]
fn clause_cut_needs_four_words_and_never_ends_in_a_path_or_url() {
    let long_tail = "word ".repeat(40);
    // Only boundary gives three words: abstain.
    assert_eq!(anchor_intent(&format!("Fix the pane, {long_tail}")), None);
    // A clause ending in a URL or path is never the cut.
    assert_eq!(
        anchor_intent(&format!(
            "review this repo https://github.com/x/y, {long_tail}"
        )),
        None
    );
    assert_eq!(
        anchor_intent(&format!(
            "compare the two takes in ~/Movies/take.mov; {long_tail}"
        )),
        None
    );
    // The last valid boundary at or before 160 chars wins; no ellipsis.
    let got = anchor_intent(&format!(
        "Build the reel from both clips, sync the melodica first; then {long_tail}"
    ))
    .unwrap();
    assert_eq!(
        got,
        "Build the reel from both clips, sync the melodica first"
    );
}

#[test]
fn a_clipped_anchor_is_still_recognised_as_a_csr_emission() {
    let mut ep = episode();
    ep.anchor_request = anchor_intent(REYCITAL_1);
    ep.completions.clear();
    ep.next_steps = None;
    ep.todos.clear();
    ep.blockers = None;
    let got = compose_recap(&ep, &RecapFeeds::empty(), "11h ago").unwrap();
    assert_eq!(
        got,
        "recap [11h ago]: This ia new Reycital challenge. [[CSR:RECAP]]"
    );
    assert!(crate::extraction::provenance::is_csr_emission(&got));
    assert!(crate::extraction::provenance::extractable(&got).is_none());
    assert_eq!(crate::extraction::provenance::strip_csr_emissions(&got), "");
}

#[test]
fn astra_r5_6_relative_paths_never_survive_a_clause_cut() {
    let text = format!(
        "Please fix the parser in src/parser.rs, {}",
        "word ".repeat(40)
    );
    assert_eq!(anchor_intent(&text), None);
    // A slash in prose is not a path.
    assert!(!is_path_or_url("and/or"));
    assert!(is_path_or_url("csr-engine/src/hooks/stop.rs"));
}

// ---- read-only probe against a real transcript ----------------------
//
// `cargo test --release --lib probe_real_transcript_recap -- --ignored --nocapture`
// with CSR_RECAP_PROBE_TRANSCRIPT=<jsonl>, optional CSR_RECAP_PROBE_UNTIL
// (ISO timestamp; lines from the first one at or after it are dropped, so
// the episode is rebuilt as the Stop hook saw it then), CSR_RECAP_PROBE_AGE,
// CSR_RECAP_PROBE_PROJECT, and CSR_RECAP_PROBE_DB (opened with
// SQLITE_OPEN_READ_ONLY, no migrations; feeds are only read).
#[test]
#[ignore = "reads a real transcript named by CSR_RECAP_PROBE_TRANSCRIPT"]
fn probe_real_transcript_recap() {
    let Ok(path) = std::env::var("CSR_RECAP_PROBE_TRANSCRIPT") else {
        eprintln!("CSR_RECAP_PROBE_TRANSCRIPT unset; nothing to probe");
        return;
    };
    let until = std::env::var("CSR_RECAP_PROBE_UNTIL").ok();
    let age = std::env::var("CSR_RECAP_PROBE_AGE").unwrap_or_else(|_| "now".into());
    let project = std::env::var("CSR_RECAP_PROBE_PROJECT").unwrap_or_else(|_| "probe".into());
    let raw = std::fs::read_to_string(&path).expect("transcript readable");
    let lines: Vec<&str> = raw
        .lines()
        .take_while(|line| {
            let Some(until) = until.as_deref() else {
                return true;
            };
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| v.get("timestamp")?.as_str().map(str::to_owned))
                .is_none_or(|ts| ts.as_str() < until)
        })
        .collect();
    let session_id = std::path::Path::new(&path)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ep = crate::hooks::stop::extract_episode(&lines, &session_id, &project);
    let feeds = match std::env::var("CSR_RECAP_PROBE_DB") {
        Ok(db) => {
            let storage = crate::storage::Storage::open_read_only(std::path::Path::new(&db))
                .expect("read-only open");
            let (settled, still_open) = storage
                .recap_ledger_feeds(&project, &ep.session_id)
                .unwrap_or_default();
            let since = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339();
            RecapFeeds {
                settled,
                still_open,
                retired_while_away: storage
                    .recap_retired_since(&project, &ep.timestamp)
                    .unwrap_or_default(),
                corrections: storage
                    .recap_corrections(&project, &since, 2)
                    .unwrap_or_default(),
                open_proposals: storage.recap_open_proposals(&project).unwrap_or(0),
                native_warn: None,
                top_dream: storage.recap_top_dream(&project).unwrap_or(None),
            }
        }
        Err(_) => RecapFeeds::empty(),
    };
    eprintln!("lines kept: {}", lines.len());
    eprintln!(
        "RECAP>>>\n{}\n<<<RECAP",
        compose_recap(&ep, &feeds, &age).unwrap_or_else(|| "(abstained)".into())
    );
}

// ---- negative corpus: genuine prose about recaps must stay extractable ----

#[test]
fn genuine_prose_mentioning_recaps_is_not_rejected() {
    let samples = [
        "Let's recap what we discussed: the login bug is fixed and deployed.",
        "The recap feature works great, though it's verbose in the Now: clause sometimes.",
        "I want to recap [the auth changes], then prioritize them.",
        "Can you write a quick recap of yesterday's session for the standup?",
    ];
    for sample in samples {
        assert!(
            extractable(sample).is_some(),
            "genuine prose wrongly rejected: {sample:?}"
        );
    }
}

// (a) The head is the first sentence of the stored intent, cut at a word.
// This reverses the earlier "quote the whole ask" rule on purpose: a head that
// is the raw prefix of a long prompt is noise (the SessionStart block holds 3 KB).
#[test]
fn the_head_is_the_first_sentence_cut_to_twelve_words() {
    let ask = "please pull the latest 24h data - then ask the anukriti campagin about our latest chakho reel for mirabhai then check this boost proposal for mamta";
    let mut ep = episode();
    ep.anchor_request = Some(ask.into());
    let got = session_start(&ep, &RecapFeeds::empty()).expect("composes");
    assert!(
        got.starts_with(
            "recap [3d ago]: please pull the latest 24h data - then ask the anukriti campagin\u{2026} Latest: "
        ),
        "{got}"
    );
    ep.anchor_request = Some(
        "Please check studio connectivity and then review .plans/10.1-core-rewrite - and suggest how you plan to execute it".into(),
    );
    let got = session_start(&ep, &RecapFeeds::empty()).unwrap();
    assert!(
        got.starts_with("recap [3d ago]: Please check studio connectivity and then review .plans/10.1-core-rewrite - and suggest how\u{2026} "),
        "{got}"
    );
    assert!(!got.contains("you plan to execute it"), "{got}");
    ep.anchor_request = Some(FIELD_PROMPT.into());
    let got = session_start(&ep, &RecapFeeds::empty()).unwrap();
    assert!(
        got.starts_with(
            "recap [3d ago]: Review previous sesison - create a simple unwall for me where we\u{2026} "
        ),
        "{got}"
    );
}

#[test]
fn the_head_stops_at_the_first_sentence_and_never_cuts_a_short_one() {
    let mut ep = episode();
    ep.anchor_request = Some(
        "Fix the login regression. Then rewrite the docs for the whole onboarding flow, please."
            .into(),
    );
    let got = session_start(&ep, &RecapFeeds::empty()).expect("composes");
    assert!(
        got.starts_with("recap [3d ago]: Fix the login regression. Latest: "),
        "{got}"
    );
    // Twelve words exactly: kept whole, no ellipsis.
    let twelve = "one two three four five six seven eight nine ten eleven twelve";
    ep.anchor_request = Some(twelve.into());
    assert_eq!(composed_intent(&ep).as_deref(), Some(twelve));
    // Thirteen: cut after the twelfth word.
    ep.anchor_request = Some(format!("{twelve} thirteen"));
    assert_eq!(
        composed_intent(&ep),
        Some(format!("{twelve}\u{2026}")),
        "cut at a word boundary"
    );
}

#[test]
fn no_head_is_a_raw_prompt_prefix_of_more_than_twelve_words() {
    let prompts = [
        "please pull the latest 24h data - then ask the anukriti campagin about our latest chakho reel for mirabhai then check this boost proposal for mamta",
        FIELD_PROMPT,
        "review 57552011 - previous session, the quality is still off - deploy a blind worker",
        "Fix the auth middleware regression.",
    ];
    for prompt in prompts {
        let mut ep = episode();
        ep.anchor_request = Some(prompt.into());
        let head = composed_intent(&ep).expect("a head");
        // Whatever the prompt, the head is at most twelve words, so a head that
        // is a prefix of the prompt is never a long one.
        assert!(
            head.split_whitespace().count() <= HEAD_INTENT_MAX_WORDS,
            "{head}"
        );
        let line = session_start(&ep, &RecapFeeds::empty()).expect("composes");
        assert!(
            crate::extraction::provenance::is_csr_emission(&line),
            "{line}"
        );
    }
}

#[test]
fn an_ask_over_forty_words_is_cut_at_a_word_with_an_ellipsis() {
    let words: Vec<String> = (1..=45).map(|n| format!("word{n}")).collect();
    let long = words.join(" ");
    let cut = cap_words(&long);
    assert_eq!(cut, format!("{}\u{2026}", words[..40].join(" ")));
    assert_eq!(cap_words(&words[..40].join(" ")), words[..40].join(" "));
    assert_eq!(cap_words("short ask, kept"), "short ask, kept");
    // The stored anchor is the opening clause of a longer first prompt: the
    // whole ask (`full_intent`, which `Then:` compares against) quotes the
    // prompt and marks the 200-byte cut with an ellipsis; the head clause
    // itself is the 12-word `composed_intent`.
    let mut ep = bare_episode();
    ep.anchor_request = Some(
        "review 57552011 - previous session, the quality is still off - deploy a blind worker"
            .into(),
    );
    ep.request = "review 57552011 - previous session, the quality is still off - deploy a blind worker - explain the environment and the desire and let it propose something completely blind on how the ranking should work, then compare it with ou".into();
    assert!(ep.request.len() > 196);
    assert_eq!(
        full_intent(&ep).as_deref(),
        Some("review 57552011 - previous session, the quality is still off - deploy a blind worker - explain the environment and the desire and let it propose something completely blind on how the ranking should work, then compare it with\u{2026}")
    );
    ep.next_steps = Some("Rank the blind set".into());
    let got = session_start(&ep, &RecapFeeds::empty()).expect("composes");
    assert!(
        got.starts_with("recap [3d ago]: review 57552011 - previous session, the quality is still off - deploy\u{2026} [[CSR:RECAP]]"),
        "{got}"
    );
    // A first prompt that is not the anchor's start is never quoted.
    ep.request = "something else entirely that the session opened with".into();
    assert_eq!(
        full_intent(&ep).as_deref(),
        Some(
            "review 57552011 - previous session, the quality is still off - deploy a blind worker"
        )
    );
}

#[test]
fn then_names_a_later_substantive_ask_and_never_a_remark() {
    let mut ep = episode();
    let then = |ep: &Episode| {
        session_start(ep, &RecapFeeds::empty())
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string()
    };
    for remark in [
        "save this",
        "ok",
        "continue",
        "please make this memory from the draft above",
        "status?",
    ] {
        ep.latest_ask = Some(remark.into());
        assert!(!then(&ep).contains("Then:"), "{remark}: {}", then(&ep));
    }
    // The last message is a remark: the latest task prompt after the anchor is named.
    ep.latest_ask = Some("i am off TB now on wifi".into());
    ep.compact_anchor = Some(crate::hooks::stop::CompactAnchor {
        request:
            "please fix the homepage bow bug, first check how headless browsers see the preview"
                .into(),
        completions_untied: true,
        receipts: Some(vec![]),
    });
    assert!(
        then(&ep).contains(
            "Then: please fix the homepage bow bug, first check how headless browsers see the preview."
        ),
        "{}",
        then(&ep)
    );
    // A long later ask follows the same cap as the intent.
    let words: Vec<String> = (1..=45).map(|n| format!("w{n}")).collect();
    ep.latest_ask = Some(format!("Why did the importer drop {}?", words.join(" ")));
    let got = then(&ep);
    assert!(got.contains("Then: Why did the importer drop w1"), "{got}");
    assert!(got.contains("w35\u{2026}"), "{got}");
    assert!(!got.contains("w36"), "{got}");
}

// (c) The LAST finding is never lost.
#[test]
fn a_head_keeps_the_last_finding_and_a_bare_head_needs_named_evidence() {
    let mut ep = bare_episode();
    // Replay case 1: the finding sits in the bold verdict, not the first sentence.
    ep.completed = "Deployed and verified. All 37 articles still ship in the server HTML and the cap is paint-only, so the crawl is intact. **The bug was the opposite of what it looked like.** The mobile grid is 6 columns.".into();
    let mut feeds = RecapFeeds::empty();
    feeds.settled = vec![SettledFact {
        claim: "schema is stable".into(),
        receipt: "810283b".into(),
        status: "resolved".into(),
        source: "user_confirmed".into(),
    }];
    let got = session_start(&ep, &feeds).expect("composes");
    assert_eq!(
        lines_of(&got)[1],
        "Last: The bug was the opposite of what it looked like. \u{2026} The mobile grid is 6 columns (conv session-).",
        "{got}"
    );
    // The same finding the fragment block's LAST line carries.
    let fragment = crate::hooks::session_start::format_tier0_block_with_recap(
        &ep,
        &[],
        "3d ago",
        None,
        crate::provenance::TrustTier::Unknown,
    );
    let last = fragment
        .lines()
        .find_map(|line| line.strip_prefix("LAST: "))
        .and_then(|line| line.rsplit_once(" ("))
        .map(|(finding, _)| finding.trim_end_matches('.').to_string())
        .expect("a LAST line");
    assert!(
        lines_of(&got)[1].starts_with(&format!("Last: {last} (conv")),
        "{last}\n{got}"
    );
    // The repo recap keeps its own grammar.
    assert!(!compose_recap(&ep, &feeds, "now").unwrap().contains("Last:"));
    // No intent: a correction alone does not make a bare head, the fragment block runs.
    ep.anchor_request = None;
    let mut correction_only = RecapFeeds::empty();
    correction_only.corrections = vec![CorrectionLine {
        quote: "no, keep the old path".into(),
        session8: "abcd1234".into(),
        turn: 3,
        date: "2026-10-30".into(),
    }];
    assert!(session_start(&ep, &correction_only).is_none());
    // A named todo stands: the bare head carries the finding too.
    ep.todos = vec![
        TodoItem {
            content: "Decide scene nav label casing".into(),
            status: "pending".into(),
        },
        TodoItem {
            content: "Fix the bento grid on mobile".into(),
            status: "pending".into(),
        },
    ];
    let got = session_start(&ep, &correction_only).expect("composes");
    assert!(
        got.starts_with("recap [3d ago]: [[CSR:RECAP]]\nLast: The bug was"),
        "{got}"
    );
    assert!(
        got.contains("Now: open todo: Fix the bento grid on mobile (conv session-)."),
        "{got}"
    );
    assert!(
        got.ends_with("Next: Decide scene nav label casing."),
        "{got}"
    );
    assert!(
        crate::extraction::provenance::is_csr_emission(&got),
        "{got}"
    );
}

// (b) Now names todos.
#[test]
fn now_names_open_todos_by_title_and_never_a_bare_count() {
    let mut ep = episode();
    ep.blockers = None;
    ep.todos = vec![
        TodoItem {
            content: "wire it".into(),
            status: "in_progress".into(),
        },
        TodoItem {
            content: "test it".into(),
            status: "pending".into(),
        },
        TodoItem {
            content: "ship it".into(),
            status: "pending".into(),
        },
    ];
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert!(
        got.contains("Now: open todos: wire it; test it; +1 more (conv session-)."),
        "{got}"
    );
    assert!(!got.contains("todos open"), "{got}");
    // `Next:` names the first open todo; `Now:` does not repeat it.
    ep.next_steps = None;
    ep.todos.pop();
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert!(
        got.contains("Now: open todo: test it (conv session-)."),
        "{got}"
    );
    assert!(got.ends_with("Next: wire it."), "{got}");
    // A stored next step that is an open todo is not said twice either.
    ep.next_steps = Some("Wire it".into());
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert!(
        got.contains("Now: open todo: test it (conv session-)."),
        "{got}"
    );
    assert!(got.ends_with("Next: Wire it."), "{got}");
    // Todos with no nameable title: no `Now:` entry at all.
    ep.todos = vec![TodoItem {
        content: "...".into(),
        status: "pending".into(),
    }];
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert!(!got.contains("Now:"), "{got}");
}

// (d) Project-wide evidence must be about the request.
#[test]
fn a_dream_on_a_file_the_session_touched_is_dropped_when_the_recap_does_not_name_it() {
    // Replay case 996285c6: the session edited work/page.tsx, but nothing the
    // recap says mentions GATES or that file.
    let mut ep = episode();
    ep.anchor_request =
        Some("Review the previous sessions where we focused on the homepage".into());
    ep.files_modified = vec!["/Users/x/client-website/src/app/work/page.tsx".into()];
    let feeds = RecapFeeds {
        top_dream: Some(witness_dream(
            "GATES in work/page.tsx",
            "src/app/work/page.tsx",
            "GATES",
        )),
        ..RecapFeeds::empty()
    };
    let got = session_start(&ep, &feeds).unwrap();
    assert!(!got.contains("Dreamt:"), "{got}");
    assert!(!got.contains("GATES"), "{got}");
    // The repo recap is unchanged.
    let got = compose_recap(&ep, &feeds, "now").unwrap();
    assert!(got.contains("Dreamt: GATES in work/page.tsx"), "{got}");
}

#[test]
fn a_dream_about_another_page_is_dropped_under_an_unrelated_ask() {
    // Replay case d63802a8: a Chakar Rakho campaign ask, a dream about the
    // Mahalakshmi Ashtakam page the session also edited.
    let mut ep = episode();
    ep.anchor_request = Some("a pair comes calling about a new campaign chakar rakh lo".into());
    ep.files_modified =
        vec!["/Users/x/anukriti-website/src/app/mahalakshmi-ashtakam-meaning/page.tsx".into()];
    let feeds = RecapFeeds {
        top_dream: Some(witness_dream(
            "MahalakshmiAshtakamPage in mahalakshmi-ashtakam-meaning/page.tsx",
            "src/app/mahalakshmi-ashtakam-meaning/page.tsx",
            "MahalakshmiAshtakamPage",
        )),
        retired_while_away: vec![RetiredLine {
            label: "MahalakshmiAshtakamPage".into(),
            receipt_oid: "e72ad1b".into(),
            date: "2026-10-27".into(),
        }],
        ..RecapFeeds::empty()
    };
    let got = session_start(&ep, &feeds).unwrap();
    assert!(!got.contains("Dreamt:"), "{got}");
    assert!(!got.contains("Learnt-then-retired"), "{got}");
    assert!(!got.contains("Mahalakshmi"), "{got}");
}

#[test]
fn a_dream_or_retired_line_is_kept_when_the_recap_names_its_file_or_symbol() {
    let mut ep = episode();
    let feeds = RecapFeeds {
        top_dream: Some(witness_dream(
            "GATES in work/page.tsx",
            "src/app/work/page.tsx",
            "GATES",
        )),
        retired_while_away: vec![RetiredLine {
            label: "src/app/work/page.tsx".into(),
            receipt_oid: "41748ea".into(),
            date: "2026-08-05".into(),
        }],
        ..RecapFeeds::empty()
    };
    // The ask names the file (its basename is enough).
    ep.anchor_request = Some("Why does the work page.tsx hero still miss on mobile".into());
    let got = session_start(&ep, &feeds).unwrap();
    assert!(
        got.contains("Dreamt: GATES in work/page.tsx went stale (2026-08-11, abc1234)."),
        "{got}"
    );
    assert!(
        got.contains("Learnt-then-retired while away: src/app/work/page.tsx (superseded 2026-08-05, 41748ea)."),
        "{got}"
    );
    // The symbol named by a line the recap shows (here `Next:`), not the ask.
    ep.anchor_request = Some("Fix the recap composer".into());
    ep.next_steps = Some("Rename GATES to the new gate list".into());
    let got = session_start(&ep, &feeds).unwrap();
    assert!(got.contains("Dreamt: GATES in work/page.tsx"), "{got}");
    // A word that merely contains the symbol does not name it.
    ep.next_steps = Some("Run the GATESKEEPER suite".into());
    let got = session_start(&ep, &feeds).unwrap();
    assert!(!got.contains("Dreamt:"), "{got}");
    assert!(!got.contains("Learnt-then-retired"), "{got}");
}

#[test]
fn a_then_clause_that_does_not_fit_cannot_justify_a_dream() {
    // Review P2: the relevance check read the `Then:` ask even when the
    // clause was dropped, so a dream only that ask named survived.
    let mut ep = episode();
    ep.anchor_request = Some("Fix the login regression".into());
    ep.next_steps = Some("Run the login tests".into());
    ep.latest_ask = Some(format!(
        "Review the implementation in kitty.rs {}",
        "x".repeat(800)
    ));
    let feeds = RecapFeeds {
        top_dream: Some(witness_dream(
            "transmit in pane/kitty.rs",
            "src/pane/kitty.rs",
            "transmit",
        )),
        ..RecapFeeds::empty()
    };
    let got = session_start(&ep, &feeds).unwrap();
    assert!(!got.contains("Then:"), "{got}");
    assert!(!got.contains("Dreamt:"), "{got}");
    // A `Then:` that fits and names the file still cannot add the dream:
    // `compose_with_then` keeps `Then:` only when the lines below match the
    // paragraph without it.
    ep.latest_ask = Some("Review the implementation in kitty.rs again".into());
    let got = session_start(&ep, &feeds).unwrap();
    assert!(!got.contains("Dreamt:"), "{got}");
}

#[test]
fn a_long_branch_name_cannot_push_the_head_past_the_budget() {
    // Review P2: two marked receipts with a 240-char branch name made a
    // 765-char head.
    let mut ep = episode();
    ep.anchor_request = Some("Fix the login regression".into());
    let claim = |n: &str| format!("{n} {}", "c".repeat(90 - n.len() - 1));
    ep.completions = vec![
        receipt(&claim("first"), "abc1234"),
        receipt(&claim("second"), "def5678"),
    ];
    ep.anchor_receipts = Some(vec![0, 1]);
    let ctx = off_branch(&"b".repeat(240), &["abc1234", "def5678"]);
    let got = compose_recap_in_context(&ep, &RecapFeeds::empty(), "9m ago", &ctx)
        .unwrap()
        .text;
    let head = got.lines().next().unwrap();
    assert!(
        head.chars().count() <= MAX_CHARS,
        "{} chars: {head}",
        head.chars().count()
    );
    assert!(got.chars().count() <= MAX_CHARS, "{got}");
    let note = format!(
        "(abc1234, not on {}\u{2026})",
        "b".repeat(BRANCH_LABEL_MAX_CHARS - 1)
    );
    assert!(head.contains(&note), "{head}");
    // A branch name that fits is quoted whole.
    let ctx = off_branch("fix/recap4", &["abc1234"]);
    let got = compose_recap_in_context(&ep, &RecapFeeds::empty(), "9m ago", &ctx)
        .unwrap()
        .text;
    assert!(got.contains("(abc1234, not on fix/recap4)"), "{got}");
}

#[test]
fn a_session_receipt_made_before_the_anchor_request_is_not_settled() {
    let mut ep = episode();
    ep.completions = vec![
        receipt("chore: bump the lockfile", "1111111"),
        receipt("Implemented deterministic recap output", "abc1234"),
    ];
    ep.anchor_receipts = Some(vec![1]);
    let got = session_start(&ep, &RecapFeeds::empty()).unwrap();
    assert!(
        !got.contains("1111111"),
        "made under an earlier prompt: {got}"
    );
    assert!(got.contains("(abc1234)"), "{got}");
    // The repo recap keeps every session receipt.
    assert!(compose_recap(&ep, &RecapFeeds::empty(), "now")
        .unwrap()
        .contains("1111111"));
}

#[test]
fn off_branch_receipts_are_marked_with_the_head_branch_name() {
    let mut ep = episode();
    // The ask names the dream's symbol, so SessionStart shows it.
    ep.anchor_request = Some("Fix the recap composer and parse_config".into());
    ep.completions.push(receipt("Second change", "def5678"));
    ep.anchor_receipts = Some(vec![0, 1]);
    let mut feeds = RecapFeeds::empty();
    feeds.settled = vec![SettledFact {
        claim: "schema is stable".into(),
        receipt: "810283b".into(),
        status: "resolved".into(),
        source: "user_confirmed".into(),
    }];
    feeds.top_dream = Some(dream());
    let ctx = off_branch("main", &["abc1234", "810283b"]);
    let got = compose_recap_in_context(&ep, &feeds, "9m ago", &ctx)
        .unwrap()
        .text;
    assert!(
        got.contains("Implemented deterministic recap output (abc1234, not on main)"),
        "{got}"
    );
    assert!(
        got.contains("Second change (def5678)"),
        "an on-branch receipt stays bare: {got}"
    );
    assert!(
        got.contains("schema is stable (810283b, not on main)"),
        "{got}"
    );
    assert!(
        got.contains("went stale (2026-08-11, abc1234, not on main)"),
        "{got}"
    );
    // Detached HEAD has no branch name.
    let detached = off_branch("HEAD", &["abc1234"]);
    let got = compose_recap_in_context(&ep, &feeds, "now", &detached)
        .unwrap()
        .text;
    assert!(got.contains("(abc1234, not on HEAD)"), "{got}");
    // No context (no repo, timeout, failure): the recap is what it always was.
    let plain = compose_recap_with_deliveries(&ep, &feeds, "now")
        .unwrap()
        .text;
    assert!(!plain.contains("not on"), "{plain}");
}

#[test]
fn candidate_receipts_are_the_commits_the_recap_can_show() {
    let mut ep = episode();
    ep.completions
        .push(receipt("Task done", "task #3 completed"));
    let mut feeds = RecapFeeds::empty();
    feeds.top_dream = Some(dream());
    let got = candidate_receipts(&ep, &feeds);
    assert_eq!(
        got,
        vec!["abc1234".to_string()],
        "one entry per distinct commit, no task receipts"
    );
}

#[test]
fn now_says_the_last_turn_did_not_finish_only_with_evidence() {
    let mut ep = episode();
    ep.blockers = None;
    ep.todos.clear();
    ep.outcome_version = Some(crate::hooks::stop::OUTCOME_VERSION);
    ep.closing_ended = false;
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert!(
        got.contains("Now: last turn did not finish (conv session-)."),
        "{got}"
    );
    // The session now running is mid-turn by definition.
    let live = RecapContext {
        session_is_live: true,
        ..Default::default()
    };
    let got = compose_recap_in_context(&ep, &RecapFeeds::empty(), "now", &live)
        .unwrap()
        .text;
    assert!(!got.contains("Now:"), "{got}");
    // An episode whose outcome rule never ran cannot say it.
    ep.outcome_version = None;
    assert!(!compose_recap(&ep, &RecapFeeds::empty(), "now")
        .unwrap()
        .contains("Now:"));
    // A session that ended on a finished reply does not say it.
    ep.outcome_version = Some(crate::hooks::stop::OUTCOME_VERSION);
    ep.closing_ended = true;
    assert!(!compose_recap(&ep, &RecapFeeds::empty(), "now")
        .unwrap()
        .contains("Now:"));
}

#[test]
fn the_unfinished_turn_note_alone_does_not_make_a_paragraph() {
    let mut ep = bare_episode();
    ep.outcome_version = Some(crate::hooks::stop::OUTCOME_VERSION);
    ep.closing_ended = false;
    assert!(
        compose_recap_with_deliveries(&ep, &RecapFeeds::empty(), "now").is_none(),
        "intent plus the note is still only the prompt"
    );
}

#[test]
fn a_bare_one_word_dream_label_is_never_printed() {
    let mut feeds = RecapFeeds::empty();
    let mut bare = dream();
    bare.label = "send".into();
    feeds.top_dream = Some(bare);
    let got = compose_recap(&episode(), &feeds, "now").unwrap();
    assert!(!got.contains("Dreamt:"), "{got}");
    let mut qualified = dream();
    qualified.label = "send in pane/kitty.rs".into();
    feeds.top_dream = Some(qualified);
    let got = compose_recap(&episode(), &feeds, "now").unwrap();
    assert!(
        got.contains("Dreamt: send in pane/kitty.rs went stale"),
        "{got}"
    );
}

#[test]
fn a_first_sentence_over_the_char_limit_still_gets_a_twelve_word_head() {
    let mut ep = bare_episode();
    let long = format!(
        "Rework the whole recap composer so that {phrase} and {phrase} and keep every receipt intact.",
        phrase = "the head clause stays short while the stored anchor keeps the rest of the ask"
    );
    assert!(long.chars().count() > INTENT_MAX_CHARS);
    ep.anchor_request = Some(long);
    assert_eq!(recap_intent(&ep), None, "the stored-intent rule abstains");
    let head = composed_intent(&ep).expect("a head from the first sentence");
    assert_eq!(
        head,
        "Rework the whole recap composer so that the head clause stays short\u{2026}"
    );
}

#[test]
fn an_old_episode_falls_back_to_the_first_prompt_in_a_short_session() {
    let mut ep = bare_episode();
    ep.recap_evidence_version = None;
    ep.anchor_request = None;
    ep.request = format!(
        "Fix the login regression. Then rewrite the docs. {}",
        "Padding words keep this stored prompt past the old extractor cut. ".repeat(4)
    );
    assert!(ep.request.len() >= LEGACY_REQUEST_CAP_BYTES);
    assert_eq!(recap_intent(&ep), None, "the stored-intent rule abstains");
    assert_eq!(
        composed_intent(&ep).as_deref(),
        Some("Fix the login regression.")
    );
    // A current episode with no stored anchor has no head: Stop found no
    // clean task, and the first prompt is not a stand-in.
    ep.recap_evidence_version = Some(crate::hooks::stop::RECAP_EVIDENCE_VERSION);
    assert_eq!(composed_intent(&ep), None);
    ep.recap_evidence_version = None;
    // A long session's first prompt is a stale topic: no head from it.
    ep.message_count = LEGACY_SINGLE_TOPIC_MAX_MESSAGES + 1;
    assert_eq!(composed_intent(&ep), None);
    // A first prompt that only introduces material that follows names nothing.
    ep.message_count = 4;
    ep.request = "Here is the note:\n- one\n- two".into();
    assert_eq!(composed_intent(&ep), None);
}
