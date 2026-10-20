//! Compaction figures, retained thread actions, and the carry that reads the preceding session.
//! UI checks drive the actual pane; carry tests verify that background work is never awaited.
use csr_engine::companion::{cli::PaneArgs, pane::Pane, paths::Paths};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Once,
    time::{Duration, Instant, SystemTime},
};

/// The pinned clock of the rich fixture.
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
/// The goldens carry local times, so the clock is UTC; and nothing here may type into a real agent pane.
fn quiet_env() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::env::set_var("TZ", "UTC");
        std::env::remove_var("HERDR_PANE_ID");
    });
}
/// The rich HOME as tests/companion_frame.rs builds it.
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
/// `watch` is the running pane (it keeps what it works out, and reads the earlier session off the draw path); without
/// it, the one-shot `--frame`.
fn args(sid: &str, watch: bool) -> PaneArgs {
    use clap::Parser;
    #[derive(Parser)]
    struct TestArgs {
        #[command(flatten)]
        pane: PaneArgs,
    }
    let repo_dir = repo();
    let mut argv = vec!["test", "--repo", repo_dir.to_str().unwrap()];
    if !watch {
        argv.push("--frame");
    } else {
        argv.push("--watch");
    }
    argv.extend([
        "--page", "main", "--cols", "100", "--rows", "70", "--sid", sid,
    ]);
    TestArgs::parse_from(argv).pane
}
fn pane(sid: &str, watch: bool) -> (tempfile::TempDir, Paths, Pane) {
    let (home, paths) = rich_home();
    let pane = Pane::new(paths.clone(), &args(sid, watch));
    (home, paths, pane)
}
fn draw(pane: &mut Pane, at: f64) -> Vec<String> {
    pane.frame(100, 70, at)
}
/// A 100x100 frame: room for a second thread opened with its asks, below In flight, How Claude
/// read you and the value row.
fn tall(pane: &mut Pane, at: f64) -> Vec<String> {
    pane.frame(100, 100, at)
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
fn screen(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| plain(l))
        .collect::<Vec<_>>()
        .join("\n")
}
/// The row and the act of the first act whose key satisfies `want`.
fn find(pane: &Pane, want: impl Fn(&str) -> bool) -> (usize, Value) {
    pane.acts
        .iter()
        .find(|(_, act)| act["key"].as_str().is_some_and(&want))
        .map(|(row, act)| (*row, act.clone()))
        .expect("no row with that act")
}
/// Every thread's own row, top to bottom: `lost:<key>`, not one of its answers (`lost:<key>:side`).
fn thread_rows(pane: &Pane) -> Vec<(usize, Value)> {
    let mut rows: Vec<(usize, Value)> = vec![];
    // a thread's title takes two rows, and both carry its act: keep the first
    for (row, act) in &pane.acts {
        let own = act["key"]
            .as_str()
            .is_some_and(|k| k.starts_with("lost:") && k.matches(':').count() == 1);
        if own
            && rows
                .last()
                .is_none_or(|(_, last)| last["key"] != act["key"])
        {
            rows.push((*row, act.clone()));
        }
    }
    rows
}
fn thread_row(pane: &Pane) -> (usize, Value) {
    thread_rows(pane).remove(0)
}
/// The row of a thread's answer and a column inside the span for it.
fn answer(pane: &Pane, pick: &str, thread: &str) -> (usize, usize) {
    for (row, act) in &pane.acts {
        if act["lost"] != thread {
            continue;
        }
        for span in act["spans"].as_array().into_iter().flatten() {
            if span[2] == pick {
                return (*row, span[0].as_u64().unwrap() as usize + 1);
            }
        }
    }
    panic!("no {pick} answer for {thread}");
}
fn lost_log(paths: &Paths) -> String {
    fs::read_to_string(paths.keep.join("lost-found.jsonl")).unwrap_or_default()
}

#[test]
fn a_click_on_the_compaction_figure_opens_it_in_place() {
    let (_home, _paths, mut pane) = pane("sess-now", false);
    draw(&mut pane, NOW);
    let (row, _) = find(&pane, |k| k == "instrument:figure");
    pane.click(row, 0, NOW);
    assert!(pane.instrument_open.figure);
    assert_eq!(pane.view, "main");
    pane.key("escape");
    assert!(!pane.instrument_open.figure);
}

#[test]
fn the_figure_tells_the_compaction_count_once() {
    let (_home, _paths, mut pane) = pane("sess-now", false);
    let shown = screen(&draw(&mut pane, NOW));
    assert!(shown.contains("2 by CSR"), "{shown}");
    assert_eq!(shown.matches("2 by CSR").count(), 1);
}

#[test]
fn the_threads_endpoint_keeps_real_thread_details_and_asks() {
    let (_home, _paths, mut pane) = pane("sess-now", false);
    pane.view = "threads".into();
    let before = screen(&tall(&mut pane, NOW));
    assert!(before.contains("Lost and found"));
    let (row, act) = thread_rows(&pane)[1].clone();
    pane.click(row, 0, NOW);
    tall(&mut pane, NOW);
    assert_eq!(pane.open, act["key"]);
    answer(&pane, "side", act["lost"].as_str().unwrap());
}

