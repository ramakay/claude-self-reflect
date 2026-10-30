//! The pane contract: every scenario fixture, at every pane size and in both themes, through the
//! real `Pane::frame`, held to the rules in `src/companion/render/PANE_RULES.md`. A failure names
//! the rule, the fixture, the size, the need and the limit, and asks for a redo.
#[path = "support/pane_home.rs"]
mod pane_home;
use csr_engine::companion::{
    pane::Pane,
    paths::Paths,
    render::{
        instrument::{Metrics, CARD_REGISTRY, LAYOUT_WIDTH, SHOULD_BE_KINDS, STACKABLE},
        kitty::{bands, repaint_pictures, set_cell_override, Drawn, Placement},
        theme::Theme,
    },
};
use pane_home::*;
use serde_json::{json, Value};
use std::fs;

/// The pane sizes: cols x rows at the cell the terminal reports. 35x30 and 35x20 are the owner's
/// pane at its real 26x58 cell; 30x16 is the smallest supported (text rows); 32x16 the smallest
/// that draws the picture.
const SIZES: [(usize, usize, (u32, u32)); 7] = [
    (35, 30, (26, 58)),
    (44, 30, (16, 34)),
    (26, 58, (16, 34)),
    (35, 20, (26, 58)),
    (60, 24, (16, 34)),
    (30, 16, (16, 34)),
    (32, 16, (16, 34)),
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
/// The owner's session with a performance trial failing 40 checks: the card's leanest level
/// still has 40 rows, so the accordion must cut them.
fn perf_red() -> (tempfile::TempDir, Paths) {
    let (home, paths) = owner_home();
    let path = paths
        .home
        .join(".claude-self-reflect/trials/combo-soak.json");
    let mut trial: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    trial["checks"] = Value::Array(
        (0..40)
            .map(|i| {
                json!({"id":format!("rss{i}"),"label":format!("Memory under 1 MB, probe {i}"),
                    "metric":"rss_mb","op":"<=","threshold":1,"kind":"latest","unit":"MB",
                    "warmup_hours":0.})
            })
            .collect(),
    );
    fs::write(&path, trial.to_string()).unwrap();
    (home, paths)
}
/// The owner's session with the live trial: a 94-character candidate label and its short sha,
/// longer than the room right of the card's title.
const LIVE_LABEL: &str =
    "combo + pane + native-warn + herdr-events + flake-hunt + memtrend (bands rolled back)";
fn perf_long_label() -> (tempfile::TempDir, Paths) {
    let (home, paths) = owner_home();
    let path = paths
        .home
        .join(".claude-self-reflect/trials/combo-soak.json");
    let mut trial: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    trial["candidate"] = json!({"label": LIVE_LABEL, "sha": "a3744ba4c0de1f2a"});
    fs::write(&path, trial.to_string()).unwrap();
    (home, paths)
}
/// The owner's pane with a configured window under the 33k buffer: all of it is buffer.
fn window_20k() -> (tempfile::TempDir, Paths) {
    let (home, paths) = owner_home();
    let path = paths.home.join(".claude/settings.json");
    let mut settings: Value = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    settings["autoCompactWindow"] = json!(20000);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, settings.to_string()).unwrap();
    (home, paths)
}
/// The owner's pane with Claude Code's 300k auto-compact window in settings.json and two native
/// auto-compactions this week that came while CSR's compactor was on: the notice says the window
/// beat CSR, and how to let CSR lead.
fn native_warn() -> (tempfile::TempDir, Paths) {
    let (home, paths) = window_300k();
    let wins: String = [(3600., "auto"), (2. * 86400., "suggest")]
        .iter()
        .map(|(ago, compactor)| {
            json!({"at": NOW - ago, "sid": "sess-old", "compactor": compactor}).to_string() + "\n"
        })
        .collect();
    fs::write(paths.keep.join("native-wins.jsonl"), wins).unwrap();
    (home, paths)
}

