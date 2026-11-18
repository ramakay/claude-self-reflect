use super::*;

// ---------------------------------------------------------------- dreams on and off

#[tokio::test]
async fn dreams_are_on_by_default_with_no_option_set_a_look_can_run_as_claude_works() {
    let r = rig(World {
        replies: vec![dream_reply()],
        ..world(None)
    });
    r.go(30).await;
    assert!(until(|| !r.forks().is_empty()).await);
    assert!(!r.forks().is_empty());
    // `/csr dreams` says what the pace says: not that they are off.
    assert!(!pace::pace_text(None, None, true).contains("Dreams are off"));
}

#[tokio::test]
async fn dreams_turned_off_in_config_no_look_however_long_claude_works_and_csr_dreams() {
    let r = rig(World {
        replies: vec![dream_reply()],
        ..world(Some(false))
    });
    r.go(30).await;
    quiesce().await;
    assert_eq!(r.forks(), Vec::<String>::new());
    assert_eq!(r.looks(), Vec::<Value>::new());
    let said = pace::pace_text(None, None, false);
    assert!(said.contains("Dreams are off"));
    assert!(said.contains("/config"));
}

#[tokio::test]
async fn a_look_that_finds_nothing_does_not_slow_the_next_and_with_nothing_new_about_the() {
    let r = rig(replies(&["dream: none".into(), "dream: none".into()]));
    r.go(12).await;
    assert!(until(|| !r.forks().is_empty()).await);
    quiesce().await;
    assert_eq!(r.forks().len(), 1);
    let pace = r.row(SID, "pace");
    assert_eq!(
        (pace["looks"].clone(), pace["dreams"].clone()),
        (json!(1), json!(0))
    );
    assert!(
        !r.band().await.drew,
        "a look that found nothing leaves the band bare"
    );
    // Step 6 waits for its share; step 12 finds nothing.
    let looks = r.looks();
    assert_eq!(looks.len(), 2, "{looks:?}");
    assert_eq!(looks[0]["outcome"], "budget");
    assert_eq!(
        (looks[0]["every"].clone(), looks[0]["tokens"].clone()),
        (json!(6), json!(0))
    );
    assert_eq!(looks[1]["outcome"], "none");
    assert_eq!(looks[1]["every"], json!(22)); // (120 - 12) / 5
    assert_eq!(looks[1]["tokens"], num(LOOK));
    assert_eq!(looks[1]["left"], json!(5));
    assert_eq!(looks[1]["usage"], used_usage());
    assert_eq!(looks[1]["model"], "opus");
    assert!(looks[0].get("usage").is_none(), "no fork: nothing spent");

    r.go(30).await; // step 42, past the next look (34): affordable, but nothing new
    assert!(until(|| r.outcomes("quiet") >= 2).await); // 34 and 40
    assert_eq!(r.forks().len(), 1);
    let last = r.looks().pop().unwrap();
    assert_eq!(
        (last["outcome"].clone(), last["tokens"].clone()),
        (json!("quiet"), json!(0))
    );

    // A past line: not news.
    let past = "- 10-08 (csr-engine): “the import should retry” [session 3f2a91c0]";
    r.out.set_brief(&format!("{BRIEF}\n{past}"));
    r.go(12).await;
    assert!(until(|| r.outcomes("quiet") >= 4).await); // 46 and 52
    assert_eq!(r.forks().len(), 1);
    assert_eq!(r.looks().pop().unwrap()["outcome"], "quiet");

    r.out.set_brief(&format!("{BRIEF}\n{past}\n{NEWS}"));
    r.go(12).await;
    assert!(until(|| r.row(SID, "pace")["looks"].as_u64().unwrap_or(0) >= 2).await);
    assert_eq!(r.forks().len(), 2);
    assert_eq!(r.row(SID, "pace")["looks"], json!(2));
}

#[tokio::test]
async fn one_dreamer_per_session_a_fresh_lease_held_by_another_dev_copy_keeps_this_one() {
    let r = rig(replies(&[dream_reply()]));
    r.out.put_lease(
        SID,
        json!({ "root": elsewhere(), "rank": 1, "at": NOW - 60_000. }),
    );
    r.go(30).await;
    quiesce().await;
    assert_eq!(r.forks(), Vec::<String>::new());
    assert_eq!(
        r.looks(),
        Vec::<Value>::new(),
        "no budget, no quiet: it never looked"
    );
    assert_eq!(r.out.lease_file(SID)["root"], elsewhere());
}

