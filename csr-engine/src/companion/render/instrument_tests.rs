use super::*;

fn timeline_data() -> (Value, Value) {
    let now = 1_800_000_000.;
    let s = json!({"id":"tl","compacts":[now - 5400.],"main":[],"prompts":[]});
    let entry = |claim: &str, receipt: &str| {
        json!({"sig":claim,"kind":"shift","claim":claim,"now":claim,"was":null,
            "before":[],"after":[{"scope":"summarizer.rs","sym":"store_story","mark":"~"}],
            "receipts":[receipt],"grounded":true,"moved":true})
    };
    let data = json!({
        "story_map": {"at": now - 6200., "entries": [
            entry("Keep prior shift reflections on story rewrite", "oid:aaaaaaa"),
            entry("Pane fails open when the map is absent", "oid:bbbbbbb"),
            entry("Unlanded edit of the ledger", "edit:ledger"),
            entry("Dated by neither", "oid:ccccccc"),
        ]},
        "shifts": [
            {"oid":"aaaaaaa1111","date":"2026-10-21","at": now - 3000.},
            {"oid":"bbbbbbb2222","date":"2026-10-20","at": now - 86400.},
        ],
        "dreams": [{"session":"tl","at": now - 600., "line":"What if recall started from the file?", "answer":"yes"}],
        "threads": [{"text":"Decide the release name","last_seen": now - 9000., "state":"open"}],
    });
    (data, s)
}

fn at_local(h: u32, m: u32, day: u32) -> f64 {
    use chrono::TimeZone;
    chrono::FixedOffset::east_opt(2 * 3600)
        .unwrap()
        .with_ymd_and_hms(2026, 10, day, h, m, 0)
        .unwrap()
        .timestamp() as f64
}

fn write_row(data: &Value, s: &Value, now: f64) -> RiverRow {
    let c = Composition::build(data, s, now);
    c.river
        .into_iter()
        .find(|r| r.kind == RiverKind::Write)
        .expect("an unlanded edit row")
}

