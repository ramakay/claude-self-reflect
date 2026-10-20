//! The main page's keys and clicks, through `Pane::click`: In flight (the agent rows, the Codex rows and
//! their folds), "How Claude read you", and the dream answer the pane keeps for six seconds. Each
//! test builds the same rich HOME as tests/companion_frame.rs (base home, rich overlay, DB from
//! home.sql then rich/home.sql) and drives a Pane at the pinned time.
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
/// herdr pane, so the pane's own id is dropped from this process.
fn quiet_env() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::env::set_var("TZ", "UTC");
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
fn a_click_on_the_more_row_shows_the_rest_and_a_click_on_fewer_folds_it_again() {
    let (_home, paths) = rich_home();
    let mut pane = pane(&paths, 100, 70);
    let text = draw(&mut pane, NOW);
    assert!(has(&text, "› and 3 more"), "{text:#?}");
    assert!(!has(&text, "a-orphan"));
    let row = row_of(&pane, "fold:flight-more");
    assert!(pane.click(row, 8, NOW));
    let text = draw(&mut pane, NOW);
    assert!(has(&text, "⌄ fewer"), "{text:#?}");
    assert!(!has(&text, "and 3 more"));
    let row = row_of(&pane, "fold:flight-more");
    pane.click(row, 8, NOW);
    let text = draw(&mut pane, NOW);
    assert!(has(&text, "› and 3 more"), "{text:#?}");
}

#[test]
fn a_click_on_the_finished_row_unfolds_the_agents_and_a_second_folds_them() {
    let (_home, paths) = rich_home();
    finished_agents(&paths);
    let mut pane = pane(&paths, 100, 70);
    let text = draw(&mut pane, NOW);
    let folded = text
        .iter()
        .find(|l| l.contains("3 agents finished"))
        .unwrap();
    assert!(folded.contains("›") && folded.contains("ago"), "{folded}");
    assert!(!has(&text, "Explore · claude-sonnet-4-5 · done"));
    let row = row_of(&pane, "fold:flight");
    pane.click(row, 8, NOW);
    let text = draw(&mut pane, NOW);
    assert!(has(&text, "⌄ 3 agents finished"), "{text:#?}");
    assert!(
        has(&text, "Explore · claude-sonnet-4-5 · done"),
        "{text:#?}"
    );
    let row = row_of(&pane, "fold:flight");
    pane.click(row, 8, NOW);
    let text = draw(&mut pane, NOW);
    assert!(has(&text, "› 3 agents finished"), "{text:#?}");
}

#[test]
fn a_click_on_an_agent_opens_its_report_and_a_second_click_folds_it_and_sends_nothing() {
    let (_home, paths) = rich_home();
    let mut pane = pane(&paths, 100, 70);
    draw(&mut pane, NOW);
    let row = row_of(&pane, "flight:a-done");
    pane.click(row, 8, NOW);
    let text = draw(&mut pane, NOW);
    assert_eq!(pane.open, "flight:a-done");
    assert!(has(&text, "What it found — the agent’s words"), "{text:#?}");
    assert!(has(
        &text,
        "the agent's words, not yours: click again to fold"
    ));
    let row = row_of(&pane, "flight:a-done");
    pane.click(row, 8, NOW);
    let text = draw(&mut pane, NOW);
    assert!(pane.open.is_null(), "open is {}", pane.open);
    assert!(
        pane.sent.is_null(),
        "an agent's report is never sent: {}",
        pane.sent
    );
    assert!(!has(&text, "What it found"));
}

#[test]
fn a_click_on_the_codex_rows_opens_them_and_a_second_click_closes_without_sending() {
    let (_home, paths) = rich_home();
    let mut pane = pane(&paths, 100, 70);
    draw(&mut pane, NOW);
    for key in ["codex:week", "codex:codex-live-2"] {
        let row = row_of(&pane, key);
        pane.click(row, 8, NOW);
        draw(&mut pane, NOW);
        assert_eq!(pane.open, key);
        let row = row_of(&pane, key);
        pane.click(row, 8, NOW);
        draw(&mut pane, NOW);
        assert!(pane.open.is_null(), "{key}: open is {}", pane.open);
        assert!(pane.sent.is_null(), "{key}: sent {}", pane.sent);
    }
}