#[tokio::test]
async fn one_dreamer_per_session_a_dev_copy_takes_the_lease_from_the_installed_plugin_and() {
    let installed = "/home/u/.claude/plugins/cache/claude-self-reflect/csr-companion/0.4.0";
    let r = rig(replies(&[dream_reply()]));
    r.out.put_lease(
        SID,
        json!({ "root": installed, "rank": 0, "at": NOW - 60_000. }),
    );
    r.go(12).await;
    assert!(until(|| !r.forks().is_empty()).await);
    quiesce().await;
    assert_eq!(r.forks().len(), 1);
    let lease = r.out.lease_file(SID);
    assert_ne!(lease["root"], installed);
    assert_eq!(lease["root"], ROOT);
    assert_eq!(lease["rank"], json!(1));
    assert_eq!(super::outside::rank_of(installed), 0);
}

/// Shape changed: `lease.py` and `python3` are gone. The lease step is the engine's own
/// (`lease::step_under_lock`), so the cases are the three it can end in: it ran and said held,
/// it ran and said not held, and it did not run (a crash, or its lock not got in time). Only the
/// first grants the lease. The TypeScript's last case, a `python3` that cannot start, had the
/// look go on with no lease kept; the engine is always there, so the one case left that keeps
/// no lease is no HOME (`no_home`): a take or a release is granted, a renewal is not.
#[tokio::test]
async fn one_dreamer_per_session_only_lease_py_saying_held_grants_the_lease_python3() {
    use super::outside::{lease_held, no_home};
    use crate::companion::lease::{LeaseOutput, Step};
    let said = |held: bool| {
        Ok(LeaseOutput {
            held,
            lease: json!({}),
        })
    };
    assert!(lease_held(&said(true)));
    assert!(!lease_held(&said(false)));
    // Its lock not got in time, crashed or killed: another copy decides now.
    assert!(!lease_held(&Err(anyhow::anyhow!(
        "timed out after 2000 ms"
    ))));
    assert!(!lease_held(&Err(anyhow::anyhow!("killed"))));
    // No HOME: nothing can be leased on this machine, and the look goes on.
    assert!(no_home(Step::Take) && no_home(Step::Release));
    assert!(
        !no_home(Step::Renew),
        "an active look cannot keep ownership with no lease"
    );
    // A lease step that did not run takes nothing: no look starts.
    let r = rig(replies(&[dream_reply()]));
    r.out
        .lease_fails
        .store(true, std::sync::atomic::Ordering::SeqCst);
    r.go(12).await;
    quiesce().await;
    assert_eq!(r.forks(), Vec::<String>::new());
    assert!(!r.out.steps("take").is_empty());
}

#[tokio::test]
async fn one_dreamer_per_session_a_lease_is_taken_under_a_lock_marked_busy_while_the_look() {
    let installed = "/home/u/.claude/plugins/cache/claude-self-reflect/csr-companion/0.5.0";
    let r = rig(replies(&[dream_reply()]));
    let (cx, gate) = r.gated();
    r.out.put_lease(
        SID,
        json!({ "root": installed, "rank": 0, "at": NOW - 60_000., "busy": true }),
    );
    r.steps(&cx, 12, None, None).await;
    quiesce().await;
    assert_eq!(
        gate.arrived(),
        0,
        "the installed copy's look runs: the dev copy waits"
    );
    r.out.put_lease(
        SID,
        json!({ "root": installed, "rank": 0, "at": NOW - 60_000. }),
    );
    r.steps(&cx, 12, None, None).await;
    assert!(until(|| gate.arrived() >= 1).await);
    let ours = r.out.lease_file(SID);
    assert_eq!(
        (ours["rank"].clone(), ours["busy"].clone()),
        (json!(1), json!(true))
    );
    let taken = r.out.steps("take");
    let last = taken.last().unwrap();
    let folder = format!("{HOME}/.claude-self-reflect/csr-companion/dreamer-lease");
    assert_eq!(last.folder.to_string_lossy(), folder);
    assert_eq!((last.session.as_str(), last.busy), (SID, true));
    gate.open();
    assert!(until(|| r.out.lease_file(SID).get("busy").is_none()).await);
    assert!(!r.out.steps("release").is_empty(), "the look ended");
}

#[tokio::test]
async fn one_dreamer_per_session_a_lease_older_than_15_minutes_is_taken_whoever_held_it() {
    let r = rig(replies(&[dream_reply()]));
    let stale = json!({ "root": elsewhere(), "rank": 1, "at": NOW - 16. * 60_000. });
    r.out.put_lease(SID, stale);
    r.go(12).await;
    assert!(until(|| !r.forks().is_empty()).await);
    assert_eq!(r.forks().len(), 1);
    assert_ne!(r.out.lease_file(SID)["root"], elsewhere());
}