/// CIEDE2000 between two sRGB colours (Sharma, Wu, Dalal 2005).
fn ciede2000(a: [u8; 4], b: [u8; 4]) -> f64 {
    fn lab(c: [u8; 4]) -> (f64, f64, f64) {
        let lin = |v: u8| {
            let v = f64::from(v) / 255.;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let (r, g, bl) = (lin(c[0]), lin(c[1]), lin(c[2]));
        let x = 0.4124564 * r + 0.3575761 * g + 0.1804375 * bl;
        let y = 0.2126729 * r + 0.7151522 * g + 0.0721750 * bl;
        let z = 0.0193339 * r + 0.1191920 * g + 0.9503041 * bl;
        let f = |t: f64| {
            if t > 216. / 24389. {
                t.cbrt()
            } else {
                (24389. / 27. * t + 16.) / 116.
            }
        };
        let (fx, fy, fz) = (f(x / 0.95047), f(y), f(z / 1.08883));
        (116. * fy - 16., 500. * (fx - fy), 200. * (fy - fz))
    }
    let ((l1, a1, b1), (l2, a2, b2)) = (lab(a), lab(b));
    let (c1, c2) = (a1.hypot(b1), a2.hypot(b2));
    let cb = (c1 + c2) / 2.;
    let g = 0.5 * (1. - (cb.powi(7) / (cb.powi(7) + 25f64.powi(7))).sqrt());
    let (a1p, a2p) = ((1. + g) * a1, (1. + g) * a2);
    let (c1p, c2p) = (a1p.hypot(b1), a2p.hypot(b2));
    let hue = |b: f64, a: f64| b.atan2(a).to_degrees().rem_euclid(360.);
    let (h1, h2) = (hue(b1, a1p), hue(b2, a2p));
    let (dl, dc) = (l2 - l1, c2p - c1p);
    let mut dh = h2 - h1;
    if c1p * c2p == 0. {
        dh = 0.;
    } else if dh > 180. {
        dh -= 360.;
    } else if dh < -180. {
        dh += 360.;
    }
    let dhh = 2. * (c1p * c2p).sqrt() * (dh.to_radians() / 2.).sin();
    let (lb, cbp) = ((l1 + l2) / 2., (c1p + c2p) / 2.);
    let hb = if c1p * c2p == 0. {
        h1 + h2
    } else if (h1 - h2).abs() <= 180. {
        (h1 + h2) / 2.
    } else if h1 + h2 < 360. {
        (h1 + h2 + 360.) / 2.
    } else {
        (h1 + h2 - 360.) / 2.
    };
    let t = 1. - 0.17 * (hb - 30.).to_radians().cos()
        + 0.24 * (2. * hb).to_radians().cos()
        + 0.32 * (3. * hb + 6.).to_radians().cos()
        - 0.20 * (4. * hb - 63.).to_radians().cos();
    let dth = 30. * (-((hb - 275.) / 25.).powi(2)).exp();
    let rc = 2. * (cbp.powi(7) / (cbp.powi(7) + 25f64.powi(7))).sqrt();
    let sl = 1. + 0.015 * (lb - 50.).powi(2) / (20. + (lb - 50.).powi(2)).sqrt();
    let (sc, sh) = (1. + 0.045 * cbp, 1. + 0.015 * cbp * t);
    let rt = -(2. * dth).to_radians().sin() * rc;
    ((dl / sl).powi(2) + (dc / sc).powi(2) + (dhh / sh).powi(2) + rt * (dc / sc) * (dhh / sh))
        .sqrt()
}

fn agent_at(description: &str, status: &str, last_at_ms: i64) -> agents::Agent {
    serde_json::from_value(json!({
        "id": description, "parent": null, "description": description, "type": "Explore",
        "model": null, "status": status, "started": 1_799_999_000_000_i64,
        "first_seen": 1_799_999_000_000_i64, "last_at": last_at_ms, "tools": 3, "tokens": 10,
        "edits": [], "edits_omitted": 0, "overlaps": [], "overlaps_omitted": 0,
        "last": "", "last_tool": null, "current_turn": null, "completion": null,
        "listed": true,
    }))
    .expect("an agent row")
}

fn quota_composition() -> Composition {
    let mut c = drill_composition(0, 0, true);
    c.figure["quotas"] = json!([
        {"code":"5H","used":18.,"pace":0.1,"text":"5H 18% · 3h left ↻ 11:20"},
        {"code":"7D","used":45.,"pace":0.4,"text":"7D 45% · 4d left ↻ Sun 07:00"},
    ]);
    c.figure["cache"] = json!({"left": 1800., "ttl": 3600., "text": "CACHE 30m left"});
    c.codex_week = json!({"percent": 41.});
    c.csr_cost.share = Some(4.7);
    // The checks that use this draw the circles; the dashboard has its own.
    c.figure["layout"] = json!("rings");
    c
}

/// Pixels within `tol` of `ink` in a band of rows (layout pt) across the card.
fn ink_px(
    pm: &tiny_skia::Pixmap,
    rows: (f32, f32),
    scale: f32,
    left: u32,
    w: u32,
    ink: Rgba,
) -> usize {
    let near =
        |p: &[u8], c: [u8; 4]| (0..3).all(|i| (i32::from(p[i]) - i32::from(c[i])).abs() <= 6);
    let mut n = 0;
    for y in (rows.0 * scale) as u32..(rows.1 * scale) as u32 {
        for x in left..(left + w).min(pm.width()) {
            let px = pm.pixel(x, y).expect("in the picture");
            n += usize::from(near(&[px.red(), px.green(), px.blue()], ink));
        }
    }
    n
}

fn forecast_composition(text: &str, out: bool) -> Composition {
    let mut c = quota_composition();
    for q in c.figure["quotas"].as_array_mut().unwrap() {
        q["forecast"] = json!({"text": text, "out": out});
    }
    c
}

/// Every pane height a terminal can give, as frames whose figure card is a full card (tall
/// enough for the mid layout; a shorter one is the strip, which has no notes line and no caption):
/// the figure's top and height, and the frame's metrics.
fn figure_sweep(c: &Composition, open: Open) -> Vec<(f32, f32, Metrics)> {
    let closed = !open.figure;
    let mut out = vec![];
    for width in [560_u32, 704] {
        for avail in (600..=1400).step_by(12) {
            let Some((_, m)) = draw(c, width, avail, Theme::Dark, open, false) else {
                continue;
            };
            let (top, h) = m.figure_region;
            if h >= mid_min_h(&c.figure) - MID_MIN_SLOP {
                out.push((top, h, m));
            } else {
                // Only an open section squeezes the card to a strip; a closed pane never.
                assert!(!closed, "closed pane {width}x{avail}: figure card {h:.1}pt");
            }
        }
    }
    out
}

/// A figure the dashboard draws: windows with their outlook, a breakdown, CSR's tallies.
fn dash_composition(sev: &str) -> Composition {
    let mut c = quota_composition();
    c.figure.as_object_mut().unwrap().remove("layout");
    c.figure["outlook"] = json!({
        "headline": {"text": "On track. You should reach Thursday's reset with room to spare.", "sev": sev},
        "windows": [
            {"code":"5H","name":"5 Hour","used":18.,"sev":"ok","tick":46.,"early":false,
             "blended":false,"line":"On pace for about 46% by reset · resets in 4h 35m"},
            {"code":"7D","name":"Weekly","used":45.,"sev":"ok","tick":null,"early":true,
             "blended":false,"line":"Early · last week peaked at 71% · resets in 2d 20h"},
        ],
    });
    c.figure["breakdown"] = json!({
        "caption": "this turn +1.6k · Claude 1.3k · you 0.3k (est.)",
        "floor": 28000., "floor_name": "Setup", "text": 20000., "you": 4000.,
        "tool": 60000., "csr": 8000., "free": 880000., "total": 120000., "size": 1000000.,
        "turn": null, "estimated": true,
    });
    c.figure["asks"] = json!(4);
    c.tally = json!({"by_csr": 2, "by_you": 0, "payback": 851_000.});
    c
}

/// The dashboard's sizes: (width px, available height px) of panes the dashboard fits whole,
/// with its credit row, its rings and the chart `i` asks for, at the width-fit text size.
/// Shorter panes drop those first; the fit tests below cover them.
// The rows are a third larger than they were, so the panes that hold the whole dashboard are a
// third taller (they were 1060, 1500 and 2000 px).
const DASH_SIZES: [(u32, u32); 3] = [(560, 1420), (704, 2000), (960, 2650)];

fn credit_of(
    left: f64,
    week: f64,
    active: usize,
    caps: f64,
) -> crate::companion::api_credit::Credit {
    crate::companion::api_credit::Credit {
        left,
        active,
        running_caps: caps,
        week,
        ..Default::default()
    }
}
fn dash_of(c: &Composition, size: (u32, u32), theme: Theme, open: Open) -> (Dash, Metrics) {
    let (_, m) = draw(c, size.0, size.1, theme, open, false).expect("draws");
    (m.dash.clone().expect("a dashboard"), m)
}

fn soak_card(rows: usize) -> crate::companion::live_eval::Card {
    use crate::companion::live_eval::{Card, Row};
    let checks = [
        ("\u{2713}", "ok", "Daemon errors 0 \u{2192} (limit 0)"),
        (
            "\u{2713}",
            "ok",
            "Memory trend -5943 MB/h \u{2193} (limit +150)",
        ),
        (
            "\u{2713}",
            "ok",
            "Prompt hook p95, worst 481 ms \u{2192} (limit 800)",
        ),
        ("\u{2026}", "quiet", "Replay finished: no, need yes by end"),
        ("\u{2026}", "quiet", "Dream cycles 0, need 1 by end"),
    ];
    Card {
        title: "CSR performance".into(),
        candidate: "combo ac9c24c3".into(),
        sev: "ok".into(),
        line1: "3 of 5 checks passing \u{b7} 2 waiting \u{b7} On track".into(),
        line2: "Combo soak \u{b7} ends 7:04 AM \u{b7} 6h 22m left".into(),
        progress: 0.2,
        rows: checks[..rows]
            .iter()
            .map(|(mark, sev, text)| Row {
                mark: (*mark).into(),
                sev: (*sev).into(),
                text: (*text).into(),
            })
            .collect(),
        warning: None,
        next_change: None,
        road: None,
    }
}

fn breakdown_composition() -> Composition {
    let mut c = quota_composition();
    c.figure["breakdown"] = json!({
        "caption": "this turn +9k · tool results 6k · CSR 1.1k (est.)",
        "floor": 28000., "text": 20000., "tool": 60000., "csr": 12000.,
        "free": 880000., "total": 120000., "size": 1000000.,
        "turn": null, "estimated": true,
    });
    c
}

fn fresh_with_history(prior: usize) -> Composition {
    let mut c = Composition::build(&json!({}), &json!({"id":"fresh"}), 1_800_000_000.);
    assert!(c.empty_session);
    c.repo_recap = Some(crate::companion::repo_recap::RepoRecap {
        session: "64f9b5b9aaaa".into(),
        at: 1_799_900_000.,
        age: "1d ago".into(),
        text: "you asked: Fix the footer".into(),
        history: (0..prior)
            .map(|i| crate::companion::repo_recap::RepoSessionSummary {
                session: format!("s{i}"),
                at: 1_799_000_000. - i as f64,
                age: format!("{}d ago", i + 2),
                intent: format!("you asked: Question number {i}"),
                outcome: if i == 1 { "done" } else { "unknown" }.into(),
            })
            .collect(),
    });
    c
}

fn observed_data() -> Value {
    let item = |line: &str, evidence: &str| json!({"line":line,"evidence":evidence});
    let should =
        |kind: &str, line: &str| json!({"kind":kind,"line":line,"evidence":"Turns 4 and 8: 'x'"});
    json!({
        "dreams": [{"id":"d1","session":"o","at":100.,"line":"What if?",
            "confusing_claude":[item("Misread the pane.","Turn 4: 'pane'")],
            "should_be":[should("hook","Check formatting before commits.")]}],
        "dream_records": [
            {"kind":"look","session":"o","at":300.,"outcome":"none",
                "confusing_claude":[],"should_be":[]},
            {"kind":"look","session":"o","at":200.,"outcome":"none",
                "confusing_claude":[item("Repeated a rejected approach.","Turn 12"),
                    item("Misread the pane.","Turn 15")],
                "should_be":[should("skill","Bump the version in both files.")]},
            {"kind":"look","session":"other","at":400.,"outcome":"none",
                "confusing_claude":[item("Not this session.","Turn 1")]},
        ],
    })
}

fn start_composition(reading: Option<f64>) -> Composition {
    start_on(
        Composition::build(&json!({}), &json!({"id":"fresh"}), 1000.),
        reading,
    )
}
fn start_on(mut c: Composition, reading: Option<f64>) -> Composition {
    let thread = |key: &str, text: &str, seen: f64| {
        json!({"key": key, "text": text, "line": text, "state": "lost", "waiting": false,
            "last_seen": seen, "origin_sid": "abc-123"})
    };
    c.figure["start"] = json!({
        "replies": 0,
        "reading": reading,
        "parts": [
            {"key": "csr", "name": "CSR injected", "tokens": 700.},
            {"key": "memory", "name": "Memory", "tokens": 3000.},
            {"key": "skills", "name": "Skills", "tokens": null},
            {"key": "system", "name": "System prompt", "tokens": null}],
        "rest": null,
        "recap": "fix the pane",
        "episodes": 2,
        "threads": [thread("k1", "Retry the upload", 9.), thread("k2", "Check the hub", 5.)],
    });
    c
}

fn threads_open() -> Open {
    Open {
        threads: true,
        ..Open::default()
    }
}

// ---- l, t and i each change the pane, and every "…" row opens its words ----

const LONG_STORY: &str = "The Maestro memory-screen flows pass on the controls code once the Mac's low disk is bypassed with a storage override, and the remaining flaky flow, number eight, still fails about one run in three, which the person has not yet decided to chase, so the release waits on that call, while the feed caption writer keeps its landmark context and the colour mix keeps its per-group controls, neither of which has been verified against a real device yet, and the privacy line and landmark toggle agents have both reported back with their own notes about what they changed, which still need reading before anything is merged. A second sentence follows.";

/// A session with `shifts` changes (each with a "Was:" line), `dreams` river rows, a long story and
/// two observations, plus a figure with data when `figure` is set.
fn drill_composition(shifts: usize, dreams: usize, figure: bool) -> Composition {
    let now = 1_800_000_000.;
    let entries: Vec<Value> = (0..shifts)
        .map(|i| {
            json!({"sig":format!("s{i}"),"kind":"shift",
                "claim":format!("Change number {i} says something long enough that the narrow pane has to cut it somewhere before the end"),
                "now":format!("Change number {i} says something long enough that the narrow pane has to cut it somewhere before the end"),
                "was":format!("The old behaviour number {i}"),
                "before":[],"after":[{"scope":format!("file{i}.rs"),"sym":"f","mark":"~"}],
                "receipts":["edit:x"],"grounded":false,"moved":true})
        })
        .collect();
    let dream_rows: Vec<Value> = (0..dreams)
        .map(|i| json!({"session":"dr","at": now - 100. * (i + 1) as f64,
            "line":format!("What if thread number {i} asked a question long enough to be cut by a narrow pane, as most of them are?")}))
        .collect();
    let mut data = json!({
        "story": {"now": LONG_STORY, "at": now - 3600.},
        "story_map": {"at": now - 60., "entries": entries},
        "dreams": dream_rows,
        "dream_records": [{"kind":"look","session":"dr","at":now - 10.,"outcome":"none",
            "confusing_claude":[{"line":"Claude told the person the other 15 test flows had already run, then had to take it back, which cost a day.","evidence":"Turn 15"}],
            "should_be":[{"kind":"skill","line":"Every Codex review repeats the same usage gate, model resolver and worker flags by hand.","evidence":"Turns 2, 3"}]}],
    });
    data["limits"] = json!({});
    let mut c = Composition::build(
        &data,
        &json!({"id":"dr","compacts":[],"main":[],"prompts":[]}),
        now,
    );
    if figure {
        c.figure = json!({"ctx":{"tokens":120_000.,"size":1_000_000.},"trend":1800.,
            "quotas":[{"code":"5H","used":20.,"pace":0.5},{"code":"7D","used":45.,"pace":0.4}]});
        c.requests = (0..12)
            .map(|i| (i as f64, 200. + 150. * ((i * 5) % 7) as f64))
            .collect();
    }
    c
}
fn drill_keys() -> [(&'static str, Open); 3] {
    [
        (
            "l",
            Open {
                changes: true,
                ..Open::default()
            },
        ),
        (
            "t",
            Open {
                threads: true,
                ..Open::default()
            },
        ),
        (
            "i",
            Open {
                figure: true,
                ..Open::default()
            },
        ),
    ]
}

#[path = "instrument_tests/away_tests.rs"]
mod away_tests;

#[path = "instrument_tests/mixed_tests.rs"]
mod mixed_tests;

#[path = "instrument_tests/card_pane_tests.rs"]
mod card_pane_tests;
