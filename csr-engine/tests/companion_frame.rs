//! Frame-level goldens for the companion pane: the whole screen `Pane::frame` writes, over a rich HOME.
//!
//! The goldens in tests/fixtures/companion/rich/ come from the Python pane's own `Pane.frame` (not `draw_main` on a
//! thin dict), driven over the same fixture files by the job-folder oracle. Each test builds the rich HOME the way
//! tests/companion.rs builds its HOME (base home, rich overlay, DB from home.sql then rich/home.sql), replays the
//! scenario in rich/frames.json (frames at the pinned times, clicks, scrolls, the busy file, the dream line, the
//! finished agents file) and compares the text, the acts and, for pixel scenarios, the picture placements.
//!
//! `frame_*` tests compare a whole screen; `section_*` tests find one section's rows inside the screen (so a package
//! can un-ignore the section it ports). An `#[ignore]` names the package that ports what is missing.
use csr_engine::companion::{cli::PaneArgs, pane::Pane, paths::Paths};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Once, OnceLock},
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
fn args(sid: &str, cols: usize, rows: usize) -> PaneArgs {
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
        sid,
    ])
    .pane
}
fn manifest() -> &'static Value {
    static M: OnceLock<Value> = OnceLock::new();
    M.get_or_init(|| {
        serde_json::from_str(&fs::read_to_string(rich().join("frames.json")).unwrap()).unwrap()
    })
}
fn scenario(name: &str) -> &'static Value {
    manifest()["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("no scenario {name} in rich/frames.json"))
}

struct Shown {
    text: Vec<String>,
    acts: BTreeMap<usize, Value>,
    pictures: Vec<[usize; 4]>,
}
/// Replay one scenario the way the oracle does: a frame at each step's time, then each action followed by a frame.
fn show(name: &str) -> (tempfile::TempDir, Shown) {
    let sc = scenario(name);
    let (cols, rows) = (
        sc["cols"].as_u64().unwrap() as usize,
        sc["rows"].as_u64().unwrap() as usize,
    );
    let (home, paths) = rich_home();
    let mut pane = Pane::new(
        paths.clone(),
        &args(sc["sid"].as_str().unwrap(), cols, rows),
    );
    pane.pixels = sc["pixels"] == true;
    let mut text = vec![];
    for step in sc["steps"].as_array().unwrap() {
        let at = step["at"].as_f64().unwrap();
        text = pane.frame(cols, rows, at);
        for action in step["do"].as_array().unwrap() {
            act(&mut pane, &paths, action, at);
            text = pane.frame(cols, rows, at);
        }
    }
    let pictures = pane
        .placed
        .iter()
        .map(|p| [p.row, p.col, p.cols, p.rows])
        .collect();
    let acts = pane.acts.clone();
    // CSR_FRAME_DUMP=<dir> writes what the pane drew, in the golden's format, so a failing diff can be read
    if let Some(dir) = std::env::var_os("CSR_FRAME_DUMP") {
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            Path::new(&dir).join(format!("{name}.txt")),
            text.join("\n") + "\n",
        )
        .unwrap();
        let shown: BTreeMap<String, &Value> = acts
            .iter()
            .map(|(row, act)| (row.to_string(), act))
            .collect();
        fs::write(
            Path::new(&dir).join(format!("{name}.acts.json")),
            serde_json::to_string_pretty(&shown).unwrap() + "\n",
        )
        .unwrap();
    }
    (
        home,
        Shown {
            text,
            acts,
            pictures,
        },
    )
}
fn act(pane: &mut Pane, paths: &Paths, action: &Value, at: f64) {
    if let Some(key) = action["click_key"].as_str() {
        let row = pane
            .acts
            .iter()
            .find(|(_, act)| act["key"].as_str().is_some_and(|k| k.starts_with(key)))
            .map(|(row, _)| *row)
            .unwrap_or_else(|| panic!("no row with act key {key:?}"));
        pane.click(row, 0, at);
    } else if let Some(n) = action["scroll"].as_i64() {
        pane.scroll(n as isize);
    } else if action["busy"] == true {
        let folder = paths.home.join(".claude/sessions");
        fs::create_dir_all(&folder).unwrap();
        let text = fs::read_to_string(rich().join("busy-session.json")).unwrap();
        fs::write(
            folder.join("4242.json"),
            fill(&text, &paths.home.to_string_lossy()),
        )
        .unwrap();
    } else if action["dream"] == true {
        use std::io::Write;
        let line = serde_json::json!({
            "kind": "dream", "id": "arrived", "session": "sess-now", "at": 1792320000. + 150.,
            "line": "What if the export queue paged its output?", "title": "Page the export",
            "text": "- Page the output.\n- Keep each page small."
        });
        let mut log = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(paths.keep.join("dreams.jsonl"))
            .unwrap();
        writeln!(log, "{line}").unwrap();
    } else if let Some(variant) = action["agents"].as_str() {
        let text =
            fs::read_to_string(rich().join(format!("variants/agents-{variant}.json"))).unwrap();
        fs::write(
            paths.keep.join("agents/sess-now.json"),
            fill(&text, &paths.home.to_string_lossy()),
        )
        .unwrap();
    } else {
        panic!("unknown action {action}");
    }
}

