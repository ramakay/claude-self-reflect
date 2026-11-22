use super::*;

#[test]
fn weekly_caption_raster_uses_label_and_csr_inks_on_one_line() {
    let f = Fonts::load();
    let mut data = Composition::build(&json!({}), &json!({"id":"caption"}), 1000.);
    data.csr_cost.share = Some(0.9);
    let q = json!({"code":"7D","used":57});
    assert_eq!(quota_caption(&data, &q), "7D · CSR 0.9 pts");
    for theme in [Theme::Dark, Theme::Light] {
        let t = theme.tokens();
        let mut c = Canvas::new(240., 60.);
        c.background(t);
        weekly_caption(&mut c, &f, &data, &q, 120., 30., 200., 12., t);
        for ink in [t.quiet, t.csr_share] {
            let pixels =
                c.pm.data()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|p| (0..3).all(|i| (i16::from(p[i]) - i16::from(ink[i])).abs() < 20))
                    .count();
            assert!(pixels > 3, "{theme:?}: missing caption ink {ink:?}");
        }
    }
}
/// Rule 12's threshold: Tools or Context at half the session or more names it; anything
/// else, an Other-majority session included, is "mixed" (the owner's three words).
#[test]
fn text_monologue_rows_are_tagged_with_their_own_thread_through_wrapping_and_scrolling() {
    let now = 1_800_000_000.;
    let (mut data, s) = timeline_data();
    let thread = |key: &str, tail: &str| {
        json!({"key": key, "text": format!("Retry the upload of the nightly export bundle then verify the checksum {tail}"),
               "last_seen": now - 9000., "state": "lost", "waiting": false,
               "origin_sid": "abc-123", "times": 1})
    };
    data["threads"] = json!([thread("k1", "alpha"), thread("k2", "bravo")]);
    let c = Composition::build(&data, &s, now);
    let keyof = |tag: &Option<usize>| {
        tag.and_then(|i| c.river[i].thread.as_ref())
            .map(|t| t["key"].as_str().unwrap().to_owned())
    };
    let open = Open {
        threads: true,
        ..Open::default()
    };
    let page = text_rows(&c, 40, 90, open);
    let tags = river_row_tags(&c, 40, 90, open);
    assert_eq!(tags.len(), page.len());
    let rows_of = |tags: &[Option<usize>], k: &str| -> Vec<usize> {
        (0..tags.len())
            .filter(|i| keyof(&tags[*i]).as_deref() == Some(k))
            .collect()
    };
    let (a, b) = (rows_of(&tags, "k1"), rows_of(&tags, "k2"));
    assert!(a.len() >= 2 && b.len() >= 2, "{page:#?}");
    assert!(a.iter().all(|r| !b.contains(r)));
    // the two begin alike: the same time and the same first wrapped line
    assert_eq!(page[a[0]], page[b[0]], "{page:#?}");
    // a page too short for its rows scrolls: the first line of k1 goes off the top and its
    // continuation, still on screen, is still k1's, not the next thread's
    let head = tags.iter().position(Option::is_some).unwrap();
    let first_body = a[0] - head;
    let short = Open {
        scroll: first_body + 1,
        ..open
    };
    let scrolled = text_rows(&c, 40, 10, short);
    let scrolled_tags = river_row_tags(&c, 40, 10, short);
    assert_eq!(scrolled_tags.len(), scrolled.len());
    assert_eq!(
        keyof(&scrolled_tags[head]).as_deref(),
        Some("k1"),
        "{scrolled:#?}"
    );
    assert_eq!(scrolled[head], page[a[1]], "{scrolled:#?}");
    // headings, the footer and other pages carry no tag
    assert!(scrolled_tags[..head].iter().all(Option::is_none));
    assert!(river_row_tags(&c, 40, 10, Open::default()).is_empty());
}

#[test]
fn a_session_known_by_its_sizes_reads_by_the_same_verdict_words_and_threshold() {
    let (shares, verdict) = session_mix_of(58., 31., 11.).unwrap();
    assert_eq!(verdict, "tool-heavy");
    assert!((shares[0] - 0.58).abs() < 1e-9 && (shares[2] - 0.11).abs() < 1e-9);
    assert_eq!(session_mix_of(20., 60., 20.).unwrap().1, "context-heavy");
    assert_eq!(session_mix_of(30., 30., 40.).unwrap().1, "mixed");
    assert_eq!(session_mix_of(50., 25., 25.).unwrap().1, "tool-heavy");
    assert_eq!(session_mix_of(0., 0., 0.), None);
}

#[test]
fn the_session_mix_verdict_follows_the_half_threshold() {
    let ink = [0u8; 4];
    let mix = |tools: f64, context: f64, other: f64| {
        session_mix(&[
            ("Tool results", tools, ink),
            ("Messages", context, ink),
            ("CSR", other, ink),
        ])
        .map(|(_, v)| v)
    };
    assert_eq!(mix(50., 30., 20.), Some("tool-heavy"));
    assert_eq!(mix(49., 30., 21.), Some("mixed"));
    assert_eq!(mix(20., 50., 30.), Some("context-heavy"));
    assert_eq!(mix(10., 10., 80.), Some("mixed"));
    assert_eq!(mix(0., 0., 0.), None);
    assert_eq!(session_mix(&[("In use", 5., ink)]), None);
}

#[test]
fn timeline_rows_carry_their_kind_and_their_own_age() {
    let now = 1_800_000_000.;
    let (data, s) = timeline_data();
    let c = Composition::build(&data, &s, now);
    let rows: Vec<_> = c.river.iter().map(|r| (r.kind, r.at)).collect();
    assert_eq!(
        rows,
        vec![
            (RiverKind::Dream, now - 600.),
            (RiverKind::Commit, now - 3000.),
            (RiverKind::Compact, now - 5400.),
            (RiverKind::Write, now - 6200.),
            (RiverKind::Commit, now - 6200.),
            (RiverKind::Prompt, now - 9000.),
            (RiverKind::Commit, now - 86400.),
        ]
    );
    let lines: Vec<_> = c.river.iter().map(|r| r.line.as_str()).collect();
    assert!(lines[0].starts_with("What if"), "{lines:?}");
    assert_eq!(lines[2], "Compacted");
    assert!(lines
        .iter()
        .all(|l| !l.starts_with(['☾', '♻', '↳', '○', '✓'])));
}

#[test]
fn the_threads_drilldown_draws_one_icon_per_row_and_no_gradient() {
    let now = 1_800_000_000.;
    let (data, s) = timeline_data();
    let mut c = Composition::build(&data, &s, now);
    c.river.push(river_row(
        now - 700.,
        RiverKind::Agent,
        "",
        "Scan the repo started".into(),
    ));
    c.river.push(river_row(
        now - 800.,
        RiverKind::Other,
        "",
        "Something else".into(),
    ));
    c.river.sort_by(|a, b| b.at.total_cmp(&a.at));
    c.requests = (0..20)
        .map(|i| (150_000. + f64::from(i) * 1000., 2_000.))
        .collect();
    let open = Open {
        threads: true,
        ..Open::default()
    };
    let png = std::env::var_os("CSR_THREADS_PNG");
    for theme in [Theme::Dark, Theme::Light] {
        let (pm, _) = draw(&c, 1210, 1700, theme, open, false).expect("instrument draws");
        if let Some(dir) = &png {
            let name = format!("{}/threads-{theme:?}.png", dir.to_string_lossy());
            save(&name, &pm);
        }
    }
}

