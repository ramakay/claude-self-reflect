//! The pane contract: every scenario fixture, at every pane size and in both themes, through the
//! real `Pane::frame`, held to the rules in `src/companion/render/PANE_RULES.md`. A failure names
//! the rule, the fixture, the size, the need and the limit, and asks for a redo.
#[path = "support/pane_home.rs"]
mod pane_home;
use csr_engine::companion::{
    pane::Pane,
    paths::Paths,
    render::{
        instrument::{Metrics, CARD_REGISTRY, STACKABLE},
        kitty::{set_cell_override, Placement},
        theme::Theme,
    },
};
use pane_home::*;
use serde_json::{json, Value};
use std::fs;

/// The pane sizes: cols x rows at the cell the terminal reports. 35x30 and 35x20 are the owner's
/// pane at its real 26x58 cell; 30x16 is the smallest supported.
const SIZES: [(usize, usize, (u32, u32)); 6] = [
    (35, 30, (26, 58)),
    (44, 30, (16, 34)),
    (26, 58, (16, 34)),
    (35, 20, (26, 58)),
    (60, 24, (16, 34)),
    (30, 16, (16, 34)),
];
const THEMES: [Theme; 2] = [Theme::Dark, Theme::Light];

/// A scenario: its name, how its HOME is built, and the keys pressed before the frame.
struct Fixture {
    name: &'static str,
    home: fn() -> (tempfile::TempDir, Paths),
    keys: &'static [&'static str],
}
/// Answer the three open what-ifs of a HOME, so the dreamer has no question up.
fn answer_all(paths: &Paths) {
    let log = paths.keep.join("dreams.jsonl");
    let mut text = fs::read_to_string(&log).unwrap_or_default();
    for (i, id) in ["b-top", "b-next", "b-third"].iter().enumerate() {
        text.push_str(
            &json!({"kind":"answer","id":id,"answer":"helpful","at":NOW - 100. + i as f64})
                .to_string(),
        );
        text.push('\n');
    }
    fs::write(&log, text).unwrap();
}
fn append_dream_log(paths: &Paths, row: Value) {
    let log = paths.keep.join("dreams.jsonl");
    let mut text = fs::read_to_string(&log).unwrap_or_default();
    text.push_str(&row.to_string());
    text.push('\n');
    fs::write(&log, text).unwrap();
}
/// The owner's session with the dreamer at work: no question up, a look in flight.
fn dreaming() -> (tempfile::TempDir, Paths) {
    let (home, paths) = owner_home();
    answer_all(&paths);
    append_dream_log(
        &paths,
        json!({"kind":"looking","session":"sess-now","at":NOW - 120.,"outcome":"dreaming",
            "budget":{"spent":247000,"cap":2200000}}),
    );
    (home, paths)
}
/// The owner's session with the dreamer idle: it looks as you work.
fn quiet() -> (tempfile::TempDir, Paths) {
    let (home, paths) = owner_home();
    answer_all(&paths);
    (home, paths)
}
/// The owner's session with every line three times as long.
fn long_text() -> (tempfile::TempDir, Paths) {
    let (home, paths) = owner_home();
    let log = paths.keep.join("dreams.jsonl");
    let long = |s: &str| format!("{s} {s} {s}");
    let text = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| {
            let mut v: Value = serde_json::from_str(line).unwrap();
            for key in ["confusing_claude", "should_be"] {
                if let Some(items) = v[key].as_array_mut() {
                    for item in items {
                        let l = long(item["line"].as_str().unwrap_or(""));
                        item["line"] = json!(l);
                    }
                }
            }
            if let Some(dreams) = v["dreams"].as_array_mut() {
                for d in dreams {
                    let l = long(d["line"].as_str().unwrap_or(""));
                    d["line"] = json!(l);
                }
            }
            v.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&log, text).unwrap();
    (home, paths)
}
fn few() -> (tempfile::TempDir, Paths) {
    state_home("02-running")
}
fn empty() -> (tempfile::TempDir, Paths) {
    state_home("01-empty")
}
fn compacting() -> (tempfile::TempDir, Paths) {
    state_home("05-compacting")
}
fn agents() -> (tempfile::TempDir, Paths) {
    state_home("03-agents")
}
fn offer() -> (tempfile::TempDir, Paths) {
    state_home("08-offer")
}
/// The owner's pane right after a compaction: the transcript ends at a compact boundary, so the
/// breakdown is gone while the context reading stays (no session-mix donut).
fn no_breakdown() -> (tempfile::TempDir, Paths) {
    let (home, paths) = owner_home();
    let path = paths.projects.join("fixture/sess-now.jsonl");
    let mut text = fs::read_to_string(&path).unwrap();
    let at = chrono::DateTime::from_timestamp(NOW as i64 - 30, 0)
        .unwrap()
        .to_rfc3339();
    text.push_str(
        &json!({"type":"system","subtype":"compact_boundary","timestamp":at,
            "compactMetadata":{"preTokens":190000,"durationMs":40000}})
        .to_string(),
    );
    text.push('\n');
    fs::write(&path, text).unwrap();
    (home, paths)
}
/// The owner's pane with Claude Code's auto-compact window set in settings.json, as the owner
/// runs it: the Context bar spans 300k, its last 33k the auto-compact buffer.
fn window_300k() -> (tempfile::TempDir, Paths) {
    let (home, paths) = owner_home();
    let path = paths.home.join(".claude/settings.json");
    let mut settings: Value = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    settings["autoCompactWindow"] = json!(300000);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, settings.to_string()).unwrap();
    (home, paths)
}
fn after() -> (tempfile::TempDir, Paths) {
    state_home("06-after")
}
fn away() -> (tempfile::TempDir, Paths) {
    state_home("07-away")
}