// ---------------------------------------------------------------- a dream sent on

#[tokio::test]
async fn a_prompt_that_carries_a_dream_is_logged_sent_once_from_the_band_fill_or_the_pane() {
    let r = rig(world(Some(true)));
    r.out.set_log(&logged());
    r.put(SID, "looksToSkip", json!(1));
    r.put(SID, "typedPastInARow", json!(1));
    r.hour_old_chapter();
    let cx = r.cx();
    // The pane's send-text, with a band hotkey that landed in the box first.
    let text =
        format!("2CSR's dreamer asked this while you worked: “{DREAM_LINE}” Its reasoning: Notes.");
    r.prompt(&cx, &text, "composer").await;
    let sent = || r.with("answer", "answer", "sent");
    assert_eq!(sent().len(), 1, "{:?}", r.records());
    assert_eq!(sent()[0]["id"], LOGGED_ID);
    assert_eq!(sent()[0]["by"], "prompt");
    assert_eq!(
        r.row(SID, "looksToSkip"),
        json!(0),
        "an answer: the back-off stops"
    );
    assert_eq!(r.row(SID, "chapter")["adopted"], json!(1), "its place back");
    r.out.set_log(&format!("{}\n{}", logged(), sent()[0]));
    // The band's fill, sent again: already logged.
    r.prompt(&cx, &fill(), "composer").await;
    assert_eq!(sent().len(), 1);
    // A prompt that only talks about the dreamer is not a sending.
    r.prompt(&cx, "why did the dreamer ask that?", "composer")
        .await;
    assert_eq!(sent().len(), 1);
}

#[tokio::test]
async fn a_dream_sent_on_with_a_no_beside_it_is_rejected_with_your_words_never_sent_and() {
    let r = rig(world(Some(true)));
    r.out.set_log(&logged());
    r.put(SID, "looksToSkip", json!(2));
    r.hour_old_chapter();
    let cx = r.cx();
    let fill = ask_claude(DREAM_LINE, TITLE, "* CSR only checks main.");
    r.prompt(&cx, &format!("{fill}\n\n{OVERDREAMT}"), "composer")
        .await;
    let answers = r.of_kind("answer");
    assert_eq!(answers.len(), 1, "{answers:?}");
    assert_eq!(answers[0]["id"], LOGGED_ID);
    assert_eq!(answers[0]["answer"], "rejected");
    assert_eq!(answers[0]["by"], "prompt");
    assert_eq!(answers[0]["said"], OVERDREAMT);
    assert_eq!(
        r.row(SID, "looksToSkip"),
        json!(0),
        "an answer: the back-off stops"
    );
    assert_eq!(r.row(SID, "chapter")["adopted"], json!(0));
}

#[tokio::test]
async fn your_next_prompt_after_a_dream_you_sent_rejects_it_when_it_says_no_only_the_next() {
    let r = rig(world(Some(true)));
    r.out.set_log(&logged());
    r.hour_old_chapter();
    let cx = r.cx();
    let said = |answer: &str| r.with("answer", "answer", answer);
    r.prompt(&cx, &fill(), "composer").await;
    assert_eq!(said("sent").len(), 1);
    assert_eq!(r.row(SID, "chapter")["adopted"], json!(1));
    r.out.set_log(&format!("{}\n{}", logged(), said("sent")[0]));
    let no = "No, that won't work: we can't restart a session from the phone.";
    r.prompt(&cx, no, "composer").await;
    let rejected = said("rejected");
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0]["id"], LOGGED_ID);
    assert_eq!(rejected[0]["by"], "reply");
    assert_eq!(rejected[0]["said"], no);
    assert_eq!(
        r.row(SID, "chapter")["adopted"],
        json!(0),
        "its place, given back at sent, is taken again"
    );
    // A second reply is not a reply to it.
    r.prompt(&cx, "no, really not", "composer").await;
    assert_eq!(said("rejected").len(), 1);
}

#[tokio::test]
async fn a_reply_that_agrees_or_one_after_the_half_hour_leaves_the_dream_sent() {
    let r = rig(world(Some(true)));
    r.out.set_log(&logged());
    let cx = r.cx();
    let said = |answer: &str| r.with("answer", "answer", answer);
    let agree = format!("{}\n\nyes, try it on one branch first", fill());
    r.prompt(&cx, &agree, "composer").await;
    assert_eq!(said("sent").len(), 1);
    r.prompt(&cx, "go on, I can't wait to see it", "composer")
        .await;
    assert_eq!(said("rejected"), Vec::<Value>::new());
    // Sent afresh, then a no 31 minutes on: no longer a reply to it.
    r.out.set_log(&logged());
    r.prompt(&cx, &fill(), "bridge").await;
    r.fx.clock.set(NOW + 31. * 60_000.);
    r.prompt(&cx, "no, not that", "composer").await;
    assert_eq!(said("rejected"), Vec::<Value>::new());
}