#[test]
fn time_labels_follow_the_local_day() {
    let tz = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
    let now = at_local(15, 0, 21);
    let label = |at: f64| time_label_in(at, now, &tz);
    assert_eq!(label(now), "just now");
    assert_eq!(label(now - 59.), "just now");
    assert_eq!(label(now + 30.), "just now");
    assert_eq!(label(now - 60.), "1 min ago");
    assert_eq!(label(now - 59. * 60.), "59 min ago");
    assert_eq!(label(now - 3599.), "59 min ago");
    // An hour or more, still today: the clock time in the local zone.
    assert_eq!(label(now - 3600.), "14:00");
    assert_eq!(label(at_local(0, 5, 21)), "00:05");
    // Yesterday and older carry the date.
    assert_eq!(label(at_local(23, 30, 20)), "Oct 20 23:30");
    assert_eq!(label(at_local(9, 7, 3)), "Oct 3 09:07");
    // Crossing midnight: 1.5 hours ago is still yesterday.
    let early = at_local(0, 30, 21);
    assert_eq!(
        time_label_in(at_local(23, 0, 20), early, &tz),
        "Oct 20 23:00"
    );
    // The same instant reads differently in another zone's day.
    let utc = chrono::Utc;
    assert_eq!(time_label_in(at_local(1, 0, 21), now, &utc), "Oct 20 23:00");
}

#[test]
fn every_row_kind_has_its_mood_word() {
    let word = |kind, lead| mood(kind, lead);
    assert_eq!(word(RiverKind::Prompt, "↳ "), "asked");
    assert_eq!(word(RiverKind::Dream, "☾ "), "wondered");
    assert_eq!(word(RiverKind::Compact, "♻ "), "made room");
    assert_eq!(word(RiverKind::Write, "~ "), "tinkered");
    assert_eq!(word(RiverKind::Commit, "~ "), "landed");
    assert_eq!(word(RiverKind::Agent, "○ "), "delegated");
    assert_eq!(word(RiverKind::Agent, "✓ "), "heard back");
    assert_eq!(word(RiverKind::Agent, "✗ "), "no answer");
    assert_eq!(word(RiverKind::Write, "! "), "worried");
    assert_eq!(word(RiverKind::Commit, "! "), "worried");
    assert_eq!(word(RiverKind::Other, ""), "");
}

/// The Monologue page's text rows that name the "Scan the repo" agent, for one agent started 30
/// min before a fixed `now`, quiet for `quiet` seconds, in `status`, with or without a completion.
fn agent_timeline_rows(status: &str, quiet: f64, done: bool) -> Vec<String> {
    let now = 1_800_000_000.;
    let ms = |t: f64| (t * 1000.) as i64;
    let completion = if done {
        json!({"key":"k","turnId":"t","at":ms(now - 30.),"reason":"answer",
            "answer":"Found it","disposition":"looked"})
    } else {
        Value::Null
    };
    let agent = json!({
        "id":"a1","parent":null,"description":"Scan the repo","type":"Explore","model":null,
        "status":status,"started":ms(now - 1800.),"first_seen":ms(now - 1800.),
        "last_at":ms(now - quiet),"tools":3,"tokens":1200,"edits":[],"edits_omitted":0,
        "overlaps":[],"overlaps_omitted":0,"last":"Reading","last_tool":"Read",
        "current_turn":"turn-1","completion":completion,"listed":true,
    });
    let data = json!({"agents":{"agents":[agent],"omitted_agents":0,"at":ms(now)}});
    let s = json!({"id":"tl","compacts":[],"main":[],"prompts":[]});
    let c = Composition::build(&data, &s, now);
    let open = Open {
        threads: true,
        ..Open::default()
    };
    page_text_rows(&c, 100, 40, open)
        .into_iter()
        .filter(|l| l.contains("Scan the repo"))
        .collect()
}

#[test]
fn a_running_agent_still_active_reads_delegated() {
    assert_eq!(
        agent_timeline_rows("running", 5., false),
        ["30 min ago · ○ delegated · Scan the repo started"]
    );
}

#[test]
fn a_running_agent_quiet_for_600_seconds_reads_no_answer() {
    assert_eq!(
        agent_timeline_rows("running", 600., false),
        ["30 min ago · ✗ no answer · Scan the repo started"]
    );
    // One second short of the stall threshold it is still delegated.
    assert_eq!(
        agent_timeline_rows("running", 599., false),
        ["30 min ago · ○ delegated · Scan the repo started"]
    );
}

#[test]
fn a_failed_agent_with_no_completion_reads_no_answer() {
    assert_eq!(
        agent_timeline_rows("failed", 5., false),
        ["30 min ago · ✗ no answer · Scan the repo started"]
    );
}

#[test]
fn a_killed_agent_with_no_completion_reads_no_answer() {
    assert_eq!(
        agent_timeline_rows("killed", 5., false),
        ["30 min ago · ✗ no answer · Scan the repo started"]
    );
}

#[test]
fn a_completed_agent_with_no_completion_record_reads_no_answer() {
    assert_eq!(
        agent_timeline_rows("completed", 5., false),
        ["30 min ago · ✗ no answer · Scan the repo started"]
    );
}

#[test]
fn a_completed_agent_with_a_completion_is_unchanged() {
    let rows = agent_timeline_rows("completed", 5., true);
    assert_eq!(rows.len(), 2);
    assert!(rows.contains(&"30 min ago · ○ delegated · Scan the repo started".to_string()));
    assert!(rows.contains(&"just now · ✓ heard back · Scan the repo finished".to_string()));
}

#[test]
fn a_failed_agent_that_did_complete_keeps_its_delegated_row() {
    // Its completion record is its answer, even a failed one: no "no answer" while one exists.
    let rows = agent_timeline_rows("failed", 5., true);
    assert!(rows.contains(&"30 min ago · ○ delegated · Scan the repo started".to_string()));
}

#[test]
fn a_write_row_is_dated_by_its_files_last_edit_never_the_maps_time() {
    let now = 1_800_000_000.;
    let (data, mut s) = timeline_data();
    // No per-file edit times: no label, but the row keeps its sort position at the map's time.
    let row = write_row(&data, &s, now);
    assert_eq!(row.when, None);
    assert_eq!(row.at, now - 6200.);
    assert_eq!(row.time_label(now), "");
    // The file was last edited 20 minutes ago.
    s["edit_at"] = json!({"/repo/csr-engine/src/summarizer.rs": now - 1200.});
    let row = write_row(&data, &s, now);
    assert_eq!(row.when, Some(now - 1200.));
    assert_eq!(row.at, now - 1200.);
    assert_eq!(row.time_label(now), "20 min ago");
    // A read-only touch in `last` dates nothing.
    s["edit_at"] = json!({});
    s["last"] = json!({"/repo/csr-engine/src/summarizer.rs": now - 60.});
    assert_eq!(write_row(&data, &s, now).when, None);
    // A different file with a similar name does not match.
    s["edit_at"] = json!({"/repo/csr-engine/src/not_summarizer.rs": now - 60.});
    assert_eq!(write_row(&data, &s, now).when, None);
    // Two files answering to one scope are ambiguous: borrow neither.
    s["edit_at"] = json!({"/a/summarizer.rs": now - 60., "/b/summarizer.rs": now - 90.});
    assert_eq!(write_row(&data, &s, now).when, None);
}