#[test]
fn the_asks_under_a_thread_answer_a_click_by_column() {
    let (_home, paths, mut pane) = pane("sess-now", false);
    pane.view = "threads".into();
    draw(&mut pane, NOW);
    let (_, top) = thread_row(&pane);
    let key = top["lost"].as_str().unwrap().to_owned();
    let (row, _) = thread_row(&pane);
    pane.click(row, 0, NOW);
    draw(&mut pane, NOW);
    // "side task" and "subagent" share a row, "drop" and "later" the next: four spans, four acts
    for pick in ["side", "sub", "drop", "later"] {
        let (row, col) = answer(&pane, pick, &key);
        let act = &pane.acts[&row];
        assert_eq!(act["lost"], key.as_str());
        assert_eq!(act["item"], format!("lost:{key}"));
        assert!(col > 0);
    }
    // a click on "drop" sets the thread aside for good, and the section moves up
    let (row, col) = answer(&pane, "drop", &key);
    pane.click(row, col, NOW);
    assert!(lost_log(&paths).contains(&key));
    assert!(lost_log(&paths).contains("\"drop\""));
    let after = screen(&draw(&mut pane, NOW));
    assert!(
        after.contains("set aside")
            || after.contains("Dropped")
            || after.contains("10 threads")
            || after.contains("later"),
        "{after}"
    );
    assert!(lost_log(&paths).contains("drop"));
}

#[test]
fn later_sets_a_thread_aside_until_the_next_compaction() {
    let (_home, paths, mut pane) = pane("sess-now", false);
    pane.view = "threads".into();
    tall(&mut pane, NOW);
    // the top thread's subagent failed, which brings it back whatever was said: set the second one aside
    let (row, second) = thread_rows(&pane)[1].clone();
    let key = second["lost"].as_str().unwrap().to_owned();
    pane.click(row, 0, NOW);
    tall(&mut pane, NOW);
    let (row, col) = answer(&pane, "later", &key);
    pane.click(row, col, NOW);
    assert!(lost_log(&paths).contains("\"later\""));
    let after = screen(&tall(&mut pane, NOW + 1.));
    assert!(
        after.contains("set aside")
            || after.contains("Dropped")
            || after.contains("10 threads")
            || after.contains("later"),
        "{after}"
    );
}

#[test]
fn an_ask_that_cannot_reach_claude_says_so_where_the_asks_were() {
    let (_home, paths, mut pane) = pane("sess-now", false);
    pane.view = "threads".into();
    draw(&mut pane, NOW);
    let (_, top) = thread_row(&pane);
    let key = top["lost"].as_str().unwrap().to_owned();
    let (row, _) = thread_row(&pane);
    pane.click(row, 0, NOW);
    draw(&mut pane, NOW);
    let (row, col) = answer(&pane, "side", &key);
    let logged = lost_log(&paths);
    // no agent pane in this run: the words go nowhere, and the thread is not marked sent
    pane.click(row, col, NOW);
    assert_eq!(pane.sent[0], format!("lost:{key}"));
    assert_eq!(pane.sent[1], false);
    assert_eq!(lost_log(&paths), logged);
    let after = screen(&draw(&mut pane, NOW));
    assert!(after.contains("✗ couldn't reach Claude's pane"), "{after}");
    // the line is the asks' row, so the two asks are gone; drop and later stay a click away
    assert!(pane
        .acts
        .values()
        .all(|a| a["key"] != format!("lost:{key}:side")));
    answer(&pane, "drop", &key);
}

#[test]
fn f_opens_the_thread_chronology_and_escape_closes_it() {
    let (_home, _paths, mut pane) = pane("sess-now", false);
    draw(&mut pane, NOW);
    pane.key("f");
    let shown = screen(&tall(&mut pane, NOW));
    assert!(pane.instrument_open.threads);
    assert_eq!(pane.view, "main");
    assert!(shown.contains("↳"), "{shown}");
    pane.key("escape");
    assert!(!pane.instrument_open.threads);
}

#[test]
fn a_fresh_session_keeps_carried_threads_behind_the_chronology() {
    let (_home, _paths, mut pane) = pane("sess-fresh", false);
    let shown = screen(&draw(&mut pane, NOW));
    assert!(!shown.contains("Last session"));
    pane.key("t");
    draw(&mut pane, NOW);
    assert!(pane.instrument_open.threads);
    assert_eq!(pane.view, "main");
}

#[test]
fn the_previous_session_figure_keeps_only_historical_facts() {
    let (_home, _paths, mut pane) = pane("sess-fresh", false);
    draw(&mut pane, NOW);
    let facts = pane.last_picture.as_ref().expect("previous session facts");
    assert!(!facts.series.is_empty());
    let shown = screen(&draw(&mut pane, NOW));
    assert!(!shown.contains("Last session"));
}