#[test]
fn the_words_beside_a_dream_are_all_the_person_s_and_the_sent_dream_is_the_one_its() {
    let a = ("What if the import reused the upload queue?", "t", "x");
    let b = ("What if we disable validation?", "t", "x");
    let dreams = [logged_dream(a.0, 1.), logged_dream(b.0, 2.)];
    let reply = "CSR's dreamer asked this while you worked yesterday, but this is not useful.";
    let said = words_beside(&format!("{}\n\n{reply}", ask_claude(a.0, a.1, a.2)));
    assert_eq!(said, reply);
    assert!(rebuts(&said));
    let quoting = format!(
        "{}\n\nNever follow “What if we disable validation?”",
        ask_claude(a.0, a.1, a.2)
    );
    let found = |text: &str| dream_sent(text, &dreams).map(|d| d.line.clone());
    assert_eq!(found(&quoting), Some(a.0.to_string()));
    let pane = "CSR's dreamer asked this while you worked, quoted: “What if the import reused \
        the upload queue?” Its reasoning: “x.” Never “What if we disable validation?”";
    assert_eq!(found(pane), Some(a.0.to_string()));
    let other = "CSR's dreamer asked this while you worked yesterday: “What if we disable \
        validation?”";
    assert_eq!(
        found(other),
        None,
        "a header and a line that is not the dream's"
    );
    let says = [
        OVERDREAMT,
        "nope",
        "I can’t do that from the phone",
        "this is not what I asked",
        "yes please",
        "no-op is fine",
        "not now, after the tests",
    ]
    .map(rebuts);
    assert_eq!(says, [true, true, true, true, false, false, false]);
}

#[test]
fn dreamsent_finds_the_dream_a_prompt_carries_and_nothing_in_prose_that_names_the() {
    let dreams = [
        logged_dream("What if a?", 1.),
        logged_dream("What if b?", 2.),
    ];
    let at = |text: &str, from: &[LoggedDream]| dream_sent(text, from).map(|d| d.at);
    let carried =
        "What if b?\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> **t**";
    assert_eq!(at(carried, &dreams), Some(2.));
    let header = "CSR's dreamer asked this while you worked: “What if a?” Its reasoning: t.";
    assert_eq!(at(header, &dreams), Some(1.));
    assert_eq!(
        at("What if b? I wonder", &dreams),
        None,
        "no header: the person's own words"
    );
    let prose = "I read \"CSR's dreamer asked this while you worked\" in the log";
    assert_eq!(at(prose, &dreams), None);
    // v2 writes a question's own quotes straight; the dream is still found, from the pane and
    // the band.
    let line = "What if the pane said “fresh” first?";
    let fresh = [logged_dream(line, 3.)];
    let pane = "CSR's dreamer asked this while you worked, quoted: “What if the pane said \
        \"fresh\" first?” Its reasoning: “t.”";
    assert_eq!(at(pane, &fresh), Some(3.));
    assert_eq!(at(&ask_claude(line, "t", "x"), &fresh), Some(3.));
    assert_eq!(
        at(&format!("2{}", ask_claude("What if b?", "t", "x")), &dreams),
        Some(2.)
    );
    let nope = format!("{}\n\nnope, overdreaming", ask_claude(line, "t", "x"));
    assert_eq!(words_beside(&nope), "nope, overdreaming");
    assert_eq!(words_beside(&format!("{pane} no thanks")), "no thanks");
}

// ---------------------------------------------------------------- what a look keeps

#[tokio::test]
async fn a_look_keeps_how_long_it_took_and_why_a_fork_came_back_empty_a_chapter_says() {
    let r = rig(World {
        fork_reason: Some("api-error".into()),
        ..world(Some(true))
    });
    let cx = r.cx();
    r.start(&cx).await;
    r.go(12).await;
    assert!(until(|| r.outcomes("none") > 0).await);
    let none = r.find(|record| record["outcome"] == "none").unwrap();
    assert_eq!(none["reason"], "api-error");
    assert_eq!(none["took_ms"], json!(0));
    let budget = r.find(|record| record["outcome"] == "budget").unwrap();
    assert_eq!(budget["took_ms"], json!(0));
    assert!(budget.get("reason").is_none());
    let chapter = r.find(|record| record["kind"] == "chapter").unwrap();
    assert_eq!(chapter["interactive"], json!(true));
}

