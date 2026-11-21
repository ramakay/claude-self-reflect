use super::*;

#[test]
fn the_road_line_adds_one_row_and_a_short_pane_keeps_the_card_without_it() {
    let height = |card: crate::companion::live_eval::Card, h: u32| {
        let mut c = dash_composition("ok");
        c.soak = Some(card);
        let (_, m) = draw(&c, 704, h, Theme::Dark, Open::default(), false).unwrap();
        m.soak_region.expect("the card stays").1
    };
    let mut with = soak_card(5);
    with.road = Some(crate::companion::live_eval::Row {
        mark: "\u{2026}".into(),
        sev: "quiet".into(),
        text: "Road to quality bar 4.5 \u{b7} 3 of 7 \u{b7} next: Recap judge: recaps worse".into(),
    });
    let (plain, road) = (height(soak_card(5), 1800), height(with.clone(), 1800));
    assert!(road > plain, "{road} vs {plain}");
    assert!(height(with, 900) >= 80., "the card stays on a short pane");
    let mut c = dash_composition("ok");
    let mut card = soak_card(5);
    card.road = Some(crate::companion::live_eval::Row {
        mark: "\u{2026}".into(),
        sev: "quiet".into(),
        text: "Road to quality bar 4.5 \u{b7} 3 of 7 \u{b7} next: Recap judge".into(),
    });
    c.soak = Some(card);
    let text = text_rows(&c, 80, 40, Open::default()).join("\n");
    assert!(text.contains("next: Recap judge"), "{text}");
}

#[test]
fn the_performance_card_is_scheduled_by_the_minute_and_text_rows_carry_it() {
    let mut c = dash_composition("ok");
    let mut card = soak_card(5);
    card.next_change = Some(c.now + 17.);
    c.soak = Some(card);
    assert_eq!(c.next_change_at(Open::default()), Some(c.now + 17. + 0.01));
    let text = text_rows(&c, 60, 40, Open::default()).join("\n");
    assert!(
        text.contains("3 of 5 checks passing") && text.contains("Combo soak \u{b7} ends 7:04 AM"),
        "{text}"
    );
}

#[test]
fn the_cache_row_says_warm_with_its_time_left_and_cold_with_its_cost() {
    let row = |cache: Value| {
        let mut c = dash_composition("ok");
        c.figure["cache"] = cache;
        let (_, m) = draw(&c, 704, 1700, Theme::Dark, Open::default(), false).unwrap();
        m.dash.unwrap().cache_row
    };
    let warm = row(json!({"long": "WARM · 59m left", "cold": false, "left": 3540.}));
    assert_eq!(warm.as_deref(), Some("Cache warm · 59m left"));
    let cold = row(json!({"long": "COLD · re-reads ~92k", "cold": true}));
    assert_eq!(cold.as_deref(), Some("Cache cold · re-reads ~92k"));
    // No reading, no row, and the CSR row never claims the cache was kept warm.
    assert_eq!(row(json!({"long": "--", "cold": false})), None);
    let mut c = dash_composition("ok");
    c.figure["cache"] = json!({"long": "WARM · 59m left", "cold": false});
    let (_, m) = draw(&c, 704, 1700, Theme::Dark, Open::default(), false).unwrap();
    assert!(!m.dash.unwrap().csr_row.unwrap().contains("warm"));
}

#[test]
fn the_codex_bar_shows_only_when_codex_data_exists() {
    let mut c = dash_composition("ok");
    c.codex_week = json!({});
    let (_, m) = draw(&c, 704, 1020, Theme::Dark, Open::default(), false).unwrap();
    let codes: Vec<_> = m
        .dash
        .unwrap()
        .limits
        .iter()
        .map(|l| l.code.clone())
        .collect();
    assert_eq!(codes, vec!["5H", "7D"]);
}

#[test]
fn a_figure_without_an_outlook_still_shows_its_windows_and_no_headline() {
    let mut c = dash_composition("ok");
    c.figure.as_object_mut().unwrap().remove("outlook");
    let (_, m) = draw(&c, 704, 1020, Theme::Dark, Open::default(), false).unwrap();
    let d = m.dash.unwrap();
    assert!(d.headline.is_none());
    assert_eq!(d.limits.len(), 3);
    assert!(
        d.limits[0].line.starts_with("18% used"),
        "{}",
        d.limits[0].line
    );
    assert_eq!(d.limits[0].tick, None);
}

#[test]
fn the_fit_drops_the_credit_row_after_the_lists_are_down_to_one_row_and_before_the_rings() {
    // perf rows, perf clock, lists to 3, lists to 1, credit, rings, the Context card's turn
    // note and cache line, lists fold, story folds, performance folds.
    let order: Vec<_> = BASE_STEPS
        .iter()
        .map(|s| (s.soak, s.cap, s.credit, s.compact, s.fold))
        .collect();
    let lean_from = BASE_STEPS.iter().position(|s| s.lean).unwrap();
    assert_eq!(
        lean_from, 7,
        "lean comes after the rings, before any card folds"
    );
    assert!(BASE_STEPS[lean_from..].iter().all(|s| s.lean));
    assert_eq!(
        order[..11],
        [
            (2, usize::MAX, true, false, 0),
            (1, usize::MAX, true, false, 0),
            (0, usize::MAX, true, false, 0),
            (0, 3, true, false, 0),
            (0, 1, true, false, 0),
            (0, 1, false, false, 0),
            (0, 1, false, true, 0),
            (0, 1, false, true, 0),
            (0, 1, false, true, FOLD_LISTS),
            (0, 1, false, true, FOLD_LISTS | FOLD_STORY),
            (0, 1, false, true, FOLD_ALL),
        ]
    );
}

#[test]
fn the_dashboard_fills_the_closed_card_at_the_live_44_column_size_without_a_blank_gap() {
    // The closed card at 44x30 is the dashboard in its compact form: the rows two to a row.
    let c = dash_composition("amber");
    let (_, m) = draw(&c, 704, 1020, Theme::Dark, Open::default(), false).unwrap();
    let d = m.dash.unwrap();
    assert!(!d.chart);
    let (top, h) = m.figure_region;
    // Whatever the pane dropped, the card holds the dashboard it kept: at the least, the
    // one without the credit row, with the rings as a line, and without the turn and cache rows.
    let mut plan = dash_plan(&fonts().unwrap(), &c, Theme::Dark.tokens()).unwrap();
    plan.fit(DashFit {
        credit: false,
        compact_rings: true,
        lean: true,
        bare: false,
    });
    assert!(
        h + 0.5 >= dash_compact_h(&plan),
        "{h} < compact {}",
        dash_compact_h(&plan)
    );
    assert!(top > 0. && top + h <= m.height);
}

