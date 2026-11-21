use super::*;

#[tokio::test]
async fn a_compaction_carries_the_dream_that_is_up_once_marked_still_open_the_next_one() {
    let r = rig(replies(&[dream_reply()]));
    let cx = r.cx();
    r.go(12).await;
    assert!(r.band_has(NOTES_READ).await);
    r.compact(&cx, "manual").await;
    let view = r.row(SID, "view");
    assert_eq!(view["carried"], json!(true));
    assert!(r.band_has("still open from before the compaction").await);
    assert!(r.band().await.has(NOTES_READ));
    let carried = r.with("answer", "answer", "carried");
    assert_eq!(carried.len(), 1, "{:?}", r.records());
    let chapter = r.row(SID, "chapter");
    assert_eq!(
        (chapter["shown"].clone(), chapter["steps"].clone()),
        (json!(0), json!(0))
    );
    // Two prompts again to answer it.
    assert_eq!(view["promptsSurvived"], json!(0));
    r.prompt(&cx, "one", "human").await;
    assert!(r.band().await.has(NOTES_READ));
    r.compact(&cx, "manual").await;
    assert!(!r.band().await.has(NOTES_READ));
    let answers: Vec<Value> = r
        .of_kind("answer")
        .into_iter()
        .map(|record| record["answer"].clone())
        .collect();
    assert_eq!(answers, [json!("carried"), json!("expired")]);
}

#[tokio::test]
async fn a_dream_typed_past_is_never_asked_again_and_the_dreamer_backs_off() {
    let r = rig(replies(&[dream_reply(), dream_reply(), dream_reply()]));
    let cx = r.cx();
    r.go(12).await;
    assert!(r.band_has(NOTES_READ).await);
    r.prompt(&cx, "one", "human").await;
    assert!(
        r.band().await.has(NOTES_READ),
        "two prompts again to answer it"
    );
    r.prompt(&cx, "two", "human").await;
    assert!(!r.band().await.has(NOTES_READ));
    assert_eq!(r.row(SID, "looksToSkip"), json!(1));
    let answers = r.row(MACHINE, "answers");
    assert_eq!(
        answers.as_array().unwrap().last().unwrap()["answer"],
        "ignored"
    );

    r.out.set_brief(&format!("{BRIEF}\n{NEWS}"));
    r.go(27).await; // step 39, when the next look is due: skipped, backing off
    quiesce().await;
    assert_eq!(r.forks().len(), 1);
    let last = r.looks().pop().unwrap();
    assert_eq!(last["outcome"], "skip");

    r.go(20).await; // step 59: looks again, but the same dream is not shown twice
    assert!(until(|| r.forks().len() >= 2).await);
    quiesce().await;
    assert_eq!(r.forks().len(), 2);
    assert!(
        r.forks()[1].contains(DREAM_LINE),
        "the seen list rides in the prompt"
    );
    assert!(!r.band().await.has(NOTES_READ));
}

#[tokio::test]
async fn a_dream_about_the_pane_that_cites_no_other_session_still_reaches_you_you_judge() {
    // The one dream acted on (10-09, asked Claude): about the pane, no receipt.
    let pigment = [
        "dream: What if the pane took its colours from pigment's nyt inks, the palette your \
         10.x deck already uses?",
        "explain:",
        "**The pane could share the deck's palette**",
        "* We're tuning the pane's colours by hand.",
    ]
    .join("\n");
    let r = rig(replies(&[pigment]));
    r.go(12).await;
    assert!(r.band_has("pigment's nyt inks").await);
    assert_eq!(r.dreams().len(), 1);
    assert!(r.find(|record| record["outcome"] == "ungrounded").is_none());
}

#[tokio::test]
async fn a_dream_answered_in_the_pane_leaves_the_band_and_is_never_counted_as_typed_past() {
    let r = rig(replies(&[dream_reply()]));
    let cx = r.cx();
    r.go(12).await;
    assert!(r.band_has(NOTES_READ).await);
    let id = r.dreams()[0]["id"].as_str().unwrap().to_string();
    let said = json!({ "kind": "dream", "id": id, "line": DREAM_LINE });
    let answer = json!({ "kind": "answer", "id": id, "answer": "helpful", "at": 1, "by": "pane" });
    r.out.set_log(&format!("{said}\n{answer}\n"));
    r.prompt(&cx, "one", "human").await;
    assert!(!r.band().await.has(NOTES_READ));
    assert_eq!(r.of_kind("answer").len(), 0, "the pane logged it, once");
    assert_eq!(r.row(SID, "looksToSkip").as_i64().unwrap_or(0), 0);
    let answers = r.row(MACHINE, "answers");
    let rows = answers.as_array().unwrap();
    assert!(rows
        .iter()
        .any(|row| row["id"] == id && row["answer"] == "helpful"));
    assert!(rows.iter().all(|row| row["answer"] != "ignored"));
}

