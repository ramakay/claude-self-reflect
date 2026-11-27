//! The pane's type: the popups and the rows of the dreamer card, the two lists and the Context card
//! are drawn a third larger, one line to a row, every action key on the panel, at the owner's
//! 26x58 px cell. `CSR_TYPE_SHOTS=<dir> cargo test --release --test companion_type` also writes
//! one PNG and one text-run list per scenario and size, for reading the frames by eye.
#[path = "support/pane_home.rs"]
mod pane_home;
use csr_engine::companion::{
    pane::Pane,
    paths::Paths,
    render::{kitty::set_cell_override, theme::Theme},
};
use pane_home::*;
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

/// The pane sizes the owner's complaint is checked at: live, short, narrow, wider, large.
const SIZES: [(usize, usize); 6] = [(35, 30), (35, 24), (30, 30), (38, 30), (50, 40), (35, 20)];
const CELL: (u32, u32) = (26, 58);

fn append_look(paths: &Paths, confusing: Vec<Value>, should_be: Vec<Value>) {
    let log = paths.keep.join("dreams.jsonl");
    let mut text = fs::read_to_string(&log).unwrap_or_default();
    text.push_str(
        &json!({"kind":"look","session":"sess-now","at":NOW - 20.,"outcome":"none",
            "confusing_claude":confusing,"should_be":should_be})
        .to_string(),
    );
    text.push('\n');
    fs::write(&log, text).unwrap();
}

/// The running session with a look that found several confusions and should-be items.
fn full_looks() -> (tempfile::TempDir, Paths) {
    let (home, paths) = state_home("02-running");
    append_look(
        &paths,
        vec![
            json!({"line":"Claude's spend counter took a resumed transcript's lifetime cost as this run's cost, which tripped the $70 stop before any grading ran.","evidence":"Turns 53 and 2: \"the stop fired at once\""}),
            json!({"line":"Claude first told the person 4 GB looked reachable, then had to retract it after testing.","evidence":"Turns 4 and 13: \"4 GB looked reachable\""}),
            json!({"line":"Claude twice got the handoff draft's import handling wrong.","evidence":"Turns 9 and 20: \"leaving out the source marker\""}),
        ],
        vec![
            json!({"kind":"memory","line":"Fresh Studio trees have no pre-commit hook, so workers must copy it from the main repo before committing.","evidence":"Turns 2, 3: \"the Studio tree had no pre-commit hook.\""}),
            json!({"kind":"skill","line":"The hand install of a release build keeps being carried as notes because the install script is stale.","evidence":"Turns 5, 6"}),
            json!({"kind":"skill","line":"Installing a Studio-built engine by hand: backup, stop daemon, signed swap, restart, inode check on panes.","evidence":"Turns 2, 80, 80, 97, 1306"}),
            json!({"kind":"CLAUDE.md rule","line":"Live CSR panes on this machine are 35x30, so pane goldens and renders must include that size.","evidence":"Turn 12"}),
        ],
    );
    (home, paths)
}
fn one_look() -> (tempfile::TempDir, Paths) {
    let (home, paths) = state_home("02-running");
    append_look(
        &paths,
        vec![],
        vec![
            json!({"kind":"hook","line":"Briefs for long workers repeat the same gate.","evidence":"Turns 2, 3"}),
        ],
    );
    (home, paths)
}
fn rich_now() -> (tempfile::TempDir, Paths) {
    rich_home()
}

