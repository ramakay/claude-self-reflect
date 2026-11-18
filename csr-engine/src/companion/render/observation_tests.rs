use super::*;
use serde_json::json;

fn at(card: ObsCard, index: usize) -> PopupAt {
    PopupAt {
        card,
        index,
        copied: false,
        note: None,
        scroll: 0,
    }
}

/// The 11 real confusing rows of one session (line and evidence, newest first): rows 0-2 tell the
/// "4 GB looked reachable" event, rows 3-10 the "handoff draft import guard" one.
fn real_rows() -> Vec<Value> {
    let looks: Vec<Value> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/companion/confused_b046750d.json"
    ))
    .unwrap();
    looks
        .iter()
        .flat_map(|look| look["confusing_claude"].as_array().unwrap().clone())
        .collect()
}

#[test]
fn the_threshold_sits_in_the_gap_between_real_events_and_real_repeats() {
    let rows = real_rows();
    assert_eq!(rows.len(), 11);
    let keys: Vec<EventKey> = rows.iter().map(event_key).collect();
    let event = |i: usize| usize::from(i >= 3);
    let (mut across_max, mut within_min_link) = (0_f64, 1_f64);
    for i in 0..keys.len() {
        for j in (i + 1)..keys.len() {
            let score = overlap(&keys[i], &keys[j]);
            println!(
                "pair {i}-{j} overlap {score:.2} same_event {}",
                same_event(&keys[i], &keys[j])
            );
            if event(i) != event(j) {
                across_max = across_max.max(score);
                assert!(!same_event(&keys[i], &keys[j]), "{i}-{j} merged: {score}");
            }
        }
    }
    // Each row's best link to an earlier row of its own event, the weakest of those.
    for j in 1..keys.len() {
        let best = (0..j)
            .filter(|i| event(*i) == event(j))
            .map(|i| overlap(&keys[i], &keys[j]))
            .fold(None, |m: Option<f64>, s| Some(m.map_or(s, |m| m.max(s))));
        if let Some(best) = best {
            within_min_link = within_min_link.min(best);
        }
    }
    println!(
        "different events: at most {across_max:.2}; weakest same-event link: {within_min_link:.2}"
    );
    assert!(across_max < SAME_EVENT_OVERLAP && SAME_EVENT_OVERLAP <= within_min_link);
    // The pair the owner saw as the two evidence-only rows (3 and 4) is the weakest link.
    assert!(same_event(&keys[3], &keys[4]));
}

#[test]
fn two_events_on_the_same_turns_share_no_words_from_the_citation() {
    let row = |line: &str, evidence: &str| json!({"line": line, "evidence": evidence});
    let long = event_key(&row(
        "Claude said the migration had already run, then took it back.",
        "Turns 4 and 13: 'the migration already ran'",
    ));
    let other = event_key(&row(
        "Claude renamed the exported config key without updating its callers.",
        "Turns 4 and 13: 'renamed the key'",
    ));
    // Short lines on the same turns: only the citation is common to them.
    let short_a = event_key(&row(
        "Dropped the retry flag.",
        "Turns 12 and 15: 'no retry'",
    ));
    let short_b = event_key(&row(
        "Wrote to the wrong branch.",
        "Turns 12 and 15: 'wrong branch'",
    ));
    println!(
        "same turns, different events: {:.2} and {:.2}",
        overlap(&long, &other),
        overlap(&short_a, &short_b)
    );
    assert!(!same_event(&long, &other));
    assert!(!same_event(&short_a, &short_b));
    assert!(overlap(&long, &other) < 0.2);
    assert!(!short_a.words.contains("turn") && !short_a.words.contains("12"));
    // The same event on the same turns, worded anew, still merges.
    let again = event_key(&row(
        "Claude told the person the migration already ran, then retracted that.",
        "Turns 4 and 13: 'the migration already ran' and later 'it had not run'",
    ));
    println!("same turns, same event: {:.2}", overlap(&long, &again));
    assert!(same_event(&long, &again));
}