#[test]
fn a_forecast_never_takes_the_breakdown_bars_row_in_the_mid_card() {
    let mut c = breakdown_composition();
    for q in c.figure["quotas"].as_array_mut().unwrap() {
        q["forecast"] = json!({"text": "→ ~15% at reset", "out": false});
    }
    let mut no_forecast = breakdown_composition();
    no_forecast.figure["quotas"] = c.figure["quotas"].clone();
    for q in no_forecast.figure["quotas"].as_array_mut().unwrap() {
        q.as_object_mut().unwrap().remove("forecast");
    }
    // The live 44x30 pane's closed card is 163pt tall: the mid layout, a forecast under the
    // circles, the caption and the bar all in it.
    let mut checked = 0;
    for (comp, forecast) in [(&c, true), (&no_forecast, false)] {
        for (top, h, m) in figure_sweep(comp, Open::default()) {
            // 160pt before the headings grew to 17pt serif: the card's title line is taller.
            if m.mid_layout.is_none() || h < 160. + (theme::TITLE_LINE - 18.) {
                continue;
            }
            checked += 1;
            assert_eq!(m.note_rows.len(), usize::from(forecast), "card {h:.1}pt");
            assert_eq!(
                m.breakdown_bars.len(),
                1,
                "card {h:.1}pt at {top:.1}: forecast={forecast}: no breakdown bar"
            );
        }
    }
    assert!(
        checked >= 4,
        "{checked} mid cards of 160pt or more were swept"
    );
}

#[test]
fn the_context_card_draws_what_fills_the_context_and_what_the_last_turn_added() {
    let with = breakdown_composition();
    let bare = quota_composition();
    for open in [
        Open::default(),
        Open {
            figure: true,
            ..Open::default()
        },
    ] {
        let frames = figure_sweep(&with, open);
        assert!(frames.len() > 30, "{} frames drawn", frames.len());
        let mut drawn = 0;
        for (top, h, m) in &frames {
            assert!(m.breakdown_bars.len() <= 1, "figure={}", open.figure);
            for (_, y, w, bh) in &m.breakdown_bars {
                drawn += 1;
                assert!(*w > 60., "bar {w}pt wide");
                assert!(
                    *y >= *top && y + bh <= top + h,
                    "figure={}: bar {y:.1}..{:.1} outside the card {top:.1}..{:.1}",
                    open.figure,
                    y + bh,
                    top + h
                );
            }
        }
        // Most card heights have room for it; the rest keep the words and drop the bar.
        assert!(
            drawn * 2 > frames.len(),
            "figure={}: {drawn}/{}",
            open.figure,
            frames.len()
        );
        for (_, _, m) in figure_sweep(&bare, open) {
            assert!(m.breakdown_bars.is_empty(), "no breakdown, no bar");
        }
    }
    // The bar shows CSR's share in CSR's ink, and the words are in the facts.
    for theme in [Theme::Dark, Theme::Light] {
        let t = theme.tokens();
        let (width, avail) = (1210_u32, 2000_u32);
        let (pm, m) = draw(&with, width, avail, theme, Open::default(), false).expect("draws");
        let scale = (width as f32 / W).min(avail as f32 / m.height);
        let left = ((width as f32 - (W * scale).round().min(width as f32)) / 2.) as u32;
        let (x, y, w, bh) = *m.breakdown_bars.first().expect("a bar");
        let band = (y, y + bh);
        let wide = (W * scale) as u32;
        assert!(
            ink_px(&pm, band, scale, left, wide, t.csr_share) >= 4,
            "{theme:?}: no CSR share"
        );
        assert!(
            ink_px(&pm, band, scale, left, wide, t.mint) >= 4,
            "{theme:?}: no tool results"
        );
        assert!(x >= 0. && x + w <= W);
    }
    let rows = figure_text_rows(&with, 100, true, None).join("\n");
    assert!(
        rows.contains("this turn +9k · tool results 6k · CSR 1.1k (est.)"),
        "{rows}"
    );
    assert!(!figure_text_rows(&bare, 100, true, None)
        .join("\n")
        .contains("this turn"));
}

#[test]
fn the_by_csr_caption_stays_inside_the_context_card() {
    let mut c = quota_composition();
    c.tally = json!({"by_csr": 3});
    let mut mid = 0;
    for open in [
        Open::default(),
        Open {
            figure: true,
            ..Open::default()
        },
    ] {
        for (top, h, m) in figure_sweep(&c, open) {
            mid += usize::from(m.mid_layout.is_some());
            assert_eq!(m.by_csr.len(), 1, "figure card {h:.1}pt: caption count");
            let (cap_top, cap_bottom) = m.by_csr[0];
            assert!(
                cap_top >= top && cap_bottom <= top + h,
                "figure card {top:.1}..{:.1}: caption at {cap_top:.1}..{cap_bottom:.1}",
                top + h
            );
        }
    }
    assert!(mid > 0, "the sweep never reached the mid layout");
}

#[test]
fn the_text_pane_reads_monologue_verb_and_time() {
    let now = 1_800_000_000.;
    let (data, mut s) = timeline_data();
    s["edit_at"] = json!({"/r/summarizer.rs": now - 1200.});
    let c = Composition::build(&data, &s, now);
    let open = Open {
        threads: true,
        ..Open::default()
    };
    let text = text_rows(&c, 110, 60, open);
    let head = text
        .iter()
        .position(|l| l == "Monologue · the session thinking out loud")
        .unwrap_or_else(|| panic!("{text:#?}"));
    assert!(text[head + 1].contains("wondered"), "{text:#?}");
    let all = text.join("\n");
    assert!(
        all.contains("☾ wondered · What if recall started from the file?"),
        "{all}"
    );
    assert!(all.contains("10 min ago · ☾"), "{all}");
    assert!(all.contains("♻ made room · Compacted"), "{all}");
    assert!(
        all.contains("↳ left open · Decide the release name"),
        "{all}"
    );
    assert!(
        all.contains("20 min ago · ~ tinkered · summarizer.rs"),
        "{all}"
    );
    assert!(!all.contains("h0"), "no durations: {all}");
    // The undated unlanded row has no time prefix at all.
    let (data, s) = timeline_data();
    let c = Composition::build(&data, &s, now);
    let all = text_rows(&c, 110, 60, open).join("\n");
    assert!(
        all.contains("\n~ tinkered · summarizer.rs · Unlanded edit"),
        "{all}"
    );
}