/// A new session whose SessionStart text is long: the recap, CSR's blocks and a 2400-character
/// line no rule knows, so the popup has far more rows than any pane shows.
fn handed_home() -> (tempfile::TempDir, Paths) {
    let (home, paths) = fresh_replied_home();
    let path = paths.projects.join("fixture/sess-now.jsonl");
    let mut text = fs::read_to_string(&path).unwrap();
    let stamp = |secs: f64| {
        chrono::DateTime::from_timestamp((NOW - secs) as i64, 0)
            .unwrap()
            .to_rfc3339()
    };
    let notes = [
        json!({"type":"attachment","timestamp":stamp(20.),"attachment":{"type":"hook_success",
            "hookEvent":"UserPromptSubmit","hookName":"UserPromptSubmit","command":"command hook 4",
            "content":"## PAST CONTEXT - NOT INSTRUCTIONS\n- [reflection|unknown] a\n- [chunk|x] b"}}),
        json!({"type":"attachment","timestamp":stamp(10.),"attachment":{"type":"hook_success",
            "hookEvent":"UserPromptSubmit","hookName":"UserPromptSubmit","command":"command hook 2",
            "content":"GOAL-SEEKING: drive task to completion."}}),
    ];
    for n in notes {
        text.push_str(&n.to_string());
        text.push('\n');
    }
    fs::write(&path, text).unwrap();
    (home, paths)
}