#[test]
fn rows_that_cite_different_turns_or_say_too_little_are_never_one_event() {
    let row = |line: &str, evidence: &str| json!({"line": line, "evidence": evidence});
    let said = "Claude told the person the migration had already run, then took it back.";
    let a = event_key(&row(said, "Turn 4: 'the migration already ran'"));
    let same_turn = event_key(&row(said, "Turn 4: 'the migration already ran'"));
    let other_turn = event_key(&row(said, "Turn 9: 'the migration already ran'"));
    assert!(same_event(&a, &same_turn));
    assert!(!same_event(&a, &other_turn));
    // Digits inside a quote are not turns.
    assert!(cited_turns("\"4 GB looks reachable with 13 sessions\"").is_empty());
    assert_eq!(cited_turns("Turns 4 and 13: 'x'").len(), 2);
    let thin = event_key(&row("Line 0", "Turn 1"));
    assert!(!same_event(&thin, &event_key(&row("Line 1", "Turn 1"))));
}

#[test]
fn a_word_wider_than_the_popup_breaks_between_letters_and_loses_nothing() {
    let f = Fonts::bundled();
    let st = sans(theme::BODY_SIZE, theme::REGULAR);
    let blob = format!(
        "Signature: {{\"completion_status\":\"{}\"}} done",
        "partial".repeat(30)
    );
    let lines = lines_of(&f, &blob, st, 300., usize::MAX);
    assert!(lines.len() > 3, "{lines:?}");
    assert!(lines.iter().all(|l| f.width(l, st) <= 300.), "{lines:?}");
    let joined: String = lines.concat();
    assert!(
        joined.replace(' ', "").contains(&"partial".repeat(30)),
        "{joined}"
    );
}

#[test]
fn an_old_confusing_item_gets_a_composed_explanation_and_next_step() {
    let items = vec![
        json!({"line":"Repeated the rejected approach.","evidence":"Turns 12 and 15: 'keep it local'"}),
    ];
    let d = detail(&items, at(ObsCard::Confusing, 0)).unwrap();
    assert_eq!(d.line, "Repeated the rejected approach.");
    assert_eq!(d.place, (1, 1));
    assert_eq!(d.kind, None);
    assert!(d.explain.contains("misread you"), "{}", d.explain);
    assert!(d.explain.contains("turns 12, 15"), "{}", d.explain);
    assert!(d.next.starts_with("Re-read turns 12, 15"), "{}", d.next);
    // The copy carries the item and its explanation.
    for want in [
        "What confused the agent: Repeated the rejected approach.",
        "Evidence: Turns 12 and 15: 'keep it local'",
        "What happened: The dreamer flagged",
        "Next step: Re-read turns 12, 15",
    ] {
        assert!(d.copy.contains(want), "{}", d.copy);
    }
}

#[test]
fn a_quote_without_turn_numbers_is_cited_as_its_quote() {
    let items = vec![json!({"line":"Misread the target.","evidence":"'the pane, not the page'"})];
    let d = detail(&items, at(ObsCard::Confusing, 0)).unwrap();
    assert!(d.explain.contains("its quoted evidence"), "{}", d.explain);
    assert!(
        d.next.starts_with("Re-read the quoted evidence"),
        "{}",
        d.next
    );
}

#[test]
fn the_dreamers_own_explanation_and_next_step_win_over_the_composed_ones() {
    let items = vec![
        json!({"line":"L","evidence":"Turn 3","explain":"It misread you.","next":"Say it twice."}),
    ];
    let d = detail(&items, at(ObsCard::Confusing, 0)).unwrap();
    assert_eq!(
        (d.explain.as_str(), d.next.as_str()),
        ("It misread you.", "Say it twice.")
    );
    assert!(d.copy.contains("What happened: It misread you."));
    // A blank field is the same as a missing one.
    let items = vec![json!({"line":"L","evidence":"Turn 3","explain":"  ","next":""})];
    let d = detail(&items, at(ObsCard::Confusing, 0)).unwrap();
    assert!(d.explain.starts_with("The dreamer flagged"));
}