#[test]
fn a_near_twin_of_a_dream_already_offered_is_a_repeat_the_closest_unrelated_pair_is() {
    assert!(jaccard(&entry(HEARTS_1), &entry(HEARTS_2)) >= NEAR);
    assert!(near(HEARTS_2, &entry(HEARTS_1)));
    assert!(!near(BROCHURE, &entry(INKS)));
    // The exact match still holds, old entries too.
    assert!(near_dream(
        &HEARTS_1.0.to_uppercase(),
        Some("x"),
        HEARTS_1.0
    ));
}

#[tokio::test]
async fn a_twin_of_a_seen_dream_is_logged_as_a_repeat_and_never_shown() {
    let twin = reply_of(
        HEARTS_2.0,
        HEARTS_2.1,
        "The hearts you give could teach the dreamer.",
    );
    let r = rig(replies(&[twin]));
    r.put(MACHINE, "seenDreams", json!([entry(HEARTS_1)]));
    r.go(12).await;
    assert!(until(|| r.outcomes("repeat") > 0).await);
    let paid = r
        .find(|record| record["kind"] == "look" && record["outcome"] == "repeat")
        .unwrap();
    assert_eq!(paid["tokens"], num(LOOK));
    assert_eq!(r.up(), None);
    assert_eq!(r.row(MACHINE, "seenDreams"), json!([entry(HEARTS_1)]));
}

#[test]
fn a_dream_in_any_language_is_a_question_when_it_ends_on_its_question_mark_no() {
    for line in [
        DREAM_LINE,
        "Have you thought about one queue for both jobs?",
        HI_HEARTS_1.0,
        JA_NOTES_1.0,
        AR,
        MIXED_1.0,
    ] {
        assert_eq!(asked(line).as_deref(), Some(line));
    }
    // Bold and quotes off.
    assert_eq!(
        asked(&format!("**“{}”**", HI_BROCHURE.0)).as_deref(),
        Some(HI_BROCHURE.0)
    );
    assert!(
        asked(&format!("{}?", "क".repeat(239))).is_some(),
        "240 characters"
    );
    assert!(asked(&format!("{}?", "क".repeat(240))).is_none(), "241");
    assert!(
        asked(&format!("{}？", "𠮷".repeat(199))).is_some(),
        "200 characters, 399 units"
    );
    for line in [
        "none",
        "none in Hindi either",
        "What if we tested it first.",
        "क्या हो अगर हम इसे पहले आज़माएँ",
        "ノートがサイドブランチも読む。",
    ] {
        assert!(asked(line).is_none(), "{line}");
    }
    // ASCII is no sign of English: the English openings are accepted, never required.
    for line in [
        "Et si on partageait le cache entre les services ?",
        "Was wäre, wenn wir den Cache teilen?",
        "Y si compartimos la cache entre los servicios?",
        "Wenn wir den Cache teilen, was dann?",
        "Should we test it first?",
        "« Et si on partageait le cache ? »",
    ] {
        assert!(asked(line).is_some(), "{line}");
    }
    assert_eq!(
        asked("« Et si on partageait le cache ? »").as_deref(),
        Some("Et si on partageait le cache ?")
    );
    assert!(parse_dream(HI_HEARTS_1.0).is_none(), "no dream: label");
}

#[test]
fn spaced_scripts_are_told_apart_and_their_twins_caught_at_the_english_near() {
    assert!(
        !same_dream(HI_INKS.0, HI_BROCHURE.0),
        "keyed '' both, they were one dream"
    );
    assert!(!same_dream("?", "؟"));
    for (a, b) in [
        (HI_INKS, HI_BROCHURE),
        (HI_HEARTS_1, HI_INKS),
        (HI_HEARTS_2, HI_BROCHURE),
        (JA_NOTES_1, JA_INKS),
        (MIXED_1, MIXED_2),
        (MIXED_1, HI_HEARTS_1),
    ] {
        assert!(!near(b, &entry(a)), "{a:?} {b:?}");
    }
    assert!(jaccard(&entry(HI_HEARTS_1), &entry(HI_HEARTS_2)) >= NEAR);
    assert!(near(HI_HEARTS_2, &entry(HI_HEARTS_1)));
    for dream in [HI_HEARTS_1, JA_NOTES_1, MIXED_1] {
        assert!(
            near_dream(dream.0, Some("x"), &entry(dream)),
            "a true duplicate"
        );
    }
    // Character pairs of Japanese share its grammar: unspaced scripts match exactly (NFKC,
    // punctuation aside) until a filter is calibrated for them.
    let cache: Said = (
        "もしキャッシュを共有したらどうでしょうか？",
        "キャッシュ共有",
    );
    let screen: Said = ("もし画面を暗くしたらどうでしょうか？", "画面の配色");
    assert!(!near(screen, &entry(cache)));
    let ascii_mark = "もしキャッシュを共有したらどうでしょうか?";
    assert!(
        near_dream(ascii_mark, Some("別の題"), &entry(cache)),
        "？ or ?"
    );
    assert!(!near(JA_NOTES_2, &entry(JA_NOTES_1)));
    assert!(near_dream(
        &format!("{} ", JA_INKS.0),
        Some(JA_INKS.1),
        &entry(JA_INKS)
    ));
    assert!(near(HEARTS_2, &entry(HEARTS_1)), "English as before");
    assert!(!near(BROCHURE, &entry(INKS)));
}

