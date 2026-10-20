//! The watch pane's info, summary and threads pages (keys i, s, f; acts view:info, view:summary,
//! view:threads), against pane.py's own frames. The goldens carry clock times, so every test pins
//! TZ to UTC; this file is its own test binary, so that touches no other test.
use csr_engine::companion::{cli::PaneArgs, pane::Pane, paths::Paths};
use std::{fs, path::PathBuf};

const NOW: f64 = 1792320000.;

fn utc() {
    std::env::set_var("TZ", "UTC");
}
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/companion")
}
fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}
fn home() -> (tempfile::TempDir, Paths) {
    let d = tempfile::tempdir().unwrap();
    fn copy(from: &std::path::Path, to: &std::path::Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let e = entry.unwrap();
            if e.file_type().unwrap().is_dir() {
                copy(&e.path(), &to.join(e.file_name()));
            } else {
                let bytes = fs::read(e.path()).unwrap();
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    let text = text.replace("<REPO>", &repo().to_string_lossy());
                    fs::write(to.join(e.file_name()), text).unwrap();
                } else {
                    fs::write(to.join(e.file_name()), &bytes).unwrap();
                }
            }
        }
    }
    copy(&fixture().join("home"), d.path());
    let paths = Paths::new(d.path().into());
    // *.db is gitignored, so the DB is built from home.sql, never copied.
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
    // Pinned input: pane.py counts a chunk whose created_at is NULL as learned, the Rust session
    // read drops it. Real rows always carry created_at, so the fixture's one chunk is given one.
    con.execute(
        "update chunks set created_at=timestamp where id='chunk-now'",
        [],
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
fn args(page: &str, cols: usize, rows: usize) -> PaneArgs {
    args_for(page, cols, rows, "sess-now")
}
fn args_for(page: &str, cols: usize, rows: usize, sid: &str) -> PaneArgs {
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

#[test]
fn a_click_on_the_masthead_opens_the_info_page_and_a_click_on_its_title_goes_back() {
    utc();
    let (_home, paths) = home();
    let mut pane = Pane::new(paths, &args("main", 100, 40));
    pane.pixels = false;
    pane.frame(100, 40, NOW);
    assert_eq!(pane.acts[&0]["key"], "view:info");
    assert!(pane.click(0, 5, NOW));
    assert_eq!(pane.view, "info");
    pane.frame(100, 40, NOW);
    assert_eq!(pane.acts[&0]["key"], "view:main");
    assert!(pane.click(0, 5, NOW));
    assert_eq!(pane.view, "main");
}

const SIZES: [(usize, usize); 2] = [(46, 30), (100, 40)];
const PAGES: [&str; 3] = ["info", "summary", "threads"];

fn golden(name: &str) -> String {
    fs::read_to_string(fixture().join(name)).unwrap_or_else(|_| panic!("missing golden {name}"))
}
/// One frame of `page` for `sid`, with a thread opened when `open` is given.
fn draw(
    page: &str,
    sid: &str,
    (cols, rows): (usize, usize),
    pixels: bool,
    open: Option<&str>,
) -> (Pane, Vec<String>, tempfile::TempDir) {
    utc();
    let (home, paths) = home();
    let mut pane = Pane::new(paths, &args_for(page, cols, rows, sid));
    pane.pixels = pixels;
    if let Some(key) = open {
        pane.open = serde_json::json!(key);
    }
    let lines = pane.frame(cols, rows, NOW);
    (pane, lines, home)
}
fn acts_of(pane: &Pane) -> Vec<(usize, serde_json::Value)> {
    pane.acts.iter().map(|(r, a)| (*r, a.clone())).collect()
}
fn acts_golden(name: &str) -> Vec<(usize, serde_json::Value)> {
    golden(name)
        .lines()
        .map(|line| {
            let (row, json) = line.split_once('\t').unwrap();
            (row.parse().unwrap(), serde_json::from_str(json).unwrap())
        })
        .collect()
}
fn assert_same(actual: &[String], expected: &str, what: &str) {
    let actual = actual.join("\n") + "\n";
    assert_eq!(actual.lines().count(), expected.lines().count(), "{what}");
    for (i, (a, b)) in actual.lines().zip(expected.lines()).enumerate() {
        assert_eq!(a, b, "{what}, row {}", i + 1);
    }
    assert_eq!(actual, expected, "{what}");
}

#[test]
fn text_frames_are_byte_equal_to_pane_py() {
    for page in PAGES {
        for (tag, sid) in [("", "sess-now"), ("lost-", "sess-lost")] {
            for size in SIZES {
                let name = format!("{page}-{tag}{}x{}", size.0, size.1);
                let (pane, lines, _home) = draw(page, sid, size, false, None);
                assert_same(&lines, &golden(&format!("{name}.txt")), &name);
                assert_eq!(
                    acts_of(&pane),
                    acts_golden(&format!("{name}.acts.txt")),
                    "{name} acts"
                );
            }
        }
    }
}

#[test]
fn pixel_frames_are_byte_equal_to_pane_py_and_place_the_same_pictures() {
    for page in PAGES {
        for (tag, sid) in [("", "sess-now"), ("lost-", "sess-lost")] {
            for size in SIZES {
                let name = format!("pixels-{page}-{tag}{}x{}", size.0, size.1);
                let (pane, lines, _home) = draw(page, sid, size, true, None);
                assert_same(&lines, &golden(&format!("{name}.txt")), &name);
                assert_eq!(
                    acts_of(&pane),
                    acts_golden(&format!("{name}.acts.txt")),
                    "{name} acts"
                );
                let mut placed: Vec<String> = pane
                    .placed
                    .iter()
                    .map(|p| format!("{} {} {} {}", p.row, p.col, p.cols, p.rows))
                    .collect();
                let mut wanted: Vec<String> = golden(&format!("{name}.pictures.txt"))
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(str::to_owned)
                    .collect();
                placed.sort();
                wanted.sort();
                assert_eq!(placed, wanted, "{name} pictures");
            }
        }
    }
}

/// The key of the first thread the lost session has, as the threads page's acts carry it.
fn first_thread(size: (usize, usize)) -> String {
    let (pane, _, _home) = draw("threads", "sess-lost", size, false, None);
    pane.acts
        .values()
        .find_map(|a| a["key"].as_str().filter(|k| k.starts_with("lost:")))
        .expect("the lost session has a thread")
        .to_owned()
}

#[test]
fn an_opened_thread_shows_its_answers_as_pane_py_draws_them() {
    for pixels in [false, true] {
        for size in SIZES {
            let key = first_thread(size);
            let name = format!(
                "{}threads-open-lost-{}x{}",
                if pixels { "pixels-" } else { "" },
                size.0,
                size.1
            );
            let (pane, lines, _home) = draw("threads", "sess-lost", size, pixels, Some(&key));
            assert_same(&lines, &golden(&format!("{name}.txt")), &name);
            assert_eq!(
                acts_of(&pane),
                acts_golden(&format!("{name}.acts.txt")),
                "{name} acts"
            );
        }
    }
}

fn plain_rows(lines: &[String]) -> Vec<String> {
    let escape = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
    lines
        .iter()
        .map(|l| escape.replace_all(l, "").into_owned())
        .collect()
}
fn pane_on(page: &str, sid: &str, size: (usize, usize)) -> (Pane, tempfile::TempDir) {
    utc();
    let (home, paths) = home();
    let mut pane = Pane::new(paths, &args_for(page, size.0, size.1, sid));
    pane.pixels = false;
    (pane, home)
}

#[test]
fn s_f_and_i_open_their_pages_and_the_same_key_or_escape_goes_back() {
    for (key, view) in [("s", "summary"), ("f", "threads"), ("i", "info")] {
        let (mut pane, _home) = pane_on("main", "sess-now", (100, 40));
        assert!(pane.key(key));
        assert_eq!(pane.view, view);
        assert!(pane.key(key));
        assert_eq!(pane.view, "main", "{key} again");
        assert!(pane.key(key));
        assert!(pane.key("escape"));
        assert_eq!(pane.view, "main", "escape from {view}");
        assert!(pane.key(key));
        assert!(pane.key("d"));
        assert_eq!(pane.view, "main", "d from {view}");
    }
    let (mut pane, _home) = pane_on("main", "sess-now", (100, 40));
    pane.key("s");
    pane.key("i");
    assert_eq!(pane.view, "info", "i from the summary page opens info");
    assert!(!pane.key("q"));
}

#[test]
fn the_legend_of_these_pages_is_back_scroll_close_with_no_change_marks() {
    for page in PAGES {
        let (_pane, lines, _home) = draw(page, "sess-now", (100, 40), false, None);
        let last = plain_rows(&lines).pop().unwrap();
        assert_eq!(last, " d back   j/k scroll   q close", "{page}");
        assert!(!last.contains("changed"));
        let (pane, ..) = draw(page, "sess-now", (100, 40), false, None);
        let spans: Vec<String> = pane.acts[&39]["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s[2].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(spans, ["d", "j/k", "q"], "{page}");
    }
}

#[test]
fn a_click_on_a_legend_label_opens_that_page() {
    for (key, view) in [("s", "summary"), ("f", "threads"), ("i", "info")] {
        let (mut pane, _home) = pane_on("main", "sess-now", (100, 40));
        pane.frame(100, 40, NOW);
        let span = pane.acts[&39]["keys"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s[2] == key)
            .unwrap_or_else(|| panic!("no {key} on the main legend"))
            .clone();
        assert!(pane.click(39, span[0].as_u64().unwrap() as usize, NOW));
        assert_eq!(pane.view, view, "{key}");
        pane.frame(100, 40, NOW);
        let back = pane.acts[&39]["keys"][0].clone();
        assert_eq!(back[2], "d");
        assert!(pane.click(39, back[0].as_u64().unwrap() as usize, NOW));
        assert_eq!(pane.view, "main");
    }
}

#[test]
fn the_view_acts_open_summary_threads_and_info() {
    for view in ["summary", "threads", "info"] {
        let (mut pane, _home) = pane_on("main", "sess-now", (100, 40));
        pane.frame(100, 40, NOW);
        pane.acts
            .insert(3, serde_json::json!({"key": format!("view:{view}")}));
        assert!(pane.click(3, 4, NOW));
        assert_eq!(pane.view, view);
    }
}

#[test]
fn only_the_first_two_masthead_rows_open_the_info_page() {
    let (mut pane, _home) = pane_on("main", "sess-now", (100, 40));
    pane.frame(100, 40, NOW);
    let rows: Vec<usize> = pane
        .acts
        .iter()
        .filter(|(_, a)| a["key"] == "view:info")
        .map(|(r, _)| *r)
        .collect();
    assert_eq!(rows, [0, 1]);
}

#[test]
fn the_info_page_marks_its_title_and_instruments_as_back() {
    let (pane, ..) = draw("info", "sess-now", (100, 40), false, None);
    let back: Vec<usize> = pane
        .acts
        .iter()
        .filter(|(_, a)| a["key"] == "view:main")
        .map(|(r, _)| *r)
        .collect();
    assert_eq!(back, (0..10).collect::<Vec<_>>());
}

#[test]
fn j_k_and_space_scroll_the_summary_and_never_past_its_ends() {
    let (mut pane, _home) = pane_on("summary", "sess-lost", (46, 5));
    let facts = |pane: &mut Pane| plain_rows(&pane.frame(46, 5, NOW))[1].clone();
    assert!(facts(&mut pane).contains("lines 1-2 of 4"));
    pane.key("j");
    assert!(facts(&mut pane).contains("lines 3-4 of 4"));
    pane.key("j");
    pane.key(" ");
    assert!(
        facts(&mut pane).contains("lines 3-4 of 4"),
        "clamped at the end"
    );
    pane.key("k");
    assert!(facts(&mut pane).contains("lines 1-2 of 4"));
    pane.key("k");
    assert!(
        facts(&mut pane).contains("lines 1-2 of 4"),
        "clamped at the start"
    );
}

#[test]
fn j_k_scroll_the_threads_and_info_pages() {
    for (page, rows) in [("threads", 8), ("info", 12)] {
        let (mut pane, _home) = pane_on(page, "sess-lost", (46, rows));
        let first = plain_rows(&pane.frame(46, rows, NOW));
        pane.key("j");
        let down = plain_rows(&pane.frame(46, rows, NOW));
        assert_ne!(first, down, "{page} moved down");
        assert_eq!(down.last(), first.last(), "the legend stays");
        pane.key("k");
        assert_eq!(
            plain_rows(&pane.frame(46, rows, NOW)),
            first,
            "{page} back up"
        );
        pane.key("k");
        assert_eq!(
            plain_rows(&pane.frame(46, rows, NOW)),
            first,
            "{page} stays at top"
        );
    }
}

#[test]
fn a_click_on_a_thread_opens_it_and_drop_sets_it_aside_in_the_log() {
    let (mut pane, home) = pane_on("threads", "sess-lost", (46, 30));
    pane.frame(46, 30, NOW);
    let row = *pane
        .acts
        .iter()
        .find(|(_, a)| a["lost"].is_string() && a["spans"].is_null())
        .unwrap()
        .0;
    let key = pane.acts[&row]["key"].clone();
    assert!(pane.click(row, 8, NOW));
    assert_eq!(pane.open, key);
    let shown = plain_rows(&pane.frame(46, 30, NOW)).join("\n");
    assert!(
        shown.contains("→ side task") && shown.contains("✗ drop"),
        "{shown}"
    );
    let (drop_row, drop_col) = pane
        .acts
        .iter()
        .filter(|(_, a)| a["spans"].is_array())
        .find_map(|(r, a)| {
            a["spans"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s[2] == "drop")
                .map(|s| (*r, s[0].as_u64().unwrap() as usize))
        })
        .expect("a drop answer");
    assert!(pane.click(drop_row, drop_col, NOW));
    let log = fs::read_to_string(
        home.path()
            .join(".claude-self-reflect/csr-companion/lost-found.jsonl"),
    )
    .expect("drop is kept in the lost-found log");
    let kept: serde_json::Value = serde_json::from_str(log.lines().last().unwrap()).unwrap();
    assert_eq!(kept["answer"], "drop");
    assert_eq!(
        kept["key"],
        key.as_str().unwrap().trim_start_matches("lost:")
    );
    assert_eq!(kept["at"], NOW);
    let after = plain_rows(&pane.frame(46, 30, NOW)).join("\n");
    assert!(after.contains("Dropped"), "{after}");
}

#[test]
fn a_click_between_the_answers_changes_nothing() {
    let (mut pane, home) = pane_on("threads", "sess-lost", (46, 30));
    pane.frame(46, 30, NOW);
    let key = pane
        .acts
        .values()
        .find_map(|a| a["key"].as_str().filter(|k| k.starts_with("lost:")))
        .unwrap()
        .to_owned();
    pane.open = serde_json::json!(key);
    pane.frame(46, 30, NOW);
    let (row, end) = pane
        .acts
        .iter()
        .find_map(|(r, a)| {
            a["spans"]
                .as_array()
                .map(|s| (*r, s.last().unwrap()[1].as_u64().unwrap() as usize))
        })
        .unwrap();
    assert!(pane.click(row, end + 1, NOW));
    assert!(!home
        .path()
        .join(".claude-self-reflect/csr-companion/lost-found.jsonl")
        .exists());
}

#[test]
fn a_subagent_handoff_is_kept_as_a_draft_with_its_envelope() {
    let (_pane, home) = pane_on("threads", "sess-lost", (46, 30));
    let paths = Paths::new(home.path().into());
    let sid = "11111111-1111-4111-8111-111111111111";
    let (request, envelope) =
        csr_engine::companion::threads::new_handoff(&paths, sid, "2b67b51d8d4a").unwrap();
    assert!(
        request.starts_with("req-") && request.len() == 20,
        "{request}"
    );
    assert!(envelope.contains(&request) && envelope.contains("2b67b51d8d4a"));
    let drafts = fs::read_to_string(
        paths
            .keep
            .join("handoffs")
            .join(format!("{sid}.drafts.jsonl")),
    )
    .unwrap();
    let draft: serde_json::Value = serde_json::from_str(drafts.lines().next().unwrap()).unwrap();
    assert_eq!(draft["request"], request.as_str());
    assert_eq!(draft["thread"], "2b67b51d8d4a");
    let (other, _) =
        csr_engine::companion::threads::new_handoff(&paths, sid, "2b67b51d8d4a").unwrap();
    assert_ne!(other, request, "a retry is always a new request");
    assert!(csr_engine::companion::threads::new_handoff(&paths, sid, "not a thread").is_none());
    assert!(csr_engine::companion::threads::new_handoff(&paths, "", "2b67b51d8d4a").is_none());
}

#[test]
fn the_cli_draws_the_three_pages() {
    utc();
    let (home, paths) = home();
    for page in PAGES {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_csr-engine"))
            .env("HOME", &paths.home)
            .args([
                "companion",
                "pane",
                "--repo",
                repo().to_str().unwrap(),
                "--frame",
                "--page",
                page,
                "--sid",
                "sess-now",
                "--cols",
                "46",
                "--rows",
                "30",
                "--now",
                "1792320000",
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{page}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = plain_rows(&[String::from_utf8(out.stdout).unwrap()]).concat();
        assert!(
            text.contains("d back   j/k scroll   q close"),
            "{page}: {text}"
        );
    }
    drop(home);
}
