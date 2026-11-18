use super::*;

#[test]
fn a_pasted_wrapper_tag_never_reaches_the_recap_intent() {
    for request in [
        "<pasted_content id=\"818d\">\nspawn a worker to fix the feature earlier h",
        "<pasted_content id=\"818d\">\nspawn a worker\n</pasted_content>",
    ] {
        let mut ep = episode();
        ep.recap_evidence_version = None;
        ep.request = request.into();
        ep.anchor_request = None;
        let intent = recap_intent(&ep);
        assert!(
            intent.as_deref().is_none_or(|i| !i.contains('<')),
            "{intent:?}"
        );
    }
}

#[test]
fn no_evidence_means_no_settled_now_or_next_clause() {
    let text = compose_recap(&bare_episode(), &bare_feeds(), "1h ago").unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    for clause in ["Settled:", "Now:", "Next:"] {
        assert!(!text.contains(clause), "{clause}: {text}");
    }
}

#[test]
fn session_start_abstains_when_the_recap_would_only_echo_the_prompt() {
    let mut ep = bare_episode();
    // A later typed ask adds a `Then:` clause, which is the user's words too.
    ep.latest_ask = Some("Please fix the other composer bug".into());
    assert!(compose_recap_with_deliveries(&ep, &bare_feeds(), "1h ago").is_none());
    // The repo recap labels the bare intent itself and keeps getting it.
    let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
    assert!(
        text.starts_with("recap [1h ago]: Fix the recap composer"),
        "{text}"
    );
    assert_eq!(text.lines().count(), 1, "{text}");
}

#[test]
fn any_evidenced_clause_keeps_the_recap_and_stays_a_recognised_emission() {
    use crate::extraction::provenance::is_csr_emission;
    let evidenced: Vec<(&str, Episode, RecapFeeds)> = {
        let mut completed = bare_episode();
        completed.completions = vec![receipt("fix(auth): keep form errors", "abc1234")];
        completed.anchor_receipts = Some(vec![0]);
        let mut settled = bare_episode();
        settled.completions = vec![receipt("fix(auth): keep form errors", "abc1234")];
        settled.completions_untied = true;
        let mut now = bare_episode();
        now.todos = vec![TodoItem {
            content: "wire session start".into(),
            status: "pending".into(),
        }];
        let mut next = bare_episode();
        next.next_steps = Some("run the device tests".into());
        let mut corrected = bare_feeds();
        corrected.corrections = vec![CorrectionLine {
            quote: "no, use the shared cache".into(),
            session8: "761bf75c".into(),
            turn: 12,
            date: "2026-10-30".into(),
        }];
        vec![
            ("completed", completed, bare_feeds()),
            ("settled", settled, bare_feeds()),
            ("now", now, bare_feeds()),
            ("next", next, bare_feeds()),
            ("corrected", bare_episode(), corrected),
        ]
    };
    for (kind, ep, feeds) in evidenced {
        let composed = compose_recap_with_deliveries(&ep, &feeds, "1h ago")
            .unwrap_or_else(|| panic!("{kind}: abstained"));
        assert!(composed.text.starts_with("recap [1h ago]: "), "{kind}");
        assert!(is_csr_emission(&composed.text), "{kind}: {}", composed.text);
        // The sentinel closes the first line, as the re-import guard expects.
        assert!(
            composed
                .text
                .lines()
                .next()
                .unwrap()
                .ends_with("[[CSR:RECAP]]"),
            "{kind}: {}",
            composed.text
        );
    }
}

#[test]
fn settled_names_session_commits_the_head_clause_does_not_carry() {
    let mut ep = bare_episode();
    ep.completions = vec![
        receipt("fix(auth): keep form errors", "abc1234"),
        receipt("merged PR #78", "def5678"),
    ];
    // Neither commit was made under the anchor's own task.
    ep.completions_untied = true;
    let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
    let settled = lines_of(&text)
        .into_iter()
        .find(|line| line.starts_with("Settled: "))
        .unwrap_or_else(|| panic!("no Settled clause: {text}"));
    assert_eq!(
        settled,
        "Settled: fix(auth): keep form errors (abc1234); merged PR #78 (def5678)."
    );
}

#[test]
fn settled_skips_a_commit_the_head_clause_already_names() {
    let mut ep = bare_episode();
    ep.completions = vec![
        receipt("fix(auth): keep form errors", "abc1234"),
        receipt("merged PR #78", "def5678"),
    ];
    ep.anchor_receipts = Some(vec![0]);
    let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
    assert!(
        text.contains("Latest: fix(auth): keep form errors (abc1234)"),
        "{text}"
    );
    assert!(text.contains("Settled: merged PR #78 (def5678)."), "{text}");
    assert_eq!(text.matches("abc1234").count(), 1, "{text}");
    // When the head clause names every receipt, there is nothing to settle.
    ep.anchor_receipts = Some(vec![0, 1]);
    let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
    assert!(!text.contains("Settled:"), "{text}");
}

#[test]
fn a_receiptless_session_commit_is_never_settled() {
    let mut ep = bare_episode();
    ep.completions = vec![receipt("fix(auth): keep form errors", "")];
    let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
    assert!(!text.contains("Settled:"), "{text}");
}