#[test]
fn rows_without_a_time_of_their_own_show_none() {
    let now = 1_800_000_000.;
    let (data, s) = timeline_data();
    let c = Composition::build(&data, &s, now);
    // "Dated by neither" names a commit the session never saw: sorted by the map, not labelled.
    let unmatched = c
        .river
        .iter()
        .find(|r| r.line.starts_with("summarizer.rs · Dated by neither"))
        .expect("row");
    assert_eq!(unmatched.kind, RiverKind::Commit);
    assert_eq!(unmatched.when, None);
    // Every other row carries its own time.
    assert!(c
        .river
        .iter()
        .filter(|r| r.when.is_none())
        .all(|r| r.line.contains("Dated by neither") || r.kind == RiverKind::Write));
}

#[test]
fn a_lost_thread_row_carries_its_conversation_into_the_picture_and_opens_a_handoff() {
    let now = 1_800_000_000.;
    let (mut data, s) = timeline_data();
    let thread = |key: &str, text: &str, state: &str| {
        json!({"key": key, "text": text, "last_seen": now - 9000., "state": state,
               "waiting": false, "origin_sid": "abc-123", "times": 1})
    };
    data["threads"] = json!([
        thread("k1", "Retry the upload once the hub is back", "lost"),
        thread("k2", "Faded away", "faded"),
        thread("k3", "Carried over", "carried"),
    ]);
    let c = Composition::build(&data, &s, now);
    let rows: Vec<_> = c.river.iter().filter(|r| r.thread.is_some()).collect();
    assert_eq!(rows.len(), 2, "lost, open and carried threads only");
    let (plain, m) = draw(
        &c,
        1210,
        1700,
        Theme::Dark,
        Open {
            threads: true,
            ..Open::default()
        },
        false,
    )
    .expect("draws");
    let lost: Vec<_> = m.cuts.iter().filter(|c| c.lost.is_some()).collect();
    assert_eq!(lost.len(), 2, "{:?}", m.cuts);
    assert_eq!(lost[0].lost.as_ref().unwrap()["origin_sid"], "abc-123");
    assert!(
        m.cuts
            .iter()
            .all(|c| c.title != "Monologue" || c.lost.is_none()),
        "a thread row is not also a plain cut row"
    );
    // the popup floats a page over the pane, and its text rows name the thread
    let mut c = c;
    c.lost = lost.iter().map(|c| c.lost.clone().unwrap()).collect();
    let at = PopupAt {
        card: ObsCard::Lost,
        index: 0,
        copied: false,
        note: None,
        scroll: 0,
    };
    let open = Open {
        popup: Some(at),
        ..Open::default()
    };
    let rows = text_rows(&c, 60, 40, open);
    assert_eq!(rows[0], "Lost and found · 1 of 2");
    assert_eq!(rows[2], "Retry the upload once the hub is back");
    let (popup, pm) = draw(&c, 1210, 1700, Theme::Dark, open, false).expect("draws");
    assert_ne!(plain.data(), popup.data());
    let keys: Vec<_> = pm.popup_keys.iter().map(|k| k.0).collect();
    assert_eq!(keys, ["c", "s", "tab", "esc"]);
}

#[test]
fn a_thread_row_says_asked_only_when_it_waits_on_the_user() {
    let now = 1_800_000_000.;
    let (mut data, s) = timeline_data();
    let thread = |text: &str, state: &str, waiting: bool| json!({"text": text, "last_seen": now - 9000., "state": state, "waiting": waiting});
    let mut footer = thread("Compaction footer", "open", false);
    footer["text"] = json!(
        "If you need specific details from before compaction (like exact code \
             snippets), read the full transcript at: /x/y.jsonl"
    );
    data["threads"] = json!([
        thread("Waits on the user", "lost", true),
        thread("Open and quiet", "open", false),
        thread("Lost and quiet", "lost", false),
        thread("Faded away", "faded", false),
        thread("Carried over", "carried", false),
        footer,
    ]);
    let c = Composition::build(&data, &s, now);
    let moods: Vec<_> = c
        .river
        .iter()
        .filter(|r| r.kind == RiverKind::Prompt)
        .map(|r| {
            (
                r.mood(),
                r.line.split(" · ").next().unwrap_or("").to_owned(),
            )
        })
        .collect();
    assert_eq!(
        moods,
        vec![
            ("asked", "Waits on the user".to_owned()),
            ("left open", "Open and quiet".to_owned()),
            ("left open", "Lost and quiet".to_owned()),
            ("noted", "Faded away".to_owned()),
            ("noted", "Carried over".to_owned()),
        ],
        "the compaction footer row is dropped, the rest keep their order"
    );
}

#[test]
fn claude_orange_stays_visibly_apart_from_the_warning_amber_in_both_themes() {
    for theme in [Theme::Dark, Theme::Light] {
        let t = theme.tokens();
        let claude = t.claude;
        assert!(ciede2000(claude, t.amber) >= 20., "{theme:?} vs amber");
        assert!(ciede2000(claude, t.warn) >= 20., "{theme:?} vs warn");
        assert!(
            ciede2000(claude, t.csr_share) >= 20.,
            "{theme:?} vs CSR pink"
        );
    }
}

#[test]
fn the_agents_bar_warns_only_of_stalled_agents_and_names_one_of_them() {
    let now = 1_800_000_000.;
    let stalled_at = 1_799_999_000_000_i64;
    let live_at = 1_799_999_990_000_i64;
    let said = |a: &[agents::Agent]| agents_bar(a, now).0;
    // Nothing stalled: running agents, plainly. No warning word, no "stalled" at all.
    let one = [agent_at("Fresh scan", "running", live_at)];
    assert_eq!(said(&one), "1 running · Fresh scan");
    let mut two = vec![
        agent_at("Early scan", "running", live_at),
        agent_at("Late scan", "running", live_at - 5000),
    ];
    two[0].first_seen -= 100_000.;
    assert_eq!(
        said(&two),
        "2 running · Late scan",
        "the newest agent is named"
    );
    let waiting = [agent_at("  ", "waiting", live_at)];
    assert_eq!(said(&waiting), "1 running");
    // The live case of 2026-11-01: two agents "waiting" for 12.9 h and 20.6 h, never
    // finished, beside the one that runs. Only the running one is counted and named.
    let ago = |hours: f64| ((now - hours * 3600.) * 1000.) as i64;
    let beside = [
        agent_at("Check the relay", "waiting", ago(20.6)),
        agent_at("Fix load-flaky pane frame tests", "waiting", ago(12.9)),
        agent_at("Fix load-flaky pane frame tests", "running", live_at),
    ];
    assert_eq!(said(&beside), "1 running · Fix load-flaky pane frame tests");
    assert!(!agents_bar(&beside, now).1, "and nothing is called stalled");
    // A waiting row is counted until it has been quiet for two hours, then it is not.
    let edge = |quiet_s: f64| {
        let rows = [
            agent_at("Wait on build", "waiting", ((now - quiet_s) * 1000.) as i64),
            agent_at("Fresh scan", "running", live_at),
        ];
        agents_bar(&rows, now).0
    };
    assert_eq!(edge(2. * 3600. - 60.), "2 running · Fresh scan");
    assert_eq!(edge(2. * 3600.), "1 running · Fresh scan");
    // One stalled agent: the warning, naming that agent even when a live one is newer.
    let only = [agent_at("Scan the repo", "running", stalled_at)];
    assert_eq!(said(&only), "1 stalled · Scan the repo");
    let mixed = [
        agent_at("Old build", "running", stalled_at),
        agent_at("Fresh scan", "running", live_at),
    ];
    assert_eq!(said(&mixed), "1 stalled · Old build");
    // Several stalled: the one stuck longest is named.
    let many = [
        agent_at("Stuck a while", "running", stalled_at + 300_000),
        agent_at("Stuck longest", "running", stalled_at),
        agent_at("Fresh scan", "running", live_at),
    ];
    assert_eq!(said(&many), "2 stalled · Stuck longest");
    // A stalled agent with no name leaves no dot, and another stalled name is used if any.
    let nameless = [agent_at("  ", "running", stalled_at)];
    assert_eq!(said(&nameless), "1 stalled");
    let some = [
        agent_at("  ", "running", stalled_at),
        agent_at("Named", "running", stalled_at + 1000),
    ];
    assert_eq!(said(&some), "2 stalled · Named");
    // A finished agent is neither counted nor named.
    let done = [
        agent_at("Old build", "completed", stalled_at),
        agent_at("Stuck", "running", stalled_at),
    ];
    assert_eq!(said(&done), "1 stalled · Stuck");
}