#[test]
fn each_should_be_kind_has_its_own_next_step_and_a_draft_to_paste() {
    let item = |kind: &str| json!({"kind":kind,"line":"Run fmt before each commit.","evidence":"Turns 4 and 12: 'run fmt'"});
    for (kind, next, draft_start) in [
        (
            "skill",
            "SKILL.md",
            "---\nname: run-fmt-before-each-commit\n",
        ),
        (
            "memory",
            "remember it",
            "---\nname: run-fmt-before-each-commit\n",
        ),
        ("hook", "hook to your settings", "# Hook idea: Run fmt"),
        ("CLAUDE.md", "CLAUDE.md", "- Run fmt before each commit."),
        ("tool", "small script", "Tool idea: Run fmt"),
    ] {
        let items = vec![item(kind)];
        let d = detail(&items, at(ObsCard::ShouldBe, 0)).unwrap();
        assert!(d.next.contains(next), "{kind}: {}", d.next);
        assert!(d.copy.starts_with(draft_start), "{kind}: {}", d.copy);
        assert!(
            d.copy.contains("turns 4, 12") || d.copy.contains("Turns 4 and 12"),
            "{kind}: {}",
            d.copy
        );
        assert!(
            d.explain.contains("more than once"),
            "{kind}: {}",
            d.explain
        );
    }
    let items = vec![item("skill")];
    let d = detail(&items, at(ObsCard::ShouldBe, 0)).unwrap();
    assert_eq!(d.kind, Some("skill"));
    assert!(d.explain.contains("belongs in a skill"), "{}", d.explain);
    assert!(d.copy.contains("description: Run fmt before each commit."));
    assert!(d.copy.contains("Why: You asked for this more than once"));
}

#[test]
fn a_missing_item_has_no_detail() {
    assert!(detail(&[], at(ObsCard::Confusing, 0)).is_none());
    let items = vec![json!({"line":"L","evidence":"Turn 1"})];
    assert!(detail(&items, at(ObsCard::Confusing, 1)).is_none());
}

#[test]
fn the_text_rows_carry_every_part_and_the_copied_mark() {
    let items = vec![json!({"kind":"hook","line":"Check formatting.","evidence":"Turn 4: 'fmt'"})];
    let mut d = detail(&items, at(ObsCard::ShouldBe, 0)).unwrap();
    let rows = rows(&d, 60);
    assert_eq!(rows[0], "Should be a skill / memory / tool · 1 of 1 · hook");
    assert_eq!(rows[1], "Check formatting.");
    assert!(rows.iter().any(|r| r.starts_with("Evidence: Turn 4")));
    assert!(rows.iter().any(|r| r.starts_with("What happened: ")));
    assert!(rows.iter().any(|r| r.starts_with("Next step for you: ")));
    assert!(!rows.iter().any(|r| r == "Copied."));
    d.copied = true;
    assert_eq!(
        super::rows(&d, 60).last().map(String::as_str),
        Some("Copied.")
    );
    assert_eq!(footer_keys(Some(&d))[0], ("c", "copy draft"));
    assert_eq!(footer_keys(None)[0], ("c", "copy"));
}

#[test]
fn the_text_popup_breaks_a_word_wider_than_the_pane_and_loses_nothing() {
    let blob = format!("{{\"status\":\"{}\"}}", "partial".repeat(12));
    let items = vec![json!({"line": format!("Story {blob} end"), "evidence": ""})];
    let d = detail(&items, at(ObsCard::Confusing, 0)).unwrap();
    let rows = rows(&d, 30);
    assert!(
        rows.iter()
            .all(|r| crate::companion::render::text::len(r) <= 30),
        "{rows:?}"
    );
    assert!(rows.concat().contains(&"partial".repeat(12)), "{rows:?}");
}