#[test]
fn fuzzy_twins_only_where_near_is_calibrated_elsewhere_an_exact_match_punctuation() {
    let fr_cache: Said = (
        "Et si on partageait le cache entre les services ?",
        "Un cache commun",
    );
    let fr_status: Said = (
        "Et si on affichait le statut dans le panneau ?",
        "Un statut visible",
    );
    let parsed = |said: Said| {
        parse_dream(&format!(
            "dream: {}\nexplain:\n**{}**\nWhy.",
            said.0, said.1
        ))
        .map(|dream| dream.line)
    };
    for said in [fr_cache, fr_status] {
        assert!(parsed(said).is_some(), "{said:?}");
    }
    assert!(!same_dream(fr_cache.0, fr_status.0));
    assert!(!near(fr_status, &entry(fr_cache)));
    let again = "Et si on partageait le cache entre les services?";
    assert!(near_dream(again, Some("Autre"), &entry(fr_cache)));
    let ja_a: Said = (
        "もしキャッシュを共有したらどうでしょうか？",
        "キャッシュ共有",
    );
    let ja_b: Said = (
        "もしキャッシュを共有したら、どうでしょうか？",
        "キャッシュ共有",
    );
    for said in [ja_a, ja_b] {
        assert!(parsed(said).is_some(), "{said:?}");
    }
    assert!(same_dream(ja_a.0, ja_b.0));
    assert!(near(ja_b, &entry(ja_a)));
    // A space, an ideographic space, 。
    assert!(same_dream(
        "もしキャッシュを 共有したら\u{3000}どうでしょうか。",
        ja_a.0
    ));
    assert!(!same_dream("もし画面を暗くしたらどうでしょうか？", ja_a.0));
    assert!(
        near(HEARTS_2, &entry(HEARTS_1)),
        "English twins, as calibrated"
    );
    assert!(near(HI_HEARTS_2, &entry(HI_HEARTS_1)), "Hindi twins too");
}

#[tokio::test]
async fn a_hindi_dream_that_repeats_a_seen_one_is_dropped_a_distinct_one_is_shown() {
    let reply = |said: Said| reply_of(said.0, said.1, "* [session dc24d328]");
    let r = rig(replies(&[reply(HI_HEARTS_2), reply(HI_BROCHURE)]));
    r.put(
        MACHINE,
        "seenDreams",
        json!([entry(HI_HEARTS_1), entry(HI_INKS)]),
    );
    r.go(12).await;
    assert!(until(|| r.outcomes("repeat") > 0).await);
    assert_eq!(r.up(), None);
    // News: the next look is taken.
    r.out.set_brief(&format!("{BRIEF}\n{NEWS}"));
    r.go(40).await;
    assert!(until(|| r.up().is_some_and(|line| line.contains("ड्रीम्स-बंद पेन"))).await);
    let dream = r.dreams().pop().unwrap();
    assert_eq!(dream["line"], HI_BROCHURE.0);
}

// C4: receipts are checked and marked, never a gate.
#[test]
fn receipts_this_session_reads_this_session_an_unknown_session_or_commit_is_marked() {
    let brief = "* skill notes-sync: built over 2 sessions [session dc24d328]";
    let text = "On 08-26 [session dc24d328]. Here [session abcd1234]. Elsewhere [session \
        0450a37e]. Fixed in [commit f46448c], [commit 1234567].";
    let missing: HashSet<String> = ["1234567".to_string()].into();
    assert_eq!(
        mark_receipts(text, brief, "abcd1234-0f0f", &missing),
        "On 08-26 [session dc24d328]. Here [this session]. Elsewhere [session 0450a37e] (not in \
         CSR's notes: unchecked). Fixed in [commit f46448c], [commit 1234567] (not found in this \
         repo: unchecked)."
    );
    let prompt = dream_prompt(brief, &[], &[], &[], "", &[]);
    assert!(prompt.contains("never cite this session"));
    assert!(prompt.contains("marked as the person's own words"));
}