#[test]
fn waiting_agents_quiet_for_two_hours_draw_no_agents_bar() {
    let ago = |hours: f64| ((1_800_000_000. - hours * 3600.) * 1000.) as i64;
    let region = |rows: Vec<agents::Agent>| {
        let mut c = quota_composition();
        c.now = 1_800_000_000.;
        c.agents.agents = rows;
        let (_, m) = draw(&c, 1210, 2000, Theme::Dark, Open::default(), false).expect("draws");
        m.agents_region
    };
    // The two rows of the live case: nothing runs, so no bar.
    assert!(region(vec![
        agent_at("Fix load-flaky pane frame tests", "waiting", ago(12.9)),
        agent_at("Check the relay", "waiting", ago(20.6)),
    ])
    .is_none());
    // 1 h 59 m ago it still shows.
    assert!(region(vec![agent_at("Wait", "waiting", ago(2. - 1. / 60.))]).is_some());
}

#[test]
fn the_agents_bar_is_amber_only_when_an_agent_is_stalled() {
    let stalled_at = 1_799_999_000_000_i64;
    let live_at = 1_799_999_990_000_i64;
    for theme in [Theme::Dark, Theme::Light] {
        let t = theme.tokens();
        let amber_in_bar = |rows: Vec<agents::Agent>| {
            let mut c = quota_composition();
            c.now = 1_800_000_000.;
            c.agents.agents = rows;
            let (width, avail) = (1210_u32, 2000_u32);
            let (pm, m) = draw(&c, width, avail, theme, Open::default(), false).expect("draws");
            let scale = (width as f32 / W).min(avail as f32 / m.height);
            let left = ((width as f32 - (W * scale).round().min(width as f32)) / 2.) as u32;
            let (y, h) = m.agents_region.expect("an agents bar");
            ink_px(&pm, (y, y + h), scale, left, (W * scale) as u32, t.amber)
        };
        assert_eq!(
            amber_in_bar(vec![agent_at("Fresh scan", "running", live_at)]),
            0,
            "{theme:?}: a bar with nothing stalled has no warning ink"
        );
        assert!(
            amber_in_bar(vec![agent_at("Old build", "running", stalled_at)]) > 40,
            "{theme:?}: a stalled agent is drawn in the warning ink"
        );
    }
}

#[test]
fn a_tapped_circle_lists_only_its_own_facts() {
    let c = quota_composition();
    let all = figure_text_rows(&c, 100, true, None).join("\n");
    for code in ["5H 18%", "7D 45%", "30m left", "Codex 7D 41%"] {
        assert!(all.contains(code), "{code}: {all}");
    }
    for (gauge, has, lacks) in [
        (Gauge::FiveHour, "5H 18%", "7D 45%"),
        (Gauge::Week, "7D 45%", "5H 18%"),
        (Gauge::Cache, "30m left", "5H 18%"),
        (Gauge::Codex, "Codex 7D 41%", "7D 45%"),
    ] {
        let own = figure_text_rows(&c, 100, true, Some(gauge)).join("\n");
        assert!(own.contains(has), "{gauge:?}: {own}");
        assert!(!own.contains(lacks), "{gauge:?}: {own}");
        assert!(!own.contains("CTX"), "{gauge:?}: {own}");
    }
    // A circle that is not drawn falls back to the whole list.
    let mut bare = quota_composition();
    bare.codex_week = json!({});
    assert_eq!(
        figure_text_rows(&bare, 100, true, Some(Gauge::Codex)),
        figure_text_rows(&bare, 100, true, None)
    );
}

#[test]
fn every_drawn_circle_has_its_own_tap_and_its_swatch_sits_in_its_caption() {
    let c = quota_composition();
    for theme in [Theme::Dark, Theme::Light] {
        let t = theme.tokens();
        let (width, avail) = (1210_u32, 1800_u32);
        let (pm, m) = draw(&c, width, avail, theme, Open::default(), false).expect("draws");
        let codes: Vec<_> = m.quota_gauges.iter().map(|g| g.code.as_str()).collect();
        assert_eq!(codes, vec!["5H", "7D", "cache", "Codex 7D"], "{theme:?}");
        let taps: Vec<_> = m.quota_gauges.iter().map(|g| g.tap.expect("tap")).collect();
        for (g, tap) in m.quota_gauges.iter().zip(&taps) {
            assert!(tap.0 < g.center.0 && g.center.0 < tap.2, "{}", g.code);
            assert!(tap.1 <= g.center.1 - g.radius + 0.01, "{}", g.code);
            assert!(tap.3 > g.center.1 + g.radius, "{}", g.code);
            assert_eq!(
                Gauge::from_code(&g.code).map(Gauge::code),
                Some(g.code.as_str())
            );
        }
        for pair in taps.windows(2) {
            assert!(pair[0].2 <= pair[1].0 + 0.01, "taps overlap: {pair:?}");
        }
        // The swatch is in the caption, not in a row of its own: each slot's caption band
        // (the two caption lines under its ring) holds the colour its ring is drawn in.
        let scale = (width as f32 / W).min(avail as f32 / m.height);
        let left = (width as f32 - (W * scale).round().min(width as f32)) / 2.;
        for (i, (code, ink)) in [("5H", t.claude), ("7D", t.claude), ("cache", t.mint)]
            .into_iter()
            .enumerate()
        {
            let tap = taps[i];
            let band = (tap.3 - 2. * theme::CAPTION_LINE - S1, tap.3);
            let slot = ((tap.2 - tap.0) * scale) as u32;
            let found = ink_px(
                &pm,
                band,
                scale,
                (tap.0 * scale).max(left) as u32,
                slot,
                ink,
            );
            assert!(
                found >= 15,
                "{theme:?}: {found} {code}-coloured px in its caption"
            );
            let amber = ink_px(
                &pm,
                band,
                scale,
                (tap.0 * scale).max(left) as u32,
                slot,
                t.amber,
            );
            assert_eq!(amber, 0, "{theme:?}: {code} caption has warning amber");
        }
        // Nothing under the captions: the old legend row's band is empty of swatch colour.
        let below = (taps[0].3 + 1., taps[0].3 + NOTES_H);
        let width_px = (W * scale) as u32;
        assert_eq!(
            ink_px(&pm, below, scale, left as u32, width_px, t.claude),
            0,
            "{theme:?}: a legend row is still drawn under the captions"
        );
    }
}