#[test]
fn earlier_here_drops_noise_and_fills_spare_room_with_starter_questions() {
    let c = fresh_with_history(4);
    let text = text_rows(&c, 100, 60, Open::default()).join("\n");
    assert!(text.contains("Last time here · 1d ago"), "{text}");
    assert!(!text.contains("64f9b5b9"), "{text}");
    assert!(text.contains("2d ago · Question number 0"), "{text}");
    assert!(text.contains("✓ 3d ago · Question number 1"), "{text}");
    assert!(!text.lines().any(|l| l.starts_with("? ")), "{text}");
    assert!(!text.contains("Question number 0 ·"), "{text}");
    let after = text.split("Earlier here").nth(1).unwrap();
    assert!(after.contains("Try a question"), "{text}");
    assert_eq!(earlier_intent("you asked: x"), "x");
    assert_eq!(earlier_intent("something"), "something");
    assert_eq!(history_mark("unknown"), None);
    let png = std::env::var_os("CSR_FRESH_PNG");
    for (name, n) in [("fresh-session", 19), ("fresh-few", 4)] {
        let c = fresh_with_history(n);
        let (pm, _) = draw(&c, 1210, 1700, Theme::Dark, Open::default(), false).expect("draws");
        if let Some(dir) = &png {
            save(&format!("{}/{name}.png", dir.to_string_lossy()), &pm);
        }
    }
}

#[test]
fn confused_and_should_be_come_from_every_look_not_just_the_newest() {
    let c = Composition::build(&observed_data(), &json!({"id":"o"}), 1000.);
    let lines = |v: &[Value]| {
        v.iter()
            .map(|i| string(&i["line"]).to_owned())
            .collect::<Vec<_>>()
    };
    // The newest look found nothing; the earlier ones still count, deduped by line.
    assert_eq!(
        lines(&c.confusing),
        ["Repeated a rejected approach.", "Misread the pane."]
    );
    assert_eq!(
        lines(&c.should_be),
        [
            "Bump the version in both files.",
            "Check formatting before commits."
        ]
    );
    let mut many = observed_data();
    many["dream_records"][1]["confusing_claude"] = json!((0..9)
        .map(|i| json!({"line":format!("Line {i}"),"evidence":"Turn 1"}))
        .collect::<Vec<_>>());
    let c = Composition::build(&many, &json!({"id":"o"}), 1000.);
    assert_eq!(c.confusing.len(), OBSERVATION_CAP);
}

#[test]
fn the_two_observation_cards_are_separate_and_leave_the_dreamer_alone() {
    let c = Composition::build(&observed_data(), &json!({"id":"o"}), 1000.);
    let open = Open {
        dreams: true,
        ..Open::default()
    };
    let text = text_rows(&c, 100, 60, open);
    let at = |needle: &str| text.iter().position(|l| l == needle);
    let confused = at(CONFUSED_TITLE).expect("confused card");
    let should = at(SHOULD_BE_TITLE).expect("should-be card");
    assert!(confused < should);
    assert_eq!(
        text[confused + 1],
        "Repeated a rejected approach. · turn 12"
    );
    assert!(
        text.contains(&"skill: Bump the version in both files. · turns 4, 8".to_owned()),
        "{text:?}"
    );
    assert!(text.contains(&"hook: Check formatting before commits. · turns 4, 8".to_owned()));
    // The dreamer's own lines carry no counts and no folded-in findings.
    assert!(text[..confused]
        .iter()
        .all(|l| !l.contains("Confusing Claude") && !l.starts_with("! ")));
    let quiet = Composition::build(&json!({}), &json!({"id":"o"}), 1000.);
    let text = text_rows(&quiet, 100, 60, Open::default());
    assert!(!text
        .iter()
        .any(|l| l == CONFUSED_TITLE || l == SHOULD_BE_TITLE));
    if let Some(dir) = std::env::var_os("CSR_OBS_PNG") {
        for (name, c) in [("observed", &c), ("quiet", &quiet)] {
            let (pm, _) = draw(c, 1210, 1700, Theme::Dark, Open::default(), false).expect("draws");
            save(&format!("{}/{name}.png", dir.to_string_lossy()), &pm);
        }
    }
}

#[test]
fn the_popup_floats_over_the_pane_and_its_text_rows_replace_the_cards() {
    let mut data = observed_data();
    data["dream_records"][1]["should_be"][0]["explain"] =
        json!("You keep restating the release steps.");
    let c = Composition::build(&data, &json!({"id":"o"}), 1000.);
    let at = |card, index| PopupAt {
        card,
        index,
        copied: false,
        note: None,
        scroll: 0,
    };
    // Text: the popup's own rows, and the dreamer's explanation when it wrote one.
    let open = Open {
        popup: Some(at(ObsCard::ShouldBe, 0)),
        ..Open::default()
    };
    let text = text_rows(&c, 80, 40, open);
    assert_eq!(
        text[0],
        "Should be a skill / memory / tool · 1 of 2 · skill"
    );
    assert_eq!(text[1], "Bump the version in both files.");
    assert!(text.contains(&"What happened: You keep restating the release steps.".to_owned()));
    assert!(text
        .iter()
        .any(|l| l.starts_with("Next step for you: Press c to copy a skill draft")));
    // An item that left its card shows the plain pane, not an empty popup.
    let gone = Open {
        popup: Some(at(ObsCard::Confusing, 7)),
        ..Open::default()
    };
    assert_eq!(
        text_rows(&c, 80, 40, gone),
        text_rows(&c, 80, 40, Open::default())
    );
    // Picture: every card row is a click target, and the popup changes the picture.
    let png = std::env::var_os("CSR_POPUP_PNG");
    for theme in [Theme::Dark, Theme::Light] {
        let (plain, metrics) = draw(&c, 1210, 1700, theme, Open::default(), false).expect("draws");
        let places: Vec<_> = metrics
            .observation_rows
            .iter()
            .map(|(c, i, _)| (*c, *i))
            .collect();
        assert_eq!(
            places,
            [
                (ObsCard::Confusing, 0),
                (ObsCard::Confusing, 1),
                (ObsCard::ShouldBe, 0),
                (ObsCard::ShouldBe, 1)
            ]
        );
        let ys: Vec<f32> = metrics.observation_rows.iter().map(|r| r.2).collect();
        assert!(ys.windows(2).all(|w| w[0] < w[1]), "{ys:?}");
        for (name, popup) in [
            ("confusing", at(ObsCard::Confusing, 1)),
            ("should-be", at(ObsCard::ShouldBe, 0)),
            (
                "copied",
                PopupAt {
                    copied: true,
                    ..at(ObsCard::ShouldBe, 0)
                },
            ),
        ] {
            let open = Open {
                popup: Some(popup),
                ..Open::default()
            };
            let (shown, with) = draw(&c, 1210, 1700, theme, open, false).expect("draws");
            assert_ne!(
                shown.data(),
                plain.data(),
                "{theme:?} {name}: the popup is drawn"
            );
            // The cards beneath keep their layout: the click targets do not move.
            assert_eq!(with.observation_rows, metrics.observation_rows);
            if let Some(dir) = &png {
                save(
                    &format!("{}/popup-{name}-{theme:?}.png", dir.to_string_lossy()),
                    &shown,
                );
            }
        }
        if let Some(dir) = &png {
            save(
                &format!("{}/cards-{theme:?}.png", dir.to_string_lossy()),
                &plain,
            );
        }
    }
}