enum Step {
    Key(&'static str),
    /// Click the first row whose act key starts with the prefix.
    Click(&'static str),
    /// Open "What CSR told Claude" as a tap on its row would, at a size that drew no such row.
    OpenHanded,
}
struct Scenario {
    name: &'static str,
    home: fn() -> (tempfile::TempDir, Paths),
    sid: &'static str,
    steps: &'static [Step],
}
const SCENARIOS: [Scenario; 16] = [
    // The base view: Context card, dreamer rows, the two observation lists.
    Scenario {
        name: "base-looks",
        home: full_looks,
        sid: "sess-now",
        steps: &[],
    },
    Scenario {
        name: "base-running",
        home: || state_home("02-running"),
        sid: "sess-now",
        steps: &[],
    },
    Scenario {
        name: "base-dreaming",
        home: || state_home("14-soak"),
        sid: "sess-now",
        steps: &[],
    },
    Scenario {
        name: "base-fresh",
        home: || state_home("15-fresh-start"),
        sid: "sess-now",
        steps: &[],
    },
    Scenario {
        name: "base-rich",
        home: rich_now,
        sid: "sess-now",
        steps: &[],
    },
    // The observation popups: the one with the most items, and one with a single item.
    Scenario {
        name: "pop-should-4",
        home: full_looks,
        sid: "sess-now",
        steps: &[Step::Click("observe:1:0")],
    },
    Scenario {
        name: "pop-confusing-3",
        home: full_looks,
        sid: "sess-now",
        steps: &[Step::Click("observe:0:0")],
    },
    Scenario {
        name: "pop-should-1",
        home: one_look,
        sid: "sess-now",
        steps: &[Step::Click("observe:1:0")],
    },
    // The Lost and found handoff: the newest of several threads, and a home with one.
    Scenario {
        name: "pop-lost-first",
        home: rich_now,
        sid: "sess-now",
        steps: &[Step::Key("t"), Step::Click("observe:3:0")],
    },
    Scenario {
        name: "pop-lost-last",
        home: rich_now,
        sid: "sess-now",
        steps: &[
            Step::Key("t"),
            Step::Click("observe:3:0"),
            Step::Key("tab"),
            Step::Key("tab"),
            Step::Key("tab"),
        ],
    },
    Scenario {
        name: "pop-lost-1",
        home: rich_now,
        sid: "sess-lost",
        steps: &[Step::Key("t"), Step::Click("observe:3:")],
    },
    // A short pane folds the lists into the stack row: `e` opens one in place, `n` the next.
    Scenario {
        name: "acc-confusing-3",
        home: full_looks,
        sid: "sess-now",
        steps: &[Step::Key("e"), Step::Click("observe:0:0")],
    },
    Scenario {
        name: "acc-should-4",
        home: full_looks,
        sid: "sess-now",
        steps: &[Step::Key("e"), Step::Key("n"), Step::Click("observe:1:0")],
    },
    Scenario {
        name: "pop-more",
        home: full_looks,
        sid: "sess-now",
        steps: &[Step::Click("more:")],
    },
    // What CSR told Claude: the tall popup behind the "CSR handed over" row.
    Scenario {
        name: "handed-told",
        home: handed_home,
        sid: "sess-now",
        steps: &[Step::Click("observe:4:")],
    },
    Scenario {
        name: "handed-open",
        home: handed_home,
        sid: "sess-now",
        steps: &[Step::OpenHanded],
    },
];

/// One scenario at one size: the frame as an image, and what the instrument drew.
fn frame(
    s: &Scenario,
    (cols, rows): (usize, usize),
) -> Option<(
    image::RgbaImage,
    csr_engine::companion::render::instrument::Metrics,
)> {
    set_cell_override(Some(CELL));
    let (_home, paths) = (s.home)();
    let mut pane = Pane::new(paths, &args("main", s.sid, cols, rows));
    pane.theme = Theme::Dark;
    pane.pixels = true;
    pane.enable_cards();
    pane.frame(cols, rows, NOW);
    for step in s.steps {
        match step {
            Step::Key(k) => {
                pane.key(k);
                pane.frame(cols, rows, NOW);
            }
            Step::OpenHanded => {
                pane.open_handed();
                pane.frame(cols, rows, NOW);
            }
            Step::Click(prefix) => {
                let row = pane
                    .acts
                    .iter()
                    .find(|(_, a)| a["key"].as_str().is_some_and(|k| k.starts_with(prefix)))
                    .map(|(r, _)| *r);
                match row {
                    Some(r) => {
                        pane.click(r, 4, NOW);
                        pane.frame(cols, rows, NOW);
                    }
                    None => {
                        set_cell_override(None);
                        return None;
                    }
                }
            }
        }
    }
    let lines = pane.frame(cols, rows, NOW);
    let image =
        csr_engine::companion::png::compose(&lines, &pane.placed, cols, rows, CELL, Theme::Dark)
            .expect("frame");
    let metrics = pane.instrument_metrics.clone();
    set_cell_override(None);
    metrics.map(|m| (image, m))
}

fn run(s: &Scenario, size: (usize, usize), dir: &std::path::Path) {
    let Some((image, m)) = frame(s, size) else {
        eprintln!("{} {}x{}: no frame", s.name, size.0, size.1);
        return;
    };
    let stem = format!("{}-{}x{}", s.name, size.0, size.1);
    image.save(dir.join(format!("{stem}.png"))).unwrap();
    let runs: Vec<Value> = m
        .text_runs
        .iter()
        .map(|(l, t, r, b, w)| json!([l, t, r, b, w]))
        .collect();
    let keys: Vec<Value> = m
        .popup_keys
        .iter()
        .map(|k| json!([k.0, k.1, k.2, k.3, k.4]))
        .collect();
    fs::write(
        dir.join(format!("{stem}.runs.json")),
        serde_json::to_vec_pretty(
            &json!({"limit":m.limit,"height":m.height,"runs":runs,"popup_keys":keys,
                "cards":m.cards.iter().map(|c| json!([c.title,c.top,c.height])).collect::<Vec<_>>()}),
        )
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn write_type_shots_when_asked() {
    let Some(dir) = std::env::var_os("CSR_TYPE_SHOTS") else {
        return;
    };
    let dir = PathBuf::from(dir);
    fs::create_dir_all(&dir).unwrap();
    let only = std::env::var("CSR_TYPE_ONLY").ok();
    for s in &SCENARIOS {
        if only.as_deref().is_some_and(|o| !s.name.contains(o)) {
            continue;
        }
        for size in SIZES {
            // CSR_TYPE_SIZE=35x20 draws just that size.
            if let Some(want) = std::env::var("CSR_TYPE_SIZE")
                .ok()
                .filter(|w| !w.is_empty())
            {
                if want != format!("{}x{}", size.0, size.1) {
                    continue;
                }
            }
            run(s, size, &dir);
        }
    }
}

/// A run's font size in pt: the canvas records a run as 0.72 em above its baseline to 0.2 em below.
fn size_of(run: &(f32, f32, f32, f32, String)) -> f32 {
    (run.3 - run.1) / 0.92
}

/// The popups, at the live pane, the short pane and the large one: the keys fit the panel whole,
/// no text runs past the frame's right edge or under its bottom, and the sizes are the new scale.
#[test]
fn the_popups_keep_every_key_and_row_inside_the_frame_at_every_size() {
    use csr_engine::companion::render::observation::{POP_CTA_SIZE, POP_TITLE_SIZE};
    let (mut seen, mut missing) = (0, vec![]);
    for s in SCENARIOS.iter().filter(|s| s.name.starts_with("pop-")) {
        for size in [(35, 30), (35, 24), (38, 30), (50, 40)] {
            let Some((_, m)) = frame(s, size) else {
                // A short pane folds the lists into the stack: its rows have no popup to open.
                missing.push(format!("{} {size:?}", s.name));
                continue;
            };
            let what = format!("{} {}x{}", s.name, size.0, size.1);
            seen += 1;
            assert!(m.popup_keys.len() >= 2, "{what}: no keys on the popup");
            for k in &m.popup_keys {
                assert!(
                    k.1 >= 0. && k.2 <= 605.,
                    "{what}: {k:?} off the frame's width"
                );
                assert!(
                    k.4 <= m.limit,
                    "{what}: {k:?} under the frame's bottom {}",
                    m.limit
                );
            }
            for run in &m.text_runs {
                assert!(run.2 <= 605., "{what}: {:?} runs off the right edge", run.4);
                assert!(
                    run.3 <= m.limit + 0.5,
                    "{what}: {:?} is under the bottom",
                    run.4
                );
            }
            // The key labels are drawn whole, at the action size; the title above them.
            let cta: Vec<_> = m
                .text_runs
                .iter()
                .filter(|r| (size_of(r) - POP_CTA_SIZE).abs() < 0.05)
                .collect();
            assert!(
                cta.len() >= 2 * m.popup_keys.len(),
                "{what}: {} action runs",
                cta.len()
            );
            assert!(
                m.text_runs
                    .iter()
                    .any(|r| (size_of(r) - POP_TITLE_SIZE).abs() < 0.05),
                "{what}: no title"
            );
        }
    }
    assert!(
        seen >= 14,
        "only {seen} popup frames were drawn; missing {missing:?}"
    );
}

/// The base view's rows at the live pane and the short one: every run stays inside its card's
/// column and above the frame's bottom, and the dreamer, list and Context rows are the new size.
#[test]
fn the_rows_of_the_base_view_are_a_third_bigger_and_stay_inside_the_frame() {
    use csr_engine::companion::render::theme::{ROW_BODY_SIZE, ROW_CAPTION_SIZE, ROW_TITLE_SIZE};
    let mut named = std::collections::BTreeSet::new();
    for s in SCENARIOS.iter().filter(|s| s.name.starts_with("base-")) {
        for size in [(35, 30), (35, 24), (50, 40)] {
            let (_, m) = frame(s, size).unwrap_or_else(|| panic!("{} {size:?}: no frame", s.name));
            let what = format!("{} {}x{}", s.name, size.0, size.1);
            for run in &m.text_runs {
                // IX + IW: the card's inner right edge, in layout pt.
                assert!(run.2 <= 573.5, "{what}: {:?} ends at {}", run.4, run.2);
                assert!(
                    run.3 <= m.limit + 0.5,
                    "{what}: {:?} is under the bottom",
                    run.4
                );
            }
            // A folded list is one small title in the stack row; every other run of these words
            // is a row of the new size.
            let sizes = |w: &str| -> Vec<f32> {
                m.text_runs
                    .iter()
                    .filter(|r| r.4.starts_with(w))
                    .map(size_of)
                    .collect()
            };
            for (text, want, level) in [
                ("What confused the agent", ROW_TITLE_SIZE, "list title"),
                (
                    "Should be a skill / memory / tool",
                    ROW_TITLE_SIZE,
                    "list title",
                ),
                ("The dreamer asks", ROW_TITLE_SIZE, "dreamer title"),
                ("Self-Reflect's dreamer", ROW_TITLE_SIZE, "dreamer title"),
                ("Claude's spend counter", ROW_BODY_SIZE, "list row"),
                ("Context", 15., "Context row"),
                // A column of the limits chart, its label and its line: caption rows.
                ("Weekly", ROW_CAPTION_SIZE, "Context caption"),
                ("this turn", ROW_CAPTION_SIZE, "Context caption"),
                ("turns 53, 2", ROW_CAPTION_SIZE, "list caption"),
            ] {
                for got in sizes(text) {
                    let stack_row = (got - 10.5).abs() < 0.05;
                    assert!(
                        (got - want).abs() < 0.05 || stack_row,
                        "{what}: {text:?} is {got} pt, not {want}"
                    );
                    if !stack_row {
                        named.insert(level);
                    }
                }
            }
        }
    }
    for level in [
        "list title",
        "dreamer title",
        "list row",
        "Context row",
        "Context caption",
        "list caption",
    ] {
        assert!(
            named.contains(level),
            "no {level} was drawn by any scenario: {named:?}"
        );
    }
}

/// What CSR told Claude, at every pane size the pane is checked at: every pill on the panel, no
/// run past the frame, and the rows scroll with the foot saying how many are below.
#[test]
fn the_handed_popup_keeps_every_pill_and_row_inside_the_frame_and_scrolls() {
    use csr_engine::companion::render::observation::{POP_CTA_SIZE, POP_TITLE_SIZE};
    let s = SCENARIOS.iter().find(|s| s.name == "handed-open").unwrap();
    let (mut seen, mut missing) = (vec![], vec![]);
    for size in SIZES {
        let Some((_, m)) = frame(s, size) else {
            missing.push(size);
            continue;
        };
        let what = format!("handed {}x{}", size.0, size.1);
        seen.push(size);
        let keys: Vec<&str> = m.popup_keys.iter().map(|k| k.0).collect();
        assert_eq!(keys, ["r", "c", "esc"], "{what}: the pills");
        for k in &m.popup_keys {
            assert!(
                k.1 >= 0. && k.2 <= 605.,
                "{what}: {k:?} off the frame's width"
            );
            assert!(
                k.4 <= m.limit,
                "{what}: {k:?} under the frame's bottom {}",
                m.limit
            );
        }
        for run in &m.text_runs {
            assert!(run.2 <= 605., "{what}: {:?} runs off the right edge", run.4);
            assert!(
                run.3 <= m.limit + 0.5,
                "{what}: {:?} is under the bottom",
                run.4
            );
        }
        assert!(
            m.text_runs.iter().any(
                |r| (size_of(r) - POP_TITLE_SIZE).abs() < 0.05 && r.4 == "What CSR told Claude"
            ),
            "{what}: no title"
        );
        assert!(
            m.text_runs
                .iter()
                .filter(|r| (size_of(r) - POP_CTA_SIZE).abs() < 0.05)
                .count()
                >= 6,
            "{what}: the pill labels are drawn whole"
        );
        assert!(
            m.popup_scroll_max > 0,
            "{what}: 2400 characters of text fit no pane"
        );
        assert!(
            m.text_runs.iter().any(|r| r.4.starts_with("j/k for ")),
            "{what}: more below, and the foot says so"
        );
    }
    // At 30 columns the pane is the text frame, not the picture: it has no pills to measure.
    assert_eq!(missing, [(30, 30)], "drawn at {seen:?}");
    assert_eq!(seen.len(), SIZES.len() - 1);
    set_cell_override(Some(CELL));
    let (_home, paths) = handed_home();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 30, 30));
    pane.theme = Theme::Dark;
    pane.pixels = true;
    pane.enable_cards();
    pane.frame(30, 30, NOW);
    assert!(pane.open_handed());
    let lines = pane.frame(30, 30, NOW);
    set_cell_override(None);
    let all = lines.join("\n");
    assert!(all.contains("What CSR told Claude"), "{all}");
    assert!(all.contains("Where you left off"), "{all}");
    // Visible width: the colour codes take no room.
    let visible = |l: &str| {
        let (mut n, mut in_code) = (0, false);
        for ch in l.chars() {
            match (in_code, ch) {
                (false, '\u{1b}') => in_code = true,
                (true, 'm') => in_code = false,
                (false, _) => n += 1,
                _ => {}
            }
        }
        n
    };
    assert!(lines.iter().all(|l| visible(l) <= 30), "{all}");
    assert!(
        lines.last().is_some_and(|l| l.contains("r resume")),
        "{all}"
    );
}

#[test]
fn j_and_k_scroll_the_handed_popup_and_the_last_page_has_no_more_hint() {
    set_cell_override(Some(CELL));
    let (_home, paths) = handed_home();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 35, 30));
    pane.theme = Theme::Dark;
    pane.pixels = true;
    pane.enable_cards();
    pane.frame(35, 30, NOW);
    let row = pane
        .acts
        .iter()
        .find(|(_, a)| a["key"] == "observe:4:0")
        .map(|(r, _)| *r)
        .expect("the CSR handed over row is on the base view");
    // the tap target of the row is the popup's own act
    assert_eq!(pane.click_target(row, 4)["act"], json!("observe:4:0"));
    pane.click(row, 4, NOW);
    pane.frame(35, 30, NOW);
    let words = |p: &Pane| -> Vec<String> {
        p.instrument_metrics
            .as_ref()
            .unwrap()
            .text_runs
            .iter()
            .map(|r| r.4.clone())
            .collect()
    };
    let top = words(&pane);
    assert!(top.iter().any(|w| w == "Where you left off"), "{top:?}");
    assert!(top.iter().any(|w| w.starts_with("j/k for ")));
    let max = pane.instrument_metrics.as_ref().unwrap().popup_scroll_max;
    assert!(max > 0);
    pane.key("j");
    pane.frame(35, 30, NOW);
    let down = words(&pane);
    assert_ne!(top, down, "j scrolled");
    pane.key("k");
    pane.frame(35, 30, NOW);
    assert_eq!(words(&pane), top, "k scrolled back");
    for _ in 0..(max / 3 + 4) {
        pane.key("j");
        pane.frame(35, 30, NOW);
    }
    let end = words(&pane);
    assert!(
        !end.iter().any(|w| w.starts_with("j/k for ")),
        "nothing below the last page: {end:?}"
    );
    assert!(!end.iter().any(|w| w == "Where you left off"));
    // up and down arrows do the same, and a scroll past the ends stays on them
    for _ in 0..(max / 3 + 4) {
        pane.key("up");
        pane.frame(35, 30, NOW);
    }
    assert_eq!(words(&pane), top);
    set_cell_override(None);
}