#[test]
fn now_names_a_failed_gate_with_its_command_and_exit_code() {
    let mut ep = bare_episode();
    ep.recap_extras.gate_failure = Some(GateFailure {
        command: "cargo test --release --lib".into(),
        exit_code: 101,
    });
    let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
    assert!(
        text.contains(
            "Now: last gate `cargo test --release --lib` failed (exit 101), not rerun green."
        ),
        "{text}"
    );
}

#[test]
fn now_names_background_tasks_never_reported_finished() {
    let mut ep = bare_episode();
    ep.recap_extras.background_open = vec![BackgroundTask {
        id: "bq2".into(),
        label: "upload build".into(),
    }];
    ep.recap_extras.background_open_total = 1;
    let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
    assert!(
        text.contains("Now: background task not reported finished: upload build (bq2)."),
        "{text}"
    );
    ep.recap_extras.background_open_total = 4;
    let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
    assert!(
        text.contains("4 background tasks not reported finished, newest: upload build (bq2)"),
        "{text}"
    );
}

#[test]
fn stored_evidence_without_a_receipt_or_with_csr_text_is_dropped() {
    let mut ep = bare_episode();
    ep.recap_extras.background_open = vec![BackgroundTask {
        id: String::new(),
        label: "upload build".into(),
    }];
    ep.recap_extras.gate_failure = Some(GateFailure {
        command: "recap [2h ago]: x [[CSR:RECAP]]".into(),
        exit_code: 1,
    });
    let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
    for clause in ["Settled:", "Now:", "Next:"] {
        assert!(!text.contains(clause), "{clause}: {text}");
    }
}

#[test]
fn a_recap_with_every_evidence_clause_stays_in_budget_and_trips_the_guard() {
    let mut ep = bare_episode();
    ep.completions = vec![
        receipt(
            "fix(auth): keep form errors until the next submitted request",
            "abc1234",
        ),
        receipt("merged PR #78", "def5678"),
    ];
    ep.completions_untied = true;
    ep.recap_extras.gate_failure = Some(GateFailure {
        command: "cargo clippy --release --lib --tests".into(),
        exit_code: 101,
    });
    ep.recap_extras.background_open = vec![BackgroundTask {
        id: "bq2".into(),
        label: "upload build".into(),
    }];
    ep.recap_extras.background_open_total = 1;
    ep.next_steps = Some("listen to the first 3 seconds on a phone.".into());
    let text = compose_recap(&ep, &bare_feeds(), "1h ago").unwrap();
    for clause in ["Settled:", "Now:", "Next:"] {
        assert!(text.contains(clause), "{clause}: {text}");
    }
    assert!(
        text.chars().count() <= MAX_CHARS,
        "{}: {text}",
        text.chars().count()
    );
    assert!(text.starts_with("recap [1h ago]: "), "{text}");
    // The self-contamination guard: the new grammar is still CSR's own.
    assert!(crate::extraction::provenance::is_csr_emission(&text));
    assert!(crate::extraction::provenance::contains_machine_sentinel(
        &text
    ));
    assert!(extractable(&text).is_none());
    let no_sentinel = text.replace(RECAP_SENTINEL, "");
    assert!(crate::extraction::provenance::is_csr_emission(&no_sentinel));
}

#[test]
fn clear_handoff_is_two_short_lines_with_a_pointer_and_the_guard_grammar() {
    let ep = episode();
    let handoff = compose_clear_handoff(&ep, "3m ago").expect("a usable intent");
    let lines: Vec<&str> = handoff.lines().collect();
    assert_eq!(lines.len(), 2, "{handoff}");
    assert_eq!(
        lines[0],
        "recap [cleared, 3m ago]: Fix the recap composer. Still open: wire session start."
    );
    assert_eq!(
        lines[1],
        format!("More: csr_reflect_on_past(\"conv_session-1\") {RECAP_SENTINEL}")
    );
    assert!(handoff.chars().count() <= 400, "{handoff}");
    // No dump of the old session: none of its completions, blockers or next steps.
    for leaked in [
        "Implemented",
        "abc1234",
        "storage migration",
        "integration suite",
    ] {
        assert!(!handoff.contains(leaked), "{leaked}: {handoff}");
    }
    // Re-import drops it, by sentinel and by the `recap [` grammar alone.
    assert!(crate::extraction::provenance::is_csr_emission(&handoff));
    let no_sentinel = handoff.replace(RECAP_SENTINEL, "");
    assert!(crate::extraction::provenance::is_csr_emission(&no_sentinel));
}