#[test]
fn quota_money_drops_this_week_before_a_caption_can_hit_its_neighbour() {
    let fonts = Fonts::load();
    for theme in [Theme::Dark, Theme::Light] {
        let t = theme.tokens();
        let line = "$18.22 this week";
        let natural = fonts.width(line, sans(theme::CAPTION_SIZE, t.medium));
        assert_eq!(
            quota_money_caption(&fonts, line, natural + 1., theme::CAPTION_SIZE, t),
            line
        );
        assert_eq!(
            quota_money_caption(&fonts, line, natural - 1., theme::CAPTION_SIZE, t),
            "$18.22"
        );
    }
}

#[test]
fn fresh_series_uses_uncached_input_and_writes_and_suppresses_flat_history() {
    let bars = [(188_000., 2_000.), (180_000., 10_000.), (150_000., 40_000.)];
    let heights = fresh_bar_heights(&bars).unwrap();
    assert!((heights[2] - heights[0]) >= 0.4);
    assert_eq!(heights[2], 1.);
    assert!(fresh_bar_heights(&[(188_000., 2000.), (187_900., 2100.)]).is_none());
    assert!(fresh_bar_heights(&[(190_000., 0.), (190_000., 0.)]).is_none());
    assert!(fresh_bar_heights(&[(190_000., 0.), (150_000., 40000.)]).is_some());
}

#[test]
fn compaction_markers_attach_to_equal_timestamp_requests_before_tail_clipping() {
    let s = json!({"replies":(1..=30).map(|t|json!([t,190000,100,1000,188000])).collect::<Vec<_>>(),"compacts":[2.,7.,20.,31.]});
    assert_eq!(recent_request_compactions(&s), vec![0, 13]);
}

#[test]
fn story_fit_preserves_full_text_and_cuts_at_longest_boundary() {
    let fonts = Fonts::load();
    let st = sans(12.5, 400.);
    let short = "Text parity is ready for review.";
    assert_eq!(wrap_clauses(&fonts, short, st, IW, 4).join(" "), short);
    assert_eq!(story_text_lines(short, 80, 4).join(" "), short);

    let long = "First clause ends here. Second clause continues for a while. Third clause still goes. Fourth would overflow the pane budget entirely.";
    let pixel = wrap_clauses(&fonts, long, st, 220., 2).join(" ");
    let text_path = story_text_lines(long, 40, 2).join(" ");
    for joined in [&pixel, &text_path] {
        assert!(joined.ends_with('…'), "{joined}");
        assert!(joined.contains("First clause ends here."));
        assert!(
            !joined.contains("overflow the pane"),
            "must drop the overflowing clause: {joined}"
        );
    }
    // Longest fitting boundary: prefer keeping more clauses when width allows.
    let wider = wrap_clauses(&fonts, long, st, 400., 2).join(" ");
    assert!(
        wider.contains("Second clause") || wider.contains("First clause ends here."),
        "{wider}"
    );
}

#[test]
fn story_fit_handles_emdash_non_ascii_and_clause_free_overflow() {
    let fonts = Fonts::load();
    let st = sans(12.5, 400.);
    let em = "Alpha stays — beta is dropped because it is far too long for the budget here.";
    let cut = wrap_clauses(&fonts, em, st, 160., 1).join(" ");
    assert!(
        cut.ends_with("—…") || cut.ends_with("—…"),
        "keep spaced emdash: {cut}"
    );
    assert!(!cut.contains("beta is dropped"), "{cut}");
    let text_em = story_text_lines(em, 28, 1).join(" ");
    assert!(text_em.contains('—') || text_em == "…", "{text_em}");

    let ja = "最初の文です。次の文は長くて枠に入りませんでした。";
    let ja_cut = story_text_lines(ja, 12, 1).join(" ");
    assert!(ja_cut.ends_with('…') || ja_cut.ends_with("。…"), "{ja_cut}");

    // Many short words (no punctuation) so wrap exceeds the line budget without a clause cut.
    let no_clause = "aa bb cc dd ee ff gg hh ii jj kk ll mm nn oo pp qq rr ss tt";
    assert_eq!(
        wrap_clauses(&fonts, no_clause, st, 60., 1),
        vec!["…".to_string()]
    );
    assert_eq!(story_text_lines(no_clause, 8, 1), vec!["…".to_string()]);
    assert!(wrap_clauses(&fonts, no_clause, st, 60., 0).is_empty());
    assert!(story_text_lines(no_clause, 8, 0).is_empty());
}

#[test]
fn a_new_sessions_card_tells_its_start_and_opens_lost_and_found_from_its_row() {
    let c = start_composition(None);
    let (_, m) = draw(&c, 704, 986, Theme::Dark, Open::default(), false).expect("draws");
    let d = m.dash.expect("a new session draws the dashboard");
    assert!(d.no_reading);
    assert_eq!(
        d.context_head.as_deref(),
        Some("Context · no reading yet · sizes est.")
    );
    let names: Vec<_> = d.segments.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(
        names,
        ["CSR injected", "Memory"],
        "only what was measured is sized"
    );
    let note = d.start_note.expect("the note");
    assert!(
        note.contains("est.") && note.contains("unmeasured: Skills, System prompt"),
        "{note}"
    );
    assert!(
        d.handed[0].starts_with("CSR handed over: fix the pane"),
        "{:?}",
        d.handed
    );
    assert!(
        d.handed[1].starts_with("Lost and found: 2 open · Retry the upload"),
        "{:?}",
        d.handed
    );
    // The row opens the newest thread's handoff, as a monologue row does.
    let lost: Vec<_> = m.cuts.iter().filter(|c| c.lost.is_some()).collect();
    assert_eq!(lost.len(), 1, "{:?}", m.cuts);
    assert_eq!(lost[0].lost.as_ref().unwrap()["key"], "k1");
    assert_eq!(lost[0].title, "Lost and found");
    // The donut is drawn from the measured parts: all of it Context and Other, no Tools.
    let (verdict, shares, _) = d.mix.expect("a mix of the measured parts");
    assert_eq!(verdict, "context-heavy");
    assert_eq!(shares[0], 0.);
    // With a reading the live card has the Context section and the start is one line.
    let c = start_composition(Some(21_000.));
    let mut c = c;
    c.figure["ctx"] = json!({"tokens": 21_000., "size": 1_000_000., "sev": "normal"});
    let (_, m) = draw(&c, 704, 986, Theme::Dark, Open::default(), false).expect("draws");
    let d = m.dash.expect("dashboard");
    assert!(!d.no_reading);
    let row = d.start_row.expect("the started line");
    assert!(
        row.starts_with("Started at 21k: CSR 700 · Memory 3k"),
        "{row}"
    );
    assert!(row.contains("unmeasured: Skills, System prompt"), "{row}");
    assert!(row.ends_with("(est.)"), "{row}");
}