const FIXTURES: [Fixture; 21] = [
    Fixture {
        name: "few",
        home: few,
        keys: &[],
    },
    Fixture {
        name: "many",
        home: owner_home,
        keys: &[],
    },
    Fixture {
        name: "all-dreams",
        home: dreaming,
        keys: &[],
    },
    Fixture {
        name: "quiet",
        home: quiet,
        keys: &[],
    },
    Fixture {
        name: "proposals-open",
        home: owner_home,
        keys: &["d"],
    },
    Fixture {
        name: "long-text",
        home: long_text,
        keys: &[],
    },
    Fixture {
        name: "empty",
        home: empty,
        keys: &[],
    },
    Fixture {
        name: "offer",
        home: offer,
        keys: &[],
    },
    Fixture {
        name: "compacting",
        home: compacting,
        keys: &[],
    },
    Fixture {
        name: "compacted",
        home: after,
        keys: &[],
    },
    Fixture {
        name: "away",
        home: away,
        keys: &[],
    },
    Fixture {
        name: "no-breakdown",
        home: no_breakdown,
        keys: &[],
    },
    Fixture {
        name: "window-300k",
        home: window_300k,
        keys: &[],
    },
    Fixture {
        name: "figure-rings",
        home: few,
        keys: &["@rings", "i"],
    },
    Fixture {
        name: "section",
        home: agents,
        keys: &["a"],
    },
    Fixture {
        name: "agents-bar",
        home: agents,
        keys: &[],
    },
    Fixture {
        name: "figure",
        home: dreaming,
        keys: &["i"],
    },
    Fixture {
        name: "story-page",
        home: dreaming,
        keys: &["t"],
    },
    Fixture {
        name: "changes-page",
        home: dreaming,
        keys: &["l"],
    },
    Fixture {
        name: "stack-open",
        home: dreaming,
        keys: &["e"],
    },
    Fixture {
        name: "stack-next",
        home: dreaming,
        keys: &["e", "n"],
    },
];