#[test]
fn is_request_accepts_clear_asks_and_fails_closed_on_everything_else() {
    for ask in [
        "Why did the second worker finish so early?",
        "ok so what should happen with the scratch files?",
        "Can you check the usage numbers",
        "could we ship the build tonight",
        "Change the subagent setting please",
        "see if anything happened since the last push",
        "Share the final build with me on mobile",
        "Share it on mobile",
        "Tell me which crate pulls in the old parser",
        "Please fix the build before the release",
        "I want you to rewrite the intro section",
        "let's drop the second experiment",
        "Check /tmp/csr-out for the new log",
    ] {
        assert!(is_request(ask), "should be an ask: {ask}");
    }
    for remark in [
        // A question word inside a statement.
        "I already know how many workers finished today",
        "I already know how many workers finished today?",
        "That is why the build was slow.",
        // A command word opening a noun phrase.
        "Work on the pane is finally finished",
        "Build is green again.",
        "Fix for the login bug landed",
        "Push to main failed",
        "Update on the release: it shipped",
        "Build for the release is green",
        "Run on the studio finished in two minutes",
        "Set up of the pane is done",
        "Run this morning took two minutes",
        "Share on mobile",
        "Update available for the CLI",
        "Push failed with a lock error",
        "Test results look fine to me",
        // Reactions and acknowledgements.
        "wow that worker kill took forever",
        "ok go",
        "keeping them open for now",
        "Thanks, that works.",
        // A question word that opens, but no question.
        "How the build got slow is a long story.",
        "Why did it fail. I already know the reason.",
        "",
    ] {
        assert!(!is_request(remark), "should not be an ask: {remark}");
    }
}

#[test]
fn then_clause_is_a_pure_addition_before_the_sentinel() {
    let mut ep = episode();
    let feeds = full_feeds();
    let old = compose_recap(&ep, &feeds, "3m ago").expect("recap");
    assert!(!old.contains("Then:"));
    ep.latest_ask = Some("Why did the second worker finish so early?".into());
    let new = compose_recap(&ep, &feeds, "3m ago").expect("recap");
    let at = old.find(&format!(" {RECAP_SENTINEL}")).expect("sentinel");
    let expected = format!(
        "{} Then: Why did the second worker finish so early?{}",
        &old[..at],
        &old[at..]
    );
    assert_eq!(new, expected);
    assert!(crate::extraction::provenance::is_csr_emission(&new));
    let no_sentinel = new.replace(RECAP_SENTINEL, "");
    assert!(crate::extraction::provenance::is_csr_emission(&no_sentinel));
}

#[test]
fn then_clause_is_dropped_when_it_would_displace_any_other_line() {
    let mut ep = episode();
    ep.anchor_request = Some("r".repeat(INTENT_MAX_CHARS));
    let feeds = full_feeds();
    let old = compose_recap(&ep, &feeds, "3m ago").expect("recap");
    ep.latest_ask = Some("Why did the second worker finish so early, given that the queue was idle all morning and nothing else was running?".into());
    let new = compose_recap(&ep, &feeds, "3m ago").expect("recap");
    // Either the recap is unchanged, or it is the old one plus the clause.
    if new != old {
        assert!(new.contains(" Then: "), "{new}");
        assert_eq!(
            new.split('\n').skip(1).collect::<Vec<_>>(),
            old.split('\n').skip(1).collect::<Vec<_>>()
        );
    }
    assert!(new.chars().count() <= 700, "{new}");
}

#[test]
fn then_clause_never_pushes_the_paragraph_past_the_budget() {
    // `n` characters of plain words.
    let words = |n: usize| {
        let mut text = String::new();
        for word in ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]
            .iter()
            .cycle()
        {
            if text.len() + word.len() + 1 > n {
                break;
            }
            text.push_str(word);
            text.push(' ');
        }
        text.trim_end().to_string()
    };
    let mut ep = episode();
    ep.anchor_request = Some(format!("Rework {}.", words(148)));
    ep.completions = vec![
        CompletionReceipt {
            claim: format!("Rewrote {}", words(88)),
            receipt: "a".repeat(40),
        },
        CompletionReceipt {
            claim: format!("Added {}", words(90)),
            receipt: "b".repeat(40),
        },
    ];
    ep.todos.clear();
    ep.blockers = None;
    ep.next_steps = Some(format!("Run {}", words(120)));
    let feeds = RecapFeeds::empty();
    let base = compose_recap(&ep, &feeds, "2 weeks ago").expect("recap");
    let ask = format!("Why did {}?", words(147));
    // The preconditions that make this test mean something.
    assert!(base.chars().count() <= 700, "{}", base.chars().count());
    assert!(
        base.chars().count() + ask.chars().count() > 700,
        "{}: {base}",
        base.chars().count()
    );
    ep.latest_ask = Some(ask);
    let with = compose_recap(&ep, &feeds, "2 weeks ago").expect("recap");
    assert!(
        with.chars().count() <= 700,
        "{}: {with}",
        with.chars().count()
    );
    assert_eq!(
        with, base,
        "an over-budget clause is dropped, not squeezed in"
    );
}