#[test]
fn a_short_panes_start_card_keeps_an_estimate_marker_with_its_numbers() {
    // A pane full of other cards: the fit drops the note under the legend (the compact
    // layout) at the sizes between "everything fits" and "the lists fold".
    let c = start_on(drill_composition(7, 4, false), None);
    let (mut full, mut compact) = (0, 0);
    for cols in [38, 40, 44, 46, 50, 60] {
        for rows in 16..=44 {
            let Some((_, m)) = draw(
                &c,
                cols * 16,
                (rows - 1) * 34,
                Theme::Dark,
                Open::default(),
                false,
            ) else {
                continue;
            };
            let Some(d) = m.dash.filter(|d| d.no_reading && !d.segments.is_empty()) else {
                continue;
            };
            // A size is on the card, so the estimate marker is, whatever the layout.
            let head = d.context_head.clone().unwrap();
            let at = format!("{cols}x{rows}: {head}");
            assert!(head.contains("est."), "{at}");
            assert!(d.est_marks > 0, "{at}");
            if d.start_note.is_some() {
                full += 1;
            } else {
                // The note under the legend went: the heading says how many are unmeasured.
                compact += 1;
                assert!(head.contains("2 unmeasured"), "{at}");
            }
        }
    }
    assert!(full > 0 && compact > 0, "full {full}, compact {compact}");
}

#[test]
fn a_new_sessions_card_with_no_start_data_names_every_part_unmeasured_and_sizes_none() {
    let c = Composition::build(&json!({}), &json!({"id":"fresh"}), 1000.);
    let (_, m) = draw(&c, 704, 986, Theme::Dark, Open::default(), false).expect("draws");
    let d = m.dash.expect("dashboard");
    assert!(d.no_reading && d.segments.is_empty() && d.mix.is_none());
    assert_eq!(
        d.start_note.as_deref(),
        Some("Nothing measured yet: CSR injected, Memory, Skills, System prompt")
    );
    assert!(d.handed.is_empty());
}

#[test]
fn the_cache_ring_redraws_once_a_minute_not_every_second() {
    let draw_at = |left: f64| {
        let mut c = Composition::build(&json!({}), &json!({"id":"cache"}), 1000.);
        c.figure["cache"] = json!({"left": left, "ttl": 3600., "cold": false, "text": "CACHE"});
        // The ring layout: a new session with no reading draws the dashboard instead.
        c.figure["layout"] = json!("rings");
        draw(&c, 704, 986, Theme::Light, Open::default(), false)
            .expect("instrument draws")
            .0
    };
    // Both read "59m": a second's sweep must not repaint the whole picture.
    assert!(draw_at(3599.).data() == draw_at(3540.).data());
    assert!(draw_at(3540.).data() != draw_at(3539.).data());
}

#[test]
fn a_running_compaction_redraws_every_ten_seconds_not_every_second() {
    let draw_at = |elapsed: f64| {
        let data = json!({"compacting": {"elapsed": elapsed, "usual": {"high": 120}}});
        let mut c = Composition::build(&data, &json!({"id":"compacting"}), 1000.);
        // The ring layout: a new session with no reading draws the dashboard instead.
        c.figure["layout"] = json!("rings");
        draw(&c, 704, 986, Theme::Light, Open::default(), false)
            .expect("instrument draws")
            .0
    };
    // Each repaint re-uploads the whole picture, and the terminal blanks it meanwhile.
    assert!(draw_at(41.).data() == draw_at(49.).data());
    assert!(draw_at(49.).data() != draw_at(50.).data());
}
#[test]
fn the_draw_key_holds_until_the_next_change_and_moves_after_it() {
    let (data, s) = timeline_data();
    let open = threads_open();
    let (mut flips, mut edges) = (0, 0);
    for step in 0..300 {
        let now = 1_800_000_000. + step as f64 * 37.;
        let c = Composition::build(&data, &s, now);
        let (key, due) = (c.draw_key(open), c.next_change_at(open).unwrap());
        assert!(due > now);
        // No label moves before the deadline, whenever it is looked at.
        for at in [now + 0.5, (now + due) / 2., due - 0.05] {
            let later = Composition::build(&data, &s, at);
            assert_eq!(later.draw_key(open), key, "key moved at +{}s", at - now);
        }
        // A minute edge of a row's age moves the key; only the day edge may not.
        if due - now < 61. {
            edges += 1;
            flips += usize::from(Composition::build(&data, &s, due + 0.05).draw_key(open) != key);
        }
    }
    assert!(edges > 20, "{edges}");
    assert_eq!(flips, edges);
    // Without the threads list open no row age is on screen: the key does not move with them.
    let c = Composition::build(&data, &s, 1_800_000_000.);
    let later = Composition::build(&data, &s, 1_800_000_000. + 61.);
    assert_eq!(c.draw_key(Open::default()), later.draw_key(Open::default()));
}

#[test]
fn the_cache_ring_and_the_notice_name_their_own_edges() {
    let now = 1_800_000_000.;
    let mut c = Composition::build(&json!({}), &json!({"id":"edges"}), now);
    c.figure["cache"] = json!({"left": 3599.5, "ttl": 3600., "cold": false});
    // The minute shown changes when 59m reads 58m: at 3540 seconds left.
    let due = c.next_change_at(Open::default()).unwrap();
    assert!((due - (now + 59.5 + 0.01)).abs() < 1e-6, "{}", due - now);
    c.figure["cache"] = json!({"left": 30., "ttl": 300., "cold": false});
    assert!((c.next_change_at(Open::default()).unwrap() - (now + 30.01)).abs() < 1e-6);
    c.figure["cache"] = json!({"left": null, "cold": true});
    assert_eq!(c.next_change_at(Open::default()), None);
    c.clocks.notice_until = Some(now + 7.);
    assert!((c.next_change_at(Open::default()).unwrap() - (now + 7.01)).abs() < 1e-6);
    c.clocks.compacting_elapsed = Some(43.);
    assert!((c.next_change_at(Open::default()).unwrap() - (now + 7.01)).abs() < 1e-6);
    c.clocks.compacting_elapsed = Some(49.);
    assert!((c.next_change_at(Open::default()).unwrap() - (now + 1.01)).abs() < 1e-6);
}

#[test]
fn a_quota_ahead_of_pace_is_the_key_not_the_pace_itself() {
    let now = 1_800_000_000.;
    let quota = |pace: f64| {
        let mut c = Composition::build(&json!({}), &json!({"id":"q"}), now);
        c.figure["quotas"] = json!([{"code":"5H","used":60.,"pace":pace,"resets":now + 9000.}]);
        c.draw_key(Open::default())
    };
    assert_eq!(quota(0.10), quota(0.11));
    assert_ne!(quota(0.10), quota(0.60));
}