/// Numbers compare as numbers (62 equals 62.0); the machine-bound parts of an act are normalized like the oracle's:
/// the HOME path is `<HOME>` and a file's mtime is 0.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}
fn normalized(value: &Value, homes: &[String]) -> Value {
    match value {
        Value::String(s) => {
            Value::String(homes.iter().fold(s.clone(), |s, h| s.replace(h, "<HOME>")))
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| normalized(v, homes)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        if k == "mtime" {
                            Value::from(0)
                        } else {
                            normalized(v, homes)
                        },
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}
fn homes(home: &tempfile::TempDir) -> Vec<String> {
    let mut out = vec![home.path().to_string_lossy().into_owned()];
    if let Ok(real) = home.path().canonicalize() {
        out.push(real.to_string_lossy().into_owned());
    }
    out.sort_by_key(|h| std::cmp::Reverse(h.len()));
    out.dedup();
    out
}
fn golden_acts(name: &str) -> BTreeMap<usize, Value> {
    let text = fs::read_to_string(rich().join(format!("{name}.acts.json"))).unwrap();
    serde_json::from_str::<BTreeMap<String, Value>>(&text)
        .unwrap()
        .into_iter()
        .map(|(row, act)| (row.parse().unwrap(), act))
        .collect()
}
fn strip(line: &str) -> String {
    // the diff message shows rows without escape codes
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
fn differ(
    want: &BTreeMap<usize, Value>,
    got: &BTreeMap<usize, Value>,
    homes: &[String],
) -> Option<String> {
    for (row, w) in want {
        match got.get(row) {
            None => return Some(format!("act row {row}: golden has {w}, the pane has none")),
            Some(g) if !same(w, &normalized(g, homes)) => {
                return Some(format!("act row {row}: golden {w}, pane {g}"))
            }
            _ => {}
        }
    }
    got.keys()
        .find(|row| !want.contains_key(row))
        .map(|row| format!("act row {row}: the pane has {}, the golden none", got[row]))
}

/// The whole screen: every row of text, every act, and (pixel scenarios) every picture placement.
fn check_frame(name: &str) {
    let (home, shown) = show(name);
    let want = fs::read_to_string(rich().join(format!("{name}.txt"))).unwrap();
    let want: Vec<&str> = want
        .strip_suffix('\n')
        .unwrap_or(&want)
        .split('\n')
        .collect();
    for (i, w) in want.iter().enumerate() {
        let g = shown.text.get(i).map(String::as_str).unwrap_or("<no row>");
        assert_eq!(strip(g), strip(w), "{name}: row {i} (plain text)");
        assert_eq!(g, *w, "{name}: row {i} (with colors)");
    }
    assert_eq!(shown.text.len(), want.len(), "{name}: row count");
    if let Some(why) = differ(&golden_acts(name), &shown.acts, &homes(&home)) {
        panic!("{name}: {why}");
    }
    let pictures = rich().join(format!("{name}.pictures.json"));
    if pictures.exists() {
        let want: Vec<[usize; 4]> =
            serde_json::from_str(&fs::read_to_string(pictures).unwrap()).unwrap();
        assert_eq!(
            shown.pictures, want,
            "{name}: picture placements [row, col, cols, rows]"
        );
    }
}
/// One section: the golden's rows for it must appear, in order and unbroken, somewhere in the pane's screen, and the
/// acts on those rows must match at the same offset.
fn check_section(name: &str, section: &str) {
    let (home, shown) = show(name);
    let region = scenario(name)["regions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r[0] == section)
        .unwrap_or_else(|| panic!("{name} has no region {section}"));
    let (first, end) = (
        region[1].as_u64().unwrap() as usize,
        region[2].as_u64().unwrap() as usize,
    );
    let want = fs::read_to_string(rich().join(format!("{name}.txt"))).unwrap();
    let want: Vec<&str> = want.split('\n').collect();
    let block = &want[first..end];
    let at = (0..=shown.text.len().saturating_sub(block.len())).find(|&at| {
        shown.text.len() >= block.len()
            && shown.text[at..at + block.len()]
                .iter()
                .zip(block)
                .all(|(g, w)| g == w)
    });
    let Some(at) = at else {
        let drew = shown
            .text
            .iter()
            .any(|g| strip(g).trim() == strip(block[0]).trim());
        panic!(
            "{name}: section {section} (golden rows {first}..{end}) is {} in the pane's screen; first row {:?}",
            if drew { "drawn differently" } else { "missing" },
            strip(block[0])
        );
    };
    let want_acts: BTreeMap<usize, Value> = golden_acts(name)
        .into_iter()
        .filter(|(row, _)| (first..end).contains(row))
        .map(|(row, act)| (row - first, act))
        .collect();
    let got_acts: BTreeMap<usize, Value> = shown
        .acts
        .iter()
        .filter(|(row, _)| (at..at + block.len()).contains(row))
        .map(|(row, act)| (row - at, act.clone()))
        .collect();
    if let Some(why) = differ(&want_acts, &got_acts, &homes(&home)) {
        panic!("{name}: section {section}: {why}");
    }
}

#[test]
fn goldens_are_complete_and_free_of_machine_paths() {
    let scenarios = manifest()["scenarios"].as_array().unwrap();
    assert!(
        scenarios.len() >= 30,
        "rich/frames.json lists {} scenarios",
        scenarios.len()
    );
    for sc in scenarios {
        let name = sc["name"].as_str().unwrap();
        let text = fs::read_to_string(rich().join(format!("{name}.txt"))).unwrap();
        assert_eq!(
            text.matches('\n').count() as u64,
            sc["rows"].as_u64().unwrap(),
            "{name}: rows"
        );
        let acts = fs::read_to_string(rich().join(format!("{name}.acts.json"))).unwrap();
        serde_json::from_str::<BTreeMap<String, Value>>(&acts).unwrap();
        assert_eq!(
            rich().join(format!("{name}.pictures.json")).exists(),
            sc["pixels"] == true,
            "{name}: pictures file"
        );
        for body in [&text, &acts] {
            for machine in ["/Users/", "/private/", "/var/folders/", "csr-sbx"] {
                assert!(!body.contains(machine), "{name}: golden carries {machine}");
            }
        }
    }
}

#[test]
#[ignore = "packages C+D+E: Flight, compaction card, value row"]
fn frame_main_46x30() {
    check_frame("frame-main-46x30");
}
#[test]
#[ignore = "packages C+D+E: Flight, coach, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x40() {
    check_frame("frame-main-100x40");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, coach, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x70() {
    check_frame("frame-main-100x70");
}
#[test]
#[ignore = "package E: strip and columns"]
fn frame_main_23x30() {
    check_frame("frame-main-23x30");
}
#[test]
#[ignore = "package E: strip and columns"]
fn frame_main_10x30() {
    check_frame("frame-main-10x30");
}
#[test]
#[ignore = "package E: strip and columns"]
fn frame_main_7x30() {
    check_frame("frame-main-7x30");
}
#[test]
#[ignore = "packages C+D+E: Flight, compaction card, value row"]
fn pixels_frame_main_46x30() {
    check_frame("pixels-frame-main-46x30");
}
#[test]
#[ignore = "packages C+D+E: Flight, coach, compaction card, Lost and found / told / last / todos, value row"]
fn pixels_frame_main_100x40() {
    check_frame("pixels-frame-main-100x40");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, coach, compaction card, Lost and found / told / last / todos, value row"]
fn pixels_frame_main_100x70() {
    check_frame("pixels-frame-main-100x70");
}
#[test]
#[ignore = "package E: strip and columns"]
fn pixels_frame_main_23x30() {
    check_frame("pixels-frame-main-23x30");
}
#[test]
#[ignore = "package E: strip and columns"]
fn pixels_frame_main_10x30() {
    check_frame("pixels-frame-main-10x30");
}
#[test]
#[ignore = "package E: strip and columns"]
fn pixels_frame_main_7x30() {
    check_frame("pixels-frame-main-7x30");
}
#[test]
#[ignore = "packages C+D+E: Flight, compaction card, value row, held/tall main"]
fn frame_main_100x40_tall() {
    check_frame("frame-main-100x40-tall");
}
#[test]
#[ignore = "packages C+D+E: coach, compaction card, Lost and found / told / last / todos, value row, held/tall main"]
fn frame_main_100x40_tall_bottom() {
    check_frame("frame-main-100x40-tall-bottom");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, compaction card, value row, held/tall main"]
fn frame_main_46x30_tall() {
    check_frame("frame-main-46x30-tall");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x70_held() {
    check_frame("frame-main-100x70-held");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x70_unheld() {
    check_frame("frame-main-100x70-unheld");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, coach, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x70_fold_passed() {
    check_frame("frame-main-100x70-fold-passed");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, coach, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x70_fold_flight_more() {
    check_frame("frame-main-100x70-fold-flight-more");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, coach, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x70_flight_folded() {
    check_frame("frame-main-100x70-flight-folded");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, coach, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x70_flight_unfolded() {
    check_frame("frame-main-100x70-flight-unfolded");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, coach, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x70_open_compacted() {
    check_frame("frame-main-100x70-open-compacted");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, coach, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x70_open_agent() {
    check_frame("frame-main-100x70-open-agent");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, coach, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x70_open_reading() {
    check_frame("frame-main-100x70-open-reading");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, coach, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x70_open_rule() {
    check_frame("frame-main-100x70-open-rule");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, coach, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x70_open_story() {
    check_frame("frame-main-100x70-open-story");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, coach, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x70_open_recall() {
    check_frame("frame-main-100x70-open-recall");
}
#[test]
#[ignore = "packages C+D+E: coach, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x40_open_lost() {
    check_frame("frame-main-100x40-open-lost");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, coach, compaction card, Lost and found / told / last / todos, Earlier, value row"]
fn frame_main_100x70_earlier() {
    check_frame("frame-main-100x70-earlier");
}
#[test]
#[ignore = "packages B+C+D+E: shift map, Flight, coach, answered, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_100x70_answered() {
    check_frame("frame-main-100x70-answered");
}
#[test]
#[ignore = "packages C+D+E: Flight, Lost and found / told / last / todos, value row"]
fn frame_main_100x70_fresh() {
    check_frame("frame-main-100x70-fresh");
}
#[test]
#[ignore = "packages C+D+E: Flight, Lost and found / told / last / todos, value row"]
fn frame_main_46x30_fresh() {
    check_frame("frame-main-46x30-fresh");
}
#[test]
#[ignore = "packages B+C+D+E: phone, Flight, compaction card, value row"]
fn frame_main_46x30_phone() {
    check_frame("frame-main-46x30-phone");
}
#[test]
#[ignore = "packages B+C+D+E: phone, Flight, coach, compaction card, Lost and found / told / last / todos, value row"]
fn frame_main_38x40_phone() {
    check_frame("frame-main-38x40-phone");
}
#[test]
#[ignore = "packages B+C+D+E: phone zoom, Flight, compaction card, value row"]
fn frame_main_46x30_phone_zoomed() {
    check_frame("frame-main-46x30-phone-zoomed");
}
#[test]
fn section_frame_main_100x70_story() {
    check_section("frame-main-100x70", "story");
}
#[test]
#[ignore = "package C: Flight"]
fn section_frame_main_100x70_flight() {
    check_section("frame-main-100x70", "flight");
}
#[test]
fn section_frame_main_100x70_dreams() {
    check_section("frame-main-100x70", "dreams");
}
#[test]
fn section_frame_main_100x70_rules() {
    check_section("frame-main-100x70", "rules");
}
#[test]
#[ignore = "package D: compaction card"]
fn section_frame_main_100x70_compacted() {
    check_section("frame-main-100x70", "compacted");
}
#[test]
#[ignore = "package C: coach"]
fn section_frame_main_100x70_coach() {
    check_section("frame-main-100x70", "coach");
}
#[test]
fn section_frame_main_100x70_remembered() {
    check_section("frame-main-100x70", "remembered");
}
#[test]
#[ignore = "package D: Lost and found"]
fn section_frame_main_100x70_lost() {
    check_section("frame-main-100x70", "lost");
}
#[test]
#[ignore = "package C: Flight"]
fn section_frame_main_100x70_fresh_flight() {
    check_section("frame-main-100x70-fresh", "flight");
}
#[test]
fn section_frame_main_100x70_fresh_told() {
    check_section("frame-main-100x70-fresh", "told");
}
#[test]
fn section_frame_main_100x70_fresh_rules() {
    check_section("frame-main-100x70-fresh", "rules");
}
#[test]
#[ignore = "package D: last session"]
fn section_frame_main_100x70_fresh_last() {
    check_section("frame-main-100x70-fresh", "last");
}
#[test]
fn section_frame_main_100x70_fresh_todos() {
    check_section("frame-main-100x70-fresh", "todos");
}
#[test]
#[ignore = "package D: Lost and found"]
fn section_frame_main_100x70_fresh_lost() {
    check_section("frame-main-100x70-fresh", "lost");
}
#[test]
#[ignore = "package D: Earlier"]
fn section_frame_main_100x70_earlier_earlier() {
    check_section("frame-main-100x70-earlier", "earlier");
}
#[test]
fn section_frame_main_46x30_story() {
    check_section("frame-main-46x30", "story");
}
#[test]
#[ignore = "package C: Flight"]
fn section_frame_main_46x30_flight() {
    check_section("frame-main-46x30", "flight");
}
#[test]
fn section_frame_main_46x30_dreams() {
    check_section("frame-main-46x30", "dreams");
}
#[test]
fn section_frame_main_46x30_rules() {
    check_section("frame-main-46x30", "rules");
}
#[test]
#[ignore = "package D: compaction card"]
fn section_frame_main_46x30_compacted() {
    check_section("frame-main-46x30", "compacted");
}
