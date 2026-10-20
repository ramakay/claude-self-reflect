//! Changes and phone navigation through the single surface, plus the retained Shift Map command
//! and feedback backends. State goldens are generated through the current Rust Pane::frame.
use csr_engine::companion::{
    cli::PaneArgs, db::Db, hook_json, pane::Pane, paths::Paths, phone::Herdr,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const NOW: f64 = 1792320000.;
/// CSR_ENGINE_BIN is process-wide: the tests that set it take turns.
static ENGINE: Mutex<()> = Mutex::new(());

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/companion")
}
fn golden(name: &str) -> String {
    fs::read_to_string(fixture().join("mapzoom").join(name)).unwrap()
}
fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}
fn home() -> (tempfile::TempDir, Paths) {
    let d = tempfile::tempdir().unwrap();
    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let e = entry.unwrap();
            if e.file_type().unwrap().is_dir() {
                copy(&e.path(), &to.join(e.file_name()));
            } else {
                let bytes = fs::read(e.path()).unwrap();
                match std::str::from_utf8(&bytes) {
                    Ok(text) => fs::write(
                        to.join(e.file_name()),
                        text.replace("<REPO>", &repo().to_string_lossy()),
                    )
                    .unwrap(),
                    Err(_) => fs::write(to.join(e.file_name()), &bytes).unwrap(),
                }
            }
        }
    }
    copy(&fixture().join("home"), d.path());
    // The mapzoom goldens were drawn with no agents and no Codex runs; In flight is checked in the rich world.
    let _ = fs::remove_dir_all(d.path().join(".codex"));
    let _ = fs::remove_dir_all(d.path().join(".claude-self-reflect/csr-companion/agents"));
    let paths = Paths::new(d.path().into());
    // The goldens were drawn with no compaction of CSR's, so no "this session:" row under the masthead.
    let dreams = paths.keep.join("dreams.jsonl");
    let kept: String = fs::read_to_string(&dreams)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.contains("\"kind\": \"compact\""))
        .map(|line| format!("{line}\n"))
        .collect();
    fs::write(&dreams, kept).unwrap();
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
/// Put `raw` in the DB as `sid`'s Shift Map (or take that map out with None).
fn put_map(paths: &Paths, sid: &str, raw: Option<&Value>) {
    let con = rusqlite::Connection::open(&paths.db).unwrap();
    let key = format!("story_map:{sid}");
    con.execute("delete from meta where key=?1", [&key])
        .unwrap();
    if let Some(raw) = raw {
        con.execute("insert into meta values(?1,?2)", [&key, &raw.to_string()])
            .unwrap();
    }
}
fn frame_maps() -> Value {
    serde_json::from_str(&golden("frame-maps.json")).unwrap()
}
fn text(lines: Vec<String>) -> String {
    lines.join("\n") + "\n"
}
fn plain(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}
fn acts(pane: &Pane, rows: usize) -> BTreeMap<String, String> {
    pane.acts
        .iter()
        .filter(|(row, _)| **row < rows - 1)
        .map(|(row, act)| {
            (
                row.to_string(),
                act["key"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect()
}
fn act_row(pane: &Pane, key: &str) -> usize {
    *pane
        .acts
        .iter()
        .find(|(_, a)| a["key"] == key)
        .unwrap_or_else(|| panic!("no row acts as {key}: {:?}", pane.acts))
        .0
}

fn likes(paths: &Paths) -> Vec<Value> {
    fs::read_to_string(paths.keep.join("likes.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Set up `state` the way the oracle did and draw it: the text of the frame, and its clicks.
fn render(
    state: &str,
    cols: usize,
    rows: usize,
    pixels: bool,
) -> (String, BTreeMap<String, String>) {
    let (_home, paths) = home();
    let maps = frame_maps();
    match state {
        "landed" | "folded" => put_map(&paths, "sess-now", Some(&maps["fresh"])),
        "inherited" => {
            put_map(&paths, "sess-now", None);
            put_map(&paths, "sess-before", Some(&maps["inherited"]));
        }
        _ => put_map(&paths, "sess-now", Some(&maps["old"])),
    }
    let mut pane = Pane::new(paths, &args(cols, rows));
    pane.pixels = pixels;
    pane.open = match state {
        "open" | "inherited" => json!("shifts"),
        "entry" => json!("shiftlike:aaaa1111"),
        _ => Value::Null,
    };
    pane.instrument_open.changes = matches!(state, "open" | "entry" | "inherited");
    let mut lines = pane.frame(cols, rows, NOW);
    if state == "folded" {
        lines = pane.frame(cols, rows, NOW + 13.);
    }
    (text(lines), acts(&pane, rows))
}

#[test]
fn map_frames_are_byte_equal_to_pane_py_at_both_sizes_in_text_and_pixels() {
    let mut expected_acts: Value = serde_json::from_str(&golden("map-acts.json")).unwrap();
    for state in ["shut", "open", "entry", "landed", "folded", "inherited"] {
        for (cols, rows) in [(46, 30), (100, 40)] {
            for pixels in [false, true] {
                let (actual, acts) = render(state, cols, rows, pixels);
                let name = format!(
                    "{}map-{state}-{cols}x{rows}.txt",
                    if pixels { "pixels-" } else { "" }
                );
                if std::env::var_os("CSR_MAP_UPDATE").is_some() {
                    fs::write(fixture().join("mapzoom").join(&name), &actual).unwrap();
                    if !pixels {
                        expected_acts[format!("{state}-{cols}x{rows}")] = json!(acts);
                    }
                }
                let expected = golden(&name);
                for (i, (a, b)) in actual.lines().zip(expected.lines()).enumerate() {
                    assert_eq!(a, b, "{name}, row {}", i + 1);
                }
                assert_eq!(actual, expected, "{name}");
                if !pixels {
                    assert_eq!(
                        json!(acts),
                        expected_acts[format!("{state}-{cols}x{rows}")],
                        "clicks of {name}"
                    );
                }
                eprintln!("map parity: {name}: {rows} rows byte equal");
            }
        }
    }
    if std::env::var_os("CSR_MAP_UPDATE").is_some() {
        fs::write(
            fixture().join("mapzoom/map-acts.json"),
            serde_json::to_string_pretty(&expected_acts).unwrap(),
        )
        .unwrap();
    }
}

/// The refresh row starts csr-engine detached; a state with the refresh running or just ended
/// is reached by clicking it, against a stand-in engine that waits for a file or exits at once.
fn refreshed(state: &str, cols: usize, rows: usize, pixels: bool) -> String {
    let (_home, paths) = home();
    put_map(&paths, "sess-now", Some(&frame_maps()["old"]));
    let tools = tempfile::tempdir().unwrap();
    let quick = script(
        tools.path(),
        "engine",
        if state == "refreshing" {
            "sleep 0.2"
        } else {
            "exit 0"
        },
    );
    let db = Db::open(&paths).unwrap();
    let mut map = csr_engine::companion::shiftmap::MapState::default();
    map.load(&db, &paths, "sess-now");
    let transcript = paths.projects.join("fixture/sess-now.jsonl");
    map.start_refresh(Some(quick), transcript.to_str(), repo().to_str().unwrap());
    let (running, note) = map.poll(NOW);
    assert!(running || note.is_some());
    for _ in 0..100 {
        let (running, note) = map.poll(NOW);
        if !running {
            assert!(note.is_some());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let mut pane = Pane::new(paths, &args(cols, rows));
    pane.pixels = pixels;
    pane.key("l");
    text(pane.frame(cols, rows, NOW))
}

#[test]
fn the_refresh_backend_finishes_without_reintroducing_a_dashboard_page() {
    for state in ["refreshing", "note"] {
        for (cols, rows) in [(46, 30), (100, 40)] {
            for pixels in [false, true] {
                let actual = refreshed(state, cols, rows, pixels);
                assert_eq!(actual.lines().count(), rows);
                assert!(!plain(&actual).contains("Session info"));
            }
        }
    }
}
#[test]
fn clicking_refresh_hands_this_transcript_to_the_engine_detached_and_once() {
    let (_home, paths) = home();
    put_map(&paths, "sess-now", Some(&frame_maps()["old"]));
    let tools = tempfile::tempdir().unwrap();
    let record = script(
        tools.path(),
        "engine",
        "{ printf '%s\\n' \"$@\"; echo \"hooks=$CSR_DISABLE_RECURSIVE_HOOKS\"; } \
         >> \"$(dirname \"$0\")/argv\"; sleep 0.3",
    );
    let _turn = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    let db = Db::open(&paths).unwrap();
    let mut map = csr_engine::companion::shiftmap::MapState::default();
    map.load(&db, &paths, "sess-now");
    let transcript = paths.projects.join("fixture/sess-now.jsonl");
    map.start_refresh(
        Some(record.clone()),
        transcript.to_str(),
        repo().to_str().unwrap(),
    );
    map.start_refresh(Some(record), transcript.to_str(), repo().to_str().unwrap());
    let argv = tools.path().join("argv");
    for _ in 0..100 {
        if argv.exists() && fs::read_to_string(&argv).unwrap().contains("hooks=") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let said: Vec<String> = fs::read_to_string(&argv)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(said.len(), 7, "one run only: {said:?}");
    assert_eq!(&said[..2], ["generate-story", "--transcript"]);
    assert_eq!(
        said[2],
        paths
            .projects
            .join("fixture/sess-now.jsonl")
            .to_string_lossy()
    );
    assert_eq!(said[3], "--cwd");
    assert!(!said[4].is_empty());
    assert_eq!(&said[5..], ["--rewrite", "hooks=1"]);
}

fn mapped_pane(cols: usize, rows: usize) -> (tempfile::TempDir, Paths, Pane) {
    let (home, paths) = home();
    put_map(&paths, "sess-now", Some(&frame_maps()["old"]));
    let mut pane = Pane::new(paths.clone(), &args(cols, rows));
    pane.frame(cols, rows, NOW);
    (home, paths, pane)
}
#[test]
fn m_opens_changes_in_place_and_again_folds_them() {
    let (_h, _p, mut pane) = mapped_pane(100, 40);
    let shut = text(pane.frame(100, 40, NOW));
    assert!(!plain(&shut).contains("Was:"));
    pane.key("m");
    assert!(pane.instrument_open.changes);
    let open = text(pane.frame(100, 40, NOW));
    assert!(plain(&open).contains("Was:"));
    pane.key("m");
    assert!(!pane.instrument_open.changes);
    let folded = text(pane.frame(100, 40, NOW));
    assert!(!plain(&folded).contains("Was:"));
}
#[test]
fn m_folds_changes_a_shift_click_opened() {
    let (_h, _p, mut pane) = mapped_pane(100, 40);
    let row = act_row(&pane, "instrument:changes");
    pane.click(row, 0, NOW);
    assert!(pane.instrument_open.changes);
    pane.key("m");
    assert!(!pane.instrument_open.changes);
}
#[test]
fn changes_is_an_in_place_panel_even_when_the_feed_is_empty() {
    let (_home, paths) = home();
    put_map(&paths, "sess-now", None);
    let mut pane = Pane::new(paths, &args(46, 30));
    pane.key("m");
    pane.frame(46, 30, NOW);
    assert!(pane.instrument_open.changes);
    assert_eq!(pane.view, "main");
    pane.key("escape");
    assert!(!pane.instrument_open.changes);
}
#[test]
fn a_shift_click_opens_changes_and_a_second_closes_without_sending() {
    let (_h, _p, mut pane) = mapped_pane(100, 40);
    let row = act_row(&pane, "instrument:changes");
    pane.click(row, 0, NOW);
    assert!(pane.instrument_open.changes);
    pane.frame(100, 40, NOW);
    let row = act_row(&pane, "instrument:changes");
    pane.click(row, 0, NOW);
    assert!(!pane.instrument_open.changes);
    assert!(pane.sent.is_null());
}
#[test]
fn the_shift_signal_backend_keeps_an_expand_without_a_like() {
    let (_h, paths, _pane) = mapped_pane(100, 40);
    csr_engine::companion::send::keep_shift_signal(&paths, "aaaa1111", "Reader streams", NOW)
        .unwrap();
    let kept = likes(&paths);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0]["key"], "shift:aaaa1111");
    assert_eq!(kept[0]["source"], "story_map");
    assert_eq!(kept[0]["signal"], "expand");
    assert!(kept[0]["liked"].is_null());
}
#[test]
fn the_shift_signal_backend_never_undoes_an_explicit_like() {
    let (_h, paths, _pane) = mapped_pane(100, 40);
    let act = json!({"key":"shiftlike:aaaa1111","like":"shift:aaaa1111","like_source":"story_map","title":"Reader streams"});
    csr_engine::companion::send::keep_like(&paths, &act, true, NOW).unwrap();
    csr_engine::companion::send::keep_shift_signal(&paths, "aaaa1111", "Reader streams", NOW + 1.)
        .unwrap();
    assert_eq!(likes(&paths).len(), 2);
    assert_eq!(
        csr_engine::companion::send::read_likes(&paths)["shift:aaaa1111"],
        true
    );
}
#[test]
fn the_map_timer_notifies_the_loop_while_the_card_keeps_compact_shifts() {
    let (_home, paths) = home();
    put_map(&paths, "sess-now", Some(&frame_maps()["fresh"]));
    let mut pane = Pane::new(paths, &args(100, 40));
    assert_eq!(pane.map_wait(NOW), (1.0, false));
    let landed = text(pane.frame(100, 40, NOW));
    assert!(!plain(&landed).contains("Before:"));
    assert_eq!(pane.map_wait(NOW + 11.5), (0.5, false));
    assert_eq!(pane.map_wait(NOW + 12.), (0.05, true));
    let folded = text(pane.frame(100, 40, NOW + 13.));
    assert!(!plain(&folded).contains("Was:"));
}
#[test]
fn changes_opened_by_m_stay_open_under_the_reader() {
    let (_home, paths) = home();
    put_map(&paths, "sess-now", Some(&frame_maps()["fresh"]));
    let mut pane = Pane::new(paths, &args(100, 40));
    pane.frame(100, 40, NOW);
    pane.key("m");
    let late = text(pane.frame(100, 40, NOW + 60.));
    assert!(pane.instrument_open.changes);
    assert!(plain(&late).contains("Was:"));
}
#[test]
fn the_json_mode_shift_map_is_the_text_pane_py_gives_the_ink_pane() {
    let expected: Value = serde_json::from_str(&golden("shift-map-json.json")).unwrap();
    let (_home, paths) = home();
    let db = Db::open(&paths).unwrap();
    let got = hook_json::shift_map_json(&db, &paths, "sess-now").unwrap();
    assert_eq!(
        json!({"title": got.title, "rows": got.rows}),
        expected["home"]
    );
    drop(db);
    put_map(&paths, "sess-now", None);
    put_map(&paths, "sess-before", Some(&frame_maps()["inherited"]));
    let db = Db::open(&paths).unwrap();
    let got = hook_json::shift_map_json(&db, &paths, "sess-now").unwrap();
    assert_eq!(
        json!({"title": got.title, "rows": got.rows}),
        expected["inherited"]
    );
    drop(db);
    put_map(&paths, "sess-before", None);
    let db = Db::open(&paths).unwrap();
    assert!(hook_json::shift_map_json(&db, &paths, "sess-now").is_none());
    assert!(expected["none"].is_null());
}

type Calls = Arc<Mutex<Vec<String>>>;
/// A herdr that says the tab is `width` wide and yes to everything else; its calls are kept.
fn fake_herdr(width: u64) -> (Herdr, Calls) {
    let calls: Calls = Arc::default();
    let seen = calls.clone();
    let herdr = Herdr::with(
        "pane-7",
        Path::new("/nonexistent/herdr.toml"),
        Box::new(move |argv: &[&str]| {
            seen.lock().unwrap().push(argv.join(" "));
            if argv.get(1) == Some(&"layout") {
                return Some(
                    json!({"result": {"layout": {"area": {"width": width, "height": 40}}}})
                        .to_string(),
                );
            }
            Some(String::new())
        }),
    );
    (herdr, calls)
}
fn said(calls: &Calls) -> Vec<String> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| !c.contains("layout"))
        .cloned()
        .collect()
}
fn phone_pane(width: u64, cols: usize) -> (tempfile::TempDir, Pane, Calls) {
    let (home, paths) = home();
    let mut pane = Pane::new(paths, &args(cols, 40));
    let (herdr, calls) = fake_herdr(width);
    pane.herdr = Some(herdr);
    pane.frame(cols, 40, NOW);
    (home, pane, calls)
}
#[test]
fn a_tap_on_a_phones_narrow_column_zooms_it_and_z_gives_the_split_back() {
    let (_h, mut pane, calls) = phone_pane(50, 17);
    assert!(
        said(&calls).is_empty(),
        "drawing alone asks herdr nothing but the layout"
    );
    assert!(pane.click(5, 3, NOW));
    assert_eq!(said(&calls), ["pane zoom --pane pane-7 --on"]);
    assert!(pane.phone.zoomed);
    // Zoomed, the pane is the phone's width and its legend offers the way back.
    let lines = pane.frame(50, 40, NOW + 1.);
    let foot = plain(lines.last().unwrap());
    assert!(foot.contains("z split"), "{foot:?}");
    assert!(pane.key("z"));
    assert!(!pane.phone.zoomed);
    assert_eq!(
        said(&calls),
        [
            "pane zoom --pane pane-7 --on",
            "pane zoom --pane pane-7 --off",
            "pane focus --pane pane-7 --direction left",
        ]
    );
    let lines = pane.frame(50, 40, NOW + 2.);
    assert!(!plain(lines.last().unwrap()).contains("z split"));
}
#[test]
fn a_tap_on_the_z_label_of_the_legend_gives_the_split_back() {
    let (_h, mut pane, calls) = phone_pane(50, 17);
    pane.click(5, 3, NOW);
    pane.frame(50, 40, NOW + 1.);
    let legend = pane.acts[&39].clone();
    let span = legend["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s[2] == "z")
        .expect("a z label")
        .clone();
    assert!(pane.click(39, span[0].as_u64().unwrap() as usize, NOW + 1.));
    assert!(!pane.phone.zoomed);
    assert!(said(&calls).iter().any(|c| c.ends_with("--off")));
}
#[test]
fn a_tap_on_the_legend_of_a_narrow_column_does_not_zoom() {
    let (_h, mut pane, calls) = phone_pane(50, 17);
    pane.click(39, 2, NOW);
    assert!(said(&calls).is_empty());
    assert!(!pane.phone.zoomed);
}
#[test]
fn a_desktop_split_never_zooms_on_a_tap_or_on_z() {
    let (_h, mut pane, calls) = phone_pane(122, 17);
    pane.click(5, 3, NOW);
    pane.key("z");
    assert!(!pane.phone.zoomed);
    assert!(said(&calls).is_empty(), "{:?}", said(&calls));
}
#[test]
fn z_and_a_tap_do_nothing_outside_herdr() {
    let (_home, paths) = home();
    let mut pane = Pane::new(paths, &args(17, 40));
    pane.frame(17, 40, NOW);
    assert!(pane.click(5, 3, NOW));
    assert!(pane.key("z"));
    assert!(!pane.phone.zoomed);
}
#[test]
fn a_desktop_taking_a_zoomed_tab_unzooms_it() {
    let (_h, mut pane, calls) = phone_pane(50, 17);
    pane.click(5, 3, NOW);
    assert!(pane.phone.zoomed);
    // The tab is now a desktop's: wide, and herdr says so on the next read.
    let (desktop, desktop_calls) = fake_herdr(122);
    pane.herdr = Some(desktop);
    pane.frame(120, 40, NOW + 30.);
    assert!(!pane.phone.zoomed);
    assert_eq!(said(&desktop_calls), ["pane zoom --pane pane-7 --off"]);
    assert_eq!(said(&calls).len(), 1);
}
#[test]
fn a_click_that_misses_a_rows_answers_closes_what_is_open() {
    let (_home, paths) = home();
    let mut pane = Pane::new(paths, &args(100, 40));
    pane.frame(100, 40, NOW);
    let row = *pane
        .acts
        .iter()
        .find(|(_, a)| a["spans"][0][2] == "helpful")
        .expect("a dream's answers")
        .0;
    pane.open = json!("dream:newest");
    assert!(pane.click(row, 100, NOW));
    assert!(pane.open.is_null(), "{}", pane.open);
}

/// `attached_session` with a herdr whose `process-info` foreground argv and `pane get`
/// agent_session are the two given, and the job state.json files in the home.
fn attached(argv: &[&str], shown: Value, jobs: &[(&str, &str)]) -> Option<String> {
    let (_home, paths) = home();
    for (job, session) in jobs {
        let dir = paths.home.join(".claude/jobs").join(&job[..8]);
        fs::create_dir_all(&dir).unwrap();
        let state = json!({ "sessionId": session });
        fs::write(dir.join("state.json"), state.to_string()).unwrap();
    }
    let info = json!({"result": {"process_info": {
        "foreground_process_group_id": 7,
        "foreground_processes": [{"pid": 9, "argv": ["sh"]}, {"pid": 7, "argv": argv}],
    }}});
    let get = json!({"result": {"pane": {"agent_session": shown}}});
    csr_engine::companion::send::attached_session_with(&paths, "pane-3", &|call| match call
        .get(1)
        .copied()
    {
        Some("process-info") => Some(info.to_string()),
        Some("get") => Some(get.to_string()),
        _ => None,
    })
}
const JOB: &str = "eda7fbcf-1111-2222-3333-444455556666";
#[test]
fn the_pane_beside_claude_attach_follows_the_job_it_shows() {
    let jobs = [("eda7fbcf", JOB)];
    let none = json!(null);
    let argv = ["/usr/local/bin/claude", "attach", "--verbose", "eda7fbcf"];
    assert_eq!(attached(&argv, none.clone(), &jobs).as_deref(), Some(JOB));
    let other = [("eda7fbcf", "ffff0000-aaaa")];
    assert_eq!(attached(&argv, none.clone(), &other), None);
    let bad = ["claude", "attach", "../etc"];
    assert_eq!(attached(&bad, none.clone(), &jobs), None);
    let shell = ["bash", "attach", "eda7fbcf"];
    assert_eq!(attached(&shell, none.clone(), &jobs), None);
    let other_verb = ["claude", "resume", "eda7fbcf"];
    assert_eq!(attached(&other_verb, none, &jobs), None);
}
#[test]
fn the_pane_beside_claude_agents_follows_the_job_herdr_reports() {
    let jobs = [("eda7fbcf", JOB)];
    let argv = ["claude", "agents"];
    let shown = json!({"kind": "id", "value": "eda7fbcf"});
    assert_eq!(attached(&argv, shown, &jobs).as_deref(), Some(JOB));
    let named = json!({"kind": "name", "value": "eda7fbcf"});
    assert_eq!(attached(&argv, named, &jobs), None);
    assert_eq!(attached(&argv, json!(null), &jobs), None);
    let unknown = json!({"kind": "id", "value": "99999999"});
    assert_eq!(attached(&argv, unknown, &jobs), None);
}