/// The caption draws `»` when used is past pace, with no margin; the text row's mark has a
/// five-point one. A pace between them changes the caption and must change the key, and the
/// schedule must name the moment the pace reaches the used share.
#[test]
fn the_key_and_the_schedule_follow_the_caption_s_own_threshold() {
    let now = 1_800_000_000.;
    // A 5H window that is 58% elapsed: it resets in 7560 s.
    let quota = |pace: f64| {
        let mut c = Composition::build(&json!({}), &json!({"id":"q"}), now);
        c.figure["quotas"] = json!([{"code":"5H","used":60.,"pace":pace,"resets":now + 7560.}]);
        c
    };
    let (ahead, caught_up) = (quota(0.58), quota(0.61));
    assert_eq!(quota_caption(&ahead, &ahead.figure["quotas"][0]), "5H »");
    assert_eq!(
        quota_caption(&caught_up, &caught_up.figure["quotas"][0]),
        "5H"
    );
    assert_ne!(
        ahead.draw_key(Open::default()),
        caught_up.draw_key(Open::default())
    );
    // The pace reaches 60% of the window 360 s from now: the `»` goes then.
    let due = ahead.next_change_at(Open::default()).unwrap();
    assert!((due - (now + 360.01)).abs() < 1e-6, "due in {}", due - now);
}

/// An agent's age is drawn rounded to the nearest minute, so it moves at half minutes; the
/// schedule must name those moments, not the whole-minute edges the other ages use.
#[test]
fn an_agent_age_is_scheduled_where_its_rounding_changes() {
    let now = 1_800_000_000.;
    let open = Open {
        agents: true,
        ..Open::default()
    };
    let agent = |age: f64| {
        let mut c = Composition::build(&json!({}), &json!({"id":"a"}), now);
        let row = json!({
            "id":"a1","parent":null,"description":"scan","type":"Explore",
            "model":"claude-opus-5","status":"done","started":(now - age) * 1000.,
            "first_seen":(now - age) * 1000.,"last_at":(now - age) * 1000.,"tools":3,
            "tokens":1200,"edits":[],"edits_omitted":0,"overlaps":[],"overlaps_omitted":0,
            "last":"","last_tool":null,"current_turn":null,"completion":null,"listed":true,
        });
        c.agents.agents = vec![serde_json::from_value(row).unwrap()];
        c
    };
    for (age, in_secs) in [(29., 1.), (31., 59.), (0., 30.), (89.5, 0.5), (90.5, 59.5)] {
        let c = agent(age);
        let due = c.next_change_at(open).unwrap();
        assert!(
            (due - (now + in_secs + 0.01)).abs() < 1e-6,
            "age {age}: due in {}",
            due - now
        );
        // And the label really changes there, and not before.
        let label = |at: f64| {
            let mut later = agent(age);
            later.now = at;
            agent_lines(&later)[0].clone()
        };
        assert_ne!(label(due + 0.01), label(now), "age {age}");
        assert_eq!(label(due - 0.02), label(now), "age {age}");
    }
}

#[test]
fn a_fresh_age_reads_under_a_minute_instead_of_ticking_seconds() {
    assert_eq!(age_span(3.), "<1m");
    assert_eq!(age_span(59.), "<1m");
    assert_eq!(age_span(60.), "1m");
}

#[test]
fn l_t_and_i_each_open_their_own_card_in_the_picture_whatever_the_data_or_the_size() {
    // (width px, height px): normal, short, narrow, narrow and short.
    let sizes = [(1210, 1700), (1210, 700), (704, 520), (560, 420)];
    let datasets = [
        ("full", 7, 4, true),
        ("short", 2, 0, false),
        ("empty", 0, 0, false),
    ];
    for (name, shifts, dreams, figure) in datasets {
        let c = drill_composition(shifts, dreams, figure);
        for (w, h) in sizes {
            let (closed, closed_m) =
                draw(&c, w, h, Theme::Dark, Open::default(), false).expect("draws");
            assert!(closed_m.drill_region.is_none() && closed_m.figure_detail_region.is_none());
            for (key, open) in drill_keys() {
                let at = format!("{name} {w}x{h} key {key}");
                let (shown, m) = draw(&c, w, h, Theme::Dark, open, false).expect("draws");
                assert_ne!(shown.data(), closed.data(), "{at}: the picture must change");
                match key {
                    "i" if m.dash.as_ref().is_some_and(|d| !d.no_reading) => {
                        // The dashboard is its own facts: no second card repeats it.
                        assert!(m.figure_detail_region.is_none(), "{at}");
                    }
                    "i" => {
                        let (_, card) = m
                            .figure_detail_region
                            .unwrap_or_else(|| panic!("{at}: no figure card"));
                        assert!(card >= P * 2. + DRILL_LINE, "{at}: {card}");
                    }
                    _ => {
                        let (_, card) = m
                            .drill_region
                            .unwrap_or_else(|| panic!("{at}: the section got no room"));
                        assert!(card >= P * 2. + DRILL_LINE, "{at}: {card}");
                        assert!(m.figure_detail_region.is_none());
                    }
                }
            }
        }
    }
}

#[test]
fn the_changes_page_replaces_the_dashboard_and_keeps_every_change() {
    let c = drill_composition(7, 4, true);
    let (_, closed) = draw(&c, 704, 700, Theme::Dark, Open::default(), false).expect("draws");
    assert!(closed.cards.len() > 1, "{:?}", closed.cards);
    let (_, open) = draw(
        &c,
        704,
        700,
        Theme::Dark,
        Open {
            changes: true,
            ..Open::default()
        },
        false,
    )
    .expect("draws");
    let (_, card) = open.drill_region.expect("the changes page");
    // The page is the whole pane bar the key row; never a stub.
    assert!(card > 6. * DRILL_LINE, "{card}");
    // Nothing of the dashboard is drawn or clickable on it.
    assert!(open.observation_rows.is_empty());
    assert!(open.story_region.is_none());
    let titles: Vec<_> = open.cards.iter().map(|c| c.title).collect();
    assert_eq!(titles, vec![CHANGES], "{titles:?}");
}