#[tokio::test]
async fn a_dream_about_csr_s_own_dream_cycle_still_reaches_you_only_the_pane_and_the() {
    let work = dream_reply().replace(
        "What if the notes also read",
        "What if the dream cycle also read",
    );
    let r = rig(replies(&[work]));
    r.go(12).await;
    assert!(r.band_has("What if the dream cycle also read").await);
    assert_eq!(r.dreams().len(), 1);
}

#[tokio::test]
async fn the_dim_4_on_a_dream_turns_dreams_off_after_a_second_press() {
    let r = rig(replies(&[dream_reply()]));
    r.go(12).await;
    assert!(r.band_has(NOTES_READ).await);

    assert!(r.press("Disable").await);
    assert!(r.fx.host.seen.lock().unwrap().configured.is_empty());
    assert!(r.band().await.has_button("Press again to turn dreams off"));

    assert!(r.press("Press again to turn dreams off").await);
    let configured = r.fx.host.seen.lock().unwrap().configured.clone();
    assert_eq!(
        configured,
        [json!({ "key": "csr-companion.dreams", "value": false })]
    );
    let last = r.records().pop().unwrap();
    assert_eq!(
        (last["kind"].clone(), last["answer"].clone()),
        (json!("answer"), json!("turn_off"))
    );
    assert!(!r.band().await.has(NOTES_READ));
}

#[tokio::test]
async fn a_dream_waits_while_you_should_know_asks_so_its_4_can_never_turn_the_other_band() {
    let r = rig(replies(&[dream_reply()]));
    let cx = r.cx();
    r.go(12).await;
    assert!(until(|| !r.forks().is_empty()).await);
    assert!(
        r.band_has(NOTES_READ).await,
        "the dream is up when nothing asks"
    );
    // You should know's band, below this one, asks with its own 4.
    let asking = json!({ "type": "Box", "props": {}, "children": [
        { "type": "Button", "props": { "key": "ysk-disable", "hotkey": "4", "plain": true,
            "label": "Disable" }, "children": [] }] });
    let under = bandkit::draw(&r.table, &cx, asking).await;
    assert!(under.is_none(), "the band leaves the one below as it is");

    // Prompts typed while the dream was hidden are not "typed past".
    r.prompt(&cx, "one", "human").await;
    r.prompt(&cx, "two", "human").await;
    assert!(r
        .records()
        .iter()
        .all(|record| record["answer"] != "ignored"));
    assert_eq!(r.row(SID, "looksToSkip"), Value::Null);

    // As You should know's band redraws when its note clears: the dream, with its own 4.
    let shown = r.band().await;
    assert!(shown.has(NOTES_READ));
    assert_eq!(shown.key_of("Disable").as_deref(), Some("band-disable"));
    assert!(shown.key_of("ysk-disable").is_none());
}

#[tokio::test]
async fn a_dream_that_cannot_be_turned_off_says_so_and_stays_up() {
    let world = World {
        config_refused: true,
        ..replies(&[dream_reply()])
    };
    let r = rig(world);
    r.go(12).await;
    assert!(r.band_has(NOTES_READ).await);
    assert!(r.press("Disable").await);
    assert!(r.press("Press again to turn dreams off").await);
    let shown = r.band().await;
    assert!(shown.has("Couldn't turn dreams off here. Use /config."));
    assert!(shown.has(NOTES_READ));
    assert!(
        shown.has_button("Disable"),
        "the confirm is asked for again"
    );
}

#[tokio::test]
async fn already_thought_of_it_keeps_the_dream_from_being_asked_again() {
    let r = rig(replies(&[dream_reply()]));
    r.go(12).await;
    assert!(r.band_has(NOTES_READ).await);
    assert!(r.press("Already thought of it").await);
    assert!(!r.band().await.has(NOTES_READ));
    let seen = strings(&r.cx(), MACHINE, "knownDreams");
    assert_eq!(seen, [seen_entry(DREAM_LINE, Some(TITLE))]);
    let last = r.records().pop().unwrap();
    assert_eq!(last["answer"], "thought_of_it");
}

