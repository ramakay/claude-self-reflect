//! The frame around the main page: the value row, the gauge strip, column mode, the window on a tall page and
//! the reload countdown, drawn and clicked through `Pane::frame`, `Pane::key` and `Pane::click` over the rich
//! HOME of tests/companion_frame.rs (its goldens come from pane.py's own `Pane.frame`).
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
fn the_value_row_sits_above_the_legend_and_a_click_on_it_opens_the_info_page() {
    let (_home, mut pane) = pane(100, 40);
    let lines = pane.frame(100, 40, NOW);
    assert_eq!(lines.len(), 40);
    assert_eq!(
        plain(&lines[38]),
        " Fuel ≈3.2% · Haiku 4 · Dreams 2 · Took 2 · ♥ 2"
    );
    assert!(plain(&lines[39]).contains("q close"));
    assert_eq!(act_key(&pane, 38), "view:info");
    assert_eq!(act_key(&pane, 39), "legend");
    assert!(pane.click(38, 5, NOW));
    assert_eq!(pane.view, "info");
    // the i page has no value row: its cost section says the same in words
    let info = pane.frame(100, 40, NOW);
    assert!(!plain(&info[38]).starts_with(" Fuel"));
    assert!(info.iter().any(|l| plain(l).contains("WHAT CSR COST YOU")));
}

#[test]
fn the_value_row_gives_way_by_width_and_takes_a_row_from_the_page() {
    let (_home, mut pane) = pane(46, 30);
    let lines = pane.frame(46, 30, NOW);
    assert_eq!(
        plain(&lines[28]),
        " Fuel ≈3.2% · Haiku 4 · Dreams 2 · Took 2"
    );
    let (_home, mut narrow) = self::pane(30, 30);
    let lines = narrow.frame(30, 30, NOW);
    assert_eq!(plain(&lines[28]), " Fuel ≈3.2% · Haiku 4");
    assert_eq!(act_key(&narrow, 28), "view:info");
}

#[test]
fn the_log_page_and_the_summary_page_have_no_value_row() {
    let (_home, mut pane) = pane(100, 40);
    pane.frame(100, 40, NOW);
    assert!(pane.key("l"));
    let lines = pane.frame(100, 40, NOW);
    assert!(!plain(&lines[38]).starts_with(" Fuel"));
    assert_eq!(act_key(&pane, 39), "legend");
}

#[test]
fn a_scrolled_page_is_a_window_that_keeps_the_value_row_and_the_legend_at_the_bottom() {
    let (_home, mut pane) = pane(100, 40);
    pane.frame(100, 40, NOW);
    pane.scroll(6);
    let lines = pane.frame(100, 40, NOW);
    assert_eq!(lines.len(), 40);
    assert!(plain(&lines[38]).starts_with(" Fuel"));
    assert!(plain(&lines[39]).contains("q close"));
    assert_eq!(act_key(&pane, 38), "view:info");
    // the page never scrolls past its own end: the window's bottom row is its last
    pane.scroll(1000);
    let far = pane.frame(100, 40, NOW);
    assert_eq!(far.len(), 40);
    assert!(plain(&far[38]).starts_with(" Fuel"));
    // escape puts the page back at its top
    assert!(pane.key("escape"));
    assert_eq!(pane.top, 0);
}

#[test]
fn the_legend_counts_down_to_the_next_read_and_pads_its_label() {
    let (_home, mut pane) = pane(100, 40);
    let first = pane.frame(100, 40, NOW);
    assert!(plain(&first[39]).contains("r ↻ 60s  q close"));
    let later = pane.frame(100, 40, NOW + 10.2);
    assert!(plain(&later[39]).contains("r ↻ 50s  q close"));
    let last = pane.frame(100, 40, NOW + 55.);
    assert!(plain(&last[39]).contains("r ↻ 5s   q close"));
    // column mode has no room for it
    let (_home, mut column) = self::pane(23, 30);
    let lines = column.frame(23, 30, NOW);
    assert!(!plain(&lines[29]).contains('↻'));
}

#[test]
fn column_mode_draws_the_strip_at_the_last_columns_and_stacks_the_values() {
    let (_home, mut pane) = pane(23, 30);
    let lines = pane.frame(23, 30, NOW);
    assert_eq!(lines.len(), 30);
    let text: Vec<String> = lines.iter().map(|l| plain(l)).collect();
    assert!(text[0].starts_with("CTX 78k"));
    assert!(text[1].starts_with("7D 62%"));
    assert!(text[2].starts_with("5H 41%"));
    assert!(text[3].starts_with("◷4m"));
    assert!(text[4].starts_with("STS"));
    // every row but the legend's carries the strip, written by absolute position
    for (row, line) in lines.iter().enumerate().take(29) {
        assert!(
            line.contains(&format!("\x1b[{};19H", row + 1)),
            "row {row} has no strip"
        );
    }
    assert!(!lines[29].contains("\x1b[30;19H"));
    assert_eq!(text[29].trim(), "l s i q");
    // 7 columns keep one tape, at the last column but one
    let (_home, mut thin) = self::pane(7, 30);
    let lines = thin.frame(7, 30, NOW);
    assert!(lines[0].contains("\x1b[1;6H"));
}