#[test]
fn a_forecast_is_a_line_under_the_circles_and_no_forecast_is_no_line() {
    let with = forecast_composition("→ ~74% at reset", false);
    let hot = forecast_composition("out ~15:40", true);
    let bare = quota_composition();
    assert!(mid_min_h(&with.figure) > mid_min_h(&bare.figure));
    for theme in [Theme::Dark, Theme::Light] {
        let t = theme.tokens();
        let (width, avail) = (1210_u32, 2000_u32);
        let band_of = |c: &Composition| {
            let (pm, m) = draw(c, width, avail, theme, Open::default(), false).expect("draws");
            let scale = (width as f32 / W).min(avail as f32 / m.height);
            let left = (width as f32 - (W * scale).round().min(width as f32)) / 2.;
            (pm, m, scale, left as u32, (W * scale) as u32)
        };
        let (pm, m, scale, left, w) = band_of(&with);
        assert_eq!(m.note_rows.len(), 1, "{theme:?}");
        let row = m.note_rows[0];
        let band = (row - theme::CAPTION_LINE + S1, row + S1);
        assert!(
            ink_px(&pm, band, scale, left, w, t.quiet) >= 8,
            "{theme:?}: no forecast text in the notes line"
        );
        // The tap of each circle reaches over the line.
        for g in &m.quota_gauges {
            if matches!(g.code.as_str(), "5H" | "7D") {
                assert!(
                    g.tap.unwrap().3 >= row,
                    "{}: tap ends above the notes line",
                    g.code
                );
            }
        }
        // A forecast of a window that fills early is drawn in the warning ink.
        let (pm, m, scale, left, w) = band_of(&hot);
        let row = m.note_rows[0];
        let band = (row - theme::CAPTION_LINE + S1, row + S1);
        assert!(
            ink_px(&pm, band, scale, left, w, t.amber) >= 8,
            "{theme:?}: out is not amber"
        );
        // With nothing to say there is no line at all.
        let (pm, m, scale, left, w) = band_of(&bare);
        assert!(m.note_rows.is_empty(), "{theme:?}");
        let below = (
            m.quota_gauges[0].tap.unwrap().3 + 1.,
            m.quota_gauges[0].tap.unwrap().3 + NOTES_H,
        );
        assert_eq!(ink_px(&pm, below, scale, left, w, t.amber), 0);
    }
    // The facts say it in words, for the text pane and a tapped circle.
    let rows = figure_text_rows(&with, 100, true, None).join("\n");
    assert!(
        rows.lines()
            .any(|l| l.starts_with("5H 18%") && l.ends_with("· → ~74% at reset")),
        "{rows}"
    );
    let own = figure_text_rows(&with, 100, true, Some(Gauge::FiveHour)).join("\n");
    assert!(own.contains("→ ~74% at reset"), "{own}");
    assert!(!figure_text_rows(&bare, 100, true, None)
        .join("\n")
        .contains("at reset"));
}

#[test]
fn the_forecast_line_stays_under_the_circles_when_the_figure_is_short() {
    let c = forecast_composition("→ ~74% at reset", false);
    for open in [
        Open::default(),
        Open {
            figure: true,
            ..Open::default()
        },
    ] {
        let frames = figure_sweep(&c, open);
        assert!(frames.len() > 30, "{} frames drawn", frames.len());
        for (top, h, m) in &frames {
            // A full ring on a card with no spare room keeps the ring: the line takes only
            // room that is spare there (see `figure`). Every other card shows it.
            let spare = h - collapsed_context_layout(&c.figure, *h).2;
            if m.ring_bounds.is_some()
                && m.fresh_plots.is_empty()
                && spare < notes_h(&c.figure) + S2
            {
                continue;
            }
            assert_eq!(
                m.note_rows.len(),
                1,
                "figure={}: figure card {h:.1}pt has no notes line",
                open.figure
            );
            let row = m.note_rows[0];
            assert!(
                *top < row && row < top + h,
                "figure={}: notes line {row:.1} outside the card {top:.1}..{:.1}",
                open.figure,
                top + h
            );
        }
    }
}

#[test]
fn the_dashboard_reads_top_to_bottom_in_words_at_every_size_and_theme() {
    let c = dash_composition("ok");
    for theme in [Theme::Dark, Theme::Light] {
        for (w, h) in DASH_SIZES {
            for open in [
                Open::default(),
                Open {
                    figure: true,
                    ..Open::default()
                },
            ] {
                let at = format!("{theme:?} {w}x{h} figure={}", open.figure);
                let (_, m) = draw(&c, w, h, theme, open, false).expect("draws");
                let d = m
                    .dash
                    .as_ref()
                    .unwrap_or_else(|| panic!("{at}: no dashboard"));
                // A. one headline sentence, then B. the limits, C. the context, D. CSR.
                let (head, _) = d.headline.as_ref().expect("a headline");
                assert!(head.starts_with("On track."), "{at}: {head}");
                let codes: Vec<_> = d.limits.iter().map(|l| l.code.as_str()).collect();
                assert_eq!(codes, vec!["5H", "7D", "CX"], "{at}");
                assert!(d.limits[0].line.contains("On pace for about 46% by reset"));
                assert!(
                    d.limits[1]
                        .line
                        .starts_with("Early · last week peaked at 71%"),
                    "{at}"
                );
                for l in &d.limits {
                    let (bx, by, bw, bh) = l.bar;
                    assert!(bw > 200. && bh >= 6., "{at}: {l:?}");
                    assert!(bx >= IX && bx + bw <= IX + IW + 0.01, "{at}: {l:?}");
                    assert!((0. ..=1.).contains(&l.fill), "{at}: {l:?}");
                    assert!(by > m.figure_region.0 && by < m.figure_region.0 + m.figure_region.1);
                }
                // The forecast tick sits where the outlook said, on the bar.
                assert_eq!(d.limits[0].tick, Some(0.46));
                assert_eq!(d.limits[1].tick, None, "an early window has no tick");
                assert_eq!(d.limits[2].tick, None, "Codex has no forecast");
                assert!(
                    d.context_head
                        .as_deref()
                        .unwrap()
                        .contains("120k of 1M (12%)"),
                    "{at}"
                );
                assert!(d.context_head.as_deref().unwrap().contains("880k free"));
                let row = d.csr_row.as_deref().expect("a CSR row");
                assert!(
                    row.starts_with(
                        "Self-Reflect this session: 2 compactions · ~851k tokens saved (est.)"
                    ),
                    "{at}: {row}"
                );
                assert!(row.contains("4 recalls used"), "{at}: {row}");
                // E. the chart comes with `i`, never without it.
                if !open.figure {
                    assert!(!d.chart, "{at}");
                }
                // The card holds all of it: the figure card is inside the picture.
                let (top, ch) = m.figure_region;
                assert!(top + ch <= m.height, "{at}");
                // The facts card that repeated the circles is gone, and so are the circles.
                assert!(m.figure_detail_region.is_none(), "{at}");
                assert!(m.quota_gauges.is_empty(), "{at}");
            }
        }
    }
    // A 44-column pane tall enough for it, expanded: the chart is under the rows.
    let open = Open {
        figure: true,
        ..Open::default()
    };
    let (_, m) = draw(
        &c,
        DASH_SIZES[1].0,
        DASH_SIZES[1].1,
        Theme::Dark,
        open,
        false,
    )
    .expect("draws");
    assert!(m.dash.unwrap().chart);
    assert_eq!(m.fresh_plots.len(), 1);
}