/// A running session whose newest look suggests one should-be item of each `kinds`, with a long
/// line, so the row's text runs to its kind label.
fn should_be_home(kinds: &[&str]) -> (tempfile::TempDir, Paths) {
    let (home, paths) = few();
    let should_be: Vec<Value> = kinds
        .iter()
        .map(|kind| {
            json!({"kind":kind,"line":"Briefs for long workers repeat the same gate, resolver and flags.",
                "evidence":"Turns 2, 3"})
        })
        .collect();
    append_dream_log(
        &paths,
        json!({"kind":"look","session":"sess-now","at":NOW - 20.,"outcome":"none",
            "confusing_claude":[],"should_be":should_be}),
    );
    (home, paths)
}
/// Every label a should-be row can carry, the longest first.
fn every_kind() -> (tempfile::TempDir, Paths) {
    should_be_home(&["CLAUDE.md", "memory", "skill", "hook", "tool"])
}
fn after() -> (tempfile::TempDir, Paths) {
    state_home("06-after")
}
fn away() -> (tempfile::TempDir, Paths) {
    state_home("07-away")
}

const FIXTURES: [Fixture; 29] = [
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
        name: "native-warn",
        home: native_warn,
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
    Fixture {
        name: "perf-red",
        home: perf_red,
        keys: &[],
    },
    Fixture {
        name: "perf-red-open",
        home: perf_red,
        keys: &["stack:3"],
    },
    Fixture {
        name: "perf-long-label",
        home: perf_long_label,
        keys: &[],
    },
    Fixture {
        name: "window-20k",
        home: window_20k,
        keys: &[],
    },
    Fixture {
        name: "section-then-e",
        home: agents,
        keys: &["a", "e"],
    },
    Fixture {
        name: "every-kind",
        home: every_kind,
        keys: &[],
    },
    Fixture {
        name: "stack-esc",
        home: dreaming,
        keys: &["e", "escape"],
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
/// A live pane after `keys`, for taps and resizes; the cell override stays set until `done`.
fn live(
    home: fn() -> (tempfile::TempDir, Paths),
    keys: &[&str],
    (cols, rows, cell): (usize, usize, (u32, u32)),
) -> (tempfile::TempDir, Pane) {
    set_cell_override(Some(cell));
    let (dir, paths) = home();
    let mut pane = Pane::new(paths, &args("main", "sess-now", cols, rows));
    pane.theme = Theme::Dark;
    pane.pixels = true;
    pane.enable_cards();
    pane.frame(cols, rows, NOW);
    for key in keys {
        pane.key(key);
        pane.frame(cols, rows, NOW);
    }
    (dir, pane)
}
fn shot_of(pane: &mut Pane, cols: usize, rows: usize) -> Shot {
    let out = pane.frame(cols, rows, NOW);
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

/// Rule 9: no two text runs share any area. A run's extent is the drawn advance of its text, from
/// where the canvas placed it, so a label longer than its column overlaps the text after it.
fn overlaps(runs: &[(f32, f32, f32, f32, String)], what: &str) -> Vec<String> {
    let mut bad = vec![];
    for i in 0..runs.len() {
        for j in i + 1..runs.len() {
            let (a, b) = (&runs[i], &runs[j]);
            let w = a.2.min(b.2) - a.0.max(b.0);
            let h = a.3.min(b.3) - a.1.max(b.1);
            if w > 0.5 && h > 0.5 {
                bad.push(redo(
                    9,
                    what,
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
    bad
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
    // #2 the canvas is the pane's: nothing grew it, nothing was cut. The limit comes from the
    // pane alone (a page keeps its own floor of rows).
    let page = fixture.keys.iter().any(|k| matches!(*k, "t" | "l"));
    let pane_limit = ((rows - 1) as f32 * cell.1 as f32 * LAYOUT_WIDTH
        / (cols as f32 * cell.0 as f32))
        .max(800.);
    if !page && m.limit > pane_limit + 0.5 {
        bad.push(redo(
            2,
            &what,
            m.limit,
            pane_limit,
            "the limit grew past the pane's own",
        ));
    }
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
    // #5 content inside its card: what the drawing code reports, and every text run inside
    // some card's band, so a run in the gap between cards is caught whoever drew it.
    for (title, need, room) in &m.overdrawn {
        bad.push(redo(5, &what, *need, *room, title));
    }
    for run in &m.text_runs {
        let inside = m
            .cards
            .iter()
            .any(|c| run.1 >= c.top - 0.5 && run.3 <= c.top + c.height + 0.5);
        if !inside {
            bad.push(redo(
                5,
                &what,
                run.3,
                0.,
                &format!(
                    "text {:?} at {:.1}-{:.1} is in no card",
                    run.4, run.1, run.3
                ),
            ));
        }
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
    bad.extend(overlaps(&m.text_runs, &what));
    // #10 every panel drawn is a registered card region.
    for (y, h) in &m.panels {
        let owned = m.cards.iter().any(|c| {
            (c.top - y).abs() < 0.5 && (c.height - h).abs() < 0.5 && registered(c.title).is_some()
        });
        if !owned {
            bad.push(redo(
                10,
                &what,
                *h,
                0.,
                &format!("a panel at {y:.1} ({h:.1} pt) belongs to no registered card"),
            ));
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
                // An open card keeps every frame rule.
                bad.extend(violations(fixture, size, Theme::Dark, &open));
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
                // A tap on the title, at its cells in the stack's terminal row, opens it too.
                let (_dir, mut pane) = live(fixture.home, &[], size);
                let stack_rows: Vec<(usize, Value)> = pane
                    .acts
                    .iter()
                    .filter(|(_, a)| a["key"] == "instrument:stack")
                    .map(|(r, a)| (*r, a["stack"].clone()))
                    .collect();
                let span = stack_rows.first().and_then(|(r, spans)| {
                    spans
                        .as_array()?
                        .iter()
                        .find(|s| s[2].as_u64() == Some(*kind as u64))
                        .and_then(|s| Some((*r, s[0].as_u64()? as usize, s[1].as_u64()? as usize)))
                });
                match span {
                    Some((row, c0, c1)) if c1 > c0 => {
                        let mut wrong = None;
                        // Both edge cells (where rounding could hand a cell to a neighbour)
                        // and the middle.
                        let mut cols = vec![c0, (c0 + c1) / 2, c1 - 1];
                        cols.dedup();
                        for col in cols {
                            let (_d, mut p) = live(fixture.home, &[], size);
                            p.click(row, col, NOW);
                            p.frame(size.0, size.1, NOW);
                            let got = p
                                .instrument_metrics
                                .as_ref()
                                .and_then(|m| m.stack.as_ref())
                                .and_then(|s| s.open);
                            if got != Some(*kind) {
                                wrong = Some((col, got));
                                break;
                            }
                        }
                        if let Some((col, got)) = wrong {
                            bad.push(redo(
                                7,
                                &what,
                                0.,
                                0.,
                                &format!(
                                    "a tap at row {row} col {col} on {} opened {got:?}",
                                    STACKABLE[*kind].1
                                ),
                            ));
                        }
                    }
                    _ => bad.push(redo(
                        7,
                        &what,
                        0.,
                        0.,
                        &format!("{} has no tap target in the stack row", STACKABLE[*kind].1),
                    )),
                }
                // Resized while open, the pane still keeps every frame rule.
                pane.key("e");
                pane.frame(size.0, size.1, NOW);
                let smaller = (size.0, (size.1 * 2 / 3).max(16), size.2);
                let resized = shot_of(&mut pane, smaller.0, smaller.1);
                bad.extend(violations(fixture, smaller, Theme::Dark, &resized));
                set_cell_override(None);
            }
            for close in [["e", "e"], ["e", "escape"]] {
                let closed_again = shot(fixture.home, &close, size, Theme::Dark);
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
                        &format!("{close:?} did not close the open card"),
                    ));
                }
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
                    // Tools or Context at half the session or more names it; else "mixed".
                    let want = if shares[0] >= 0.5 {
                        "tool-heavy"
                    } else if shares[1] >= 0.5 {
                        "context-heavy"
                    } else {
                        "mixed"
                    };
                    if verdict != want {
                        bad.push(redo(
                            12,
                            &what,
                            0.,
                            0.,
                            &format!("verdict {verdict:?} for shares {shares:?}, want {want:?}"),
                        ));
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
    let mut checked = [0; 3];
    for (case, (home, window)) in [
        (
            window_300k as fn() -> (tempfile::TempDir, Paths),
            Some(300000_f64),
        ),
        (window_20k, Some(20000.)),
        (owner_home, None),
    ]
    .into_iter()
    .enumerate()
    {
        for size in SIZES {
            let s = shot(home, &[], size, Theme::Dark);
            let Some(d) = s.metrics.as_ref().and_then(|m| m.dash.as_ref()) else {
                continue;
            };
            let Some((bx, _, bw, _)) = d.context_bar else {
                continue;
            };
            checked[case] += 1;
            let what = format!("{window:?} {}x{}", size.0, size.1);
            let head = d.context_head.clone().unwrap_or_default();
            match window {
                Some(w) => {
                    let buffer = w.min(33000.);
                    if d.window != Some(w)
                        || !head.contains(&format!("of {}k", w / 1000.))
                        || !head.contains(&format!("{}k buffer", buffer / 1000.))
                    {
                        bad.push(redo(
                            13,
                            &what,
                            0.,
                            0.,
                            &format!("window {:?}, head {head:?}", d.window),
                        ));
                    }
                    // The buffer is the bar's last 33k of the window (all of a smaller one).
                    match d.buffer {
                        Some((x, bwid))
                            if ((bwid / bw) as f64 - buffer / w).abs() < 0.01
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
    eprintln!("pane contract: window frames checked {checked:?} (300k, 20k, none)");
    assert!(
        checked.iter().all(|n| *n > 0),
        "a window case drew no Context bar: {checked:?}"
    );
    assert!(
        bad.is_empty(),
        "{} violations:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// The native-window warning is a notice in the pane: both of its sentences reach the picture
/// whole, at every size; without the evidence (no wins, no window, a 1M window, the kill switch
/// is not tested here: env is shared) the notice is not drawn.
#[test]
fn the_native_window_warning_is_a_whole_notice_and_drops_without_evidence() {
    let want_start = "Claude's 300k auto-compact window beat CSR 2\u{d7} this week.";
    let want_end = "Remove autoCompactWindow from settings.json to let CSR lead.";
    let mut bad = vec![];
    let mut drawn = 0;
    for size in SIZES {
        let s = shot(native_warn, &[], size, Theme::Dark);
        let Some(m) = &s.metrics else { continue };
        let Some(card) = m.cards.iter().find(|c| c.title == "notice") else {
            continue;
        };
        drawn += 1;
        let text: Vec<&str> = m
            .text_runs
            .iter()
            .filter(|r| r.1 >= card.top && r.3 <= card.top + card.height + 0.5)
            .map(|r| r.4.as_str())
            .collect();
        let joined = text.join(" ");
        let what = format!("native-warn {}x{}", size.0, size.1);
        if !joined.contains(want_start) || !joined.contains(want_end) || joined.contains('\u{2026}')
        {
            bad.push(redo(5, &what, 0., 0., &format!("notice drew {joined:?}")));
        }
    }
    assert!(drawn > 0, "no size drew the native-window notice");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    // no wins: no notice
    for home in [
        window_300k as fn() -> (tempfile::TempDir, Paths),
        owner_home,
    ] {
        let s = shot(home, &[], SIZES[0], Theme::Dark);
        let drew = s.metrics.as_ref().is_some_and(|m| {
            m.text_runs
                .iter()
                .any(|r| r.4.contains("auto-compact window beat CSR"))
        });
        assert!(!drew, "the notice drew without a native win");
    }
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

/// Rule 9 for every kind the dreamer can write: each should-be label is drawn, whole, at every
/// size in both themes (from the stack when its card folds there), and runs into nothing. The kinds
/// come from the producer's own table, so a new kind is covered the day it is added.
#[test]
fn every_should_be_kind_is_drawn_whole_and_overlaps_nothing() {
    let mut bad = vec![];
    let mut checked = 0;
    let mut skipped = vec![];
    let mut kinds: Vec<&str> = SHOULD_BE_KINDS.iter().map(|(raw, _)| *raw).collect();
    kinds.push("an-unknown-kind");
    for raw in kinds {
        let label = SHOULD_BE_KINDS
            .iter()
            .find(|(r, _)| *r == raw)
            .map_or("tool", |(_, l)| *l);
        for (cols, rows, cell) in SIZES {
            for theme in THEMES {
                set_cell_override(Some(cell));
                let (_home, paths) = should_be_home(&[raw]);
                let mut pane = Pane::new(paths, &args("main", "sess-now", cols, rows));
                pane.theme = theme;
                pane.pixels = true;
                pane.enable_cards();
                let what = format!("kind {raw:?} {cols}x{rows} {theme:?}");
                let drawn = |pane: &Pane| {
                    pane.instrument_metrics
                        .as_ref()
                        .map(|m| m.text_runs.clone())
                        .unwrap_or_default()
                };
                pane.frame(cols, rows, NOW);
                if pane.placed.is_empty() {
                    // A text-only size: the row is plain text, `kind: line`.
                    set_cell_override(None);
                    skipped.push(what);
                    continue;
                }
                let mut runs = drawn(&pane);
                bad.extend(overlaps(&runs, &what));
                // Folded into the stack: open its cards in turn until the label shows.
                let mut keys = ["e"].into_iter().chain(std::iter::repeat_n("n", 8));
                while !runs.iter().any(|r| r.4 == label) {
                    let Some(key) = keys.next() else { break };
                    pane.key(key);
                    pane.frame(cols, rows, NOW);
                    runs = drawn(&pane);
                    bad.extend(overlaps(&runs, &what));
                }
                set_cell_override(None);
                // A size with no room for the card at all, open or folded, has no row to check.
                if !runs.iter().any(|r| r.4.contains("Should be a skill")) {
                    skipped.push(what);
                    continue;
                }
                checked += 1;
                if !runs.iter().any(|r| r.4 == label) {
                    bad.push(format!(
                        "PANE_RULES #9: {what}: the label {label:?} is never drawn whole"
                    ));
                }
            }
        }
    }
    eprintln!(
        "should-be kinds: {checked} frames checked, {} with no room for the card, {} violations",
        skipped.len(),
        bad.len()
    );
    // Only the two sizes that draw no should-be card in the picture may go unchecked (26 columns
    // is the text pane; 30x16 the smallest), and every kind is checked at every other size in both
    // themes: (kinds) x (sizes but two) x (themes) frames.
    assert!(
        skipped
            .iter()
            .all(|w| w.contains(" 26x58 ") || w.contains(" 30x16 ")),
        "{skipped:?}"
    );
    assert!(
        checked >= (SHOULD_BE_KINDS.len() + 1) * (SIZES.len() - 2) * THEMES.len(),
        "only {checked} frames checked"
    );
    assert!(
        bad.is_empty(),
        "{} violations:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// What one scroll step sends to the terminal: the bytes, the pictures sent and deleted, and
/// whether any picture is deleted before the last one is sent.
struct Step {
    bytes: usize,
    bands: usize,
    sent: usize,
    deleted: usize,
    /// Pictures sent under an id already on screen: herdr replaces those by deleting first.
    replaced: usize,
    delete_before_place: bool,
    clears: bool,
    ms: f64,
}
/// One frame of `pane` through the transport the watch loop uses, after `drawn`.
fn send(pane: &mut Pane, cols: usize, rows: usize, drawn: &mut Option<Drawn>) -> Step {
    let on_screen: Vec<usize> = drawn
        .as_ref()
        .map(|d| {
            (0..d.placed.len())
                .filter_map(|i| d.ids.get(i).copied())
                .collect()
        })
        .unwrap_or_default();
    let start = std::time::Instant::now();
    let written = pane.frame(cols, rows, NOW);
    let ms = start.elapsed().as_secs_f64() * 1000.;
    let cut = bands(&pane.placed);
    let out = repaint_pictures(drawn.as_ref(), cols, rows, &written, &cut);
    *drawn = Some(Drawn::after(
        drawn.as_ref(),
        cols,
        rows,
        written,
        cut.clone(),
    ));
    let replaced = out
        .split("\x1b_Ga=T,")
        .skip(1)
        .filter_map(|t| {
            t.split("i=")
                .nth(1)?
                .split(',')
                .next()?
                .parse::<usize>()
                .ok()
        })
        .filter(|id| on_screen.contains(id))
        .count();
    let last_place = out.rfind("\x1b_Ga=T");
    let first_delete = out.find("\x1b_Ga=d");
    Step {
        bytes: out.len(),
        bands: cut.len(),
        sent: out.matches("\x1b_Ga=T").count(),
        deleted: out.matches("\x1b_Ga=d").count(),
        replaced,
        delete_before_place: matches!((first_delete, last_place), (Some(d), Some(p)) if d < p),
        clears: out.contains("\x1b[2J") || out.contains("a=d,d=A"),
        ms,
    }
}
/// The owner's session with a Monologue longer than any pane: thirty dreams in this session.
fn long_monologue() -> (tempfile::TempDir, Paths) {
    let (home, paths) = dreaming();
    let dreams: Vec<Value> = (0..30)
        .map(|i| {
            json!({"id":format!("long-{i}"),"session":"sess-now","line":format!("What if row {i} of the monologue said more?"),
                "confidence":0.6})
        })
        .collect();
    append_dream_log(
        &paths,
        json!({"kind":"batch","session":"sess-now","at":NOW - 60.,"dreams":dreams}),
    );
    (home, paths)
}
/// The owner's session with more Changes than any pane holds: the map keeps four entries, so
/// each says a lot.
fn long_changes() -> (tempfile::TempDir, Paths) {
    let (home, paths) = dreaming();
    let con = rusqlite::Connection::open(&paths.db).unwrap();
    let raw: String = con
        .query_row(
            "SELECT value FROM meta WHERE key='story_map:sess-now'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut map: Value = serde_json::from_str(&raw).unwrap();
    let long = "the reader streams each chunk as it lands and keeps the rows it already drew, so a long transcript never waits for its tail";
    map["current"]["entries"] = (0..4)
        .map(|i| {
            json!({"kind":"added","claim":format!("Change {i}"),"before":[],
                "now":format!("Change {i}: {}.", [long; 8].join("; ")),
                "was":format!("Before change {i}: {}.", [long; 6].join("; ")),
                "after":[{"scope":format!("src/part_{i}.rs"),"sym":"run","mark":"+"}],
                "receipts":[format!("oid:{:08x}", 0x5100_0000 + i)]})
        })
        .collect();
    con.execute(
        "UPDATE meta SET value=? WHERE key='story_map:sess-now'",
        [map.to_string()],
    )
    .unwrap();
    (home, paths)
}
/// The flash on scroll: a `j` on the Monologue (`t`) and Changes (`l`) pages sends each band that
/// moved under a fresh id and deletes the old ids only after every new one is placed, with no
/// clear, inside one synchronized update. Run with `--nocapture` for the per-step receipt.
#[test]
fn a_page_scroll_places_new_bands_before_it_deletes_the_old() {
    for (page, home) in [
        ("t", long_monologue as fn() -> (tempfile::TempDir, Paths)),
        ("l", long_changes),
    ] {
        // The owner's pane at its shorter height first, then every contract size and two shorter
        // ones: the first one
        // where the page runs past the pane, so `j` scrolls.
        let scrolls = [(35, 20, (26, 58))]
            .into_iter()
            .chain(SIZES)
            .chain([(35, 12, (26, 58)), (35, 10, (26, 58))])
            .find_map(|size| {
                let (dir, mut pane) = live(home, &[page], size);
                let mut drawn = None;
                let first = send(&mut pane, size.0, size.1, &mut drawn);
                let max = pane
                    .instrument_metrics
                    .as_ref()
                    .and_then(|m| m.page_scroll_max);
                set_cell_override(None);
                (max.is_some_and(|m| m > 0) && first.bands > 1).then_some((dir, pane, drawn, size))
            });
        let Some((_dir, mut pane, mut drawn, (cols, rows, cell))) = scrolls else {
            panic!("{page}: no size scrolls the page; the probe proves nothing");
        };
        set_cell_override(Some(cell));
        eprintln!(
            "scroll {page} at {cols}x{rows}: key row {:?}",
            drawn
                .as_ref()
                .and_then(|d| d.written.last())
                .map(|l| csr_engine::companion::render::text::plain(l))
        );
        let mut moved = 0;
        for step in 1..=3 {
            pane.key("j");
            let s = send(&mut pane, cols, rows, &mut drawn);
            eprintln!(
                "scroll {page} j#{step}: {} bytes, {} bands, {} sent, {} replaced in place, {} deleted, delete before place: {}, clear: {}, frame {:.1} ms",
                s.bytes, s.bands, s.sent, s.replaced, s.deleted, s.delete_before_place, s.clears, s.ms
            );
            assert!(
                !s.delete_before_place,
                "{page} j#{step}: a picture is deleted before the last new one is placed"
            );
            assert!(!s.clears, "{page} j#{step}: a scroll clears the pane");
            assert_eq!(
                s.replaced, 0,
                "{page} j#{step}: a band is replaced under its own id"
            );
            assert_eq!(
                s.deleted, s.sent,
                "{page} j#{step}: each old band goes once its new one is up"
            );
            assert!(
                s.sent <= s.bands,
                "{page} j#{step}: more pictures than bands"
            );
            moved += s.sent;
        }
        assert!(
            moved > 0,
            "{page}: three j presses moved nothing; the probe proves nothing"
        );
        set_cell_override(None);
    }
}

/// The trial card's long candidate label gives way to the title: at the owner's pane the two
/// text runs share no area, the label keeps its short sha, and a short label is drawn whole.
#[test]
fn a_long_trial_label_keeps_clear_of_the_card_title_and_keeps_its_sha() {
    let size = (35, 30, (26, 58));
    for theme in THEMES {
        let s = shot(perf_long_label, &[], size, theme);
        let runs = &s.metrics.as_ref().expect("metrics").text_runs;
        let title = runs.iter().find(|r| r.4 == "CSR performance");
        let label = runs.iter().find(|r| r.4.ends_with("a3744ba4"));
        let (title, label) = (
            title.unwrap_or_else(|| panic!("{theme:?}: no title run in {runs:?}")),
            label.unwrap_or_else(|| panic!("{theme:?}: no label run ending in the sha")),
        );
        assert!(
            label.0 >= title.2,
            "{theme:?}: label starts at {:.1} pt, inside the title ending at {:.1} pt",
            label.0,
            title.2
        );
        assert!(label.4.len() < LIVE_LABEL.len(), "{theme:?}: label not cut");
        assert!(
            label.4.contains('\u{2026}'),
            "{theme:?}: cut without an ellipsis"
        );
        let fixture = FIXTURES
            .iter()
            .find(|f| f.name == "perf-long-label")
            .unwrap();
        let bad = violations(fixture, size, theme, &s);
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
    let s = shot(owner_home, &[], size, Theme::Dark);
    let runs = &s.metrics.as_ref().expect("metrics").text_runs;
    assert!(
        runs.iter().any(|r| r.4 == "combo ac9c24c3"),
        "a short label is drawn whole; runs with the sha: {:?}",
        runs.iter()
            .filter(|r| r.4.contains("ac9c24c3"))
            .collect::<Vec<_>>()
    );
}