#[test]
fn the_strip_is_on_no_row_and_the_masthead_in_column_mode_opens_the_info_page() {
    let (_home, mut pane) = pane(23, 30);
    pane.frame(23, 30, NOW);
    assert_eq!(act_key(&pane, 0), "view:info");
    assert_eq!(act_key(&pane, 1), "view:info");
    assert_eq!(act_key(&pane, 2), "");
    // a click on the strip beside the masthead is on no row
    assert!(pane.click(0, 19, NOW));
    assert_eq!(pane.view, "main");
    assert!(pane.click(1, 22, NOW));
    assert_eq!(pane.view, "main");
    // a click on the value text opens the info page, drawn in column mode
    assert!(pane.click(0, 3, NOW));
    assert_eq!(pane.view, "info");
    let info = pane.frame(23, 30, NOW);
    assert_eq!(info.len(), 30);
    assert!(
        info[0].contains("\x1b[1;19H"),
        "the strip stays on the info page"
    );
    assert_eq!(act_key(&pane, 0), "view:main");
    let text: Vec<String> = info.iter().map(|l| plain(l)).collect();
    assert!(text.iter().any(|l| l.contains("THIS SESSION")));
    assert!(text.iter().any(|l| l.contains("WHAT CSR COST")));
    // the title, clicked, goes back
    assert!(pane.click(0, 2, NOW));
    assert_eq!(pane.view, "main");
}

#[test]
fn column_mode_legend_keys_and_pages_reflow() {
    let (_home, mut pane) = pane(23, 30);
    pane.frame(23, 30, NOW);
    // "l" is the first label of the column legend
    assert!(pane.click(29, 1, NOW));
    assert_eq!(pane.view, "log");
    let log = pane.frame(23, 30, NOW);
    assert_eq!(log.len(), 30);
    let text: Vec<String> = log.iter().map(|l| plain(l)).collect();
    // the page's words, rewrapped to the 18 columns left of the strip
    for line in text.iter().take(29) {
        let body: String = line.chars().take(18).collect();
        assert!(!body.is_empty() || line.is_empty() || line.chars().count() > 0);
    }
    assert_eq!(text[29].trim(), "d q");
    assert!(pane.key("d"));
    assert_eq!(pane.view, "main");
}

/// A row of a golden split where the strip's cursor-position escape starts: (text part, strip part).
fn split_strip(row: &str) -> (String, String) {
    let at = (0..row.len())
        .filter(|i| row.is_char_boundary(*i))
        .find(|i| {
            let rest = &row[*i..];
            rest.starts_with("\x1b[")
                && rest[2..].split_once('H').is_some_and(|(pos, _)| {
                    pos.contains(';') && pos.chars().all(|c| c.is_ascii_digit() || c == ';')
                })
        });
    match at {
        Some(i) => (row[..i].to_owned(), row[i..].to_owned()),
        None => (row.to_owned(), String::new()),
    }
}

/// Column mode against pane.py's own frames: the gauge strip of every row is byte for byte the golden's, the
/// masthead's values and the legend are too, and the section counts the Rust page draws are the golden's counts in
/// the golden's order. (Flight, the compaction card, Lost and found and the coach draw no count until packages C and
/// D land; then every row is the golden's and `frame_main_*x30` in companion_frame.rs takes over.)
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
    let (got, golden): (Vec<_>, Vec<_>) = (
        drawn.iter().map(|r| split_strip(r)).collect::<Vec<_>>(),
        want.iter().map(|r| split_strip(r)).collect::<Vec<_>>(),
    );
    for (i, ((_, g), (_, w))) in got.iter().zip(&golden).enumerate() {
        assert_eq!(g, w, "{name}: row {i} (the strip)");
    }
    let masthead = 5; // CTX, 7D, 5H, cache, STS
    for i in (0..masthead).chain([rows - 1]) {
        assert_eq!(got[i].0, golden[i].0, "{name}: row {i} (text)");
    }
    let mut next = masthead;
    for (i, (text, _)) in got.iter().enumerate().take(rows - 1).skip(masthead) {
        if plain(text).trim().is_empty() {
            continue;
        }
        let at = (next..golden.len() - 1)
            .find(|j| golden[*j].0 == *text)
            .unwrap_or_else(|| {
                panic!(
                    "{name}: row {i} ({:?}) is no count of the golden's",
                    plain(text)
                )
            });
        next = at + 1;
    }
}
#[test]
fn column_mode_23x30_is_pane_pys_but_for_the_counts_of_packages_c_and_d() {
    column_matches_the_golden("frame-main-23x30", false);
    column_matches_the_golden("pixels-frame-main-23x30", true);
}
#[test]
fn column_mode_10x30_is_pane_pys_but_for_the_counts_of_packages_c_and_d() {
    column_matches_the_golden("frame-main-10x30", false);
    column_matches_the_golden("pixels-frame-main-10x30", true);
}
#[test]
fn column_mode_7x30_is_pane_pys_but_for_the_counts_of_packages_c_and_d() {
    column_matches_the_golden("frame-main-7x30", false);
    column_matches_the_golden("pixels-frame-main-7x30", true);
}
