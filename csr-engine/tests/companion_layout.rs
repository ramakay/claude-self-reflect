//! Layout, reflow, legend actions and in-place navigation through the real Instrument frame.
//! Narrow goldens share the complete Pane::frame scenarios in companion_frame.rs.
use csr_engine::companion::{cli::PaneArgs, pane::Pane, paths::Paths};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Once,
    time::{Duration, SystemTime},
};

const NOW: f64 = 1792320000.;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/companion")
}
fn rich() -> PathBuf {
    fixture().join("rich")
}
fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}
fn fill(text: &str, home: &str) -> String {
    text.replace("<REPO>", &repo().to_string_lossy())
        .replace("<HOME>", home)
}
fn copy_tree(from: &Path, to: &Path, home: &str) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let e = entry.unwrap();
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &target, home);
        } else {
            let bytes = fs::read(e.path()).unwrap();
            match std::str::from_utf8(&bytes) {
                Ok(text) => fs::write(&target, fill(text, home)).unwrap(),
                Err(_) => fs::write(&target, &bytes).unwrap(),
            }
        }
    }
}
/// The goldens carry local times, so the clock is UTC for every test in this file.
fn utc() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| std::env::set_var("TZ", "UTC"));
}
fn rich_home() -> (tempfile::TempDir, Paths) {
    utc();
    let d = tempfile::tempdir().unwrap();
    let home = d.path().to_string_lossy().into_owned();
    copy_tree(&fixture().join("home"), d.path(), &home);
    copy_tree(&rich().join("home"), d.path(), &home);
    let paths = Paths::new(d.path().into());
    let _ = fs::remove_file(&paths.db);
    let con = rusqlite::Connection::open(&paths.db).unwrap();
    con.execute_batch(include_str!("fixtures/companion/home.sql"))
        .unwrap();
    con.execute(
        "update intent_events set transcript_path=?",
        [paths
            .projects
            .join("fixture/sess-now.jsonl")
            .to_string_lossy()
            .as_ref()],
    )
    .unwrap();
    for (id, file) in [(1, "csr-engine/src/main.rs"), (2, "csr-engine/src/lib.rs")] {
        con.execute(
            "update witness_ledger set file=?1 where id=?2",
            rusqlite::params![repo().join(file).to_string_lossy(), id],
        )
        .unwrap();
    }
    con.execute_batch(&fill(
        &fs::read_to_string(rich().join("home.sql")).unwrap(),
        &home,
    ))
    .unwrap();
    let pins: Value =
        serde_json::from_str(&fs::read_to_string(rich().join("mtimes.json")).unwrap()).unwrap();
    for (rel, secs) in pins.as_object().unwrap() {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(d.path().join(rel))
            .unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs_f64(secs.as_f64().unwrap()))
            .unwrap();
    }
    (d, paths)
}
fn args(cols: usize, rows: usize) -> PaneArgs {
    use clap::Parser;
    #[derive(Parser)]
    struct TestArgs {
        #[command(flatten)]
        pane: PaneArgs,
    }
    TestArgs::parse_from([
        "test",
        "--repo",
        repo().to_str().unwrap(),
        "--frame",
        "--page",
        "main",
        "--cols",
        &cols.to_string(),
        "--rows",
        &rows.to_string(),
        "--sid",
        "sess-now",
    ])
    .pane
}
fn pane(cols: usize, rows: usize) -> (tempfile::TempDir, Pane) {
    let (home, paths) = rich_home();
    let pane = Pane::new(paths, &args(cols, rows));
    (home, pane)
}
fn plain(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() || d == '\\' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
fn act_key(pane: &Pane, row: usize) -> String {
    pane.acts
        .get(&row)
        .and_then(|a| a["key"].as_str())
        .unwrap_or("")
        .to_owned()
}

#[test]
fn the_context_click_toggles_the_figure_on_the_single_surface() {
    let (_home, mut pane) = pane(100, 40);
    let lines = pane.frame(100, 40, NOW);
    assert_eq!(lines.len(), 40);
    assert!(!lines.iter().any(|l| plain(l).contains("Fuel")));
    assert!(plain(&lines[39]).contains("q close"));
    let row = *pane
        .acts
        .iter()
        .find(|(_, a)| a["key"] == "instrument:figure")
        .unwrap()
        .0;
    assert!(pane.click(row, 0, NOW));
    assert!(pane.instrument_open.figure);
    assert_eq!(pane.view, "main");
    pane.key("i");
    assert!(!pane.instrument_open.figure);
}

#[test]
fn the_instrument_reflows_without_the_retired_value_row() {
    for cols in [7, 10, 23, 30, 46, 100] {
        let (_home, mut pane) = pane(cols, 30);
        pane.pixels = false;
        let lines = pane.frame(cols, 30, NOW);
        assert_eq!(lines.len(), 30);
        assert!(!lines
            .iter()
            .any(|l| plain(l).contains("Fuel") || plain(l).contains("STS")));
        assert!(lines
            .iter()
            .all(|l| csr_engine::companion::render::text::len(&plain(l)) <= cols));
    }
}

#[test]
fn changes_and_summary_have_no_retired_value_row() {
    let (_home, mut pane) = pane(100, 40);
    pane.frame(100, 40, NOW);
    pane.key("l");
    assert_eq!(pane.view, "main");
    assert!(pane.instrument_open.changes);
    for key in ["l", "s"] {
        pane.key(key);
        let rows = pane.frame(100, 40, NOW);
        assert!(!rows.iter().any(|l| plain(l).contains("Fuel")));
    }
    assert_eq!(pane.view, "summary");
}

#[test]
fn a_scrolled_drilldown_keeps_the_instrument_and_legend() {
    let (_home, mut pane) = pane(100, 40);
    pane.key("t");
    let before = pane.frame(100, 40, NOW);
    pane.scroll(6);
    assert_eq!(pane.instrument_open.scroll, 6);
    let after = pane.frame(100, 40, NOW);
    assert_eq!(before.len(), after.len());
    assert!(after.iter().any(|l| plain(l).contains("CTX")));
    assert_eq!(act_key(&pane, 39), "legend");
    pane.key("escape");
    assert_eq!(pane.instrument_open.scroll, 0);
    assert!(!pane.instrument_open.threads);
}

#[test]
fn the_legend_names_live_actions_and_hides_an_empty_queue() {
    let (_home, mut pane) = pane(100, 40);
    let rows = pane.frame(100, 40, NOW);
    assert!(plain(&rows[39]).contains("i figure"));
    assert!(plain(&rows[39]).contains("b theme"));
    assert!(!plain(&rows[39]).contains("dream now"));
    let act = &pane.acts[&39];
    for span in act["keys"].as_array().unwrap() {
        let key = span[2].as_str().unwrap();
        assert!(plain(&rows[39]).contains(key));
    }
    let before = pane.theme;
    let span = act["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s[2] == "b")
        .unwrap();
    let col = span[0].as_u64().unwrap() as usize;
    pane.click(39, col, NOW);
    assert_ne!(before, pane.theme);
}

#[test]
fn narrow_text_stacks_the_same_live_instrument_facts() {
    let (_home, mut pane) = pane(23, 40);
    pane.pixels = false;
    let rows = pane.frame(23, 40, NOW);
    let text = rows.iter().map(|l| plain(l)).collect::<Vec<_>>().join("\n");
    for fact in ["CTX 78k/1M", "7D 62%", "5H 41%"] {
        assert!(text.contains(fact), "missing {fact}: {text}");
    }
    assert!(!text.contains("STS"));
    assert!(!rows.iter().any(|l| l.contains(";19H")));
}

#[test]
fn narrow_context_click_opens_the_figure_in_place() {
    let (_home, mut pane) = pane(23, 40);
    pane.pixels = false;
    pane.frame(23, 40, NOW);
    let row = *pane
        .acts
        .iter()
        .find(|(_, a)| a["key"] == "instrument:figure")
        .unwrap()
        .0;
    assert!(pane.click(row, 0, NOW));
    assert!(pane.instrument_open.figure);
    assert_eq!(pane.view, "main");
    pane.frame(23, 40, NOW);
    pane.click(row, 0, NOW);
    assert!(!pane.instrument_open.figure);
}

#[test]
fn column_drilldowns_reflow_and_escape_returns_to_the_surface() {
    let (_home, mut pane) = pane(23, 30);
    for key in ["l", "t", "d"] {
        pane.key(key);
        assert_eq!(pane.view, "main");
        let rows = pane.frame(23, 30, NOW);
        assert_eq!(rows.len(), 30);
        assert!(rows
            .iter()
            .all(|r| csr_engine::companion::render::text::len(&plain(r)) <= 23));
        pane.key("escape");
        assert!(
            !pane.instrument_open.changes
                && !pane.instrument_open.threads
                && !pane.instrument_open.dreams
        );
    }
}

/// Complete narrow frames, regenerated through Pane::frame.
fn column_matches_the_golden(name: &str, pixels: bool) {
    let sc = {
        let text = fs::read_to_string(rich().join("frames.json")).unwrap();
        let manifest: Value = serde_json::from_str(&text).unwrap();
        manifest["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .unwrap()
            .clone()
    };
    let (cols, rows) = (
        sc["cols"].as_u64().unwrap() as usize,
        sc["rows"].as_u64().unwrap() as usize,
    );
    let (_home, mut pane) = pane(cols, rows);
    pane.pixels = pixels;
    let drawn = pane.frame(cols, rows, sc["steps"][0]["at"].as_f64().unwrap());
    let want = fs::read_to_string(rich().join(format!("{name}.txt"))).unwrap();
    let want: Vec<&str> = want.strip_suffix('\n').unwrap().split('\n').collect();
    assert_eq!(drawn.len(), want.len(), "{name}: row count");
    for (i, (got, want)) in drawn.iter().zip(want).enumerate() {
        assert_eq!(got, want, "{name}: row {i}");
    }
}
#[test]
fn column_mode_23x30_matches_the_instrument_top_frame() {
    column_matches_the_golden("frame-main-23x30", false);
    column_matches_the_golden("pixels-frame-main-23x30", true);
}
#[test]
fn column_mode_10x30_matches_the_instrument_top_frame() {
    column_matches_the_golden("frame-main-10x30", false);
    column_matches_the_golden("pixels-frame-main-10x30", true);
}
#[test]
fn column_mode_7x30_matches_the_instrument_top_frame() {
    column_matches_the_golden("frame-main-7x30", false);
    column_matches_the_golden("pixels-frame-main-7x30", true);
}