#[test]
fn then_clause_needs_a_distinct_ask_and_never_walks_back() {
    let feeds = full_feeds();
    let mut ep = episode();
    // The same words as the anchor, or part of it: nothing new to say.
    for same in [
        "Fix the recap composer",
        "fix the recap composer?",
        "Fix the recap composer please",
    ] {
        ep.latest_ask = Some(same.into());
        let out = compose_recap(&ep, &feeds, "3m ago").expect("recap");
        assert!(!out.contains("Then:"), "{same}: {out}");
    }
    // An acknowledgement, a remark and no ask at all: the recap is unchanged.
    let old = compose_recap(&episode(), &feeds, "3m ago").expect("recap");
    for quiet in [
        Some("ok go"),
        Some("Work on the pane is finally finished"),
        None,
    ] {
        ep.latest_ask = quiet.map(str::to_string);
        assert_eq!(
            compose_recap(&ep, &feeds, "3m ago").as_deref(),
            Some(old.as_str())
        );
    }
    // CSR's own text pasted back is not an ask.
    ep.latest_ask = Some(format!("Why did this repeat? {RECAP_SENTINEL}"));
    let out = compose_recap(&ep, &feeds, "3m ago").expect("recap");
    assert!(!out.contains("Then:"), "{out}");
}

#[test]
fn the_clear_handoff_names_the_last_ask_between_intent_and_still_open() {
    let mut ep = episode();
    ep.latest_ask = Some("Why did the second worker finish so early?".into());
    let handoff = compose_clear_handoff(&ep, "3m ago").expect("handoff");
    assert_eq!(
        handoff.lines().next(),
        Some("recap [cleared, 3m ago]: Fix the recap composer. Then: Why did the second worker finish so early? Still open: wire session start.")
    );
    assert!(crate::extraction::provenance::is_csr_emission(&handoff));
    ep.latest_ask = Some("ok go".into());
    assert_eq!(
        compose_clear_handoff(&ep, "3m ago"),
        compose_clear_handoff(&episode(), "3m ago")
    );
}

#[test]
fn clear_handoff_falls_back_to_next_steps_cuts_long_text_and_abstains_without_intent() {
    let mut ep = episode();
    ep.todos.clear();
    let handoff = compose_clear_handoff(&ep, "1h ago").unwrap();
    assert!(
        handoff.contains("Still open: Run the integration suite."),
        "{handoff}"
    );
    ep.next_steps = Some("word ".repeat(80));
    let handoff = compose_clear_handoff(&ep, "1h ago").unwrap();
    assert!(handoff.chars().count() <= 400, "{handoff}");
    assert!(handoff.lines().next().unwrap().ends_with('…') || handoff.contains("…."));
    ep.next_steps = None;
    ep.blockers = None;
    let handoff = compose_clear_handoff(&ep, "1h ago").unwrap();
    assert!(!handoff.contains("Still open"), "{handoff}");
    ep.anchor_request = None;
    assert!(compose_clear_handoff(&ep, "1h ago").is_none());
}

#[test]
fn recap_claims_that_are_csr_text_are_skipped() {
    let rule = "A standing rule from 10-12: “Keep receipts”";
    let mut feeds = full_feeds();
    feeds.settled.insert(
        0,
        SettledFact {
            claim: rule.into(),
            receipt: "abc1234".into(),
            status: "resolved".into(),
            source: "user_confirmed".into(),
        },
    );
    feeds.still_open.insert(
        0,
        SettledFact {
            claim: rule.into(),
            receipt: "abc1234".into(),
            status: "still_open".into(),
            source: "user_confirmed".into(),
        },
    );
    let recap = compose_recap(&episode(), &feeds, "2h ago").unwrap();
    assert!(!recap.contains("Keep receipts"), "{recap}");
    // the other claims still stand
    assert!(recap.contains("schema is stable"), "{recap}");
}

#[test]
fn composes_full_recap_in_causal_clause_order() {
    let got = compose_recap(&episode(), &full_feeds(), "28m ago").unwrap();
    assert_eq!(
        got,
        "recap [28m ago]: Fix the recap composer. Latest: Implemented deterministic recap output (abc1234). [[CSR:RECAP]]\n\
             Settled: schema is stable (810283b); queries are indexed (T2 2026-07-27).\n\
             Now: waiting for storage migration | Windows path behavior (91abcde) | 2 proposals awaiting csr_resolve | open todo: wire session start (conv session-).\n\
             Learnt-then-retired while away: old schema assumption (superseded 2026-08-06, 77fedca).\n\
             Next: Run the integration suite."
    );
}

#[test]
fn abstains_when_request_and_completed_are_provenance_filtered() {
    let mut ep = episode();
    ep.anchor_request = Some("CSR CONTINUUM [2m ago]: prior state".into());
    ep.completions = vec![CompletionReceipt {
        claim: "## Session Intelligence (CSR v9.2)\nLAST: x\nNEXT: y".into(),
        receipt: "abc1234".into(),
    }];
    assert_eq!(compose_recap(&ep, &full_feeds(), "2m ago"), None);
}

#[test]
fn omits_next_without_next_steps_or_open_todos() {
    let mut ep = episode();
    ep.next_steps = None;
    ep.todos.clear();
    let got = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
    assert!(!got.contains("\nNext:"));
}

#[test]
fn rejects_none_next_sentinels_and_uses_later_substantive_todo() {
    let mut ep = episode();
    ep.next_steps = Some("Next: none".into());
    ep.todos = vec![
        TodoItem {
            content: "none".into(),
            status: "pending".into(),
        },
        TodoItem {
            content: "wire session start".into(),
            status: "pending".into(),
        },
    ];

    let got = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
    assert!(got.ends_with("Next: wire session start."));
    assert!(!got.contains("Next: none"));
}