/// Tapping the Resume pill is the key: with no Claude pane to type into it says so, and nothing is
/// typed; a tap on the popup's body closes it.
#[test]
fn a_scroll_of_the_handed_popup_redraws_an_idle_pane() {
    set_cell_override(Some(CELL));
    let (_home, paths) = handed_home();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 35, 30));
    pane.theme = Theme::Dark;
    pane.pixels = true;
    pane.enable_cards();
    pane.frame(35, 30, NOW);
    let row = pane
        .acts
        .iter()
        .find(|(_, a)| a["key"] == "observe:4:0")
        .map(|(r, _)| *r)
        .expect("the CSR handed over row is on the base view");
    pane.click(row, 4, NOW);
    pane.frame(35, 30, NOW);
    let before = pane.ui_mark();
    pane.key("j");
    assert_ne!(
        pane.ui_mark(),
        before,
        "the watch loop redraws on this mark"
    );
}

#[test]
fn j_and_k_reach_every_row_of_the_text_popup() {
    set_cell_override(Some(CELL));
    let (_home, paths) = handed_home();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 30, 12));
    assert!(!pane.pixels);
    pane.frame(30, 12, NOW);
    assert!(pane.open_handed(), "the popup opens");
    let text = |p: &mut Pane| p.frame(30, 12, NOW).join("\n");
    let top = text(&mut pane);
    assert!(top.contains("What CSR told Claude"), "{top}");
    let mut seen = top.clone();
    for _ in 0..200 {
        pane.key("j");
        seen.push_str(&text(&mut pane));
    }
    assert!(seen.contains("Where you left off"), "{seen}");
    assert_ne!(text(&mut pane), top, "j moved the popup");
    pane.key("k");
    let up = text(&mut pane);
    for _ in 0..200 {
        pane.key("k");
    }
    assert_eq!(text(&mut pane), top, "k came back to the top");
    assert_ne!(up, top);
}