#[test]
fn a_page_too_long_for_the_pane_scrolls_to_its_last_row_and_no_further() {
    // More rows than any pane holds: the river runs to about a hundred.
    let c = drill_composition(60, 40, false);
    let page = |scroll: usize| Open {
        threads: true,
        scroll,
        ..Open::default()
    };
    let (w, h) = (40 * 16, 40 * 34);
    let (top, m) = draw(&c, w, h, Theme::Dark, page(0), false).expect("draws");
    let most = m
        .page_scroll_max
        .expect("a page too long for the pane scrolls");
    assert!(most > 0, "{most}");
    let (down, _) = draw(&c, w, h, Theme::Dark, page(1), false).expect("draws");
    assert_ne!(top.data(), down.data(), "one row down moves the page");
    // Past the end is the end.
    let (last, _) = draw(&c, w, h, Theme::Dark, page(most), false).expect("draws");
    let (beyond, _) = draw(&c, w, h, Theme::Dark, page(most + 5), false).expect("draws");
    assert_eq!(last.data(), beyond.data());
}

#[test]
fn an_open_section_never_makes_the_canvas_taller_and_keeps_its_least_content() {
    // Terminal sizes in cells (columns, rows) at the 16 x 34 px cell.
    for (cols, rows) in [(46, 30), (46, 60), (40, 50), (80, 40)] {
        let (w, h) = (cols * 16, (rows - 1) * 34);
        for (name, shifts, dreams, figure) in [
            ("rich", 7, 4, true),
            ("thin", 2, 0, false),
            ("empty", 0, 0, false),
        ] {
            let c = drill_composition(shifts, dreams, figure);
            let (closed, closed_m) =
                draw(&c, w, h, Theme::Dark, Open::default(), false).expect("draws");
            for (key, open) in drill_keys() {
                let at = format!("{name} {cols}x{rows} key {key}");
                let (shown, m) = draw(&c, w, h, Theme::Dark, open, false).expect("draws");
                assert_eq!(m.height, closed_m.height, "{at}: the canvas grew");
                assert_eq!(
                    (shown.width(), shown.height()),
                    (closed.width(), closed.height()),
                    "{at}"
                );
                match key {
                    "l" => {
                        let (_, card) = m.drill_region.unwrap_or_else(|| panic!("{at}: no card"));
                        // The heading and the least of the first change.
                        assert!(
                            card >= P * 2. + MONOLOGUE_H + DRILL_MIN_LINES as f32 * DRILL_LINE,
                            "{at}: {card}"
                        );
                    }
                    "t" => {
                        let (_, card) = m.drill_region.unwrap_or_else(|| panic!("{at}: no card"));
                        let bands = c.river.len().clamp(1, PAGE_MIN_ROWS);
                        let least =
                            P * 2. + MONOLOGUE_H + (bands as f32 * ROW_PITCH - ROW_GAP).max(40.);
                        assert!(card >= least, "{at}: {card} < {least}");
                    }
                    _ if m.dash.as_ref().is_some_and(|d| !d.no_reading) => {
                        assert!(m.figure_detail_region.is_none(), "{at}");
                    }
                    _ => {
                        let (_, card) = m
                            .figure_detail_region
                            .unwrap_or_else(|| panic!("{at}: no facts card"));
                        let lines = figure_detail_lines(&c, None).len().min(3);
                        assert!(card >= P * 2. + lines as f32 * DRILL_LINE, "{at}: {card}");
                    }
                }
                // A page is the whole pane; the `i` figure took its room from the cards.
                if key != "l" && key != "t" {
                    let (fig_top, fig_h) = m.figure_region;
                    assert!(fig_h > 0. && fig_top + fig_h <= m.height, "{at}: {fig_h}");
                }
            }
        }
    }
}

#[test]
fn a_closed_pane_never_draws_a_stub_card_whatever_its_size() {
    for (name, shifts, dreams, figure) in [
        ("rich", 7, 4, true),
        ("thin", 2, 0, false),
        ("empty", 0, 0, false),
    ] {
        let c = drill_composition(shifts, dreams, figure);
        // Heights of 9 to 60 rows of 34 px cells, and the edges either side: each draw
        // rasterizes the whole picture, so a dense sweep costs minutes in a debug build.
        for w in [560, 736] {
            for h in [300, 476, 986, 1020, 1666, 2000] {
                let (_, m) = draw(&c, w, h, Theme::Dark, Open::default(), false).expect("draws");
                let at = format!("{name} {w}x{h}");
                // A story card is drawn whole or not at all, never as a stub.
                if let Some((_, story)) = m.story_region {
                    assert!(story > P * 2. + theme::TITLE_LINE + 1., "{at}: {story}");
                }
                assert!(
                    m.drill_region.is_none() && m.figure_detail_region.is_none(),
                    "{at}"
                );
            }
        }
    }
}

#[test]
fn the_text_pane_changes_in_the_section_each_key_owns_at_narrow_and_normal_width() {
    for (cols, rows) in [(30, 32), (42, 40), (100, 60)] {
        for (name, shifts, dreams, figure) in [
            ("full", 7, 4, true),
            ("short", 2, 0, false),
            ("empty", 0, 0, false),
        ] {
            let c = drill_composition(shifts, dreams, figure);
            let closed = text_rows(&c, cols, rows, Open::default());
            for (key, open) in drill_keys() {
                let at = format!("{name} {cols}x{rows} key {key}");
                let shown = text_rows(&c, cols, rows, open);
                assert_ne!(shown, closed, "{at}");
                let all = shown.join("\n");
                let flat = all.split_whitespace().collect::<Vec<_>>().join(" ");
                match (key, shifts) {
                    ("l", 0) => assert!(
                        flat.contains("No changes yet in this session."),
                        "{at}\n{all}"
                    ),
                    ("l", _) => {
                        assert!(flat.starts_with("Changes"), "{at}\n{all}");
                        assert!(flat.contains("each with what it replaced"), "{at}\n{all}");
                        assert!(flat.contains("Was: "), "{at}\n{all}");
                    }
                    ("t", _) if dreams == 0 && shifts == 0 => {
                        assert!(flat.contains("No threads yet"), "{at}\n{all}")
                    }
                    ("t", _) => assert!(flat.contains("Monologue"), "{at}\n{all}"),
                    ("i", _) if figure => {
                        assert!(flat.contains("Fresh tokens per turn"), "{at}\n{all}")
                    }
                    ("i", _) => assert!(flat.contains("No figures yet"), "{at}\n{all}"),
                    _ => unreachable!(),
                }
                // The pane still ends on the figure facts and keeps the open section on screen.
                assert!(shown.len() < rows, "{at}: {} rows\n{all}", shown.len());
            }
        }
    }
}