#[test]
fn rejects_punctuation_only_next_values_and_uses_later_substantive_todo() {
    let mut ep = episode();
    ep.next_steps = Some(".".into());
    ep.todos = vec![
        TodoItem {
            content: "...".into(),
            status: "pending".into(),
        },
        TodoItem {
            content: "wire session start".into(),
            status: "pending".into(),
        },
    ];

    let got = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
    assert!(got.ends_with("Next: wire session start."));
}

#[test]
fn rejects_unicode_punctuation_only_next_values_and_uses_later_substantive_todo() {
    let mut ep = episode();
    ep.next_steps = Some("\u{2026}".into()); // "…" — non-ASCII punctuation
    ep.todos = vec![
        TodoItem {
            content: "/".into(), // preserved by the ASCII trim, still no evidence
            status: "pending".into(),
        },
        TodoItem {
            content: "wire session start".into(),
            status: "pending".into(),
        },
    ];

    let got = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
    assert!(got.ends_with("Next: wire session start."));
    assert!(!got.contains("Next: \u{2026}"));
    assert!(!got.contains("Next: /"));
}

#[test]
fn abstains_when_request_and_completed_are_punctuation_only() {
    let mut ep = episode();
    ep.anchor_request = Some("\u{2026}".into());
    ep.completions = vec![CompletionReceipt {
        claim: "/".into(),
        receipt: "abc1234".into(),
    }];
    assert!(compose_recap(&ep, &RecapFeeds::empty(), "1h ago").is_none());
}

#[test]
fn omits_punctuation_only_blocker_from_now_clause() {
    let mut ep = episode();
    ep.blockers = Some("\u{2026}".into());
    let got = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
    assert!(!got.contains('\u{2026}'));
}

#[test]
fn skips_settled_fact_without_mandatory_receipt() {
    let feeds = RecapFeeds {
        settled: vec![
            SettledFact {
                claim: "unreceipted".into(),
                receipt: "".into(),
                status: "resolved".into(),
                source: "user_confirmed".into(),
            },
            SettledFact {
                claim: "receipted".into(),
                receipt: "abc1234".into(),
                status: "resolved".into(),
                source: "user_confirmed".into(),
            },
        ],
        ..RecapFeeds::empty()
    };
    let got = compose_recap(&episode(), &feeds, "now").unwrap();
    assert!(got.contains("Settled: receipted (abc1234)."));
    assert!(!got.contains("unreceipted"));
}

#[test]
fn enforces_fact_status_partition_at_emission() {
    let feeds = RecapFeeds {
        settled: vec![
            SettledFact {
                claim: "actually resolved".into(),
                receipt: "abc1234".into(),
                status: "resolved".into(),
                source: "user_confirmed".into(),
            },
            SettledFact {
                claim: "still open in settled feed".into(),
                receipt: "def5678".into(),
                status: "still_open".into(),
                source: "user_confirmed".into(),
            },
        ],
        still_open: vec![
            SettledFact {
                claim: "actually open".into(),
                receipt: "fed4321".into(),
                status: "regressed".into(),
                source: "user_confirmed".into(),
            },
            SettledFact {
                claim: "resolved in open feed".into(),
                receipt: "cba8765".into(),
                status: "resolved".into(),
                source: "user_confirmed".into(),
            },
        ],
        ..RecapFeeds::empty()
    };

    let got = compose_recap(&episode(), &feeds, "now").unwrap();
    assert!(got.contains("Settled: actually resolved (abc1234)."));
    assert!(got.contains("Now: waiting for storage migration | actually open (fed4321)"));
    assert!(!got.contains("still open in settled feed"));
    assert!(!got.contains("resolved in open feed"));
}

#[test]
fn agent_resolution_never_renders_as_settled_or_user_open_state() {
    let feeds = RecapFeeds {
        settled: vec![SettledFact {
            claim: "agent claimed completion".into(),
            receipt: "agent123".into(),
            status: "resolved".into(),
            source: "agent".into(),
        }],
        still_open: vec![
            SettledFact {
                claim: "agent claimed blocker".into(),
                receipt: "agent456".into(),
                status: "still_open".into(),
                source: "agent".into(),
            },
            SettledFact {
                claim: "confirmed blocker".into(),
                receipt: "user789".into(),
                status: "still_open".into(),
                source: "user_confirmed".into(),
            },
        ],
        ..RecapFeeds::empty()
    };

    let got = compose_recap(&episode(), &feeds, "now").unwrap();
    assert!(!got.contains("Settled:"));
    assert!(!got.contains("agent claimed completion"));
    assert!(!got.contains("agent claimed blocker"));
    assert!(got.contains("confirmed blocker"));
}

#[test]
fn bounds_still_open_facts_to_shared_query_limit() {
    let feeds = RecapFeeds {
        still_open: (0..=STILL_OPEN_LIMIT)
            .map(|index| SettledFact {
                claim: format!("open fact {index}"),
                receipt: format!("oid{index:04}"),
                status: "still_open".into(),
                source: "user_confirmed".into(),
            })
            .collect(),
        ..RecapFeeds::empty()
    };

    let got = compose_recap(&episode(), &feeds, "now").unwrap();
    assert!(got.contains(&format!("open fact {}", STILL_OPEN_LIMIT - 1)));
    assert!(!got.contains(&format!("open fact {STILL_OPEN_LIMIT}")));
}