#[test]
fn the_resume_pill_answers_as_the_key_does() {
    set_cell_override(Some(CELL));
    let (_home, paths) = handed_home();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 35, 30));
    pane.theme = Theme::Dark;
    pane.pixels = true;
    pane.enable_cards();
    pane.frame(35, 30, NOW);
    let row = pane
        .acts
        .iter()
        .find(|(_, a)| a["key"] == "observe:4:0")
        .map(|(r, _)| *r)
        .unwrap();
    pane.click(row, 4, NOW);
    pane.frame(35, 30, NOW);
    let (r, cols) = pane
        .acts
        .iter()
        .find(|(_, a)| a["key"] == "legend")
        .map(|(r, a)| (*r, a["keys"].as_array().unwrap().clone()))
        .expect("the pills' row");
    let resume = cols.iter().find(|k| k[2] == "r").expect("a Resume pill");
    let col = resume[0].as_u64().unwrap() as usize + 1;
    assert!(pane.click(r, col, NOW));
    pane.frame(35, 30, NOW);
    let note = pane
        .instrument_metrics
        .as_ref()
        .unwrap()
        .text_runs
        .iter()
        .any(|run| run.4.starts_with("No Claude pane to type into"));
    assert!(note, "the tap ran the send path");
    set_cell_override(None);
}