#[test]
fn every_bar_segment_has_a_labelled_swatch_row_and_est_is_said_once() {
    let c = dash_composition("ok");
    for theme in [Theme::Dark, Theme::Light] {
        for (w, h) in DASH_SIZES {
            for open in [
                Open::default(),
                Open {
                    figure: true,
                    ..Open::default()
                },
            ] {
                let at = format!("{theme:?} {w}x{h} figure={}", open.figure);
                let (_, m) = draw(&c, w, h, theme, open, false).expect("draws");
                let d = m.dash.expect("dashboard");
                let names: Vec<_> = d.segments.iter().map(|s| s.name.as_str()).collect();
                assert_eq!(
                    names,
                    vec!["Setup", "Messages", "Tool results", "CSR injected"],
                    "{at}"
                );
                let (bx, _, bw, _) = d.context_bar.expect("the context bar");
                let mut end = bx;
                for s in &d.segments {
                    let legend = s.legend.as_deref().unwrap_or_else(|| panic!("{at}: {s:?}"));
                    // One inline legend line: a dot, the short name, the number.
                    assert!(
                        legend.starts_with(part_short_name(&s.name)) && !legend.contains('\n'),
                        "{at}: {legend}"
                    );
                    assert!(s.w >= 1.5 && s.x >= end - 0.01 && s.x + s.w <= bx + bw + 0.01);
                    end = s.x + s.w;
                }
                // Messages are the person's and Claude's text together: 24k of 120k.
                assert_eq!(d.segments[1].legend.as_deref(), Some("Messages 24k"));
                assert_eq!(d.est_marks, 1, "{at}");
            }
        }
    }
    // After a compaction the floor holds the summary too, and the legend says so, whole.
    let mut after = dash_composition("ok");
    after.figure["breakdown"]["floor_name"] = json!("Summary + setup");
    for (w, h) in DASH_SIZES {
        let (_, m) = draw(&after, w, h, Theme::Dark, Open::default(), false).unwrap();
        let d = m.dash.unwrap();
        assert_eq!(d.segments[0].name, "Summary + setup", "{w}x{h}");
        assert_eq!(
            d.segments[0].legend.as_deref(),
            Some("Summary + setup 28k"),
            "{w}x{h}"
        );
    }
    // With no turn caption the card still says est. once, and with none of the parts it
    // draws one segment and says nothing of estimates.
    let mut quiet = dash_composition("ok");
    quiet.figure["breakdown"]["caption"] = Value::Null;
    let (_, m) = draw(&quiet, 704, 1700, Theme::Dark, Open::default(), false).unwrap();
    let d = m.dash.unwrap();
    assert!(d.turn.is_none() && d.est_marks == 1);
    quiet.figure["breakdown"] = Value::Null;
    let (_, m) = draw(&quiet, 704, 1700, Theme::Dark, Open::default(), false).unwrap();
    let d = m.dash.unwrap();
    assert_eq!(d.segments.len(), 1);
    assert_eq!(d.segments[0].name, "In use");
    assert_eq!(d.est_marks, 0);
}

#[test]
fn the_headline_is_in_the_colour_of_its_state() {
    type Pick = fn(&Tokens) -> Rgba;
    let states: [(&str, Pick); 3] = [
        ("ok", |t| t.mint),
        ("amber", |t| t.amber),
        ("red", |t| t.risk),
    ];
    for (sev, pick) in states {
        for theme in [Theme::Dark, Theme::Light] {
            let t = theme.tokens();
            let c = dash_composition(sev);
            let (w, h) = (704_u32, 1020_u32);
            let (pm, m) = draw(&c, w, h, theme, Open::default(), false).expect("draws");
            let scale = (w as f32 / W).min(h as f32 / m.height);
            let left = ((w as f32 - (W * scale).round().min(w as f32)) / 2.) as u32;
            let top = m.figure_region.0 + P;
            let own = ink_px(
                &pm,
                (top, top + DASH_HEAD_LINE),
                scale,
                left,
                (W * scale) as u32,
                pick(t),
            );
            assert!(own >= 40, "{theme:?} {sev}: {own} px of its own ink");
            // And the other states' inks are not in the headline.
            for other in ["ok", "amber", "red"].into_iter().filter(|o| *o != sev) {
                let ink = match other {
                    "ok" => t.mint,
                    "amber" => t.amber,
                    _ => t.risk,
                };
                let n = ink_px(
                    &pm,
                    (top, top + DASH_HEAD_LINE),
                    scale,
                    left,
                    (W * scale) as u32,
                    ink,
                );
                assert!(n < own / 4, "{theme:?} {sev}: {n} px of {other}");
            }
        }
    }
}

#[test]
fn the_csr_row_drops_each_part_that_has_no_data() {
    let row = |edit: &dyn Fn(&mut Composition)| {
        let mut c = dash_composition("ok");
        c.csr_cost.share = Some(0.4);
        edit(&mut c);
        let (_, m) = draw(&c, 960, 1700, Theme::Dark, Open::default(), false).unwrap();
        m.dash.unwrap().csr_row
    };
    let all = row(&|_| {}).expect("a row");
    // "Self-Reflect this session: " is longer than the "CSR this session: " it replaced, so at the
    // card's width the last part (the share of the weekly limit) is the one that gives way; it
    // shows whenever the row has room for it (below). The rows are a third larger now, so at
    // this width the recalls part gives way too, and the rows below check it with the room.
    for part in ["2 compactions", "~851k tokens saved (est.)"] {
        assert!(all.contains(part), "{all}");
    }
    let no_tally = row(&|c| c.tally = Value::Null).unwrap();
    assert!(
        !no_tally.contains("compaction") && !no_tally.contains("saved"),
        "{no_tally}"
    );
    assert!(no_tally.contains("4 recalls used"));
    let no_asks = row(&|c| c.figure["asks"] = json!(0)).unwrap();
    assert!(
        !no_asks.contains("recall") && no_asks.contains("2 compactions"),
        "{no_asks}"
    );
    let only_share = row(&|c| {
        c.tally = Value::Null;
        c.figure["asks"] = json!(0);
    })
    .unwrap();
    assert!(
        only_share.contains("0.4 pts of the weekly limit"),
        "{only_share}"
    );
    let none = row(&|c| {
        c.tally = Value::Null;
        c.figure["asks"] = json!(0);
        c.csr_cost.share = None;
    });
    assert_eq!(none, None, "no data, no row and no empty label");
    // One compaction and one recall are singular.
    let one = row(&|c| {
        c.tally = json!({"by_csr": 1});
        c.figure["asks"] = json!(1);
    })
    .unwrap();
    assert!(
        one.contains("1 compaction ·") && one.contains("1 recall used"),
        "{one}"
    );
}