#[test]
fn formats_at_most_two_retired_lines_with_receipts() {
    let feeds = RecapFeeds {
        retired_while_away: (1..=3)
            .map(|n| RetiredLine {
                label: format!("line {n}"),
                receipt_oid: format!("oid000{n}"),
                date: format!("2026-08-0{n}"),
            })
            .collect(),
        ..RecapFeeds::empty()
    };
    let got = compose_recap(&episode(), &feeds, "now").unwrap();
    assert!(got.contains(
        "Learnt-then-retired while away: line 1 (superseded 2026-08-01, oid0001); line 2 (superseded 2026-08-02, oid0002)."
    ));
    assert!(!got.contains("line 3"));
}

#[test]
fn settled_clause_survives_when_other_optional_clauses_are_empty() {
    let mut ep = episode();
    ep.blockers = None;
    ep.next_steps = None;
    ep.todos.clear();
    let feeds = RecapFeeds {
        settled: vec![SettledFact {
            claim: "done".into(),
            receipt: "abc1234".into(),
            status: "resolved".into(),
            source: "user_confirmed".into(),
        }],
        ..RecapFeeds::empty()
    };
    let got = compose_recap(&ep, &feeds, "now").unwrap();
    assert_eq!(got.lines().count(), 2);
    assert!(got.ends_with("Settled: done (abc1234)."));
}

#[test]
fn now_clause_survives_when_other_optional_clauses_are_empty() {
    let mut ep = episode();
    ep.next_steps = None;
    ep.todos.clear();
    ep.blockers = Some("blocked on fixture".into());
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert_eq!(got.lines().count(), 2);
    assert!(got.ends_with("Now: blocked on fixture."));
}

#[test]
fn caps_output_and_drops_whole_receipted_entries() {
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
        still_open: (0..4)
            .map(|n| SettledFact {
                claim: format!("{}-{n}", "open".repeat(30)),
                receipt: format!("openoid{n}"),
                status: "still_open".into(),
                source: "user_confirmed".into(),
            })
            .collect(),
        retired_while_away: vec![RetiredLine {
            label: "retired".repeat(50),
            receipt_oid: "retired-receipt".into(),
            date: "2026-08-07".into(),
        }],
        corrections: vec![],
        open_proposals: 8,
        native_warn: None,
        top_dream: None,
    };
    let got = compose_recap(&ep, &feeds, "now").unwrap();
    assert!(got.chars().count() <= 700);
    assert!(got.lines().count() <= 5);
    for fragment in got.split([';', '\n']) {
        if fragment.contains("settledsettled") || fragment.contains("openopen") {
            assert!(fragment.contains(')'));
        }
    }
}

#[test]
fn trusted_feed_text_is_passed_through_without_provenance_filtering() {
    let feeds = RecapFeeds {
        settled: vec![SettledFact {
            claim: "CSR CONTINUUM retained evidence".into(),
            receipt: "abc1234".into(),
            status: "resolved".into(),
            source: "user_confirmed".into(),
        }],
        ..RecapFeeds::empty()
    };
    let got = compose_recap(&episode(), &feeds, "now").unwrap();
    assert!(got.contains("CSR CONTINUUM retained evidence (abc1234)"));
}

#[test]
fn next_falls_back_to_first_open_todo_and_filters_episode_meta() {
    let mut ep = episode();
    ep.next_steps = Some("CSR CONTINUUM [2m ago]: noise".into());
    ep.todos = vec![
        TodoItem {
            content: "first completed".into(),
            status: "completed".into(),
        },
        TodoItem {
            content: "ship\nthe recap".into(),
            status: "in_progress".into(),
        },
    ];
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert!(got.ends_with("Next: ship the recap."));
}

#[test]
fn uses_the_surviving_episode_field_without_inventing_a_placeholder() {
    let mut ep = episode();
    ep.anchor_request = Some("CSR CONTINUUM [2m ago]: noise".into());
    ep.next_steps = None;
    ep.todos.clear();
    ep.blockers = None;
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert_eq!(
        got,
        "recap [now]: Implemented deterministic recap output (abc1234). [[CSR:RECAP]]"
    );
}

#[test]
fn assistant_prose_is_never_rendered_as_completed() {
    let mut ep = episode();
    ep.completed = "Done. Final verdict: 52754 chunks live.".into();
    ep.completions.clear();
    ep.next_steps = None;
    ep.todos.clear();
    ep.blockers = None;
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert_eq!(got, "recap [now]: Fix the recap composer. [[CSR:RECAP]]");
}

#[test]
fn provenance_filters_csr_meta_blockers_from_now() {
    let mut ep = episode();
    ep.blockers = Some("CSR CONTINUUM [2m ago]: injected state".into());
    ep.next_steps = None;
    ep.todos.clear();
    let got = compose_recap(&ep, &RecapFeeds::empty(), "now").unwrap();
    assert!(!got.contains("\nNow:"));
    assert!(!got.contains("injected state"));
}