/// The Lost and found card: one row on the main view while a thread is open, kept at the sizes
/// the fit allows, and a tap on it opens that thread's handoff popup (`observe:3:<i>`).
#[test]
fn the_lost_and_found_card_is_one_row_that_opens_its_thread() {
    use csr_engine::companion::render::theme::Theme as T;
    let s = SCENARIOS.iter().find(|s| s.name == "base-rich").unwrap();
    let (mut drawn, mut dropped) = (vec![], vec![]);
    for size in SIZES {
        let Some((_, m)) = frame(s, size) else {
            continue;
        };
        let card = m.cards.iter().find(|c| c.title == "Lost and found");
        match card {
            Some(c) => {
                assert_eq!(c.body_rows, 1, "{size:?}: one row, whatever the cap");
                assert!(
                    c.top + c.height <= m.limit + 0.5,
                    "{size:?}: inside the pane"
                );
                drawn.push(size);
            }
            None => dropped.push(size),
        }
    }
    eprintln!("Lost and found card drawn at {drawn:?}, dropped at {dropped:?}");
    assert!(
        drawn.contains(&(50, 40)),
        "the roomy pane keeps it: {drawn:?}"
    );
    // a tap on its row opens the thread
    set_cell_override(Some(CELL));
    let (_home, paths) = rich_now();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 50, 40));
    pane.theme = T::Dark;
    pane.pixels = true;
    pane.enable_cards();
    pane.frame(50, 40, NOW);
    let row = pane
        .acts
        .iter()
        .find(|(_, a)| a["key"] == "observe:3:0")
        .map(|(r, _)| *r)
        .expect("the card's row answers a tap");
    pane.click(row, 4, NOW);
    pane.frame(50, 40, NOW);
    pane.key("c");
    let copied = pane.take_copy().expect("the thread's handoff popup is up");
    assert!(
        copied.starts_with("## Pick up a quiet thread: "),
        "{copied}"
    );
    set_cell_override(None);
}