#[test]
fn every_admin_key_state_fits_its_credit_row_inside_the_card_at_every_size() {
    use crate::companion::api_credit::KeyNote;
    let notes = [
        ("connected", KeyNote::Connected, false, None),
        ("missing", KeyNote::Missing, false, None),
        ("rejected", KeyNote::Rejected("usr".into()), true, None),
        ("individual", KeyNote::Individual, false, None),
        ("offline", KeyNote::Offline, false, None),
        ("stale", KeyNote::Connected, true, Some(7200.)),
        // A connected row with two days the API lacks, counted from the ledger.
        ("ledger", KeyNote::Connected, false, None),
    ];
    let png = std::env::var_os("CSR_KEY_PNG");
    for (name, key, dup, stale) in notes {
        let mut c = dash_composition("ok");
        let mut credit = credit_of(231.76, 11.8, 1, 5.);
        (credit.key, credit.dup, credit.stale) = (key, dup, stale);
        if name == "ledger" {
            credit.ledger_added = vec![("on 10-27".into(), 3.), ("today".into(), 14.4)];
        }
        let row = credit.row();
        c.credit = Some(credit);
        // Every size in the dark theme, and the narrowest in the light one.
        for (theme, size) in [
            (Theme::Dark, DASH_SIZES[0]),
            (Theme::Dark, DASH_SIZES[1]),
            (Theme::Dark, DASH_SIZES[2]),
            (Theme::Light, DASH_SIZES[0]),
        ] {
            let (d, m) = dash_of(&c, size, theme, Open::default());
            assert_eq!(
                d.credit_row.as_deref(),
                Some(row.as_str()),
                "{name} {size:?}"
            );
            let (top, h) = m.figure_region;
            assert!(
                top + h <= m.height,
                "{name} {size:?}: the row is inside the card"
            );
        }
        if let Some(dir) = &png {
            let (pm, _) = draw(&c, 704, 1020, Theme::Dark, Open::default(), false).expect("draws");
            save(&format!("{}/{name}.png", dir.to_string_lossy()), &pm);
        }
    }
}

#[test]
fn the_api_credit_row_sits_under_the_limit_bars_and_says_it_is_by_ledger() {
    let mut c = dash_composition("ok");
    c.credit = Some(credit_of(188.25, 21.75, 0, 0.));
    for theme in [Theme::Dark, Theme::Light] {
        for size in DASH_SIZES {
            let (d, m) = dash_of(&c, size, theme, Open::default());
            assert_eq!(
                d.credit_row.as_deref(),
                Some("API credit $188.25 left by ledger \u{b7} about 61 days at this week's pace ($3.11/day)"),
                "{theme:?} {size:?}"
            );
            // Plenty of credit: the headline is still the limits'.
            assert!(d.headline.as_ref().unwrap().0.starts_with("On track."));
            let (top, h) = m.figure_region;
            assert!(top + h <= m.height, "{size:?}: the row is inside the card");
        }
    }
    // Running workers: the days count from the credit after their caps, and the caps are said.
    c.credit = Some(credit_of(29.23, 94.63, 2, 12.));
    let (d, _) = dash_of(&c, DASH_SIZES[1], Theme::Dark, Open::default());
    let row = d.credit_row.unwrap();
    assert!(
        row.contains("about 1 day at") && row.ends_with("up to $12.00 still running"),
        "{row}"
    );
}

#[test]
fn credit_under_three_days_takes_the_headline_unless_a_limit_warning_is_more_severe() {
    let headline = |sev: &str, credit: crate::companion::api_credit::Credit| {
        let mut c = dash_composition(sev);
        c.credit = Some(credit);
        dash_of(&c, DASH_SIZES[1], Theme::Dark, Open::default())
            .0
            .headline
            .unwrap()
    };
    let two_days = credit_of(29.23, 94.63, 0, 0.);
    let hours = credit_of(5., 94.63, 0, 0.);
    // Over a calm limit headline, the credit warns in amber.
    assert_eq!(
        headline("ok", two_days.clone()),
        ("API credit runs out in about 2 days".into(), "amber".into())
    );
    // An amber limit warning is no more severe than amber credit: the credit speaks.
    assert_eq!(
        headline("amber", two_days.clone()).0,
        "API credit runs out in about 2 days"
    );
    // A red limit warning stays.
    let red = headline("red", two_days.clone());
    assert!(red.0.starts_with("On track."), "{red:?}");
    assert_eq!(red.1, "red");
    // Under a day the credit is red and takes over from amber.
    assert_eq!(
        headline("amber", hours),
        (
            "API credit runs out in less than a day".into(),
            "red".into()
        )
    );
}

#[test]
fn no_credit_on_record_no_row_and_no_spend_in_a_week_no_days_figure() {
    let c = dash_composition("ok");
    assert_eq!(
        dash_of(&c, DASH_SIZES[1], Theme::Dark, Open::default())
            .0
            .credit_row,
        None
    );
    let mut idle = dash_composition("ok");
    idle.credit = Some(credit_of(29.23, 0., 0, 0.));
    let (d, _) = dash_of(&idle, DASH_SIZES[1], Theme::Dark, Open::default());
    assert_eq!(
        d.credit_row.as_deref(),
        Some("API credit $29.23 left by ledger \u{b7} no spend in 7 days")
    );
    assert!(
        d.headline.unwrap().0.starts_with("On track."),
        "no days figure, no warning"
    );
}

#[test]
fn csr_this_session_is_a_ring_to_each_measured_value_with_its_words_under_it() {
    let c = {
        let mut c = dash_composition("ok");
        c.csr_cost.share = Some(0.4);
        c
    };
    for theme in [Theme::Dark, Theme::Light] {
        for size in DASH_SIZES {
            let at = format!("{theme:?} {size:?}");
            let (d, m) = dash_of(&c, size, theme, Open::default());
            let rings: Vec<_> = d
                .csr_rings
                .iter()
                .map(|r| (r.number.as_str(), r.label.as_str()))
                .collect();
            assert_eq!(
                rings,
                [
                    ("2", "2 compactions"),
                    ("~851k", "~851k tokens saved (est.)"),
                    ("4", "4 recalls used"),
                    ("0.4", "0.4 pts of the weekly limit"),
                ],
                "{at}"
            );
            // A count has no fraction: a full ring. The weekly share is a real one.
            let fills: Vec<_> = d.csr_rings.iter().map(|r| r.fill).collect();
            assert_eq!(fills[..3], [None, None, None], "{at}");
            assert!((fills[3].unwrap() - 0.004).abs() < 1e-6, "{at}: {fills:?}");
            let (top, h) = m.figure_region;
            for pair in d.csr_rings.windows(2) {
                assert!(
                    pair[1].center.0 - pair[0].center.0 >= 2. * pair[0].radius,
                    "{at}: rings overlap"
                );
            }
            for r in &d.csr_rings {
                assert!(
                    r.center.0 - r.radius >= IX && r.center.0 + r.radius <= IX + IW,
                    "{at}: {r:?}"
                );
                assert!(
                    r.center.1 - r.radius > top && r.center.1 + r.radius < top + h,
                    "{at}: {r:?}"
                );
            }
            assert!(
                d.csr_row
                    .as_deref()
                    .unwrap()
                    .starts_with("Self-Reflect this session: 2 compactions"),
                "{at}"
            );
        }
    }
}