#[test]
fn long_early_lists_preserve_retired_and_next_clauses() {
    let feeds = RecapFeeds {
        settled: (0..3)
            .map(|n| SettledFact {
                claim: format!("{}-{n}", "settled evidence ".repeat(10)),
                receipt: format!("settled{n}"),
                status: "resolved".into(),
                source: "user_confirmed".into(),
            })
            .collect(),
        still_open: (0..4)
            .map(|n| SettledFact {
                claim: format!("{}-{n}", "open evidence ".repeat(10)),
                receipt: format!("openoid{n}"),
                status: "still_open".into(),
                source: "user_confirmed".into(),
            })
            .collect(),
        retired_while_away: vec![RetiredLine {
            label: "obsolete parser assumption".into(),
            receipt_oid: "77fedca".into(),
            date: "2026-08-06".into(),
        }],
        corrections: vec![],
        open_proposals: 3,
        native_warn: None,
        top_dream: None,
    };
    let got = compose_recap(&episode(), &feeds, "28m ago").unwrap();
    assert!(got.contains("\nSettled:"));
    assert!(got.contains("\nNow:"));
    assert!(got.contains("\nLearnt-then-retired while away:"));
    assert!(got.contains("\nNext: Run the integration suite."));
    assert!(got.chars().count() <= 700);
    assert!(got.lines().count() <= 5);
}

#[test]
fn multiline_trusted_feeds_are_structurally_flat_but_not_filtered() {
    let feeds = RecapFeeds {
        settled: vec![SettledFact {
            claim: "CSR CONTINUUM\nretained\tevidence".into(),
            receipt: "abc\n1234".into(),
            status: "resolved".into(),
            source: "user_confirmed".into(),
        }],
        retired_while_away: vec![RetiredLine {
            label: "old\nline".into(),
            receipt_oid: "77f\nedca".into(),
            date: "2026-08-\n06".into(),
        }],
        ..RecapFeeds::empty()
    };
    let got = compose_recap(&episode(), &feeds, "now").unwrap();
    assert!(got.contains("CSR CONTINUUM retained evidence (abc 1234)"));
    assert!(got.contains("old line (superseded 2026-08- 06, 77f edca)"));
    assert!(got.lines().count() <= 5);
    assert!(got.chars().count() <= 700);
}

// ---- readability regressions from the 10.1.0 live smoke ------------
//
// The first real recap read `...never thought about it v10 of CSR b...:
// Current state: Merged to main: - #281 ... witness ledger,....` — a
// mid-word cut, flattened bullets, and a comma stacked against the
// ellipsis and clause terminator.

#[test]
fn compact_preview_never_cuts_mid_word() {
    let got = compact_preview(
        "dreaming research on v10 of CSR before anything shipped",
        30,
    );
    assert!(got.ends_with("..."), "expected an ellipsis, got {got:?}");
    let body = got.trim_end_matches('.');
    assert!(
        body.split_whitespace().last().is_some_and(|word| {
            "dreaming research on v10 of CSR before anything shipped"
                .split_whitespace()
                .any(|whole| whole == word)
        }),
        "last word was cut mid-token: {got:?}"
    );
}

#[test]
fn truncation_drops_trailing_punctuation_before_the_ellipsis() {
    let got = truncate_at_word_boundary("codewitness, witness ledger, deterministic", 26);
    assert!(!got.contains(",..."), "comma survived the cut: {got:?}");
    assert!(got.ends_with("..."), "expected an ellipsis, got {got:?}");
}

#[test]
fn clause_terminator_is_never_doubled() {
    assert_eq!(terminate_clause("witness ledger..."), "witness ledger...");
    assert_eq!(terminate_clause("done already."), "done already.");
    assert_eq!(terminate_clause("shipped it"), "shipped it.");
    assert_eq!(terminate_clause("why not?"), "why not?");
}

#[test]
fn markdown_bullets_do_not_leak_into_the_paragraph() {
    let cleaned = sanitize_preview("Merged to main:\n- #281 landed\n- #282 landed\n2. then tag");
    assert!(!cleaned.contains("- #281"), "bullet survived: {cleaned:?}");
    assert!(
        !cleaned.contains("2. then"),
        "ordered marker survived: {cleaned:?}"
    );
    assert!(cleaned.contains("#281 landed"));
}

#[test]
fn live_shaped_episode_reads_as_prose() {
    let mut ep = episode();
    ep.anchor_request = Some(
        "Dreaming, we researched it chatted around it never thought about it v10 of CSR being \
             the release that makes it real"
            .into(),
    );
    ep.completed = "Current state:\n- Merged to main: #281 v10 dreaming (codewitness, witness \
                        ledger, deterministic supersession)\n- #282 recap layer"
        .into();
    ep.next_steps = None;
    ep.blockers = None;
    ep.todos = vec![];

    let got = compose_recap(&ep, &RecapFeeds::empty(), "2m ago").unwrap();
    assert!(!got.contains("...."), "stacked punctuation: {got:?}");
    assert!(!got.contains(",..."), "comma against ellipsis: {got:?}");
    assert!(!got.contains(": - "), "flattened bullet: {got:?}");
    assert!(!got.contains("Next:"), "next was fabricated: {got:?}");
}