#[test]
fn a_dismissed_item_is_kept_under_its_session_card_and_line() {
    let key = dismiss_key("sess-1", "confusing_claude", "Misread the pane.");
    assert!(
        key.starts_with("observation:sess-1:confusing_claude:"),
        "{key}"
    );
    assert_eq!(key.len(), "observation:sess-1:confusing_claude:".len() + 16);
    assert_eq!(
        key,
        dismiss_key("sess-1", "confusing_claude", "Misread the pane.")
    );
    assert_ne!(
        key,
        dismiss_key("sess-2", "confusing_claude", "Misread the pane.")
    );
    assert_ne!(key, dismiss_key("sess-1", "should_be", "Misread the pane."));
    assert_ne!(
        key,
        dismiss_key("sess-1", "confusing_claude", "Misread the page.")
    );
}

#[test]
fn what_s_sends_quotes_csrs_words_and_ends_with_yours() {
    let items = vec![json!({"line":"Said it passed.","evidence":"Turn 3: 'it should pass'"})];
    let d = detail(&items, at(ObsCard::Confusing, 0)).unwrap();
    assert_eq!(
        d.send,
        "Here is a note offered by CSR (claude-self-reflect):\n\n> What confused the agent: Said it passed.\n> Evidence: Turn 3: 'it should pass'\n\nYou may have gone off course here. Please say what you got wrong and correct course."
    );
    let items = vec![json!({"kind":"CLAUDE.md","line":"Run fmt.","evidence":"Turn 4: 'fmt'"})];
    let d = detail(&items, at(ObsCard::ShouldBe, 0)).unwrap();
    assert!(
        d.send.starts_with(
            "Here is a note offered by CSR (claude-self-reflect):\n\n> CLAUDE.md rule: Run fmt.\n"
        ),
        "{}",
        d.send
    );
    assert!(
        d.send
            .contains("\n> Draft:\n> - Run fmt.\n>   <!-- seen: Turn 4: 'fmt' -->\n"),
        "{}",
        d.send
    );
    assert!(
        d.send.ends_with(
            "\n\nPlease make this CLAUDE.md rule from the draft above, and show me what you wrote."
        ),
        "{}",
        d.send
    );
    // The import reads the quote as CSR's and keeps only the ask as the person's words.
    assert_eq!(
        crate::extraction::provenance::strip_forwarded_note(&d.send).as_deref(),
        Some("Please make this CLAUDE.md rule from the draft above, and show me what you wrote.")
    );
    // No quoted line is empty: a blank draft line is a bare `>`, which a quote keeps whole.
    assert!(d.send.lines().all(|l| l != "> "), "{}", d.send);
    // A shortened row is only read: nothing to send.
    let more = vec![json!({"title":"Where the work stands","line":"All of it."})];
    assert_eq!(detail(&more, at(ObsCard::More, 0)).unwrap().send, "");
}

#[test]
fn the_footer_offers_send_and_dismiss_only_where_there_is_something_to_act_on() {
    let items = vec![json!({"line":"L","evidence":"Turn 1"})];
    let keys = |card| footer_keys(detail(&items, at(card, 0)).as_ref());
    assert_eq!(
        keys(ObsCard::Confusing),
        [
            ("c", "copy"),
            ("s", "send"),
            ("x", "dismiss"),
            ("tab", "next"),
            ("esc", "close")
        ]
    );
    assert_eq!(keys(ObsCard::ShouldBe)[0], ("c", "copy draft"));
    assert_eq!(keys(ObsCard::ShouldBe)[1], ("s", "send"));
    assert_eq!(keys(ObsCard::More), [("c", "copy"), ("esc", "close")]);
}

#[test]
fn a_note_is_a_row_of_the_text_popup() {
    let items = vec![json!({"line":"L","evidence":"Turn 1"})];
    let mut a = at(ObsCard::Confusing, 0);
    a.note = Some(("No Claude pane to type into.", false));
    let d = detail(&items, a).unwrap();
    assert_eq!(
        rows(&d, 60).last().map(String::as_str),
        Some("No Claude pane to type into.")
    );
}