#[tokio::test]
async fn a_dream_citing_this_session_and_a_commit_the_repo_lacks_is_shown_its_receipts() {
    let body = "* We did this here [session abcd1234].\n* Landed in [commit abcdef1].";
    let r = rig_in(
        replies(&[reply_of(DREAM_LINE, TITLE, body)]),
        "abcd1234-0f0f",
    );
    r.out.commits.lock().unwrap().insert("f46448c".into());
    let cx = r.cx();
    r.steps(&cx, 12, None, None).await;
    assert!(until(|| !r.dreams().is_empty()).await);
    let text = r.dreams()[0]["text"].as_str().unwrap().to_string();
    assert!(text.contains("We did this here [this session]."), "{text}");
    assert!(
        text.contains("[commit abcdef1] (not found in this repo: unchecked)"),
        "{text}"
    );
}

// C5: the person's last three prompts.
#[tokio::test]
async fn the_person_s_last_three_prompts_reach_the_dreamer_as_already_on_the_table() {
    let r = rig(world(Some(true)));
    let cx = r.cx();
    let pasted = "the system should offer to enable dreams and be a brochure \
        <pasted_content id=\"a\">pane</pasted_content>";
    let forwarded = "Here is a note offered by a side agent:\n\n> You should know · x";
    for text in ["first ask", pasted, forwarded, "third ask", "fourth ask"] {
        r.prompt(&cx, text, "composer").await;
    }
    assert_eq!(
        r.row(SID, "asked"),
        json!([
            "the system should offer to enable dreams and be a brochure",
            "third ask",
            "fourth ask"
        ])
    );
    r.go(12).await;
    assert!(until(|| !r.forks().is_empty()).await);
    let prompt = &r.forks()[0];
    assert!(prompt.contains("already on the table"));
    assert!(prompt.contains("- the system should offer to enable dreams and be a brochure"));
    assert!(!prompt.contains("first ask"));
}

#[tokio::test]
async fn a_forwarded_card_story_or_dream_is_never_what_the_person_asked_for_a_reply_they() {
    let card = "CSR's compaction card (compacted by csr): Context: 274k → 70k tokens. Learnt: \
        Never skip validation.";
    let forward = ask_claude(
        "What if the import reused the upload queue?",
        "Reuse the queue",
        "* It backs off already.",
    );
    assert_eq!(own_prompt(card), None);
    assert_eq!(
        own_prompt(&format!("{card}\nwhy did it keep that?")).as_deref(),
        Some("why did it keep that?")
    );
    assert_eq!(
        own_prompt("CSR's story of this session, rev 2: We moved the queue."),
        None
    );
    let side = "Here is a note offered by a side agent:\n\n> Heads up · x\n>\n> **T**\n\nwhy?";
    assert_eq!(own_prompt(side).as_deref(), Some("why?"));
    let csr = "Here is a note offered by CSR (claude-self-reflect):\n\n> Heads up · y";
    assert_eq!(own_prompt(csr), None);
    let beside = format!("{forward}\n\nNo, keep the current palette.\n\n{forward}");
    assert_eq!(
        own_prompt(&beside).as_deref(),
        Some("No, keep the current palette.")
    );
    let sidebar = "CSR's compaction card (the one in the sidebar) should show tokens";
    assert_eq!(own_prompt(sidebar).as_deref(), Some(sidebar));
    let mine = "CSR's dreamer asked this while you worked yesterday, but I wrote this one";
    assert_eq!(own_prompt(mine).as_deref(), Some(mine));
    let r = rig(world(Some(true)));
    let cx = r.cx();
    for text in [card.to_string(), format!("{card}\n\nkeep the card")] {
        r.prompt(&cx, &text, "composer").await;
    }
    assert_eq!(r.row(SID, "asked"), json!(["keep the card"]));
    let kept = strings(&cx, SID, "asked");
    let types = Dream {
        line: "What if we checked types instead of validation?".into(),
        title: "t".into(),
        text: String::new(),
    };
    assert_eq!(offers_another_way(&types, &kept), None);
}

