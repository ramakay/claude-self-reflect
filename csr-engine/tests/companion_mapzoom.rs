//! The Shift Map (`m`, its rows, its refresh) and the phone's zoom (`z`, the tap), against
//! pane.py's own frames in tests/fixtures/companion/mapzoom and main-*.txt.
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
fn refresh_row(pane: &Pane) -> usize {
    *pane
        .acts
        .iter()
        .find(|(_, a)| a["refresh"] == true)
        .expect("a refresh row")
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
    let mut lines = pane.frame(cols, rows, NOW);
    if state == "folded" {
        lines = pane.frame(cols, rows, NOW + 13.);
    }
    (text(lines), acts(&pane, rows))
}

#[test]
fn map_frames_are_byte_equal_to_pane_py_at_both_sizes_in_text_and_pixels() {
    let expected_acts: Value = serde_json::from_str(&golden("map-acts.json")).unwrap();
    for state in ["shut", "open", "entry", "landed", "folded", "inherited"] {
        for (cols, rows) in [(46, 30), (100, 40)] {
            for pixels in [false, true] {
                let (actual, acts) = render(state, cols, rows, pixels);
                let name = format!(
                    "{}map-{state}-{cols}x{rows}.txt",
                    if pixels { "pixels-" } else { "" }
                );
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
}

/// The refresh row starts csr-engine detached; a state with the refresh running or just ended
/// is reached by clicking it, against a stand-in engine that waits for a file or exits at once.
fn refreshed(state: &str, cols: usize, rows: usize, pixels: bool) -> String {
    let (_home, paths) = home();
    put_map(&paths, "sess-now", Some(&frame_maps()["old"]));
    let tools = tempfile::tempdir().unwrap();
    let slow = script(
        tools.path(),
        "slow",
        "while [ ! -e \"$(dirname \"$0\")/done\" ]; do sleep 0.05; done",
    );
    let quick = script(tools.path(), "quick", "exit 0");
    struct Release(PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = fs::write(&self.0, "");
        }
    }
    let _release = Release(tools.path().join("done"));
    let _turn = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    let mut pane = Pane::new(paths, &args(cols, rows));
    pane.pixels = pixels;
    if state == "note" {
        pane.open = json!("shifts");
    }
    pane.frame(cols, rows, NOW);
    let row = refresh_row(&pane);
    std::env::set_var(
        "CSR_ENGINE_BIN",
        if state == "refreshing" { &slow } else { &quick },
    );
    assert!(pane.click(row, 8, NOW));
    std::env::remove_var("CSR_ENGINE_BIN");
    for _ in 0..200 {
        let lines = pane.frame(cols, rows, NOW);
        let want = if state == "refreshing" {
            "shiftrefreshing"
        } else {
            "shiftrefreshnote"
        };
        if pane.acts.values().any(|a| a["key"] == want) {
            return text(lines);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("the {state} row never appeared");
}
#[test]
fn the_refresh_running_and_the_note_it_leaves_are_byte_equal_to_pane_py() {
    for state in ["refreshing", "note"] {
        for (cols, rows) in [(46, 30), (100, 40)] {
            for pixels in [false, true] {
                let actual = refreshed(state, cols, rows, pixels);
                let name = format!(
                    "{}map-{state}-{cols}x{rows}.txt",
                    if pixels { "pixels-" } else { "" }
                );
                let expected = golden(&name);
                for (i, (a, b)) in actual.lines().zip(expected.lines()).enumerate() {
                    assert_eq!(plain(a), plain(b), "{name}, row {}", i + 1);
                    assert_eq!(a, b, "{name}, row {} (ink)", i + 1);
                }
                assert_eq!(actual, expected, "{name}");
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
    let mut pane = Pane::new(paths.clone(), &args(46, 30));
    pane.frame(46, 30, NOW);
    let row = refresh_row(&pane);
    std::env::set_var("CSR_ENGINE_BIN", &record);
    assert!(pane.click(row, 8, NOW));
    pane.frame(46, 30, NOW);
    // The row is now "updating"; a second click on that row starts nothing.
    let again = refresh_row_or(&pane);
    if let Some(row) = again {
        pane.click(row, 8, NOW);
    }
    std::env::remove_var("CSR_ENGINE_BIN");
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
fn refresh_row_or(pane: &Pane) -> Option<usize> {
    pane.acts
        .iter()
        .find(|(_, a)| a["refresh"] == true)
        .map(|(r, _)| *r)
}

fn mapped_pane(cols: usize, rows: usize) -> (tempfile::TempDir, Paths, Pane) {
    let (home, paths) = home();
    put_map(&paths, "sess-now", Some(&frame_maps()["old"]));
    let mut pane = Pane::new(paths.clone(), &args(cols, rows));
    pane.frame(cols, rows, NOW);
    (home, paths, pane)
}
#[test]
fn m_opens_the_map_at_full_size_and_m_again_folds_it() {
    let (_h, _p, mut pane) = mapped_pane(100, 40);
    let shut = text(pane.frame(100, 40, NOW));
    assert!(plain(&shut).contains("+3 more shifts · click or m to expand"));
    assert!(pane.key("m"));
    assert_eq!(pane.open, "shifts");
    let open = text(pane.frame(100, 40, NOW));
    assert!(!plain(&open).contains("click or m to expand"));
    assert!(plain(&open).contains("decision · "));
    assert!(plain(&open).contains("removal · "));
    assert!(pane.key("m"));
    assert!(pane.open.is_null());
    // The page stays laid out tall (pane.py keeps it so until escape), the map is folded again.
    let folded = text(pane.frame(100, 40, NOW));
    assert!(plain(&folded).contains("+3 more shifts · click or m to expand"));
    assert!(!plain(&folded).contains("decision · "));
}
#[test]
fn m_also_folds_a_map_a_click_opened_on_an_entry() {
    let (_h, _p, mut pane) = mapped_pane(100, 40);
    pane.key("m");
    pane.frame(100, 40, NOW);
    let row = act_row(&pane, "shiftlike:aaaa1111");
    pane.click(row, 8, NOW);
    assert_eq!(pane.open, "shiftlike:aaaa1111");
    assert!(pane.key("m"));
    assert!(pane.open.is_null());
}
#[test]
fn m_does_nothing_without_a_map_and_off_the_main_page() {
    let (_home, paths) = home();
    put_map(&paths, "sess-now", None);
    let mut pane = Pane::new(paths, &args(46, 30));
    pane.frame(46, 30, NOW);
    assert!(pane.key("m"));
    assert!(pane.open.is_null(), "no map to open");
    let (_h, _p, mut pane) = mapped_pane(46, 30);
    pane.key("l");
    pane.key("m");
    assert!(pane.open.is_null(), "the log page has no map");
    assert_eq!(pane.view, "log");
}
#[test]
fn a_click_on_the_map_opens_it_and_a_second_click_folds_it_without_sending() {
    let (_h, _p, mut pane) = mapped_pane(100, 40);
    let row = act_row(&pane, "shifts");
    assert!(pane.click(row, 8, NOW));
    assert_eq!(pane.open, "shifts");
    pane.frame(100, 40, NOW);
    let row = act_row(&pane, "shifts");
    assert!(pane.click(row, 8, NOW));
    assert!(pane.open.is_null());
    assert!(
        pane.sent.is_null(),
        "a report is never put in Claude's prompt"
    );
}
#[test]
fn opening_an_entry_keeps_one_expand_signal_and_it_is_no_like() {
    let (_h, paths, mut pane) = mapped_pane(100, 40);
    pane.key("m");
    pane.frame(100, 40, NOW);
    let row = act_row(&pane, "shiftlike:aaaa1111");
    pane.click(row, 8, NOW);
    assert_eq!(pane.open, "shiftlike:aaaa1111");
    let kept = likes(&paths);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0]["key"], "shift:aaaa1111");
    assert_eq!(kept[0]["source"], "story_map");
    assert_eq!(kept[0]["signal"], "expand");
    assert!(kept[0]["liked"].is_null(), "an expand carries no like");
    // A second click on the opened entry goes back to the open map and keeps nothing more.
    pane.frame(100, 40, NOW);
    let row = act_row(&pane, "shiftlike:aaaa1111");
    pane.click(row, 8, NOW);
    assert_eq!(pane.open, "shifts");
    assert_eq!(likes(&paths).len(), 1);
    assert!(pane.sent.is_null());
}
#[test]
fn an_expand_never_undoes_a_like_and_a_heart_on_an_entry_is_kept_as_the_maps() {
    let (_h, paths, mut pane) = mapped_pane(100, 40);
    pane.key("m");
    pane.frame(100, 40, NOW);
    let row = act_row(&pane, "shiftlike:aaaa1111");
    let heart = pane.acts[&row]["heart"].as_array().unwrap().clone();
    let col = heart[0].as_u64().unwrap() as usize;
    assert!(pane.click(row, col, NOW));
    let kept = likes(&paths);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0]["key"], "shift:aaaa1111");
    assert_eq!(kept[0]["source"], "story_map");
    assert_eq!(kept[0]["liked"], true);
    // Now open it: the expand line comes after the like and must not read as an unlike.
    pane.frame(100, 40, NOW);
    pane.click(row, 8, NOW);
    assert_eq!(likes(&paths).len(), 2);
    let read = csr_engine::companion::send::read_likes(&paths);
    assert_eq!(read["shift:aaaa1111"], true, "{read}");
}
#[test]
fn the_map_folds_by_itself_after_its_hold_and_the_loop_is_told_when() {
    let (_home, paths) = home();
    put_map(&paths, "sess-now", Some(&frame_maps()["fresh"]));
    let mut pane = Pane::new(paths, &args(100, 40));
    assert_eq!(pane.map_wait(NOW), (1.0, false), "before the first frame");
    let landed = text(pane.frame(100, 40, NOW));
    assert!(
        !plain(&landed).contains("click or m to expand"),
        "a new map opens"
    );
    assert_eq!(pane.map_wait(NOW + 2.), (1.0, false));
    assert_eq!(pane.map_wait(NOW + 11.5), (0.5, false));
    assert_eq!(pane.map_wait(NOW + 12.), (0.05, true));
    let folded = text(pane.frame(100, 40, NOW + 13.));
    assert!(plain(&folded).contains("click or m to expand"));
    assert_eq!(pane.map_wait(NOW + 13.), (1.0, false));
}
#[test]
fn a_map_that_is_held_open_by_m_does_not_fold_under_the_reader() {
    let (_home, paths) = home();
    put_map(&paths, "sess-now", Some(&frame_maps()["fresh"]));
    let mut pane = Pane::new(paths, &args(100, 40));
    pane.frame(100, 40, NOW);
    pane.key("m");
    pane.key("m");
    pane.key("m");
    let late = text(pane.frame(100, 40, NOW + 60.));
    assert!(!plain(&late).contains("click or m to expand"));
    assert!(!pane.map_wait(NOW + 60.).1);
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
    assert!(pane.click(row, 99, NOW));
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