struct Shot {
    out: Vec<String>,
    placed: Vec<Placement>,
    metrics: Option<Metrics>,
}
fn shot(
    home: fn() -> (tempfile::TempDir, Paths),
    keys: &[&str],
    (cols, rows, cell): (usize, usize, (u32, u32)),
    theme: Theme,
) -> Shot {
    set_cell_override(Some(cell));
    let (_home, paths) = home();
    let mut pane = Pane::new(paths, &args("main", "sess-now", cols, rows));
    pane.theme = theme;
    pane.pixels = true;
    pane.enable_cards();
    // `@rings` is not a key: it asks for the older ring layout (`--rings`), which keeps the
    // figure-facts card under `i`.
    pane.rings = keys.contains(&"@rings");
    let keys: Vec<&str> = keys
        .iter()
        .copied()
        .filter(|k| !k.starts_with('@'))
        .collect();
    // Keys meet the frame before them, as in the running pane.
    pane.frame(cols, rows, NOW);
    for key in &keys {
        pane.key(key);
        pane.frame(cols, rows, NOW);
    }
    let out = pane.frame(cols, rows, NOW);
    set_cell_override(None);
    Shot {
        out,
        placed: pane.placed.clone(),
        metrics: pane.instrument_metrics.clone(),
    }
}
fn redo(rule: usize, what: &str, need: f32, limit: f32, detail: &str) -> String {
    format!(
        "PANE_RULES #{rule}: {what}: need {need:.1} pt, limit {limit:.1} pt: {detail}. \
         REDO: the layout violates PANE_RULES #{rule}"
    )
}
fn registered(title: &str) -> Option<&'static csr_engine::companion::render::instrument::CardSpec> {
    CARD_REGISTRY.iter().find(|c| c.titles.contains(&title))
}

/// Every rule a single frame can break, as redo messages.
fn violations(
    fixture: &Fixture,
    size: (usize, usize, (u32, u32)),
    theme: Theme,
    s: &Shot,
) -> Vec<String> {
    let (cols, rows, cell) = size;
    let what = format!("{} {cols}x{rows} {theme:?}", fixture.name);
    let mut bad = vec![];
    // #1 the frame: text rows and pictures stay above and on the key row.
    if s.out.len() > rows {
        bad.push(redo(
            1,
            &what,
            s.out.len() as f32,
            rows as f32,
            "text rows past the pane",
        ));
    }
    for p in &s.placed {
        if p.row + p.rows > rows.saturating_sub(1) {
            bad.push(redo(
                1,
                &what,
                (p.row + p.rows) as f32,
                rows.saturating_sub(1) as f32,
                "picture rows over the key row",
            ));
        }
        if let Ok(img) = image::load_from_memory(&p.png) {
            let limit = (rows.saturating_sub(1) as u32 * cell.1) as f32;
            if p.rows > 1 && img.height() as f32 > limit {
                bad.push(redo(1, &what, img.height() as f32, limit, "picture px"));
            }
        }
    }
    let Some(m) = &s.metrics else {
        return bad;
    };
    for card in &m.cards {
        if card.top + card.height > m.height + 0.5 {
            bad.push(redo(1, &what, card.top + card.height, m.height, card.title));
        }
    }
    for run in &m.text_runs {
        if run.3 > m.height + 0.5 {
            bad.push(redo(
                1,
                &what,
                run.3,
                m.height,
                &format!("text {:?}", run.4),
            ));
        }
    }
    // #2 the canvas is the pane's: nothing grew it, nothing was cut.
    if m.height > m.limit + 0.5 {
        bad.push(redo(2, &what, m.height, m.limit, "the canvas grew"));
    }
    if let Some(need) = m.clipped {
        bad.push(redo(
            2,
            &what,
            need,
            m.limit,
            "the last step still overflowed: clipped",
        ));
    }
    // #4 tier order: a stackable card drawn while a card that folds before it is folded.
    if let Some(stack) = &m.stack {
        for card in &m.cards {
            let Some(kind) = registered(card.title).and_then(|c| c.stack) else {
                continue;
            };
            if stack.open == Some(kind) {
                continue;
            }
            if let Some(earlier) = stack.kinds.iter().find(|k| **k > kind) {
                bad.push(redo(
                    4,
                    &what,
                    0.,
                    m.limit,
                    &format!(
                        "{} drawn while {} is folded",
                        card.title, STACKABLE[*earlier].1
                    ),
                ));
            }
        }
        // #3 the stack never holds a T0 or T1 card.
        for kind in &stack.kinds {
            let spec = CARD_REGISTRY.iter().find(|c| c.stack == Some(*kind));
            if spec.is_none_or(|c| c.tier != 2) {
                bad.push(redo(3, &what, 0., m.limit, "a pinned card in the stack"));
            }
        }
    }
    // #5 content inside its card.
    for (title, need, room) in &m.overdrawn {
        bad.push(redo(5, &what, *need, *room, title));
    }
    // #6 no title-only card.
    for card in &m.cards {
        if card.body_rows == 0 {
            bad.push(redo(
                6,
                &what,
                card.height,
                m.limit,
                &format!("{} has only its title", card.title),
            ));
        }
    }
    // #9 no overlapping text runs.
    let runs = &m.text_runs;
    for i in 0..runs.len() {
        for j in i + 1..runs.len() {
            let (a, b) = (&runs[i], &runs[j]);
            let w = a.2.min(b.2) - a.0.max(b.0);
            let h = a.3.min(b.3) - a.1.max(b.1);
            if w > 0.5 && h > 0.5 {
                bad.push(redo(
                    9,
                    &what,
                    h,
                    0.,
                    &format!(
                        "{:?} at {:.1},{:.1} overlaps {:?} at {:.1},{:.1}",
                        a.4, a.0, a.1, b.4, b.0, b.1
                    ),
                ));
            }
        }
    }
    // #10 every drawn card is registered.
    for card in &m.cards {
        if registered(card.title).is_none() {
            bad.push(redo(
                10,
                &what,
                0.,
                m.limit,
                &format!("{} is not in CARD_REGISTRY", card.title),
            ));
        }
    }
    bad
}