#[tokio::test]
async fn a_dream_that_offers_another_way_to_what_your_last_three_prompts_decided_is() {
    let reply = reply_of(RESTART_LINE, RESTART_TITLE, RESTART_TEXT);
    let r = rig(replies(&[reply]));
    r.put(SID, "asked", json!(DECIDED));
    r.go(12).await;
    assert!(until(|| r.outcomes("decided") > 0).await);
    let decided = r.find(|record| record["outcome"] == "decided").unwrap();
    assert_eq!(decided["against"], DECIDED[0]);
    assert_eq!(r.up(), None);
    assert!(r.forks()[0].contains("or another way to them"));
    let against = |dream: &Dream, asked: &[&str]| {
        let asked: Vec<String> = asked.iter().map(|line| line.to_string()).collect();
        offers_another_way(dream, &asked)
    };
    assert_eq!(against(&restart(), &DECIDED).as_deref(), Some(DECIDED[0]));
    assert_eq!(
        against(&restart(), &["fix the importer"]),
        None,
        "nothing it replaces was decided"
    );
    let queue = plain(
        "What if the import reused the upload queue, instead of a second one?",
        "",
    );
    assert_eq!(against(&queue, &DECIDED), None);
    let compacts = plain(
        "What if the import reused the upload queue?",
        "It compacts less.",
    );
    assert_eq!(against(&compacts, &DECIDED), None, "no other way offered");
}

#[tokio::test]
async fn a_dream_whose_reason_is_only_a_line_older_than_30_days_is_dropped_one_that() {
    let reply = reply_of(RESTART_LINE, RESTART_TITLE, RESTART_TEXT);
    let r = rig(replies(&[reply]));
    r.out.set_brief(&format!("{BRIEF}\n\n{OLD_LINES}"));
    r.go(12).await;
    assert!(until(|| r.outcomes("stale") > 0).await);
    assert_eq!(r.up(), None);
    assert!(r.forks()[0].contains("context only"));
    let old = |dream: &Dream, brief: &str, session: &str| leans_on_old(dream, brief, session);
    assert!(old(&restart(), OLD_LINES, "sess-1"));
    let quoted = Dream {
        text: "You wrote \"you kill a session, you restart\" once.".into(),
        ..restart()
    };
    assert!(old(&quoted, OLD_LINES, "sess-1"), "its words, no receipt");
    let more = |extra: &str| Dream {
        text: format!("{RESTART_TEXT}\n{extra}"),
        ..restart()
    };
    let commit = more("* The recap landed in [commit 19e7d46].");
    assert!(!old(&commit, OLD_LINES, "sess-1"));
    let newer = more("* On 10-08 you asked for retries [session 3f2a91c0].");
    assert!(!old(&newer, OLD_LINES, "sess-1"));
    let here = more("* Here [session abcd1234].");
    assert!(
        old(&here, OLD_LINES, "abcd1234-0f0f"),
        "this session is no newer reason"
    );
    assert!(
        !old(&restart(), BRIEF, "sess-1"),
        "no old line in the brief"
    );
}

// ---------------------------------------------------------------- the chapter

#[tokio::test]
async fn each_chapter_s_first_look_is_taken_even_when_csr_s_notes_hold_nothing_new_later() {
    let none = "dream: none".to_string();
    let r = rig(replies(&[none.clone(), none.clone(), none]));
    r.put(SID, "briefFacts", json!(facts(BRIEF))); // the dreamer has seen every note already
    r.go(12).await;
    assert!(until(|| !r.forks().is_empty()).await);
    assert_eq!(r.forks().len(), 1, "the chapter's fresh look");
    r.go(30).await; // the next look is due at 34: nothing new, so no fork
    assert!(until(|| r.outcomes("quiet") >= 1).await);
    assert_eq!(r.forks().len(), 1);
    let cx = r.cx();
    r.compact(&cx, "auto").await;
    r.go(12).await;
    assert!(until(|| r.forks().len() >= 2).await);
    assert_eq!(r.forks().len(), 2, "a new chapter: a fresh look again");
}

#[tokio::test]
async fn a_look_still_running_when_a_compaction_starts_the_next_chapter_leaves_that() {
    let r = rig(replies(&[dream_reply()]));
    let (cx, gate) = r.gated();
    r.steps(&cx, 12, None, None).await;
    assert!(until(|| gate.arrived() >= 1).await);
    r.compact(&cx, "auto").await;
    assert!(
        until(|| r
            .find(|c| c["kind"] == "chapter" && c["why"] == "compact")
            .is_some())
        .await
    );
    let fresh = r.row(SID, "chapter");
    gate.open();
    assert!(until(|| r.outcomes("stale") > 0).await);
    assert_eq!(
        r.row(SID, "chapter"),
        fresh,
        "due, looks and shown as the compaction set them"
    );
    assert!(!r.band().await.drew, "the stale look shows nothing");
    let stale = r.find(|record| record["outcome"] == "stale").unwrap();
    assert!(stale["tokens"].is_number());
}