fn lost_item(handoff: Option<Value>) -> Value {
    let mut item = json!({
        "key": "k1", "text": "Retry the upload once the hub is back", "line": "Retry the upload once the hub is back",
        "state": "lost", "times": 1, "last_seen": 1_761_990_000.,
        "origin_sid": "3002ab28-7ba1-4c61-9d2e-0a1b2c3d4e5f",
    });
    match handoff {
        Some(facts) => item["handoff"] = facts,
        None => item["reading"] = json!(true),
    }
    item
}

#[test]
fn a_lost_thread_copies_and_types_one_handoff_prompt() {
    let facts = json!({"conv": "3002ab28-7ba1-4c61-9d2e-0a1b2c3d4e5f", "project": "p",
                       "path": null, "path_exists": false, "db": "/d/csr-engine.db",
                       "mirror": null, "summary": null, "stats": null, "stats_note": ""});
    let items = vec![lost_item(Some(facts))];
    let d = detail(&items, at(ObsCard::Lost, 0)).unwrap();
    assert!(!d.copy.is_empty());
    assert_eq!(d.copy, d.send, "c and s hand over the same prompt");
    assert!(
        d.copy.contains("csr_transcript(session=\"3002ab28-"),
        "{}",
        d.copy
    );
    assert_eq!(d.heading, "Lost and found");
    assert_eq!(
        footer_keys(Some(&d)),
        [
            ("c", "copy draft"),
            ("s", "enter in Claude Code"),
            ("tab", "next"),
            ("esc", "close")
        ]
    );
    let rows = rows(&d, 60);
    assert_eq!(rows[0], "Lost and found · 1 of 1");
    // The popup is the thread: the session's plumbing stays in the draft.
    for gone in ["CSR's summary", "The session", "Where CSR keeps it"] {
        assert!(!rows.iter().any(|r| r == gone), "{gone}: {rows:?}");
    }
}

#[test]
fn a_lost_thread_still_being_read_hands_over_nothing() {
    let items = vec![lost_item(None)];
    let d = detail(&items, at(ObsCard::Lost, 0)).unwrap();
    assert_eq!((d.copy.as_str(), d.send.as_str()), ("", ""));
    assert!(d.lost.as_ref().unwrap().reading);
}

#[test]
fn a_shared_sentence_shape_with_different_quotes_is_two_events() {
    let row = |line: &str, evidence: &str| {
        event_key(&serde_json::json!({ "line": line, "evidence": evidence }))
    };
    let test = row(
        "Claude ran cargo test without waiting for approval.",
        "Claude: 'cargo test'",
    );
    let push = row(
        "Claude ran git push without waiting for approval.",
        "Claude: 'git push'",
    );
    assert!(overlap(&test, &push) >= SAME_EVENT_OVERLAP);
    assert!(!same_event(&test, &push));
}

#[test]
fn slugs_are_short_and_file_safe() {
    assert_eq!(
        slug("Run fmt — before EACH commit, please! ok"),
        "run-fmt-before-each-commit"
    );
    assert_eq!(slug("???"), "untitled");
}

// ---- the popup's type: a hierarchy, one line to a row, every key on the panel

const LONG: &str = "Fresh Studio trees have no pre-commit hook, so workers must copy it from the main \
        repo before committing, and a commit on a tree without the hook would skip the quality gate \
        unnoticed until a reviewer found it. A second sentence follows it, to be its own item.";