#[tokio::test]
async fn learn_more_explains_the_dream_and_ask_claude_hands_it_over() {
    let r = rig(replies(&[dream_reply()]));
    r.go(12).await;
    assert!(r.band_has(NOTES_READ).await);
    assert!(r.press("Learn more").await);
    let explained = r.band().await;
    assert!(explained.has(TITLE));
    assert!(explained.has("this look 41,000 tokens"));
    assert!(r.press("Ask the agent").await);
    let filled = r.filled();
    assert_eq!(filled.len(), 1);
    assert!(filled[0].starts_with(&format!("“{DREAM_LINE}”")));
    assert!(!r.band().await.has(NOTES_READ));
    let last = r.records().pop().unwrap();
    assert_eq!(last["answer"], "ask_claude");
}

#[test]
fn yskfacts_keeps_the_brief_s_code_rule_correction_and_past_session_lines_and_drops() {
    let facts = ysk_facts(YSK_BRIEF);
    assert!(facts.contains("[commit fdaba23, session 3f2a91c0]"));
    assert!(facts.contains("never block Claude Code from a hook"));
    assert!(facts.contains("Lines the person typed in past sessions"));
    assert!(
        facts.contains("- 07-11 (demo-app): “what if there was a free station” [session 142ef262]")
    );
    assert!(!facts.contains("Files Claude handled"));
    assert!(!facts.contains("What if the recap"));
    assert_eq!(ysk_facts("Files Claude handled lately: a.ts (1×)."), "");
}

#[test]
fn withcsrfacts_goes_before_you_should_know_s_conclusion_and_leaves_a_prompt_alone() {
    let out = with_csr_facts(YSK_PROMPT, &ysk_facts(YSK_BRIEF));
    let head = out.find("## What CSR remembers near this session").unwrap();
    assert!(head < out.find("## Conclusion").unwrap());
    assert!(out.starts_with("## Overview"));
    assert!(out.ends_with("Say learn: none when in doubt."));
    assert_eq!(with_csr_facts(YSK_PROMPT, "  "), YSK_PROMPT);
    assert!(is_ysk_propose(YSK_PROMPT));
    assert!(!is_ysk_propose(
        "Answer straight away: do not think it over first."
    ));
}

#[test]
fn isyskorigin_takes_every_name_the_you_should_know_seat_answers_to_and_no_other() {
    for plugin in [
        "cc-plugin-you-should-know",
        "cc-plugin-you-should-know@builtin",
        "you-should-know",
        "suggest-learning@builtin",
    ] {
        assert!(is_ysk_origin(plugin), "{plugin}");
    }
    for plugin in [
        "csr-shifts",
        "csr-companion@claude-self-reflect",
        "engine",
        "cc-plugin-tips@builtin",
    ] {
        assert!(!is_ysk_origin(plugin), "{plugin}");
    }
}

#[test]
fn yskline_reads_a_reply_as_you_should_know_does_and_citedfrom_counts_only_receipts() {
    assert_eq!(ysk_line("learn: none"), (None, true));
    let said = "- learn: The setup handler changed after your 07-12 session.\ntag: Heads up";
    assert_eq!(
        ysk_line(said),
        (
            Some("The setup handler changed after your 07-12 session.".to_string()),
            true
        )
    );
    assert_eq!(ysk_line("I could not decide."), (None, false));
    let facts = ysk_facts(YSK_BRIEF);
    let reply = "It changed in commit fdaba23 [session 3f2a91c0], and commit abcdef1 too.";
    assert_eq!(
        cited_from(reply, &facts),
        ["commit fdaba23", "session 3f2a91c0"]
    );
}

#[test]
fn the_dreamer_s_fork_opens_with_the_side_request_frame_no_tools_one_response_no() {
    let prompt = dream_prompt("* a fact [session 3f2a91c0]", &[], &[], &[], "", &[]);
    assert!(prompt.starts_with("<system-reminder>This is a side request from CSR's dreamer"));
    assert!(prompt.contains("You have no tools"));
    assert!(prompt.find("</system-reminder>").unwrap() < prompt.find("## Overview").unwrap());
}