#[test]
fn a_cut_story_and_every_cut_row_carry_their_words_in_full() {
    let c = drill_composition(7, 4, true);
    // Tall enough that the story card is kept: a short pane hides it, it never cuts it.
    for (w, h) in [(1210, 2600), (704, 1500)] {
        let open = Open {
            changes: true,
            ..Open::default()
        };
        for open in [
            Open::default(),
            open,
            Open {
                threads: true,
                ..Open::default()
            },
        ] {
            let (_, m) = draw(&c, w, h, Theme::Dark, open, false).expect("draws");
            let titles: Vec<_> = m.cuts.iter().map(|c| c.title).collect();
            // The story is on the dashboard; a page holds only its own rows.
            if !open.changes && !open.threads {
                assert!(
                    titles.contains(&"Where the work stands"),
                    "{w}x{h} {:?} {titles:?} story {:?}",
                    (open.changes, open.threads),
                    (m.story_region, m.height, &m.cards)
                );
            }
            for cut in &m.cuts {
                assert!(!cut.full.ends_with('…'), "{cut:?}");
                assert!(!cut.full.is_empty());
                if cut.title == "Where the work stands" {
                    assert_eq!(cut.full, LONG_STORY);
                }
                if cut.title == "A change" {
                    assert!(
                        cut.full.contains("Was: The old behaviour number"),
                        "{cut:?}"
                    );
                }
            }
        }
    }
    // The popup on a cut row is the observation popup showing those words, none dropped.
    let mut c = c;
    c.more = vec![json!({"title":"Where the work stands","line":LONG_STORY})];
    let at = PopupAt {
        card: ObsCard::More,
        index: 0,
        copied: false,
        note: None,
        scroll: 0,
    };
    let rows = text_rows(
        &c,
        40,
        30,
        Open {
            popup: Some(at),
            ..Open::default()
        },
    );
    assert_eq!(rows[0], "Where the work stands");
    let flat = rows.join(" ");
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(flat.contains(LONG_STORY), "{rows:#?}");
    let (plain, _) = draw(&c, 1210, 1700, Theme::Dark, Open::default(), false).expect("draws");
    let (popup, _) = draw(
        &c,
        1210,
        1700,
        Theme::Dark,
        Open {
            popup: Some(at),
            ..Open::default()
        },
        false,
    )
    .expect("draws");
    assert_ne!(plain.data(), popup.data());
}

#[test]
fn the_text_pane_cuts_only_the_story_and_names_it() {
    let c = drill_composition(7, 4, true);
    for cols in [30_usize, 42, 100] {
        for open in [
            Open::default(),
            Open {
                changes: true,
                ..Open::default()
            },
            Open {
                threads: true,
                ..Open::default()
            },
        ] {
            for line in text_rows(&c, cols, 60, open) {
                if line.ends_with('…') {
                    let (lines, full) = text_story_cut(&c, cols).expect("a cut story names itself");
                    assert!(
                        lines.contains(&line),
                        "{cols}: only the story is cut, not: {line}"
                    );
                    assert_eq!(full, LONG_STORY);
                }
            }
        }
    }
}

#[test]
fn test_an_early_window_keeps_its_words_in_the_facts() {
    let figure = json!({"quotas": [
        {"code": "5H", "used": 2., "text": "5H 2%"},
        {"code": "7D", "used": 1., "text": "7D 1%"},
    ]});
    let forecast = json!({
        "quota": {"5H": {"text": "→ ~30% at reset", "out": false}},
        "windows": [
            {"code": "5H", "early": false, "line": "On pace for about 30% by reset · resets in 3h"},
            {"code": "7D", "early": true, "line": "Early · last week peaked at 93% · resets in 6d 21h"},
        ],
    });
    let got = with_forecast(figure, &forecast);
    let q = array(&got["quotas"]);
    // The early week has no circle caption, only its words, without the reset tail.
    assert!(q[1]["forecast"].is_null(), "{got}");
    assert_eq!(quota_fact(&q[1]), "7D 1% · Early · last week peaked at 93%");
    // A window past its early stretch keeps its caption and gets no early words.
    assert!(q[0]["early"].is_null(), "{got}");
    assert_eq!(quota_fact(&q[0]), "5H 2% · → ~30% at reset");
}

/// Real evidence strings from the dreamer's stored readings (dreams.jsonl), plus the formats
/// its prompt asks for. Only the numbers a "turn" names are turns.
#[test]
fn evidence_label_lists_only_real_turn_numbers() {
    let label = |evidence: &str| evidence_receipt(&json!({"line": "x", "evidence": evidence}));
    for (evidence, want) in [
        // The formats the prompt asks for.
        ("Turn 4: 'the companion pane'", "turn 4"),
        ("Turns 12 and 15: 'keep it local'", "turns 12, 15"),
        ("Turns 4, 13 and 20: 'x'", "turns 4, 13, 20"),
        ("turns 4 & 8: 'at most 30%'", "turns 4, 8"),
        ("Turns 12-14: 'x'", "turns 12-14"),
        // Real rows: two turns cited either side of a quote, each once, in order.
        (
            "Turn 3438: \"in some segemnts it cuts off\"; turn 5853: \"misses the songs fading out without breathing room\"",
            "turns 3438, 5853",
        ),
        (
            "Turn 5853: \"the daybreak in the end which has zero meaning\"; later: \"9:14 to 9:18 needs to go it still goes to seawall\"",
            "turn 5853",
        ),
        ("Turn 80: 'a'; Turn 80: 'b'; turn 97: 'c'", "turns 80, 97"),
        (
            "Turn 1: \"check last session and then tell me whats our next campaign\"; recent-work list: \"Review previous session, recap\"",
            "turn 1",
        ),
        // Real rows that showed junk: sizes, counts, commit ids, run numbers, times.
        (
            "\"4 GB looks reachable with 13 sessions\", later \"I said earlier it looked reachable; that was too strong.\"",
            "evidence",
        ),
        (
            "\"I'll do that by hand, because the install script still points at an old rc2 worktree\"; the same steps ran for 80f80d97 and 1306bf03",
            "evidence",
        ),
        (
            "Handoff note: \"Install: scp ... codesign ... launchctl bootstrap the plist\" and \".plans/release-10.1/install.sh is stale\"; backups pre-80f80d97 and pre-1306bf03.",
            "evidence",
        ),
        (
            "Gate runs 13 and 14 both failed for one cause: \"I had only run lib and companion_instrument before committing.\"",
            "evidence",
        ),
        (
            "Installed 7880d3f8 at 03:54 and ed97b0b4 at 05:01 with the same hand-written scp, codesign and launchctl command.",
            "evidence",
        ),
        (
            "Ruling #76: \"both fixes reasoned from herdr master (host ids at most 910k).\"",
            "evidence",
        ),
        // The word "turn" without a number after it, or a number inside a quote.
        ("User mid-turn: \"its not homie\", then \"check esp32\"", "evidence"),
        ("Earlier turn: \"I told you wrong: earlier compaction is not fixed\"", "evidence"),
        ("User: \"turn 9 broke it\"", "evidence"),
        ("Turn 03: 'x'", "evidence"),
        ("Return 5 items", "evidence"),
        ("", "evidence"),
    ] {
        assert_eq!(label(evidence), want, "{evidence}");
    }
}