#[test]
fn a_value_with_no_measure_gets_no_ring_and_no_values_no_heading() {
    let mut c = dash_composition("ok");
    c.tally = Value::Null;
    c.csr_cost.share = None;
    let (d, _) = dash_of(&c, DASH_SIZES[1], Theme::Dark, Open::default());
    assert_eq!(d.csr_rings.len(), 1, "only the recalls were measured");
    assert_eq!(d.csr_rings[0].label, "4 recalls used");
    c.figure["asks"] = json!(0);
    let (d, _) = dash_of(&c, DASH_SIZES[1], Theme::Dark, Open::default());
    assert!(d.csr_rings.is_empty() && d.csr_row.is_none());
}

#[test]
fn every_heading_is_the_serif_at_one_size_and_fits_beside_its_count() {
    let fonts = fonts().unwrap();
    let f = &*fonts;
    for theme in [Theme::Dark, Theme::Light] {
        let t = theme.tokens();
        let st = heading(t);
        assert!(
            matches!(st.fam, Fam::Serif),
            "headings are the dreamer's serif"
        );
        assert_eq!(st.size, theme::TITLE_SIZE);
        const {
            assert!(
                theme::TITLE_SIZE >= 16.,
                "bigger than the 13pt sans they were"
            );
            assert!(theme::TITLE_LINE > theme::TITLE_SIZE);
        }
        let counts = f.width("5 \u{b7} o opens", sans(theme::CAPTION_SIZE, t.regular));
        for title in [
            "Self-Reflect's dreamer",
            "The dreamer asks \u{b7} asked while you were away",
            CONFUSED_TITLE,
            SHOULD_BE_TITLE,
            "Where the work stands",
            "CSR performance",
            "Self-Reflect this session",
            "Context",
        ] {
            let w = 14. + ICON_GAP + f.width(title, st);
            assert!(w + counts + S4 < IW, "{title}: {w}pt of {IW}pt");
        }
    }
}

#[test]
fn the_waiting_memory_trend_line_fits_the_card_whole() {
    let text = "Memory trend: waiting for 6 h of samples (have 0.7 h)";
    {
        // `draw` takes the same font lock: let go of it before drawing.
        let fonts = fonts().unwrap();
        let t = Theme::Dark.tokens();
        let w = fonts.width(text, sans(11., t.regular));
        assert!(w <= IW - 16., "{w}pt of {}pt", IW - 16.);
    }
    let mut card = soak_card(5);
    card.rows[1] = crate::companion::live_eval::Row {
        mark: "\u{2026}".into(),
        sev: "quiet".into(),
        text: text.into(),
    };
    let mut c = dash_composition("ok");
    c.soak = Some(card);
    let (pm, m) = draw(&c, 704, 1800, Theme::Dark, Open::default(), false).expect("draws");
    assert!(m.soak_region.is_some());
    if let Some(dir) = std::env::var_os("CSR_MEMTREND_PNG") {
        save(
            &format!("{}/memtrend-waiting.png", dir.to_string_lossy()),
            &pm,
        );
    }
}

#[test]
fn the_performance_card_sits_right_above_the_context_card_at_every_size_and_theme() {
    let mut c = dash_composition("ok");
    for theme in [Theme::Dark, Theme::Light] {
        for size in DASH_SIZES {
            c.soak = None;
            let (_, bare) = draw(&c, size.0, size.1, theme, Open::default(), false).unwrap();
            assert_eq!(bare.soak_region, None, "no trial, no card");
            c.soak = Some(soak_card(5));
            let (_, m) = draw(&c, size.0, size.1, theme, Open::default(), false).unwrap();
            let (top, h) = m.soak_region.expect("the card is drawn");
            let (fig_top, fig_h) = m.figure_region;
            assert!(
                (top + h + S3 - fig_top).abs() < 0.01,
                "{theme:?} {size:?}: {top}+{h} vs {fig_top}"
            );
            assert!(fig_top + fig_h <= m.height + 0.01);
            assert!(
                m.dash.is_some(),
                "the Context card still draws its dashboard"
            );
            // At least its heading and its headline; the checks come with room.
            assert!(h >= 80., "{theme:?} {size:?}: {h}pt");
        }
    }
    let (_, m) = draw(&c, 704, 1800, Theme::Dark, Open::default(), false).unwrap();
    let (_, h) = m.soak_region.expect("the card is drawn");
    assert!(
        h > 150.,
        "with room: a big number, a clock and five checks: {h}pt"
    );
}

#[test]
fn a_short_pane_drops_the_card_rows_then_its_clock_and_keeps_the_headline() {
    let card_h = |card: crate::companion::live_eval::Card, h: u32| {
        let mut c = dash_composition("ok");
        c.soak = Some(card);
        let (_, m) = draw(&c, 704, h, Theme::Dark, Open::default(), false).unwrap();
        m.soak_region.expect("the card stays").1
    };
    // The taller the pane, the more of the card: rows, then clock and bar, then the headline.
    let heights: Vec<f32> = [900, 1100, 1250, 1400, 1500, 1600, 1800]
        .into_iter()
        .map(|h| card_h(soak_card(5), h))
        .collect();
    assert!(heights.windows(2).all(|w| w[0] <= w[1]), "{heights:?}");
    let (short, tall) = (heights[0], heights[heights.len() - 1]);
    assert!(
        heights.iter().any(|h| *h > short && *h < tall),
        "{heights:?}"
    );
    assert!(short >= 80., "the heading and the headline stay: {short}");
    // A failing check stays when the rows go.
    let mut failing = soak_card(5);
    failing.rows[1].sev = "red".into();
    assert!(
        card_h(failing, 900) > short,
        "a failing check keeps its row"
    );
}

#[test]
fn a_stale_warning_and_an_unreadable_trial_each_get_their_line() {
    let mut c = dash_composition("ok");
    let mut card = soak_card(2);
    let plain = {
        c.soak = Some(card.clone());
        draw(&c, 704, 1020, Theme::Dark, Open::default(), false)
            .unwrap()
            .1
            .soak_region
            .unwrap()
            .1
    };
    card.warning = Some("No new data for 35m, expected every 10m".into());
    c.soak = Some(card);
    let warned = draw(&c, 704, 1020, Theme::Dark, Open::default(), false)
        .unwrap()
        .1
        .soak_region
        .unwrap()
        .1;
    assert!(warned > plain, "{warned} vs {plain}");
    c.soak = Some(crate::companion::live_eval::unreadable(
        None,
        "gone.log not found",
        0.,
        &chrono::Utc,
    ));
    let (_, m) = draw(&c, 704, 1020, Theme::Dark, Open::default(), false).unwrap();
    assert!(
        m.soak_region.is_some(),
        "unreadable data is a card that says so, never a hole"
    );
}