fn compacting_marker(paths: &Paths, sid: &str, at: f64) -> PathBuf {
    let marker = paths.kept_file("compacting", sid).unwrap();
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, format!("{{\"at\": {at}, \"trigger\": \"auto\"}}")).unwrap();
    marker
}

#[test]
fn the_pane_draws_at_once_when_a_compaction_ends() {
    let (_home, paths, mut pane) = pane("sess-fresh", false);
    // none yet: nothing to end
    draw(&mut pane, NOW);
    assert!(!pane.compaction_gone());
    let marker = compacting_marker(&paths, "sess-fresh", NOW - 5.);
    let running = screen(&draw(&mut pane, NOW));
    assert!(running.contains("Compacting · 5s"), "{running}");
    assert!(!pane.compaction_gone());
    // the marker goes: the watch loop is told before its next tick
    fs::remove_file(&marker).unwrap();
    assert!(pane.compaction_gone());
    let done = screen(&draw(&mut pane, NOW + 1.));
    assert!(!done.contains("♻A"), "{done}");
    assert!(!pane.compaction_gone());
}

fn wait_for(file: &Path) {
    let until = Instant::now() + Duration::from_secs(30);
    while !file.exists() {
        assert!(Instant::now() < until, "{} never appeared", file.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_session_before_this_one_is_kept_off_the_draw_path() {
    let (_home, paths, mut pane) = pane("sess-now", true);

    let carried = paths.kept_file("carried", "sess-now").unwrap();
    let previous = paths.kept_file("previous", "sess-now").unwrap();
    assert!(!carried.exists() && !previous.exists());
    // held: the carry's thread has not begun its work, and the frame draws all the same
    let release = pane.hold_carry();
    let began = Instant::now();
    let held = screen(&draw(&mut pane, NOW));
    assert!(began.elapsed() < Duration::from_secs(20));
    assert!(held.contains("Story"), "{held}");
    assert!(!carried.exists(), "the frame waited for the carry");
    assert!(
        !held.contains("from sess-bef"),
        "no thread is carried in yet: {held}"
    );
    // released: the carry is kept, first
    release.send(()).unwrap();
    wait_for(&carried);
    wait_for(&previous);
    let kept: Value = serde_json::from_str(&fs::read_to_string(&carried).unwrap()).unwrap();
    assert!(kept.is_array());
    assert!(!kept.as_array().unwrap().is_empty());
    assert!(kept
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["state"] == "carried" && t["came_from"].is_string()));
    let facts: Value = serde_json::from_str(&fs::read_to_string(&previous).unwrap()).unwrap();
    assert_eq!(facts["id"], "sess-before");
    // and shown: the explicit threads endpoint draws the threads it kept.
    pane.view = "threads".into();
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let shown = screen(&draw(&mut pane, NOW));
        if shown.contains("From the session before") {
            break;
        }
        assert!(Instant::now() < until, "the kept threads never showed");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_one_shot_frame_reads_the_earlier_session_inline_and_keeps_nothing() {
    let (_home, paths, mut pane) = pane("sess-now", false);

    let shown = screen(&draw(&mut pane, NOW));
    // the first frame already has the carried threads, as the golden frames do
    assert!(shown.contains("Story"), "{shown}");
    pane.view = "threads".into();
    let threads = screen(&draw(&mut pane, NOW));
    assert!(threads.contains("From the session before"), "{threads}");
    assert!(!paths.kept_file("carried", "sess-now").unwrap().exists());
    assert!(!paths.kept_file("previous", "sess-now").unwrap().exists());
}

#[test]
fn what_is_already_kept_is_read_back_not_worked_out_again() {
    let (_home, paths, mut pane) = pane("sess-now", true);
    pane.view = "threads".into();
    let carried = paths.kept_file("carried", "sess-now").unwrap();
    let previous = paths.kept_file("previous", "sess-now").unwrap();
    fs::create_dir_all(carried.parent().unwrap()).unwrap();
    fs::create_dir_all(previous.parent().unwrap()).unwrap();
    let thread = serde_json::json!([{
        "text": "A thread kept earlier.", "detail": [], "first_seen": NOW - 900., "last_seen": NOW - 800.,
        "times": 1, "state": "carried", "waiting": true, "rank": 0, "key": "feedfacecafe", "hidden": null,
        "later": false, "came_from": "sess-kep"
    }]);
    fs::write(&carried, thread.to_string()).unwrap();
    fs::write(&previous, "{\"id\": \"sess-kept\"}").unwrap();
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let shown = screen(&draw(&mut pane, NOW));
        if shown.contains("A thread kept earlier.") {
            break;
        }
        assert!(Instant::now() < until, "the kept thread never showed");
        std::thread::sleep(Duration::from_millis(20));
    }
    // the kept file stands: it was not rewritten from the transcripts
    assert_eq!(
        fs::read_to_string(&previous).unwrap(),
        "{\"id\": \"sess-kept\"}"
    );
}