#[test]
fn a_hand_off_is_text_paste_placeholders_and_ultra_words_cannot_act_and_the_sent() {
    assert_eq!(
        clean_for_draft("see @hooks/register.tsx and ultrathink"),
        "see \u{FF20}hooks/register.tsx and ultra-think"
    );
    assert_eq!(clean_for_draft("mail me at a@b.co"), "mail me at a@b.co");
    let line = "What if @pane.py drew the brief?";
    let sent = ask_claude(line, "The pane could draw it", "x");
    let logged = [logged_dream(line, 1.)];
    let found = dream_sent(&sent, &logged);
    assert_eq!(found.map(|dream| dream.line.clone()).as_deref(), Some(line));
}

#[tokio::test]
async fn a_fork_of_another_plugin_is_noted_and_passes_untouched() {
    let r = rig(world(Some(true)));
    let before = forked(
        &r,
        fork_event("csr-shifts", YSK_PROMPT),
        json!({ "value": {} }),
    )
    .await;
    assert!(before.next.is_none() && before.answer.is_none());
    assert!(until(|| !r.records().is_empty()).await);
    let record = r.records().remove(0);
    assert_eq!(record["kind"], "ysk");
    assert_eq!(record["outcome"], "other-origin");
    assert_eq!(record["origin"], "csr-shifts/builtin");
    assert_eq!(record["session"], SID);
    assert_eq!(r.out.briefs.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn you_should_know_s_propose_fork_gets_csr_s_facts_and_a_ysk_record_of_what_came() {
    let r = rig(world(Some(true)));
    r.out.set_brief(YSK_BRIEF);
    let reply = json!({ "isAnswered": true,
        "text": "learn: The setup handler changed [commit fdaba23].", "usage": used_usage() });
    let before = forked(
        &r,
        fork_event("cc-plugin-you-should-know", YSK_PROMPT),
        json!({ "value": reply }),
    )
    .await;
    let sent = before.next.expect("the prompt carries CSR's facts");
    let prompt = sent["prompt"].as_str().unwrap();
    assert!(prompt.contains("## What CSR remembers near this session"));
    assert!(prompt.contains("[commit fdaba23, session 3f2a91c0]"));
    assert!(until(|| !r.records().is_empty()).await);
    let record = r.records().remove(0);
    assert_eq!(record["kind"], "ysk");
    assert_eq!(record["prompt"], "propose");
    assert_eq!(record["outcome"], "line");
    assert_eq!(
        record["line"],
        "The setup handler changed [commit fdaba23]."
    );
    assert_eq!(record["cited"], json!(["commit fdaba23"]));
    assert!(record["given"].as_u64().unwrap() > 0);
    assert_eq!(record["usage"], used_usage());
    // One brief per session per five minutes.
    let again = forked(
        &r,
        fork_event("you-should-know", YSK_PROMPT),
        json!({ "value": reply }),
    )
    .await;
    assert!(again.next.is_some());
    assert_eq!(r.out.briefs.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_fork_of_you_should_know_that_is_denied_or_empty_or_not_a_propose_is_kept_as_such() {
    let r = rig(world(Some(true)));
    r.out.set_brief(YSK_BRIEF);
    let ysk = "cc-plugin-you-should-know";
    forked(
        &r,
        fork_event(ysk, YSK_PROMPT),
        json!({ "deny": "timed out" }),
    )
    .await;
    let empty = json!({ "value": { "isAnswered": false, "reason": "api-error" } });
    forked(&r, fork_event(ysk, YSK_PROMPT), empty).await;
    let explain = fork_event(ysk, "Explain it straight away.");
    let text = json!({ "isAnswered": true, "text": "Because." });
    let unfacted = forked(&r, explain, json!({ "value": text })).await;
    assert!(unfacted.next.is_none(), "an explain prompt is left alone");
    r.out
        .no_ysk_brief
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let off = forked(&r, fork_event(ysk, YSK_PROMPT), json!({ "value": text })).await;
    assert!(
        off.next.is_none(),
        "CSR_NO_YSK_BRIEF keeps the record and leaves the prompt alone"
    );
    assert!(until(|| r.records().len() >= 4).await);
    let outcomes: Vec<Value> = r
        .records()
        .iter()
        .map(|record| record["outcome"].clone())
        .collect();
    assert_eq!(
        outcomes,
        [
            json!("denied"),
            json!("api-error"),
            json!("explained"),
            json!("unparsed")
        ]
    );
    assert_eq!(r.records()[2]["prompt"], "explain");
}

#[tokio::test]
async fn a_fork_whose_seat_the_relay_posts_beside_the_event_is_told_apart_by_it() {
    use crate::companion::{
        relay::{
            dispatch::{App, Log},
            host::{Hub, SystemClock},
        },
        store::Store,
    };
    let dir = tempfile::tempdir().unwrap();
    let kept: Kept = Arc::default();
    let seen = kept.clone();
    let keep: pace::Keep = Arc::new(move |cwd, record| {
        seen.lock().unwrap().push((cwd, record));
        Box::pin(async {})
    });
    let out = Arc::new(Fake::new(keep.clone(), BRIEF));
    out.set_brief(YSK_BRIEF);
    let mut table = Table::new();
    pace::register_with(&mut table, keep);
    register_with(&mut table, out);
    let (push, _lines) = tokio::sync::mpsc::unbounded_channel();
    let app = App::new(
        Hub::new(push),
        Arc::new(Store::at(dir.path().join("csr-companion/companion.db"))),
        Arc::new(SystemClock),
        Arc::new(Log::default()),
        table,
    );
    let at = |k: &str, e: Value| json!({ "sid": SID, "cwd": "/repo", "k": k, "e": e });
    posted(
        &app,
        "/event",
        at("start", json!({ "isInteractive": true })),
    )
    .await;
    let fork = |plugin: Option<&str>| {
        let mut body = at("fork", json!({ "prompt": YSK_PROMPT }));
        if let Some(plugin) = plugin {
            body["origin"] = json!({ "plugin": plugin, "tier": "builtin" });
        }
        body
    };
    // You should know's own fork: CSR's facts go in, and the seat does not go on with the event.
    let ours = posted(&app, "/event", fork(Some("cc-plugin-you-should-know"))).await;
    let prompt = ours["next"]["prompt"].as_str().expect("the facts went in");
    assert!(
        prompt.contains("## What CSR remembers near this session"),
        "{ours}"
    );
    assert!(ours["next"].get("origin").is_none(), "{ours}");
    assert!(ours["id"].is_string());
    // Another plugin's fork, and one with no seat: passed untouched, and logged as such.
    for (plugin, shown) in [
        (Some("csr-shifts"), "csr-shifts/builtin"),
        (None, "unknown"),
    ] {
        let theirs = posted(&app, "/event", fork(plugin)).await;
        assert_eq!(theirs, json!({}), "{shown}");
        let found = || {
            let kept = kept.lock().unwrap();
            kept.iter()
                .map(|(_, record)| record.clone())
                .any(|record| record["outcome"] == "other-origin" && record["origin"] == shown)
        };
        assert!(until(found).await, "{shown}: {:?}", kept.lock().unwrap());
    }
}

#[tokio::test]
async fn a_headless_child_never_forks_a_look_or_keeps_a_fork_record() {
    let r = rig(World {
        headless: true,
        replies: vec![dream_reply()],
        ..world(Some(true))
    });
    r.fx.cx.session.lock().unwrap().interactive = Some(false);
    r.go(30).await;
    let ysk = "cc-plugin-you-should-know";
    forked(&r, fork_event(ysk, YSK_PROMPT), json!({ "value": {} })).await;
    let cx = r.cx();
    r.compact(&cx, "auto").await;
    quiesce().await;
    assert_eq!(r.forks(), Vec::<String>::new());
    assert_eq!(r.records(), Vec::<Value>::new());
}

#[tokio::test]
async fn a_compaction_with_no_dream_up_or_one_a_subagent_ran_carries_nothing() {
    let r = rig(replies(&[dream_reply()]));
    let cx = r.cx();
    r.go(12).await;
    assert!(until(|| r.up().is_some()).await);
    for e in [
        json!({ "trigger": "auto", "agentId": "sub-1", "messages": [] }),
        json!({ "trigger": "precompute", "messages": [] }),
    ] {
        let before = run(&r.table, "chapter", &cx, e).await;
        r.after("chapter", &cx, json!({ "messages": [] }), &before.left)
            .await;
    }
    assert_eq!(r.row(SID, "view")["carried"], Value::Null);
    // A compaction that did not go through carries nothing either.
    let before = run(&r.table, "chapter", &cx, json!({ "trigger": "auto" })).await;
    r.after("chapter", &cx, json!({ "skip": true }), &before.left)
        .await;
    assert_eq!(r.row(SID, "view")["carried"], Value::Null);
    assert!(r.with("answer", "answer", "carried").is_empty());
}

#[tokio::test]
async fn a_compaction_that_failed_leaves_the_dream_that_is_up_as_it_was() {
    let r = rig(replies(&[dream_reply()]));
    let cx = r.cx();
    r.go(12).await;
    assert!(until(|| r.up().is_some()).await);
    let view = r.row(SID, "view");
    let chapter = r.row(SID, "chapter");
    for _ in 0..2 {
        // The relay tells the engine the call threw: `/after` with `{ deny }`.
        let before = run(&r.table, "chapter", &cx, json!({ "trigger": "manual" })).await;
        r.after(
            "chapter",
            &cx,
            json!({ "deny": "summary failed" }),
            &before.left,
        )
        .await;
    }
    assert_eq!(r.row(SID, "view"), view, "not carried, not expired");
    assert_eq!(r.row(SID, "chapter"), chapter, "no new chapter");
    assert!(r.with("answer", "answer", "carried").is_empty());
    assert!(r.with("answer", "answer", "expired").is_empty());
}

#[tokio::test]
async fn the_dreamer_registers_where_the_map_puts_it() {
    let mut t = Table::new();
    register(&mut t);
    for key in ["step", "fork", "chapter", "digit", "start"] {
        assert_eq!(t.features(key), ["dreamer"], "{key}");
    }
    assert!(
        t.entries("step")[0].event.is_none(),
        "a stream has no /event"
    );
    let chapter = &t.entries("chapter")[0];
    assert!(chapter.event.is_some() && chapter.after.is_some());
}

#[tokio::test]
async fn a_start_hands_the_fleet_its_look_and_sets_the_pane_answer_job_every_15_seconds() {
    let r = rig(world(Some(true)));
    let cx = r.cx();
    assert!(cx.kept(|f: &mut Fleet| f.dream.is_none()));
    r.start(&cx).await;
    assert!(cx.kept(|f: &mut Fleet| f.dream.is_some()));
    // A dream is up, and the pane answers it: the 15 s job takes it off the band.
    let note = json!({ "id": "dream:x:1", "kind": "dream", "line": DREAM_LINE, "at": 1 });
    let view = json!({ "kind": "offer", "note": note, "tag": "Dream", "shownAt": 1,
        "promptsSurvived": 0 });
    r.put(SID, "view", view.clone());
    let said = json!({ "kind": "answer", "id": "dream:x:1", "answer": "helpful", "by": "pane" });
    r.out.set_log(&said.to_string());
    r.fx.timers
        .advance(15_000., Arc::new(r.fx.host.clone()))
        .await;
    assert!(until(|| r.up().is_none()).await);
    // A start that is not a person's sets no job.
    let kid = rig(world(Some(true)));
    let kid_cx = kid.cx();
    let e = json!({ "cwd": "/repo", "isInteractive": false });
    run(&kid.table, "start", &kid_cx, e).await;
    kid.put(SID, "view", view);
    kid.out.set_log(&said.to_string());
    kid.fx
        .timers
        .advance(15_000., Arc::new(kid.fx.host.clone()))
        .await;
    quiesce().await;
    assert!(kid.up().is_some());
}

#[tokio::test]
async fn the_spend_of_a_look_that_lost_the_lease_is_kept_and_its_dream_is_not_shown() {
    let r = rig(World {
        fork_usage: vec![big()],
        replies: vec![dream_reply()],
        ..world(Some(true))
    });
    let (cx, gate) = r.gated();
    r.steps(&cx, 12, None, None).await;
    assert!(until(|| gate.arrived() >= 1).await);
    // Another copy holds the lease now.
    r.out
        .put_lease(SID, json!({ "root": elsewhere(), "rank": 9, "at": NOW }));
    gate.open();
    let paid = || r.row(SID, "pace")["dreamer"].as_f64().unwrap_or(0.) > 0.;
    assert!(until(paid).await);
    let pace = r.row(SID, "pace");
    assert_eq!(pace["dreamer"], num(5. * LOOK));
    assert_eq!(pace["looks"], json!(1));
    quiesce().await;
    assert_eq!(
        r.dreams(),
        Vec::<Value>::new(),
        "the look was dropped; the money was not forgotten"
    );
    assert_eq!(r.up(), None);
}

#[test]
fn the_lease_args_name_the_pane_folder_the_session_and_the_rank_of_this_copy() {
    use super::outside::{lease_args, rank_of};
    use crate::companion::{lease::Step, paths::Paths};
    let paths = Paths::new(HOME.into());
    let args = lease_args(&paths, ROOT, "sess-1", Step::Take, true, 5.);
    assert_eq!(
        args.folder.to_string_lossy(),
        "/home/u/.claude-self-reflect/csr-companion/dreamer-lease"
    );
    assert_eq!(
        (args.session.as_str(), args.root.as_str(), args.rank),
        ("sess-1", ROOT, 1)
    );
    assert!(args.busy);
    assert_eq!(
        rank_of("/home/u/.claude/plugins/cache/x/csr-companion/0.4.0"),
        0
    );
    assert_eq!(rank_of("/home/u/dev-mods/csr-companion"), 1);
}

#[test]
fn optional_observations_are_independent_bounded_and_never_pad() {
    use super::text::parse_sections;
    let legacy = "dream: What if the upload queue could serve imports?\nexplain:\n**Reuse the queue**\nTry it.";
    let reply = format!(
        r#"{legacy}
confusing_claude: [{{"line":"Repeated the rejected approach.","evidence":"Turn 12: 'keep it local'"}},{{"line":"Misread the target.","evidence":"Turn 15: 'the pane'"}},{{"line":"Third.","evidence":"Turn 18"}}]
should_be: [{{"kind":"hook","line":"Check formatting before each commit.","evidence":"Turns 4 and 12: 'run fmt'"}},{{"kind":"other","line":"Invalid kind.","evidence":"Turn 9"}}]"#
    );
    let sections = parse_sections(&reply);
    assert_eq!(sections["confusing_claude"].as_array().unwrap().len(), 2);
    let only = "dream: What if?\nexplain: **T**\nBody.\nsame_page: {\"asks\":1,\"missed\":0,\"unclear\":0,\"line\":\"Fine.\"}";
    assert!(!parse_dream(only).unwrap().text.contains("same_page"));
    let done = "dream: What if?\nexplain: **T**\nBody.\nthreads_done: []\nsame_page: {}";
    assert!(!parse_dream(done).unwrap().text.contains("threads_done"));
    assert_eq!(sections["should_be"].as_array().unwrap().len(), 1);
    assert_eq!(parse_dream(&reply), parse_dream(legacy));
    for bad in [
        "",
        "confusing_claude: nope",
        "confusing_claude: []\nshould_be: []",
        "confusing_claude: [{\"line\":\"No citation\"}]",
    ] {
        let sections = parse_sections(bad);
        assert!(sections["confusing_claude"].as_array().unwrap().is_empty());
        assert!(sections["should_be"].as_array().unwrap().is_empty());
        assert_eq!(
            parse_dream(&format!("{legacy}\n{bad}")),
            parse_dream(legacy)
        );
    }
    // explain and next ride along when they are sentences of a sane length, and only then.
    let detailed = parse_sections(&format!(
        "confusing_claude: [{{\"line\":\"A\",\"evidence\":\"Turn 1\",\"explain\":\" It misread. \",\"next\":\"{}\"}}]\nshould_be: [{{\"kind\":\"memory\",\"line\":\"B\",\"evidence\":\"Turn 2\",\"next\":\"Save it.\"}}]",
        "x".repeat(201)
    ));
    let confusing = &detailed["confusing_claude"][0];
    assert_eq!(confusing["explain"], "It misread.");
    assert!(
        confusing.get("next").is_none(),
        "an overlong next is left out"
    );
    let should = &detailed["should_be"][0];
    assert_eq!(should["next"], "Save it.");
    assert!(should.get("explain").is_none());
    assert_eq!(sections["confusing_claude"][0].get("explain"), None);
    let none = parse_sections("dream: none\nconfusing_claude: []\nshould_be: []");
    assert_eq!(none["should_be"], serde_json::json!([]));
}

#[tokio::test]
async fn same_fork_persists_optional_sections_even_with_no_what_if() {
    for first in [dream_reply(), "dream: none".into()] {
        let reply = format!(
            r#"{first}
confusing_claude: [{{"line":"Misread the target.","evidence":"Turn 4: 'the pane'"}}]
should_be: [{{"kind":"memory","line":"Keep the pane question first.","evidence":"Turns 4 and 8: 'question first'"}}]"#
        );
        let r = rig(replies(&[reply]));
        r.go(12).await;
        assert!(until(|| r.records().iter().any(|r| r["confusing_claude"].is_array())).await);
        let record = r
            .records()
            .into_iter()
            .find(|r| r["confusing_claude"].is_array())
            .unwrap();
        assert_eq!(record["confusing_claude"].as_array().unwrap().len(), 1);
        assert_eq!(record["should_be"][0]["kind"], "memory");
        assert_eq!(r.forks().len(), 1);
    }
}

#[test]
fn the_threads_block_is_asked_only_when_there_are_threads_and_before_the_reading() {
    let none = dream_prompt("* a fact", &[], &[], &[], "", &[]);
    assert!(!none.contains("threads_done"));
    let threads = [(
        "abc123abc123".to_string(),
        "Run the bench\nagain".to_string(),
    )];
    let some = dream_prompt("* a fact", &[], &[], &[], "", &threads);
    assert!(some.contains("abc123abc123: Run the bench again\n"));
    assert!(some.find("threads_done:").unwrap() < some.find("same_page:").unwrap());
    assert!(some.contains("No code fences or text after this line."));
}

#[tokio::test]
async fn a_look_asks_about_quiet_threads_and_writes_down_what_the_reply_says_finished() {
    let reply = format!(
        "{}\nthreads_done: [{{\"key\":\"abc123abc123\",\"quote\":\"Committed as 0fb9972.\"}}]",
        reply_of(DREAM_LINE, TITLE, "* A body.")
    );
    let r = rig_in(replies(&[reply]), "abcd1234-0f0f");
    *r.out.threads.lock().unwrap() =
        vec![("abc123abc123".to_string(), "Commit the work".to_string())];
    let cx = r.cx();
    r.steps(&cx, 12, None, None).await;
    assert!(until(|| !r.out.retired.lock().unwrap().is_empty()).await);
    let retired = r.out.retired.lock().unwrap().clone();
    assert_eq!(retired[0][0]["key"], "abc123abc123");
    assert_eq!(retired[0][0]["quote"], "Committed as 0fb9972.");
    assert!(
        !r.dreams()[0]["text"]
            .as_str()
            .unwrap()
            .contains("threads_done"),
        "the answer stays out of the dream"
    );
}

#[tokio::test]
async fn a_dream_record_counts_the_quiet_threads_asked_named_and_retired_and_says_why_one_failed() {
    let quote = "Committed as 0fb9972.";
    let made_up = "x".repeat(130);
    let reply = format!(
        "{}\nthreads_done: [{{\"key\":\"abc123abc123\",\"quote\":\"{quote}\"}},\
         {{\"key\":\"def456def456\",\"quote\":\"{made_up}\"}}]",
        reply_of(DREAM_LINE, TITLE, "* A body.")
    );
    let r = rig_in(replies(&[reply]), "abcd1234-0f0f");
    *r.out.threads.lock().unwrap() = vec![
        ("abc123abc123".to_string(), "Commit the work".to_string()),
        ("def456def456".to_string(), "Run the bench".to_string()),
        ("0123456789ab".to_string(), "Never named".to_string()),
    ];
    *r.out.came_to.lock().unwrap() = crate::companion::retire::Retired {
        retired: 1,
        rejected: vec![json!({"key": "def456def456", "quote": made_up,
            "why": crate::companion::retire::QUOTE_NOT_NEWER})],
    };
    let cx = r.cx();
    r.steps(&cx, 12, None, None).await;
    assert!(until(|| !r.dreams().is_empty()).await);
    let dream = &r.dreams()[0];
    assert_eq!(dream["threads_asked"], 3);
    assert_eq!(dream["threads_named"], 2);
    assert_eq!(dream["threads_retired"], 1);
    let rejected = dream["threads_rejected"].as_array().unwrap();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0]["key"], "def456def456");
    assert_eq!(rejected[0]["why"], "quote not in a newer summary");
}

#[tokio::test]
async fn a_look_that_found_no_dream_still_writes_the_thread_counts_even_when_all_are_zero() {
    let r = rig_in(replies(&["dream: none".into()]), "abcd1234-0f0f");
    *r.out.threads.lock().unwrap() =
        vec![("abc123abc123".to_string(), "Commit the work".to_string())];
    let cx = r.cx();
    r.steps(&cx, 12, None, None).await;
    assert!(until(|| r.outcomes("none") > 0).await);
    let none = r.find(|record| record["outcome"] == "none").unwrap();
    assert_eq!(none["threads_asked"], 1);
    assert_eq!(none["threads_named"], 0);
    assert_eq!(none["threads_retired"], 0);
    assert_eq!(none["threads_rejected"], json!([]));
    assert!(
        r.out.retired.lock().unwrap().is_empty(),
        "nothing was named, so nothing was written"
    );
}