/// Rules 1-6, 9 and 10 hold for every fixture at every size in both themes.
#[test]
fn every_fixture_fits_its_pane_at_every_size() {
    let mut bad = vec![];
    let mut frames = 0;
    for fixture in &FIXTURES {
        for size in SIZES {
            for theme in THEMES {
                let s = shot(fixture.home, fixture.keys, size, theme);
                frames += 1;
                bad.extend(violations(fixture, size, theme, &s));
            }
        }
    }
    eprintln!("pane contract: {frames} frames, {} violations", bad.len());
    assert!(
        bad.is_empty(),
        "{} violations:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// Rule 7: every card folded into a stack opens from it, by `e` then `n`, and by a tap on its
/// title; `e` again closes it.
#[test]
fn every_folded_card_opens_from_the_stack() {
    let mut bad = vec![];
    let mut opened = 0;
    for fixture in FIXTURES.iter().filter(|f| f.keys.is_empty()) {
        for size in SIZES {
            let closed = shot(fixture.home, &[], size, Theme::Dark);
            let Some(stack) = closed.metrics.as_ref().and_then(|m| m.stack.clone()) else {
                continue;
            };
            let what = format!("{} {}x{}", fixture.name, size.0, size.1);
            if !closed.out.last().is_some_and(|l| l.contains("e more")) {
                bad.push(redo(7, &what, 0., 0., "the key row has no `e more`"));
            }
            for (i, kind) in stack.kinds.iter().enumerate() {
                let mut keys = vec!["e"];
                keys.extend(std::iter::repeat_n("n", i));
                let open = shot(fixture.home, &keys, size, Theme::Dark);
                let m = open.metrics.as_ref().expect("the instrument drew");
                let titles = CARD_REGISTRY
                    .iter()
                    .find(|c| c.stack == Some(*kind))
                    .map(|c| c.titles)
                    .unwrap_or(&[]);
                let drawn = m.cards.iter().any(|c| titles.contains(&c.title));
                if !drawn || m.stack.as_ref().and_then(|s| s.open) != Some(*kind) {
                    bad.push(redo(
                        7,
                        &what,
                        0.,
                        m.limit,
                        &format!("{} did not open with {keys:?}", STACKABLE[*kind].1),
                    ));
                } else {
                    opened += 1;
                }
                // A tap on the title opens it too.
                let tapped = shot(
                    fixture.home,
                    &[Box::leak(format!("stack:{kind}").into_boxed_str())],
                    size,
                    Theme::Dark,
                );
                if tapped
                    .metrics
                    .as_ref()
                    .and_then(|m| m.stack.as_ref())
                    .and_then(|s| s.open)
                    != Some(*kind)
                {
                    bad.push(redo(
                        7,
                        &what,
                        0.,
                        0.,
                        &format!("a tap on {} did not open it", STACKABLE[*kind].1),
                    ));
                }
            }
            let closed_again = shot(fixture.home, &["e", "e"], size, Theme::Dark);
            if closed_again
                .metrics
                .as_ref()
                .and_then(|m| m.stack.as_ref())
                .and_then(|s| s.open)
                .is_some()
            {
                bad.push(redo(
                    7,
                    &what,
                    0.,
                    0.,
                    "`e` again did not close the open card",
                ));
            }
        }
    }
    eprintln!("pane contract: {opened} folded cards opened from the stack");
    assert!(
        opened > 0,
        "no fixture folded a card: the stack is untested"
    );
    assert!(
        bad.is_empty(),
        "{} violations:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// Rule 10: every registered card is drawn by some fixture at some size, so a new card cannot
/// skip the contract.
#[test]
fn every_registered_card_is_drawn_by_a_fixture() {
    let mut seen: Vec<&str> = vec![];
    for fixture in &FIXTURES {
        for size in SIZES {
            let s = shot(fixture.home, fixture.keys, size, Theme::Dark);
            if let Some(m) = &s.metrics {
                seen.extend(m.cards.iter().map(|c| c.title));
            }
        }
    }
    let missing: Vec<_> = CARD_REGISTRY
        .iter()
        .filter(|c| !c.titles.iter().any(|t| seen.contains(t)))
        .map(|c| c.titles[0])
        .collect();
    assert!(
        missing.is_empty(),
        "PANE_RULES #10: no fixture draws {missing:?}. REDO: the layout violates PANE_RULES #10"
    );
}

/// Before/after pictures of every fixture: `CSR_PANE_SHOTS=<dir> CSR_PANE_TAG=before|after`.
#[test]
fn pane_contract_pictures() {
    let Some(dir) = std::env::var_os("CSR_PANE_SHOTS") else {
        return;
    };
    let tag = std::env::var("CSR_PANE_TAG").unwrap_or_else(|_| "shot".into());
    let dir = std::path::PathBuf::from(dir);
    fs::create_dir_all(&dir).unwrap();
    for fixture in &FIXTURES {
        for size in SIZES {
            for theme in THEMES {
                let s = shot(fixture.home, fixture.keys, size, theme);
                if let Some(p) = s.placed.first() {
                    let name = format!(
                        "{tag}-{}-{}x{}-{}.png",
                        fixture.name,
                        size.0,
                        size.1,
                        if theme == Theme::Dark {
                            "dark"
                        } else {
                            "light"
                        }
                    );
                    fs::write(dir.join(name), &p.png).unwrap();
                }
            }
        }
    }
}

/// Rule 12: the session-mix donut and its verdict ride the parts bar's line. They add no row
/// (the donut sits inside the gaps around the bar), show whenever the breakdown has parts, and
/// stay hidden when it has none (the bar is the one "In use" part).
#[test]
fn the_session_mix_rides_the_bar_line_and_adds_no_row() {
    let mut bad = vec![];
    let (mut shown, mut hidden) = (0, 0);
    for fixture in FIXTURES {
        for size in SIZES {
            let s = shot(fixture.home, fixture.keys, size, Theme::Dark);
            let Some(d) = s.metrics.as_ref().and_then(|m| m.dash.as_ref()) else {
                continue;
            };
            let Some((_, bar_y, _, bar_h)) = d.context_bar else {
                continue;
            };
            let what = format!("{} {}x{}", fixture.name, size.0, size.1);
            let breakdown = !d.segments.is_empty() && d.segments.iter().all(|s| s.name != "In use");
            match (&d.mix, breakdown) {
                (Some((verdict, shares, (_, cy, r))), true) => {
                    shown += 1;
                    if !["tool-heavy", "context-heavy", "mixed"].contains(&verdict.as_str()) {
                        bad.push(redo(12, &what, 0., 0., &format!("verdict {verdict:?}")));
                    }
                    let sum: f32 = shares.iter().sum();
                    if (sum - 1.).abs() > 0.01 {
                        bad.push(redo(12, &what, 0., 0., &format!("shares sum {sum}")));
                    }
                    // Inside the S1 gap above the bar and the S2 gap below it.
                    if cy - r < bar_y - 4. - 0.01 || cy + r > bar_y + bar_h + 8. + 0.01 {
                        bad.push(redo(
                            12,
                            &what,
                            cy + r,
                            bar_y + bar_h + 8.,
                            "the donut leaves the bar's line",
                        ));
                    }
                }
                (None, false) => hidden += 1,
                (Some(_), false) => {
                    bad.push(redo(12, &what, 0., 0., "a mix drawn with no breakdown"))
                }
                (None, true) => {
                    bad.push(redo(12, &what, 0., 0., "a breakdown with no mix verdict"))
                }
            }
        }
    }
    eprintln!("pane contract: session mix shown {shown}, hidden {hidden}");
    assert!(shown > 0, "no fixture drew the session mix");
    assert!(
        hidden > 0,
        "no fixture drew a Context card without a breakdown"
    );
    assert!(
        bad.is_empty(),
        "{} violations:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// Rule 13: the Context bar spans the window Claude Code compacts to, read from settings.json
/// (`autoCompactWindow`) as the compactor reads it, with the last 33k drawn as the auto-compact
/// buffer; without the setting it spans the model's window and draws no buffer.
#[test]
fn the_context_bar_spans_the_auto_compact_window_from_settings() {
    let mut bad = vec![];
    for (home, window) in [
        (
            window_300k as fn() -> (tempfile::TempDir, Paths),
            Some(300000.),
        ),
        (owner_home, None),
    ] {
        for size in SIZES {
            let s = shot(home, &[], size, Theme::Dark);
            let Some(d) = s.metrics.as_ref().and_then(|m| m.dash.as_ref()) else {
                continue;
            };
            let Some((bx, _, bw, _)) = d.context_bar else {
                continue;
            };
            let what = format!("{window:?} {}x{}", size.0, size.1);
            let head = d.context_head.clone().unwrap_or_default();
            match window {
                Some(w) => {
                    if d.window != Some(w)
                        || !head.contains("of 300k")
                        || !head.contains("33k buffer")
                    {
                        bad.push(redo(
                            13,
                            &what,
                            0.,
                            0.,
                            &format!("window {:?}, head {head:?}", d.window),
                        ));
                    }
                    // The buffer is the bar's last 33/300.
                    match d.buffer {
                        Some((x, bwid))
                            if ((bwid / bw) - 33. / 300.).abs() < 0.01
                                && (x + bwid - (bx + bw)).abs() < 0.1 => {}
                        other => bad.push(redo(
                            13,
                            &what,
                            0.,
                            0.,
                            &format!("buffer {other:?} on bar {bx}+{bw}"),
                        )),
                    }
                }
                None => {
                    if d.window.is_some() || d.buffer.is_some() || !head.contains("of 1M") {
                        bad.push(redo(
                            13,
                            &what,
                            0.,
                            0.,
                            &format!("no setting, yet window {:?} head {head:?}", d.window),
                        ));
                    }
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} violations:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// Frame time receipt: 240 frames of the owner's pane (35x30 at the 26x58 cell) through
/// `Pane::frame`, p50 and p95 in ms. Run with `--ignored --nocapture` on the build machine.
#[test]
#[ignore]
fn frame_time_receipt() {
    set_cell_override(Some((26, 58)));
    let home: fn() -> (tempfile::TempDir, Paths) = if std::env::var_os("CSR_PERF_OWNER").is_some() {
        owner_home
    } else {
        window_300k
    };
    let (_home, paths) = home();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 35, 30));
    pane.theme = Theme::Dark;
    pane.pixels = true;
    pane.enable_cards();
    for _ in 0..10 {
        pane.frame(35, 30, NOW);
    }
    let encodes = pane.encodes;
    // The pane keeps a drawn frame while its draw key holds: the theme flips every frame so
    // each timed frame draws and encodes the picture.
    let mut ms: Vec<f64> = (0..240)
        .map(|i| {
            pane.theme = if i % 2 == 0 {
                Theme::Light
            } else {
                Theme::Dark
            };
            let start = std::time::Instant::now();
            pane.frame(35, 30, NOW);
            start.elapsed().as_secs_f64() * 1000.
        })
        .collect();
    set_cell_override(None);
    // Every timed frame drew and encoded the picture: nothing was served from a cache.
    assert_eq!(pane.encodes - encodes, 240);
    let mix = pane
        .instrument_metrics
        .as_ref()
        .and_then(|m| m.dash.as_ref())
        .and_then(|d| d.mix.as_ref())
        .map(|m| m.0.clone());
    eprintln!("frame_time_receipt: session mix {mix:?}");
    ms.sort_by(f64::total_cmp);
    let at = |q: f64| ms[((ms.len() - 1) as f64 * q).round() as usize];
    eprintln!(
        "frame_time_receipt: frames {} p50 {:.3} ms p95 {:.3} ms",
        ms.len(),
        at(0.5),
        at(0.95)
    );
}
