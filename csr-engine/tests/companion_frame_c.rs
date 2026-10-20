//! Agent details, dream reasons, queue answers and the Codex figure through the real single surface.
//! Each test drives Pane::frame, Pane::key or Pane::click over the rich private HOME.
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
/// The goldens carry local times, so the clock is UTC; and a second click must never reach a real
/// herdr pane, so the pane's own id is dropped from this process. The goldens are text frames:
/// `TERM_PROGRAM=herdr` (or kitty, ghostty, WezTerm) would turn pictures on, reserve rows for them,
/// and push the In flight rows and their fold row off the page, so pictures are switched off.
fn quiet_env() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::env::set_var("TZ", "UTC");
        std::env::set_var("CSR_COMPANION_PIXELS", "0");
        for name in [
            "HERDR_PANE_ID",
            "HERDR_ENV",
            "HERDR_SOCKET_PATH",
            "HERDR_BIN_PATH",
        ] {
            std::env::remove_var(name);
        }
    });
}
fn rich_home() -> (tempfile::TempDir, Paths) {
    quiet_env();
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
fn pane(paths: &Paths, cols: usize, rows: usize) -> Pane {
    use clap::Parser;
    #[derive(Parser)]
    struct TestArgs {
        #[command(flatten)]
        pane: PaneArgs,
    }
    let args = TestArgs::parse_from([
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
    .pane;
    Pane::new(paths.clone(), &args)
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
    out.trim_end().to_owned()
}
/// A frame at `at`, as plain text.
fn draw(pane: &mut Pane, at: f64) -> Vec<String> {
    pane.frame(100, 70, at).iter().map(|l| plain(l)).collect()
}
/// A 100x140 frame: room below Lost and found and the compaction card for the dreams that passed.
fn tall(pane: &mut Pane, at: f64) -> Vec<String> {
    pane.frame(100, 140, at).iter().map(|l| plain(l)).collect()
}
/// The first screen row whose act key starts with `key`.
fn row_of(pane: &Pane, key: &str) -> usize {
    pane.acts
        .iter()
        .find(|(_, act)| act["key"].as_str().is_some_and(|k| k.starts_with(key)))
        .map(|(row, _)| *row)
        .unwrap_or_else(|| panic!("no row with act key {key:?}; acts: {:?}", pane.acts.keys()))
}
fn has(text: &[String], needle: &str) -> bool {
    text.iter().any(|l| l.contains(needle))
}
fn finished_agents(paths: &Paths) {
    let text = fs::read_to_string(rich().join("variants/agents-finished.json")).unwrap();
    fs::write(
        paths.keep.join("agents/sess-now.json"),
        fill(&text, &paths.home.to_string_lossy()),
    )
    .unwrap();
}

#[test]
fn a_click_on_the_agent_summary_opens_real_details_and_folds_them() {
    let (_home, paths) = rich_home();
    let mut pane = pane(&paths, 100, 70);
    let first = draw(&mut pane, NOW);
    assert!(has(&first, "in flight"));
    assert!(!has(&first, " tools"));
    let row = row_of(&pane, "instrument:agents");
    pane.click(row, 0, NOW);
    assert!(pane.instrument_open.agents);
    let open = draw(&mut pane, NOW);
    assert!(has(&open, " tools"));
    let row = row_of(&pane, "instrument:agents");
    pane.click(row, 0, NOW);
    assert!(!pane.instrument_open.agents);
}

#[test]
fn finished_agents_stay_folded_until_the_agent_details_open() {
    let (_home, paths) = rich_home();
    finished_agents(&paths);
    let mut pane = pane(&paths, 100, 70);
    let first = draw(&mut pane, NOW);
    assert!(!has(&first, "in flight"));
    assert!(!has(&first, " tools"));
    pane.key("a");
    let open = draw(&mut pane, NOW);
    assert!(has(&open, " tools"));
    pane.key("a");
    let shut = draw(&mut pane, NOW);
    assert!(!has(&shut, " tools"));
}

#[test]
fn opening_and_closing_agent_details_never_sends_a_prompt() {
    let (_home, paths) = rich_home();
    let mut pane = pane(&paths, 100, 70);
    draw(&mut pane, NOW);
    let row = row_of(&pane, "instrument:agents");
    pane.click(row, 0, NOW);
    draw(&mut pane, NOW);
    assert!(pane.instrument_open.agents);
    assert!(pane.sent.is_null());
    pane.key("escape");
    draw(&mut pane, NOW);
    assert!(!pane.instrument_open.agents);
    assert!(pane.sent.is_null());
}

#[test]
fn codex_week_is_part_of_the_figure_and_never_sends_a_prompt() {
    let (_home, paths) = rich_home();
    let mut pane = pane(&paths, 100, 70);
    let text = draw(&mut pane, NOW);
    assert!(has(&text, "Codex 7D 41%"));
    assert!(!pane.acts.values().any(|a| a["key"] == "codex:week"));
    let row = row_of(&pane, "instrument:figure");
    pane.click(row, 0, NOW);
    assert!(pane.instrument_open.figure);
    draw(&mut pane, NOW);
    pane.click(row, 0, NOW);
    assert!(!pane.instrument_open.figure);
    assert!(pane.sent.is_null());
}

#[test]
fn the_hero_opens_its_reasons_without_sending_or_answering() {
    let (_home, paths) = rich_home();
    let mut pane = pane(&paths, 100, 70);
    draw(&mut pane, NOW);
    let before = fs::read(paths.keep.join("dreams.jsonl")).unwrap();
    let row = row_of(&pane, "instrument:dreams");
    pane.click(row, 0, NOW);
    let shown = draw(&mut pane, NOW);
    assert!(pane.instrument_open.dreams);
    assert!(has(&shown, "Catch a failing frame"));
    assert!(pane.sent.is_null());
    assert_eq!(fs::read(paths.keep.join("dreams.jsonl")).unwrap(), before);
    pane.click(row, 0, NOW);
    assert!(!pane.instrument_open.dreams);
}

#[test]
fn a_click_on_an_answer_keeps_it_in_the_log_and_remembers_it_for_six_seconds() {
    let (_home, paths) = rich_home();
    let mut pane = pane(&paths, 100, 70);
    let text = draw(&mut pane, NOW);
    assert!(has(&text, "1 helpful"), "{text:#?}");
    let row = row_of(&pane, "dream:newest");
    // the left half of the row is "helpful"
    pane.click(row, 10, NOW);
    assert_eq!(
        pane.answered.clone().map(|a| (a.0, a.1)),
        Some(("newest".into(), "helpful".into()))
    );
    let log = fs::read_to_string(paths.keep.join("dreams.jsonl")).unwrap();
    assert!(
        log.lines()
            .any(|l| l.contains("\"answer\"") && l.contains("\"helpful\"") && l.contains("newest")),
        "{log}"
    );
    draw(&mut pane, NOW + 5.9);
    assert!(pane.answered.is_some());
    draw(&mut pane, NOW + 6.1);
    assert!(pane.answered.is_none());
}

#[test]
fn answering_promotes_the_next_pending_dream_immediately() {
    let (_home, paths) = rich_home();
    let mut pane = pane(&paths, 100, 70);
    draw(&mut pane, NOW);
    let row = row_of(&pane, "dream:newest");
    pane.click(row, 1, NOW);
    draw(&mut pane, NOW + 1.);
    assert!(!pane.acts.values().any(|a| a["dream"] == "newest"));
    assert_eq!(pane.view, "main");
    assert!(pane.sent.is_null());
}

#[test]
fn the_queue_opens_in_place_and_keeps_answered_dreams_folded() {
    let (_home, paths) = rich_home();
    let mut pane = pane(&paths, 100, 140);
    let before = tall(&mut pane, NOW);
    pane.key("d");
    let open = tall(&mut pane, NOW);
    assert!(pane.instrument_open.dreams);
    assert_eq!(pane.view, "main");
    assert!(
        open.iter().filter(|l| !l.is_empty()).count()
            >= before.iter().filter(|l| !l.is_empty()).count()
    );
    assert!(pane.sent.is_null());
    pane.key("d");
    assert!(!pane.instrument_open.dreams);
}
