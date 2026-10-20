//! The cards and the pill, driven through `Pane::frame` over the rich HOME: what draws, what does not, and
//! that text mode stays as it was.
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
/// The card pictures of a frame: the ones one column in and as wide as the text.
fn card_pictures(pane: &Pane) -> Vec<(usize, usize, image::RgbaImage)> {
    pane.placed
        .iter()
        .filter(|p| p.col == 1 && p.cols == 44)
        .map(|p| {
            (
                p.row,
                p.rows,
                image::load_from_memory(&p.png).unwrap().to_rgba8(),
            )
        })
        .collect()
}
const CARD_FACE: [u8; 4] = [0x27, 0x32, 0x46, 255];
/// A picture is a card when its rounded face is painted inside the margin.
fn painted(img: &image::RgbaImage) -> bool {
    img.get_pixel(20, 20).0 == CARD_FACE
}

#[test]
fn the_info_frame_draws_every_card_that_has_data_one_picture_each_and_scrolls_by_card() {
    utc();
    let (_home, paths) = rich_home();
    let mut full = pane(&paths, "info", "sess-now", true);
    if !have_fonts(&full) {
        return;
    }
    let text = full.frame(46, 120, NOW);
    let cards = card_pictures(&full);
    assert!(
        cards.len() == 6,
        "rings, history, agents, cache, dreams and the mirror draw: got {}",
        cards.len()
    );
    for (row, rows, img) in &cards {
        assert_eq!(img.dimensions(), (44 * 16, *rows as u32 * 34), "row {row}");
        assert!(painted(img), "card at row {row} has no face");
        assert!(row + rows <= text.len());
    }
    // pictures never overlap and the rows under them are blank grounds, not text
    for pair in cards.windows(2) {
        assert!(pair[0].0 + pair[0].1 <= pair[1].0);
    }
    // a short screen shows only the cards that fit whole
    let mut short = pane(&paths, "info", "sess-now", true);
    short.frame(46, 22, NOW);
    let shown = card_pictures(&short);
    assert!(shown.len() < cards.len());
    assert!(shown.iter().all(|(row, rows, _)| row + rows < 22));
    // scrolled down, a later card comes into view
    short.scroll(12);
    short.frame(46, 22, NOW);
    assert_ne!(
        card_pictures(&short)
            .iter()
            .map(|c| c.0)
            .collect::<Vec<_>>(),
        shown.iter().map(|c| c.0).collect::<Vec<_>>()
    );
}

#[test]
fn a_session_with_less_to_say_draws_fewer_cards_and_never_an_empty_one() {
    utc();
    let (_home, paths) = rich_home();
    let mut full = pane(&paths, "info", "sess-now", true);
    if !have_fonts(&full) {
        return;
    }
    full.frame(46, 120, NOW);
    let all = card_pictures(&full).len();
    // a session the account knows nothing about: no usage file, no agents, no dreams
    let mut bare = pane(&paths, "info", "sess-before", true);
    bare.frame(46, 120, NOW);
    let some = card_pictures(&bare);
    assert!(some.len() < all, "{} !< {all}", some.len());
    assert!(some.iter().all(|(_, _, img)| painted(img)));
}

#[test]
fn the_main_frame_puts_the_pill_where_the_two_masthead_rows_were() {
    utc();
    let (_home, paths) = rich_home();
    let mut cards = pane(&paths, "main", "sess-now", true);
    if !have_fonts(&cards) {
        return;
    }
    let text = cards.frame(46, 40, NOW);
    let pill = cards
        .placed
        .iter()
        .find(|p| p.row == 0 && p.col == 1 && p.rows == 2)
        .expect("the pill picture");
    let img = image::load_from_memory(&pill.png).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (pill.cols as u32 * 16, 2 * 34));
    assert!(strip(&text[0]).trim().is_empty() && strip(&text[1]).trim().is_empty());
    // every fact the fixture's text masthead shows is on the pill: context and window, both quotas
    // with the pace arrow, the cache and the status marker; growth and payback have no data and drop
    let pill = cards.pill.clone().expect("the pill the frame drew");
    let said: String = pill
        .lines
        .iter()
        .flatten()
        .map(|g| g.iter().map(|r| r.0.as_str()).collect::<String>() + " | ")
        .collect();
    for fact in [
        "CTX 78k/1M",
        "7D 62%\u{bb}",
        "5H 41%",
        "\u{25f7} 4m",
        "STS",
        "2 by CSR",
    ] {
        assert!(said.contains(fact), "pill lost {fact:?}: {said}");
    }
    assert_eq!(pill.lines.len(), 2);
    // a click on the pill still opens the info view
    assert_eq!(cards.acts[&0]["key"], "view:info");
    assert_eq!(cards.acts[&1]["key"], "view:info");
    // the same frame without cards keeps its text masthead
    let mut plain = pane(&paths, "main", "sess-now", false);
    plain.pixels = false;
    let words = plain.frame(46, 40, NOW);
    assert!(strip(&words[0]).contains("CTX"));
    assert!(!plain.placed.iter().any(|p| p.rows == 2 && p.col == 1));
}

#[test]
fn text_mode_is_byte_identical_with_or_without_cards_enabled() {
    utc();
    let (_home, paths) = rich_home();
    for page in ["main", "info"] {
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
