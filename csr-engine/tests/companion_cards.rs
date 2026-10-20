//! Pixel composition, missing feeds, figure clicks and deterministic rendering through Pane::frame.
use csr_engine::companion::{cli::PaneArgs, pane::Pane, paths::Paths};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Once,
    time::{Duration, SystemTime},
};

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
/// Copy a fixture tree, writing `<REPO>` and `<HOME>` into every text file the way the oracle does.
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
fn fill(text: &str, home: &str) -> String {
    text.replace("<REPO>", &repo().to_string_lossy())
        .replace("<HOME>", home)
}
/// The goldens carry local times (10:37), so the clock is UTC for every test in this file.
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
    // *.db is gitignored, so the DB is built from home.sql then the overlay, never copied.
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
fn args(page: &str, sid: &str, cols: usize, rows: usize) -> PaneArgs {
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
        page,
        "--cols",
        &cols.to_string(),
        "--rows",
        &rows.to_string(),
        "--sid",
        sid,
    ])
    .pane
}

const NOW: f64 = 1792320000.;

fn strip(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
fn pane(paths: &Paths, page: &str, sid: &str, cards: bool) -> Pane {
    let mut pane = Pane::new(paths.clone(), &args(page, sid, 46, 120));
    pane.pixels = true;
    if cards {
        pane.enable_cards();
    }
    pane
}
fn have_fonts(pane: &Pane) -> bool {
    if !pane.cards {
        eprintln!("no usable font on this machine: the cards are not drawn, the text stays");
    }
    pane.cards
}
#[test]
fn the_single_surface_draws_one_complete_instrument_and_scrolls_details() {
    utc();
    let (_home, paths) = rich_home();
    let mut full = pane(&paths, "main", "sess-now", true);
    if !have_fonts(&full) {
        return;
    }
    let rows = full.frame(46, 120, NOW);
    assert_eq!(full.placed.len(), 1);
    let picture = &full.placed[0];
    let image = image::load_from_memory(&picture.png).unwrap();
    assert_eq!(image.width(), 46 * 16);
    assert!(picture.row + picture.rows <= rows.len());
    let metrics = full.instrument_metrics.as_ref().unwrap();
    assert!(metrics.dream / metrics.height <= 0.3);
    full.key("t");
    full.frame(46, 40, NOW);
    full.scroll(3);
    full.frame(46, 40, NOW);
    assert_eq!(full.instrument_open.scroll, 3);
    assert_eq!(full.view, "main");
}

#[test]
fn a_session_with_less_to_say_drops_missing_facts_without_empty_cards() {
    utc();
    let (_home, paths) = rich_home();
    let mut full = pane(&paths, "main", "sess-now", true);
    if !have_fonts(&full) {
        return;
    }
    full.frame(46, 120, NOW);
    let mut bare = pane(&paths, "main", "sess-before", true);
    bare.frame(46, 120, NOW);
    assert_eq!(bare.placed.len(), 1);
    assert_ne!(bare.placed[0].png, full.placed[0].png);
    bare.pixels = false;
    let text = bare
        .frame(46, 120, NOW)
        .iter()
        .map(|l| strip(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!text.contains("♻ --"));
    assert!(!text.contains("in flight"));
}

#[test]
fn the_main_frame_replaces_the_old_pill_with_one_live_figure() {
    utc();
    let (_home, paths) = rich_home();
    let mut cards = pane(&paths, "main", "sess-now", true);
    if !have_fonts(&cards) {
        return;
    }
    cards.frame(46, 67, NOW);
    assert_eq!(cards.placed.len(), 1);
    assert_eq!(cards.placed[0].row, 0);
    assert!(cards.pill.is_none());
    let row = *cards
        .acts
        .iter()
        .find(|(_, a)| a["key"] == "instrument:figure")
        .unwrap()
        .0;
    cards.click(row, 0, NOW);
    assert!(cards.instrument_open.figure);
    assert_eq!(cards.view, "main");
    cards.pixels = false;
    let text = cards
        .frame(46, 67, NOW)
        .iter()
        .map(|l| strip(l))
        .collect::<Vec<_>>()
        .join("\n");
    for fact in ["CTX 78k/1M", "7D 62%", "5H 41%"] {
        assert!(text.contains(fact), "{text}");
    }
    assert!(!text.contains("STS"));
}

#[test]
fn text_mode_is_byte_identical_with_or_without_cards_enabled() {
    utc();
    let (_home, paths) = rich_home();
    for page in ["main", "summary"] {
        let mut off = Pane::new(paths.clone(), &args(page, "sess-now", 46, 40));
        off.pixels = false;
        let want = off.frame(46, 40, NOW);
        let mut on = Pane::new(paths.clone(), &args(page, "sess-now", 46, 40));
        on.pixels = false;
        on.enable_cards();
        assert!(!on.cards, "cards need pixels");
        assert_eq!(on.frame(46, 40, NOW), want, "{page}");
        assert!(on.placed.is_empty());
    }
}

#[test]
fn a_frame_at_the_same_clock_is_pixel_identical() {
    utc();
    let (_home, paths) = rich_home();
    let mut p = pane(&paths, "main", "sess-now", true);
    if !have_fonts(&p) {
        return;
    }
    p.frame(46, 67, NOW);
    let first = p.placed[0].png.clone();
    p.frame(46, 67, NOW);
    assert_eq!(p.placed.len(), 1);
    assert_eq!(first, p.placed[0].png);
    p.key("i");
    p.frame(46, 67, NOW);
    assert_ne!(first, p.placed[0].png);
}