fn popup_detail(card: ObsCard) -> Detail {
    use crate::companion::handoff::{Section, View};
    let lost = (card == ObsCard::Lost).then(|| View {
        title: LONG.into(),
        kicker: "Quiet since 10-18 10:37 · carried through 3 compactions".into(),
        sections: vec![
            Section {
                heading: "CSR's summary",
                lines: vec![
                    format!("Asked: {LONG}"),
                    format!("Done before it stopped: {LONG}"),
                    format!("Outcome: {LONG}"),
                ],
            },
            Section {
                heading: "The session",
                lines: vec![
                    "27 turns (12 yours, 12 Claude's) over 41 min".into(),
                    "From 2026-10-18 09:58 to 2026-10-18 10:39".into(),
                    "5 tool calls (0 failed), 2 files touched, 3 compactions".into(),
                    "Shape: mixed (Tools 0%, Context 36%, Other 64%)".into(),
                ],
            },
            Section {
                heading: "Where CSR keeps it",
                lines: vec![
                    "Conversation: 0123456789abcdef-0123-4567-89ab-cdef01234567 (project folder -Users-someone-projects-a-long-name)".into(),
                    "Transcript: /Users/someone/.claude/projects/-Users-someone-projects-a-long-name/0123456789abcdef-0123-4567-89ab-cdef01234567.jsonl".into(),
                    "Mirror: /Users/someone/.claude-self-reflect/mirror/0123456789abcdef.jsonl.zst (12 KB from 40 KB of transcript, 12 frames, sealed, updated 2026-10-18)".into(),
                    "Database: /Users/someone/.claude-self-reflect/csr-engine.db (conv_0123 chunks, episodes and summary)".into(),
                ],
            },
        ],
        reading: false,
        mix: Some(([0., 0.36, 0.64], "mixed")),
    });
    Detail {
        same_page: false,
        card,
        heading: if card == ObsCard::More {
            "Where the work stands".into()
        } else {
            card.title().into()
        },
        place: (1, 4),
        kind: (card == ObsCard::ShouldBe).then_some("memory"),
        line: LONG.into(),
        evidence: format!("Turns 2, 3: \"{LONG}\""),
        explain: LONG.into(),
        next: LONG.into(),
        copy: String::new(),
        copied: true,
        note: Some(("Claude is not at its prompt. Nothing was typed.", false)),
        send: String::new(),
        lost,
        handed: None,
    }
}

type Runs = Vec<(f32, f32, f32, f32, String)>;
fn drawn(d: &Detail, height: f32) -> (Runs, Vec<KeyBox>) {
    let f = Fonts::bundled();
    let mut canvas = Canvas::new(W, height);
    let keys = overlay(&mut canvas, &f, d, height, height, &theme::DARK);
    (canvas.runs.clone(), keys)
}

#[test]
fn the_popup_sizes_are_a_hierarchy_a_third_up_and_the_keys_are_the_biggest_step() {
    let sizes = [
        POP_TITLE_SIZE,
        POP_CTA_SIZE,
        POP_BODY_SIZE,
        POP_CAPTION_SIZE,
    ];
    // title > action key > body > caption, no two levels nearly the same size
    for pair in sizes.windows(2) {
        assert!(pair[0] / pair[1] >= 1.1, "{sizes:?}");
    }
    // every level is 30-40% up from before, and the action key at least 40%
    for (now, was) in [
        (POP_TITLE_SIZE, WAS_TITLE_SIZE),
        (POP_BODY_SIZE, WAS_BODY_SIZE),
        (POP_CAPTION_SIZE, WAS_CAPTION_SIZE),
    ] {
        let ratio = now / was;
        assert!((1.299..=1.401).contains(&ratio), "{now} / {was} = {ratio}");
    }
    const { assert!(POP_CTA_SIZE / WAS_CTA_SIZE >= 1.4) };
    // the rows' scale (dreamer, lists, Context card) is the same third up, in the same order
    let rows = [
        (theme::ROW_TITLE_SIZE, theme::TITLE_SIZE),
        (theme::ROW_BODY_SIZE, theme::BODY_SIZE),
        (theme::ROW_CAPTION_SIZE, theme::CAPTION_SIZE),
        (theme::ROW_CHIP_SIZE, theme::CHIP_SIZE),
    ];
    for (now, was) in rows {
        assert!((1.299..=1.401).contains(&(now / was)), "{now} / {was}");
    }
    const { assert!(theme::ROW_TITLE_SIZE > theme::ROW_BODY_SIZE) };
    const { assert!(theme::ROW_BODY_SIZE > theme::ROW_CAPTION_SIZE) };
}

