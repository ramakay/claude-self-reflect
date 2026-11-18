use super::*;
use serde_json::json;
#[test]
fn the_font_lookup_tries_the_system_font_then_inter_noto_dejavu_liberation() {
    let only = |wanted: &[&str]| {
        let wanted: Vec<String> = wanted.iter().map(|s| s.to_string()).collect();
        move |p: &str| wanted.iter().any(|w| w == p)
    };
    let sys = "/System/Library/Fonts/SFNS.ttf";
    let noto = "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf";
    let dejavu = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf";
    let inter = "/home/me/.local/share/fonts/Inter-Regular.ttf";
    let liberation = "/usr/share/fonts/liberation/LiberationSans-Regular.ttf";
    let all: &[&str] = &[sys, noto, dejavu, inter, liberation];
    assert_eq!(pick_family(only(all), "/home/me").unwrap().regular, sys);
    assert!(pick_family(only(all), "/home/me").unwrap().variable);
    let linux: &[&str] = &[noto, dejavu, inter, liberation];
    assert_eq!(pick_family(only(linux), "/home/me").unwrap().regular, inter);
    assert_eq!(
        pick_family(only(&linux[..2]), "/home/me").unwrap().regular,
        noto
    );
    assert_eq!(
        pick_family(only(&linux[1..2]), "/home/me").unwrap().regular,
        dejavu
    );
    assert_eq!(
        pick_family(only(&linux[3..]), "/home/me").unwrap().regular,
        liberation
    );
    let found = pick_family(only(&linux[3..]), "/home/me").unwrap();
    assert!(!found.variable && found.bold.is_none());
    assert_eq!(pick_family(only(&[]), "/home/me"), None);
}
fn full_gauges() -> Value {
    json!({
        "ctx": {"tokens": 170000., "size": 1000000., "sev": "normal"},
        "trend": 22000.,
        "payback": {"text": "\u{267b} ~3 calls", "sev": "normal"},
        "quotas": [
            {"code": "7D", "used": 62., "sev": "normal", "pace": 0.3, "text": "7D 62% \u{21bb}Sun 09:00"},
            {"code": "5H", "used": 41., "sev": "normal", "pace": null, "text": "5H 41%"},
        ],
        "cache": {"text": "CACHE 57m", "sev": "normal"},
        "sts": {"sev": "amber"},
    })
}
fn said(p: &Pill) -> String {
    p.lines
        .iter()
        .flatten()
        .map(|g| g.iter().map(|r| r.0.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join(" | ")
}
#[test]
fn the_pill_keeps_every_fact_of_the_text_masthead_at_46_and_33_columns() {
    let Some(f) = fonts() else {
        eprintln!("no usable font: skipped");
        return;
    };
    let g = full_gauges();
    // the pane's picture width at that many columns, as draw_page works it out
    for cols in [46usize, 33] {
        let width = cols - 1;
        let avail = (width - 1) as f32 * 16. / 2. - 8.;
        let pill = pill_of(&g, 2, None, avail, f).unwrap_or_else(|| panic!("{cols} cols"));
        let text = said(&pill);
        eprintln!("{cols}: {text}");
        for fact in [
            "CTX 170k/1M",
            "22k/",
            "\u{267b} ~3",
            "2 ",
            "5H 41%",
            "7D 62%",
            "\u{25f7} 57m",
            "STS",
        ] {
            assert!(text.contains(fact), "{cols} cols lost {fact:?}: {text}");
        }
        assert!(pill_cols(&pill, (16, 34), f) < width, "{cols}");
    }
    // the pace arrow rides the 7D reading
    let mut ahead = g.clone();
    ahead["quotas"][0]["used"] = json!(80.);
    ahead["quotas"][0]["text"] = json!("7D 80% \u{21bb}Sun 09:00");
    let pill = pill_of(&ahead, 0, None, 360., f).unwrap();
    assert!(said(&pill).contains("7D 80%\u{bb}"), "{}", said(&pill));
}
#[test]
fn a_pill_fact_with_no_data_drops_and_no_data_means_no_pill() {
    let Some(f) = fonts() else {
        return;
    };
    assert!(pill_of(&json!({}), 0, None, 360., f).is_none());
    let g = json!({"ctx": {"tokens": 88000., "size": 1000000., "sev": "normal"}});
    let text = said(&pill_of(&g, 0, None, 360., f).unwrap());
    assert_eq!(text, "CTX 88k/1M");
}
#[test]
fn a_card_without_data_is_not_built() {
    let g = json!({"ctx": {"tokens": null}, "quotas": [], "cache": {"cold": false, "left": null}});
    let cards = build(
        &json!({}),
        &json!({"id": "x"}),
        &g,
        1.0,
        0,
        &agents::Projection::default(),
    );
    assert!(cards.is_empty());
}
#[test]
fn every_kind_of_card_draws_in_under_five_milliseconds() {
    // Measured on the Studio: cold draws 1.3 to 2.0 ms idle and at most 1.7 ms in 105 draws
    // under 64 busy loops, warm medians at most 0.55 ms. 50 ms is far above all of that and
    // far below a draw that really regressed.
    const COLD_BOUND_MS: u64 = 50;
    let Some(f) = fonts() else {
        eprintln!("no usable font: skipped");
        return;
    };
    let legend = vec![
        (Dot::Blue, "Cached".to_owned(), "91%".to_owned()),
        (Dot::Pink, "Fresh".to_owned(), "9%".to_owned()),
    ];
    let bar = |name: &str| BarFact {
        name: name.into(),
        share: 0.5,
        percent: 50,
        caption: "resets in 2h".into(),
        pace: Some(0.4),
        tone: Dot::Blue,
    };
    let cards = [
        Card::Moon {
            moon: Some(MoonFact {
                share: 0.17,
                percent: 17,
                tokens: 170000.,
                caption: "170k/1M".into(),
            }),
            bars: vec![bar("5H"), bar("7D\u{bb}")],
        },
        Card::Story {
            rights: "rev 3 \u{b7} Haiku \u{b7} just told \u{b7} next at compact".into(),
            now: "Work stands at the cards: the moon gauge draws and the pill keeps its facts."
                .into(),
            shifts: vec![
                ("1  moon gauge replaced the rings".into(), true),
                (
                    "   Haiku's words \u{b7} names match commit 1c4183bc".into(),
                    false,
                ),
            ],
        },
        Card::History {
            bars: (0..300).map(|i| (i as f64, 20.)).collect(),
            head: "+34k/turn".into(),
            legend: legend.clone(),
        },
        Card::Agents {
            slots: vec![Slot::Running; 10],
            legend: legend.clone(),
        },
        Card::Cache {
            share: 0.78,
            cold: false,
            legend,
        },
        Card::WhatIf {
            line: "What if the cards drew from the same gauges as the text rows?".into(),
            tally: "3 of 5 left".into(),
            count: 2,
            unread: 1,
        },
        Card::Mirror {
            recalled: 3100,
            noted: 800,
            up: (0..120).map(f64::from).collect(),
            down: (0..120).map(|i| f64::from(i % 7)).collect(),
        },
    ];
    let cell = (16, 34);
    for card in &cards {
        let rows = rows_of(card, 44, cell, f);
        // Two bounds. The first draw of each kind is cold (glyph caches, allocator) and gets a
        // generous bound that a real regression still breaks; the warmed draws must have a
        // median at or under the 5 ms the card is designed for, which one preempted sample
        // cannot move.
        let name = format!("{card:?}")
            .split([' ', '(', '{'])
            .next()
            .unwrap_or("")
            .to_owned();
        let mut took = Vec::new();
        let mut drawn = None;
        for _ in 0..26 {
            let start = std::time::Instant::now();
            let pm = draw_card(
                card,
                44,
                rows,
                cell,
                &[crate::companion::render::theme::SWATCH_0],
                f,
            )
            .unwrap();
            took.push(start.elapsed());
            drawn = Some(pm);
        }
        let cold = took.remove(0);
        took.sort();
        let median = took[took.len() / 2];
        eprintln!("{name}: cold {cold:?}, warm median {median:?}");
        let pm = drawn.unwrap();
        assert_eq!(pm.width(), 44 * 16);
        // debug builds are an order slower; the bound is the release one
        if !cfg!(debug_assertions) {
            assert!(
                cold < std::time::Duration::from_millis(COLD_BOUND_MS),
                "{name}: cold draw {cold:?}"
            );
            assert!(
                median <= std::time::Duration::from_millis(5),
                "{name}: warm median {median:?}"
            );
        }
    }
    let pill = pill_of(
        &serde_json::json!({"ctx": {"tokens": 1000., "size": 2000.}}),
        0,
        Some(2),
        theme::REGULAR,
        f,
    )
    .unwrap();
    let start = std::time::Instant::now();
    draw_pill(
        &pill,
        pill_cols(&pill, cell, f),
        2,
        cell,
        &[crate::companion::render::theme::SWATCH_0],
        f,
    )
    .unwrap();
    eprintln!("pill: {:?}", start.elapsed());
}
#[test]
fn cached_and_fresh_come_from_the_transcripts_own_usage() {
    // [t, context, output, cache_creation, cache_read]: cached is the read, fresh the rest
    let s = json!({"replies": [[1., 1000., 10., 100., 900.], [2., 2000., 10., 200., 1800.]]});
    let (data, split) = request_bars(&s);
    assert!(split);
    assert_eq!(data, vec![(900., 100.), (1800., 200.)]);
}
#[test]
fn a_quota_over_its_limit_is_labelled_with_the_measured_percent_and_drawn_full() {
    let q = json!({"code": "5H", "used": 125., "sev": "red", "pace": 0.30001});
    let bar = quota_bar(&q, 0.).unwrap();
    assert_eq!(bar.percent, 125);
    assert_eq!(bar.share, 1.);
    let g = json!({"ctx": {"tokens": 1_250_000., "size": 1_000_000.}});
    assert_eq!(ctx_moon(&g).unwrap().percent, 125);
    // an idle refresh moves the pace and the cache's seconds, not the card
    let q2 = json!({"code": "5H", "used": 125., "sev": "red", "pace": 0.30003});
    assert_eq!(quota_bar(&q2, 0.), Some(bar));
    let cache = |left: f64| cache_card_of(&json!({"left": left, "ttl": 3600.}));
    assert_eq!(cache(3421.), cache(3423.));
    // a 1h cache the clock puts at 3610 s left, then 3607: the gauge's ttl follows the left
    let warm = |now: f64| {
        let g = masthead::cache_info(
            &json!({"warm": true, "ttl": "1h", "expires_at": 13610.}),
            now,
        );
        cache_card_of(&g)
    };
    assert!(warm(10000.).is_some());
    assert_eq!(warm(10000.), warm(10003.));
}
#[test]
fn requests_with_input_but_no_cache_traffic_are_all_fresh() {
    let s = json!({"replies": [[1., 1000., 10., 0., 0.], [2., 2000., 10., 0., 0.]]});
    let (data, _) = request_bars(&s);
    assert_eq!(data, vec![(0., 1000.), (0., 2000.)]);
    let card = history_card(&json!({}), &s, 0).unwrap();
    let Card::History { legend, .. } = card else {
        panic!()
    };
    assert!(
        legend.contains(&(Dot::Blue, "Cached".into(), "0%".into())),
        "{legend:?}"
    );
    assert!(
        legend.contains(&(Dot::Pink, "Fresh".into(), "100%".into())),
        "{legend:?}"
    );
}