#[test]
fn a_click_on_a_reading_opens_it_and_a_second_click_asks_to_put_it_in_the_prompt() {
    let (_home, paths) = rich_home();
    let mut pane = pane(&paths, 100, 70);
    draw(&mut pane, NOW);
    let row = row_of(&pane, "read:");
    let key = pane.acts[&row]["key"].as_str().unwrap().to_owned();
    assert_eq!(key, "read:1792319865.0:dknr");
    pane.click(row, 8, NOW);
    let text = draw(&mut pane, NOW);
    assert_eq!(pane.open, key.as_str());
    assert!(
        has(&text, "Your word “dknr” — interpreted as: done."),
        "{text:#?}"
    );
    assert!(has(&text, "↳ click again → Claude's prompt"));
    let row = row_of(&pane, "read:");
    pane.click(row, 8, NOW);
    // no Claude pane to type into here, so the reading says it could not reach it
    assert_eq!(pane.sent[0], key.as_str());
    assert_eq!(pane.sent[1], false);
    let text = draw(&mut pane, NOW);
    assert!(has(&text, "✗ couldn't reach Claude's pane"), "{text:#?}");
    assert_eq!(
        pane.acts[&row_of(&pane, "read:")]["send"],
        "Earlier you read my “dknr” as “done”. Check that reading before relying on it."
    );
}

#[test]
fn a_click_on_an_answer_keeps_it_in_the_log_and_remembers_it_for_six_seconds() {
    let (_home, paths) = rich_home();
    let mut pane = pane(&paths, 100, 70);
    let text = draw(&mut pane, NOW);
    assert!(has(&text, "✓ helpful"), "{text:#?}");
    let row = row_of(&pane, "dream:newest:answers");
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
fn the_answer_the_pane_kept_stands_in_place_of_the_answer_rows_until_it_is_six_seconds_old() {
    let (_home, paths) = rich_home();
    let mut pane = pane(&paths, 100, 70);
    pane.answered = Some(("newest".into(), "helpful".into(), NOW - 2.));
    let text = draw(&mut pane, NOW);
    assert!(has(&text, "✓ kept: helpful"), "{text:#?}");
    assert!(!has(&text, "= thought of it") && !has(&text, "→ ask Claude"));
    // a click on the echo is on no row's answers: it opens nothing and answers nothing
    let row = text
        .iter()
        .position(|l| l.contains("✓ kept: helpful"))
        .unwrap();
    assert!(!pane.acts.contains_key(&row));
    let text = draw(&mut pane, NOW + 3.9);
    assert!(has(&text, "✓ kept: helpful"));
    let text = draw(&mut pane, NOW + 4.1);
    assert!(pane.answered.is_none());
    assert!(
        !has(&text, "✓ kept: helpful") && has(&text, "= thought of it"),
        "{text:#?}"
    );
}

#[test]
fn an_opened_earlier_dream_shows_its_title_and_the_first_two_reasons() {
    let (_home, paths) = rich_home();
    let mut pane = pane(&paths, 100, 140);
    tall(&mut pane, NOW);
    let row = row_of(&pane, "fold:passed");
    pane.click(row, 8, NOW);
    tall(&mut pane, NOW);
    let row = row_of(&pane, "dream:older");
    pane.click(row, 8, NOW);
    let text = tall(&mut pane, NOW);
    assert!(has(&text, "A small reader"), "{text:#?}");
    assert!(has(&text, "Bound the tail before parsing."), "{text:#?}");
    assert!(!has(&text, "- Bound the tail"), "{text:#?}");
}