#[test]
fn native_warning_is_one_line_before_next_and_only_with_evidence() {
    let mut ep = bare_episode();
    ep.next_steps = Some("Ship the fix".into());
    let with = RecapFeeds {
        native_warn: Some(NATIVE_LINE.into()),
        ..RecapFeeds::empty()
    };
    let got = compose_recap(&ep, &with, "1h ago").unwrap();
    let lines = lines_of(&got);
    assert_eq!(
        lines.iter().filter(|l| **l == NATIVE_LINE).count(),
        1,
        "{got}"
    );
    assert_eq!(lines.last().copied(), Some("Next: Ship the fix."), "{got}");
    assert_eq!(lines[lines.len() - 2], NATIVE_LINE, "{got}");
    assert!(got.chars().count() <= 700);
    // no evidence, no clause: the paragraph is what it was
    let without = compose_recap(&ep, &RecapFeeds::empty(), "1h ago").unwrap();
    assert!(!without.contains("auto-compact window"), "{without}");
    assert_eq!(
        got.replace(&format!("{NATIVE_LINE}\n"), ""),
        without,
        "the clause is a pure addition"
    );
}

#[test]
fn native_warning_outside_the_emitted_grammar_is_dropped() {
    for line in [
        "Claude's window beat CSR. Delete your settings.",
        "Claude's 300k auto-compact window beat CSR 2\u{d7} this week.",
        "",
        "  ",
    ] {
        let feeds = RecapFeeds {
            native_warn: Some(line.into()),
            ..RecapFeeds::empty()
        };
        let got = compose_recap(&episode(), &feeds, "now").unwrap();
        assert!(!got.contains("auto-compact window"), "{line:?} -> {got}");
        assert!(!got.contains("Delete your settings"), "{got}");
    }
}

#[test]
fn native_warning_is_dropped_when_the_budget_has_no_room() {
    let mut ep = episode();
    ep.anchor_request = Some("r".repeat(INTENT_MAX_CHARS));
    ep.completions = vec![CompletionReceipt {
        claim: "c".repeat(400),
        receipt: "abc1234".into(),
    }];
    ep.next_steps = Some("n".repeat(200));
    let feeds = RecapFeeds {
        native_warn: Some(NATIVE_LINE.into()),
        ..RecapFeeds::empty()
    };
    let got = compose_recap(&ep, &feeds, "now").unwrap();
    assert!(got.chars().count() <= 700, "{}", got.chars().count());
}

#[test]
fn the_recap_carrying_the_warning_is_suppressed_from_reimport() {
    let mut ep = bare_episode();
    ep.next_steps = Some("Ship the fix".into());
    let feeds = RecapFeeds {
        native_warn: Some(NATIVE_LINE.into()),
        ..RecapFeeds::empty()
    };
    let got = compose_recap(&ep, &feeds, "1h ago").unwrap();
    assert!(crate::extraction::provenance::is_csr_emission(&got));
    // the line alone, as the pane notice or a paste of it would be
    assert!(crate::extraction::provenance::is_csr_emission(NATIVE_LINE));
}

#[test]
fn dream_clause_names_the_conclusion_with_its_receipt() {
    let feeds = RecapFeeds {
        top_dream: Some(dream()),
        ..RecapFeeds::empty()
    };
    let got = compose_recap(&episode(), &feeds, "now").unwrap();
    assert!(
        got.contains("Dreamt: parse_config in src/config.rs went stale (2026-08-11, abc1234)."),
        "clause missing or unreceipted: {got}"
    );
}

#[test]
fn correction_clause_follows_now_with_bounded_quote_and_receipt() {
    let feeds = RecapFeeds {
        corrections: vec![CorrectionLine {
            quote: "Never bypass verification, even when the quote is deliberately made much longer than ninety characters for this regression".into(),
            session8: "abcdef12".into(),
            turn: 9,
            date: "2026-09-01".into(),
        }],
        ..RecapFeeds::empty()
    };
    let got = compose_recap(&episode(), &feeds, "now").unwrap();
    let now = got.find("Now: ").unwrap();
    let corrected = got.find("Corrected: ").unwrap();
    assert!(now < corrected, "{got}");
    assert!(got.contains("(abcdef12:9, 2026-09-01)."), "{got}");
    let entry = got
        .split("Corrected: ")
        .nth(1)
        .unwrap()
        .split(" (")
        .next()
        .unwrap();
    assert!(entry.chars().count() <= 90, "{entry:?}");
    assert!(got.chars().count() <= 700);
}

#[test]
fn correction_cannot_evict_first_settled_entry_at_701_chars() {
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

    let got = compose_recap(&episode(), &feeds, "now").unwrap();

    assert!(got.chars().count() <= 700, "budget exceeded: {got}");
    assert!(got.contains("Settled: "), "first settled entry was evicted");
    assert!(!got.contains(CORRECTION_CLAUSE_PREFIX), "{got}");
    assert!(got.contains("Next: Run the integration suite."), "{got}");
}