#[test]
fn the_action_keys_fit_the_panels_width_with_every_label_whole() {
    let f = Fonts::bundled();
    for card in [
        ObsCard::Confusing,
        ObsCard::ShouldBe,
        ObsCard::More,
        ObsCard::Lost,
    ] {
        let d = popup_detail(card);
        let width = cta_row_width(&f, &d, &theme::DARK);
        assert!(width <= TEXT_W, "{card:?}: {width} pt in {TEXT_W} pt");
    }
}

#[test]
fn a_path_too_wide_for_its_line_keeps_its_file_name() {
    let f = Fonts::bundled();
    let st = sans(POP_CAPTION_SIZE, theme::REGULAR);
    let line = "Transcript: /Users/someone/.claude/projects/-Users-someone-projects/0123456789abcdef-0123-4567.jsonl";
    let cut = one_line(&f, line, st, 300.);
    assert!(f.width(&cut, st) <= 300., "{cut}");
    assert!(cut.starts_with("Transcript: /"), "{cut}");
    assert!(cut.ends_with("4567.jsonl"), "{cut}");
}

#[test]
fn every_popup_row_is_one_line_inside_the_panel_and_every_key_is_on_it() {
    for height in [800., 887., 1052., 1118.] {
        for card in [
            ObsCard::Confusing,
            ObsCard::ShouldBe,
            ObsCard::More,
            ObsCard::Lost,
        ] {
            let d = popup_detail(card);
            let (runs, keys) = drawn(&d, height);
            let what = format!("{card:?} at {height} pt");
            // The panel's text column and the pane's height bound every run.
            for (l, t, r, b, w) in &runs {
                assert!(*l >= TEXT_X - 0.5, "{what}: {w:?} starts at {l}");
                assert!(*r <= TEXT_X + TEXT_W + 0.5, "{what}: {w:?} ends at {r}");
                assert!(
                    *t >= M - 0.5 && *b <= height - M + 0.5,
                    "{what}: {w:?} {t}-{b}"
                );
            }
            // No two runs share any area, and a row is one run on one line: no words repeat
            // on a second line of the same run (the "In full" popup's words are its one wrap).
            for i in 0..runs.len() {
                for j in i + 1..runs.len() {
                    let (a, b) = (&runs[i], &runs[j]);
                    let (w, h) = (a.2.min(b.2) - a.0.max(b.0), a.3.min(b.3) - a.1.max(b.1));
                    assert!(
                        !(w > 0.5 && h > 0.5),
                        "{what}: {:?} overlaps {:?}",
                        a.4,
                        b.4
                    );
                }
            }
            // Every key is whole: its label is drawn as one run, and its pill is on the panel.
            let want = cta_keys(Some(&d));
            assert_eq!(keys.len(), want.len(), "{what}");
            for ((key, label), k) in want.iter().zip(&keys) {
                assert_eq!(k.0, *key, "{what}");
                assert!(runs.iter().any(|r| r.4 == *label), "{what}: {label:?} cut");
                assert!(
                    k.1 >= TEXT_X - 0.5 && k.2 <= TEXT_X + TEXT_W + 0.5,
                    "{what}: {k:?}"
                );
                assert!(k.3 >= M && k.4 <= height - M, "{what}: {k:?}");
                assert!(
                    k.4 - k.3 >= POP_CTA_SIZE,
                    "{what}: {k:?} is shorter than its words"
                );
            }
            // Title above CTA above body above caption, by the sizes the runs were drawn at.
            let size = |w: &str| {
                runs.iter()
                    .find(|r| r.4 == w)
                    .map(|r| (r.3 - r.1) / 0.92)
                    .unwrap_or_else(|| panic!("{what}: no run {w:?}"))
            };
            assert!(
                (size(&d.heading.clone()) - POP_TITLE_SIZE).abs() < 0.05,
                "{what}"
            );
            assert!((size(want[0].1) - POP_CTA_SIZE).abs() < 0.05, "{what}");
        }
    }
}
