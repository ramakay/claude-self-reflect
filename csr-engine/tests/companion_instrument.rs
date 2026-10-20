//! The instrument states, through the real top frame and a rich private HOME.
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
    let d = tempfile::tempdir_in(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target")).unwrap();
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

use csr_engine::companion::render::{text, theme::Theme};
use serde_json::json;
use sha2::{Digest, Sha256};
const NOW: f64 = 1792320000.;
const STATES: [&str; 9] = [
    "01-empty",
    "02-running",
    "03-agents",
    "04-band",
    "05-compacting",
    "06-after",
    "07-away",
    "08-offer",
    "08b-nothing",
];
const QUESTION:&str="What if one of Fable's three prototypes made the what-if question the star of the pane, as you wanted on 10-09?";
fn write_json(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value.to_string()).unwrap();
}
fn state_home(state: &str) -> (tempfile::TempDir, Paths) {
    let (home, paths) = rich_home();
    let empty = state == "01-empty";
    let con = rusqlite::Connection::open(&paths.db).unwrap();
    // These are the existing learnt feed schemas, absent from the old minimal oracle DB.
    con.execute_batch("ALTER TABLE memory_registry ADD COLUMN file_path TEXT;
        ALTER TABLE memory_registry ADD COLUMN modified_ts TEXT;
        ALTER TABLE witness_ledger ADD COLUMN project TEXT;
        UPDATE witness_ledger SET project='pane-wp2';
        DELETE FROM witness_verdicts WHERE id=2;
        CREATE INDEX idx_witness_verdicts_recap_created ON witness_verdicts(julianday(created_at),id);
        CREATE TABLE provenance_events(event_id TEXT PRIMARY KEY,conversation_id TEXT);
        CREATE TABLE resolution_ledger(id INTEGER PRIMARY KEY,chunk_id TEXT,claim TEXT,evidence TEXT,status TEXT,source TEXT,event_id TEXT);").unwrap();
    if !empty {
        con.execute_batch("DELETE FROM memory_registry;
            INSERT INTO memory_registry VALUES('sess-now','pane-layout','/notes/layout','2026-10-18T10:35:00Z');
            INSERT INTO memory_registry VALUES('sess-now','batch-gate','/notes/batch','2026-10-18T10:36:00Z');
            INSERT INTO memory_registry VALUES('sess-now','figure-spacing','/notes/figure','2026-10-18T10:37:00Z');
            INSERT INTO provenance_events VALUES('confirmed-1','sess-now'),('confirmed-2','sess-now');
            INSERT INTO resolution_ledger VALUES(1,'finding-1','Budget is labelled','oid:8e4e3f27','resolved','user_confirmed','confirmed-1');
            INSERT INTO resolution_ledger VALUES(2,'finding-2','Question fits','oid:8e4e3f27','resolved','user_confirmed','confirmed-2');").unwrap();
        let raw: String = con
            .query_row(
                "SELECT value FROM meta WHERE key='story_map:sess-now'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let mut map: Value = serde_json::from_str(&raw).unwrap();
        map["current"]["entries"].as_array_mut().unwrap().extend([
            json!({"kind":"added","claim":"Label the budget","before":[],"now":"The dream budget has a label.","was":"The budget had no label.","after":[{"scope":"render/instrument.rs","sym":"meta","mark":"+"}],"receipts":["oid:7625eed7"]}),
            json!({"kind":"risk","claim":"Check section readability","before":[],"now":"Check readable section text at every height.","was":"Sections used tiny amber body text.","after":[{"scope":"render/instrument.rs","sym":"draw","mark":"!"}],"receipts":[],"grounded":false})
        ]);
        con.execute(
            "UPDATE meta SET value=? WHERE key='story_map:sess-now'",
            [map.to_string()],
        )
        .unwrap();
    }
    if state == "06-after" {
        con.execute(
            "UPDATE reflections SET timestamp='2026-10-18T10:39:00Z' WHERE id='story'",
            [],
        )
        .unwrap();
    }
    if empty {
        con.execute("delete from witness_verdicts", []).unwrap();
        con.execute("delete from reflections where id='story'", [])
            .unwrap();
        con.execute("delete from meta where key like 'story_map:%'", [])
            .unwrap();
    }
    let ctx = match state {
        "01-empty" => 0,
        "04-band" | "05-compacting" => 189000,
        "06-after" => 65000,
        _ => 192000,
    };
    write_json(
        &paths.keep.join("usage/sess-now.json"),
        &json!({"at":NOW,"context":{"tokens":ctx,"window":1000000},
        "rateLimits":[{"kind":"five_hour","percentUsed":if empty{3}else if state=="07-away"{0}else{2},"resetsAt":"2026-10-18T14:08:00Z"},
        {"kind":"seven_day","percentUsed":if state=="07-away"{54}else{52},"resetsAt":"2026-10-22T09:00:00Z"}]}),
    );
    // The transcript, usage status, dream log, agent feed and database are the real frame's inputs.
    let values = [
        61, 84, 103, 121, 140, 158, 177, 195, 214, 231, 248, 266, 65, 88, 106, 121, 137, 150, 163,
        172, 180, 186, 189, 192,
    ];
    let mut lines = vec![];
    if !empty {
        for (i, value) in values.iter().enumerate() {
            let value = if state == "04-band" && i == 12 {
                &84
            } else {
                value
            };
            let at =
                NOW - (values.len() - i) as f64 * 30. - if state == "07-away" { 8040. } else { 0. };
            let stamp = chrono::DateTime::from_timestamp(at as i64, 0)
                .unwrap()
                .to_rfc3339();
            if i == 12 {
                lines.push(json!({"type":"system","subtype":"compact_boundary","timestamp":stamp,"compactMetadata":{"preTokens":if state=="06-after"{265400}else{266000},"postTokens":21700,"durationMs":45000,"trigger":"manual"}}));
            }
            if state != "04-band" || i % 2 == 0 {
                lines.push(json!({"type":"user","timestamp":stamp,"message":{"role":"user","content":"Keep the question first."}}));
            }
            lines.push(json!({"type":"assistant","timestamp":stamp,"message":{"id":format!("b-{i}"),"role":"assistant","model":"claude-sonnet-5","stop_reason":"end_turn","usage":{"input_tokens":value*if i==12{550}else{10},"cache_read_input_tokens":value*if i==12{450}else{990},"cache_creation":{"ephemeral_1h_input_tokens":value*990},"output_tokens":400},"content":[{"type":"text","text":"Working on the pane."}]}}));
            if state == "04-band" {
                let last = lines.last().unwrap().clone();
                for call in 1..15 {
                    let mut request = last.clone();
                    request["message"]["id"] = json!(format!("band-{i}-{call}"));
                    lines.push(request);
                }
            }
        }
    }
    if state == "06-after" {
        // 38 post-compact requests repay the first compact; the second has only one so far.
        let last = lines.last().unwrap().clone();
        for call in 0..27 {
            let mut request = last.clone();
            request["message"]["id"] = json!(format!("paid-{call}"));
            lines.push(request);
        }
        let stamp = chrono::DateTime::from_timestamp((NOW - 6.) as i64, 0)
            .unwrap()
            .to_rfc3339();
        lines.push(json!({"type":"system","subtype":"compact_boundary","timestamp":stamp,"compactMetadata":{"preTokens":192000,"postTokens":21700,"durationMs":45000,"trigger":"manual"}}));
        let stamp = chrono::DateTime::from_timestamp((NOW - 4.) as i64, 0)
            .unwrap()
            .to_rfc3339();
        lines.push(json!({"type":"assistant","timestamp":stamp,"message":{"id":"after-b","role":"assistant","stop_reason":"end_turn","model":"claude-sonnet-5","usage":{"input_tokens":650,"cache_read_input_tokens":64350,"cache_creation":{"ephemeral_1h_input_tokens":64350}},"content":[{"type":"text","text":"Compacted and carried the open question."}]}}));
    }
    if state == "07-away" {
        lines.push(json!({"type":"user","timestamp":chrono::DateTime::from_timestamp((NOW-1.) as i64,0).unwrap().to_rfc3339(),"message":{"role":"user","content":"Back to the pane."}}));
    }
    fs::write(
        paths.projects.join("fixture/sess-now.jsonl"),
        lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    let mut log = vec![];
    if empty {
        log.push(json!({"kind":"look","session":"sess-now","at":NOW,"outcome":"idle","budget":{"spent":0,"cap":40000}}));
    }
    if !empty {
        log.push(json!({"kind":"compact","event":"compact","outcome":"compacted","session":"sess-now","at":NOW-359.,"ctx_before":if state=="06-after"{265400}else{266000},"ctx_after":21700,"took_ms":45000}));
        if state == "06-after" {
            log.push(json!({"kind":"compact","event":"compact","outcome":"compacted","session":"sess-now","at":NOW-5.,"ctx_before":192000,"ctx_after":21700,"took_ms":45000}));
        }
        for i in 0..4 {
            log.push(
                json!({"kind":"look","session":"sess-now","at":NOW-600.-i as f64,"outcome":"none"}),
            );
        }
        if state == "08-offer" {
            log.push(json!({"kind":"batch","session":"sess-now","at":NOW-700.,"dreams":[
                {"id":"prev-one","line":"What if the budget had a label?","title":"Label the dream budget","confidence":0.8},
                {"id":"prev-two","line":"What if the figure opened in place?","title":"Keep the figure in place","confidence":0.7}]}));
            log.push(json!({"kind":"answer","id":"prev-one","answer":"helpful","at":NOW-650.}));
            log.push(
                json!({"kind":"answer","id":"prev-two","answer":"thought_of_it","at":NOW-640.}),
            );
            log.push(json!({"kind":"looking","session":"sess-now","at":NOW-38.,"outcome":"dreaming","budget":{"spent":9000,"cap":40000},"threads":["pane redesign","launch images","release landing"]}));
        } else if state == "08b-nothing" {
            log.push(json!({"kind":"look","session":"sess-now","at":NOW-30.,"outcome":"none","budget":{"spent":14000,"cap":40000}}));
        } else {
            log.push(json!({"kind":"batch","at":NOW-300.,"session":"sess-now","budget":{"spent":14000,"cap":40000},"threads":["pane redesign","launch images","release landing"],"dreams":[
            {"id":"b-top","line":QUESTION,"title":"The what-if could lead the pane","text":"You asked to make the question the star [session 6ca2a4d5].","confidence":0.82,"receipt":"⌗6ca2a4d5 10-09","thread":"pane redesign",
            "confusing_claude":[{"line":"Misread which pane should change.","evidence":"Turn 4: 'the companion pane'"}],
            "should_be":[{"kind":"hook","line":"Check the card height after each layout edit.","evidence":"Turns 4 and 8: 'at most 30%'"}]},
            {"id":"b-next","line":"What if the pane's card export made the launch gallery shots straight from live sessions, instead of hand-made mockups?","confidence":0.74,"receipt":"⌗eda7fbcf 10-15","thread":"launch images"},
            {"id":"b-third","line":"What if we confirmed the two unlanded artifacts before any new dreams-gateway phase?","confidence":0.66,"receipt":"⌗3e1c3f4c","thread":"release landing"}]}));
            if state == "05-compacting" || state == "06-after" {
                log.push(json!({"kind":"answer","id":"b-top","answer":"carried","at":NOW-10.}));
            }
        }
    }
    fs::write(
        paths.keep.join("dreams.jsonl"),
        log.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    if state == "03-agents" {
        let file = paths.keep.join("agents/sess-now.json");
        let mut feed: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        for i in 0..3 {
            feed["agents"][i]["status"] = json!("running");
            let age = match i {
                0 => 758. * 60.,
                1 => 914. * 60.,
                _ => 4. * 60.,
            };
            feed["agents"][i]["started"] = json!((NOW - age) * 1000.);
            feed["agents"][i]["first_seen"] = json!((NOW - age) * 1000.);
            feed["agents"][i]["last_at"] = json!((NOW - if i < 2 { 44000. } else { 20. }) * 1000.);
        }
        write_json(&file, &feed);
    }
    if state != "03-agents" {
        let _ = fs::remove_file(paths.keep.join("agents/sess-now.json"));
    }
    if state == "05-compacting" {
        for entry in fs::read_dir(paths.projects.join("fixture")).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().unwrap() != "sess-now.jsonl" {
                fs::remove_file(path).unwrap();
            }
        }
        let own = (0..3).map(|i| json!({"type":"system","subtype":"compact_boundary","timestamp":chrono::DateTime::from_timestamp((NOW-5000.-i as f64*100.) as i64,0).unwrap().to_rfc3339(),"compactMetadata":{"preTokens":190000,"postTokens":21700,"durationMs":45000,"trigger":"manual"}}).to_string()).collect::<Vec<_>>().join("\n");
        fs::write(
            paths.projects.join("fixture/own-compacts.jsonl"),
            own + "\n",
        )
        .unwrap();
        write_json(
            &paths.keep.join("compacting/sess-now.json"),
            &json!({"at":(NOW-12.)*1000.,"trigger":"auto"}),
        );
    }
    (home, paths)
}
fn snapshot(state: &str, theme: Theme, cols: usize, rows: usize) -> (Value, Vec<u8>) {
    let (_home, paths) = state_home(state);
    let mut pane = Pane::new(paths.clone(), &args("main", "sess-now", cols, rows));
    pane.theme = theme;
    pane.pixels = false;
    if state == "03-agents" {
        pane.key("i");
    }
    let text = pane
        .frame(cols, rows, NOW)
        .iter()
        .map(|s| text::plain(s))
        .collect::<Vec<_>>();
    assert!(!text.join("\n").contains("Before:"));
    let mut dir = fixture().join("instrument/text");
    if (cols, rows) != (76, 67) {
        dir = dir.join(format!("{cols}x{rows}"));
    }
    let path = dir.join(format!("{state}.txt"));
    let plain = text
        .iter()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    if std::env::var_os("CSR_INSTRUMENT_UPDATE").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &plain).unwrap();
    }
    assert_eq!(fs::read_to_string(path).unwrap(), plain);
    assert!(!plain.contains("STS"));
    assert!(!plain.contains("Should be a …"));
    assert!(!plain.contains("d dream"));
    pane.pixels = true;
    pane.enable_cards();
    pane.frame(cols, rows, NOW);
    assert_eq!(
        pane.placed.len(),
        1,
        "{state} did not draw the actual main frame"
    );
    let metrics = pane.instrument_metrics.as_ref().unwrap();
    assert!(
        metrics.dream / metrics.height <= 0.30,
        "{state}: {}",
        metrics.dream / metrics.height
    );
    if !matches!(state, "01-empty" | "08-offer" | "08b-nothing") {
        assert_eq!(
            metrics.question_lines.join(" "),
            QUESTION,
            "question was truncated"
        );
        assert!(metrics.question_lines.len() <= 3);
    }
    let png = pane.placed[0].png.to_vec();
    let hash = Sha256::digest(&png)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    println!(
        "{theme:?}/{state}: dream {:.2}%",
        100. * metrics.dream / metrics.height
    );
    (
        json!({"text":text,"png_sha256":hash,"acts":pane.acts,"dream_percent":format!("{:.2}",100.*metrics.dream/metrics.height)}),
        png,
    )
}
#[test]
fn all_nine_states_and_both_themes_have_honest_top_frame_goldens() {
    for (cols, rows) in [(76, 67), (44, 30)] {
        for theme in [Theme::Dark, Theme::Light] {
            for state in STATES {
                let (value, png) = snapshot(state, theme, cols, rows);
                let name = if theme == Theme::Dark {
                    "dark"
                } else {
                    "light"
                };
                let mut dir = fixture().join("instrument");
                if (cols, rows) != (76, 67) {
                    dir = dir.join(format!("{cols}x{rows}"));
                }
                let golden = dir.join(name).join(format!("{state}.json"));
                if std::env::var_os("CSR_INSTRUMENT_UPDATE").is_some() {
                    write_json(&golden, &value);
                }
                let expected: Value = serde_json::from_slice(&fs::read(&golden).unwrap()).unwrap();
                assert_eq!(value, expected, "{name}/{state}");
                if let Some(dir) = std::env::var_os("CSR_INSTRUMENT_PNG") {
                    let dir = PathBuf::from(dir).join(format!("{cols}x{rows}")).join(name);
                    fs::create_dir_all(&dir).unwrap();
                    fs::write(dir.join(format!("{state}.png")), png).unwrap();
                }
            }
        }
    }
}
#[test]
fn live_keys_keep_one_surface_and_switch_theme_without_restart() {
    let (_home, paths) = state_home("02-running");
    let mut pane = Pane::new(paths, &args("main", "sess-now", 76, 67));
    let first = pane.theme;
    pane.key("b");
    assert_ne!(pane.theme, first);
    for key in ["i", "l", "t", "d"] {
        pane.key(key);
        assert_eq!(pane.view, "main");
    }
    pane.key("l");
    pane.key("t");
    assert!(
        pane.instrument_open.threads
            && !pane.instrument_open.changes
            && !pane.instrument_open.dreams
    );
    pane.key("m");
    assert!(pane.instrument_open.changes && !pane.instrument_open.threads);
    pane.key("escape");
    assert!(
        !pane.instrument_open.figure
            && !pane.instrument_open.threads
            && !pane.instrument_open.changes
    );
}

#[test]
fn real_pixel_answer_promotes_the_next_dream_and_live_theme_repaints() {
    let (_home, paths) = state_home("02-running");
    let mut pane = Pane::new(paths, &args("main", "sess-now", 76, 67));
    pane.pixels = true;
    pane.enable_cards();
    pane.frame(76, 67, NOW);
    let first = pane.placed[0].png.clone();
    pane.key("b");
    pane.frame(76, 67, NOW);
    assert_ne!(first, pane.placed[0].png);
    pane.key("1");
    pane.frame(76, 67, NOW + 1.);
    let question = pane
        .instrument_metrics
        .as_ref()
        .unwrap()
        .question_lines
        .join(" ");
    assert!(question.contains("launch gallery"), "{question}");
    for key in ["l", "t", "d"] {
        pane.key(key);
        pane.frame(76, 67, NOW + 2.);
        assert_eq!(pane.view, "main");
        let metrics = pane.instrument_metrics.as_ref().unwrap();
        assert!(metrics.dream / metrics.height <= 0.3);
        pane.key("j");
        pane.frame(76, 67, NOW + 3.);
        pane.key("escape");
    }
}
#[test]
fn text_fallback_rows_fit_narrow_terminals() {
    for cols in [10, 23, 30, 40, 44, 45, 46, 76] {
        let (_home, paths) = state_home("02-running");
        let mut pane = Pane::new(paths, &args("main", "sess-now", cols, 30));
        pane.pixels = false;
        for row in pane.frame(cols, 30, NOW) {
            assert!(text::len(&text::plain(&row)) <= cols, "{cols}: {row:?}");
        }
    }
}

#[test]
fn theme_key_keeps_a_visible_click_target_even_when_it_is_the_only_key() {
    let (_home, paths) = state_home("02-running");
    let mut pane = Pane::new(paths, &args("main", "sess-now", 44, 30));
    pane.pixels = false;
    for cols in [1, 2, 4, 5, 7, 10, 23, 39, 40, 44, 45] {
        let lines = pane.frame(cols, 30, NOW);
        let footer = text::plain(&lines[29]);
        assert!(
            footer.contains('b') && text::len(&footer) <= cols,
            "{cols}: {footer}"
        );
        let keys = pane.acts[&29]["keys"].as_array().unwrap();
        assert!(keys.iter().any(|span| span[2] == "b"), "{cols}: {keys:?}");
    }
}

#[test]
fn live_context_without_transcript_or_open_dream_is_not_an_empty_session() {
    let shape: Value = serde_json::from_str(include_str!(
        "fixtures/companion/instrument/live-passed.json"
    ))
    .unwrap();
    let (_home, paths) = state_home("02-running");
    write_json(&paths.keep.join("usage/sess-now.json"), &shape["usage"]);
    fs::write(paths.projects.join("fixture/sess-now.jsonl"), "").unwrap();
    fs::write(
        paths.keep.join("dreams.jsonl"),
        shape["dream_records"]
            .as_array()
            .unwrap()
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    for theme in [Theme::Dark, Theme::Light] {
        for cols in [30, 39, 40, 41, 42, 43, 44, 45, 60] {
            let mut pane = Pane::new(paths.clone(), &args("main", "sess-now", cols, 30));
            pane.theme = theme;
            pane.pixels = false;
            let lines: Vec<_> = pane
                .frame(cols, 30, NOW)
                .iter()
                .map(|r| text::plain(r))
                .collect();
            let shown = lines.join("\n");
            assert!(shown.contains("CTX 140k/1M"), "{shown}");
            assert!(shown.contains("The last look found nothing."), "{shown}");
            assert!(shown.contains("Story ·"), "{shown}");
            assert!(!shown.contains("Ask about past work.") && !shown.contains("Code changed?"));
            assert!(!shown.contains("→ passed") && !shown.contains("Launch images"));
            assert!(lines.iter().all(|line| text::len(line) <= cols));
            let last_content = lines[..29]
                .iter()
                .rposition(|line| !line.trim().is_empty())
                .unwrap();
            assert!(28 - last_content <= 3, "{cols}: {shown}");
            let footer = &lines[29];
            assert!(
                footer.contains("b thm") || footer.contains("b theme"),
                "{footer}"
            );
            pane.pixels = true;
            pane.enable_cards();
            pane.frame(cols, 30, NOW);
            assert_eq!(pane.placed.len(), 1, "{cols}");
            if cols >= 40 {
                let metrics = pane.instrument_metrics.as_ref().expect("whole Instrument");
                assert!(metrics.dream / metrics.height <= 0.3);
                assert!(metrics.story_region.is_some());
                assert_eq!(pane.placed[0].row, 0);
                assert!(pane.placed[0].rows <= 29);
            } else {
                assert!(pane.instrument_metrics.is_none());
                assert_eq!(pane.placed[0].row + pane.placed[0].rows, 29);
            }
        }
    }
}

#[test]
fn turns_alone_are_active_and_the_newest_open_what_if_wins() {
    use csr_engine::companion::render::instrument::{Batch, Composition};
    let session = json!({"id":"sess-now","prompts":[NOW-10.]});
    let mut data = json!({"look":{"session":"sess-now","outcome":"quiet"}});
    let composition = Composition::build(&data, &session, NOW);
    assert!(!composition.empty_session);
    assert_eq!(composition.dream.state, Batch::Nothing);
    data["dreams"] = json!([
        {"id":"older","session":"sess-now","at":NOW-100.,"line":"Older high confidence?","confidence":0.99,"answer":null},
        {"id":"newest","session":"sess-now","at":NOW-5.,"line":"Newest open question?","confidence":0.5,"answer":null},
        {"id":"settled","session":"sess-now","at":NOW-1.,"line":"Settled question?","answer":"ignored"}
    ]);
    let composition = Composition::build(&data, &session, NOW);
    assert_eq!(composition.dream.state, Batch::Ready);
    assert_eq!(composition.question(), "Newest open question?");
    assert_eq!(composition.dream.queue.len(), 1);
    let untouched = Composition::build(&json!({}), &session, NOW);
    let lines = csr_engine::companion::render::instrument::text_rows(
        &untouched,
        44,
        30,
        Default::default(),
    )
    .join("\n");
    assert!(lines.contains("No open what-if yet."));
    assert!(!lines.contains("looked"));
}

#[test]
fn all_states_draw_the_whole_instrument_from_40_columns_in_short_panes() {
    for state in STATES {
        let (_home, paths) = state_home(state);
        for cols in [40, 41, 42, 43, 44, 45, 60, 76, 100] {
            let mut pane = Pane::new(paths.clone(), &args("main", "sess-now", cols, 30));
            pane.pixels = true;
            pane.enable_cards();
            let lines = pane.frame(cols, 30, NOW);
            let metrics = pane
                .instrument_metrics
                .as_ref()
                .unwrap_or_else(|| panic!("{state} at {cols}"));
            assert!(metrics.dream / metrics.height <= 0.3, "{state} at {cols}");
            assert_eq!(pane.placed.len(), 1);
            assert_eq!(pane.placed[0].row, 0);
            assert!(pane.placed[0].rows <= 29);
            assert!(lines.iter().all(|r| text::len(&text::plain(r)) <= cols));
        }
    }
}

#[test]
fn learnt_facts_without_a_story_still_have_a_pixel_panel() {
    use csr_engine::companion::render::instrument::{self, Composition};
    let data = json!({"learnt":[[3,"notes kept"]],"claude_fields":{"context_window":{"total_input_tokens":140000,"context_window_size":1000000}}});
    let c = Composition::build(&data, &json!({"id":"usage-only"}), NOW);
    assert!(c.story.is_empty() && c.shifts.is_empty());
    let (_, metrics) =
        instrument::draw(&c, 44 * 16, 29 * 34, Theme::Dark, Default::default(), false).unwrap();
    assert!(metrics.story_region.is_some());
}

#[test]
fn scaled_instrument_answer_click_uses_the_centered_pixel_coordinates() {
    let (_home, paths) = state_home("02-running");
    let mut pane = Pane::new(paths, &args("main", "sess-now", 100, 30));
    pane.pixels = true;
    pane.enable_cards();
    pane.frame(100, 30, NOW);
    assert!(pane.instrument_metrics.as_ref().unwrap().content_left > 0.);
    let (row, act) = pane
        .acts
        .iter()
        .find(|(_, a)| a["dream"].is_string())
        .unwrap();
    let span = act["spans"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s[2] == "helpful")
        .unwrap();
    let col = (span[0].as_u64().unwrap() + span[1].as_u64().unwrap()) as usize / 2;
    assert!(pane.click(*row, col, NOW));
    pane.frame(100, 30, NOW + 1.);
    assert!(pane
        .instrument_metrics
        .as_ref()
        .unwrap()
        .question_lines
        .join(" ")
        .contains("launch gallery"));
}

#[test]
fn text_facts_stay_whole_at_60_and_80_columns_and_footer_uses_terminal_height() {
    for state in STATES {
        let golden = fs::read_to_string(
            fixture()
                .join("instrument/text")
                .join(format!("{state}.txt")),
        )
        .unwrap();
        let facts: Vec<_> = golden
            .lines()
            .filter(|line| {
                line.starts_with("0.82")
                    || line.starts_with("dreamer ")
                    || line.starts_with("CTX ")
                    || line.starts_with("7D ")
                    || line.starts_with("● CSR:")
                    || line.starts_with("1 by CSR")
                    || line.starts_with("3 notes kept")
                    || *line == "carried"
                    || line.starts_with("1 helpful")
            })
            .flat_map(|line| line.split(" · "))
            .collect();
        for cols in [60, 80] {
            let (_home, paths) = state_home(state);
            let mut pane = Pane::new(paths, &args("main", "sess-now", cols, 67));
            pane.pixels = false;
            let lines: Vec<_> = pane
                .frame(cols, 67, NOW)
                .iter()
                .map(|line| text::plain(line).trim_end().to_owned())
                .collect();
            for fact in &facts {
                assert!(
                    lines
                        .iter()
                        .any(|line| line.split(" · ").any(|piece| piece == *fact)),
                    "{state} at {cols}: split or missing fact {fact:?}: {lines:?}"
                );
            }
            assert!(lines.iter().all(|line| text::len(line) <= cols
                && !line.ends_with(" ·")
                && !line.starts_with("· ")));
            if let Some(ctx) = lines.iter().position(|line| line.starts_with("CTX ")) {
                let status = lines.iter().position(|line| line.contains("● CSR:"));
                assert!(
                    status.is_some_and(|row| row > ctx),
                    "{state}: CSR displaced CTX"
                );
                assert!(lines[status.unwrap()].ends_with("● CSR: daemon down"));
            }
            assert_eq!(lines.len(), 67);
            assert!(lines[66].contains("q close"));
            let shorter = pane.frame(cols, 45, NOW);
            assert_eq!(shorter.len(), 45);
            assert!(text::plain(&shorter[44]).contains("q close"));
            // Text content is pinned above the keys at either height.
            let content = |rows: Vec<String>| {
                rows.into_iter()
                    .take(44)
                    .map(|line| text::plain(&line).trim_end().to_owned())
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                content(shorter),
                lines[..66]
                    .iter()
                    .filter(|line| !line.is_empty())
                    .cloned()
                    .collect::<Vec<_>>()
            );
        }
        if !matches!(state, "01-empty" | "08b-nothing") {
            assert!(golden.contains("Text parity is ready for review."));
        }
        assert!(!golden.contains("Claude:Misread") && !golden.contains("hook:Check"));
        assert_eq!(golden.contains("Saved ~"), state == "06-after");
    }
}

#[test]
fn narrow_pixels_use_a_short_bottom_strip_and_native_text() {
    for theme in [Theme::Dark, Theme::Light] {
        let (_home, paths) = state_home("02-running");
        let mut pane = Pane::new(paths, &args("main", "sess-now", 30, 40));
        pane.theme = theme;
        pane.pixels = true;
        pane.enable_cards();
        let rows = pane.frame(30, 40, NOW);
        assert_eq!(pane.placed.len(), 1);
        let strip = &pane.placed[0];
        assert!(strip.rows <= 7);
        assert_eq!(strip.row + strip.rows, 39);
        assert!(rows
            .iter()
            .take(strip.row)
            .any(|line| text::plain(line).contains("What if")));
        assert!(
            !rows.iter().any(|line| text::plain(line).contains("CTX ")),
            "figure facts were repeated beside the strip"
        );
    }
}

#[test]
fn agent_rings_open_real_details_in_place() {
    let (_home, paths) = state_home("03-agents");
    let mut pane = Pane::new(paths, &args("main", "sess-now", 76, 67));
    pane.pixels = true;
    pane.enable_cards();
    pane.frame(76, 67, NOW);
    let row = *pane
        .acts
        .iter()
        .find(|(_, a)| a["key"] == "instrument:agents")
        .unwrap()
        .0;
    pane.click(row, 20, NOW);
    assert!(pane.instrument_open.agents);
    assert_eq!(pane.view, "main");
    pane.pixels = false;
    assert!(pane
        .frame(76, 67, NOW)
        .iter()
        .any(|line| text::plain(line).contains("tools")));
}

#[test]
fn fresh_session_does_not_inherit_another_sessions_empty_look() {
    let (_home, paths) = state_home("01-empty");
    fs::write(
        paths.keep.join("dreams.jsonl"),
        json!({"kind":"look","session":"another","at":NOW-1.,"outcome":"none"}).to_string() + "\n",
    )
    .unwrap();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 76, 67));
    pane.pixels = false;
    let text = pane.frame(76, 67, NOW).join("\n");
    assert!(text.contains("The dreamer looks as you work."));
    assert!(!text.contains("d dream"));
    assert!(!text.contains("last look found nothing"));
}

#[test]
fn corrected_states_use_real_feeds_and_empty_queue_never_requests_a_look() {
    for (state, expected) in [
        (
            "01-empty",
            vec!["The dreamer looks as you work.", "dreamer 0 of 40k"],
        ),
        (
            "02-running",
            vec![
                "3 notes kept",
                "2 findings confirmed",
                "1 fact retired",
                "unverified",
                "Should be a hook:",
                "turns 4, 8",
            ],
        ),
        ("04-band", vec!["♻ ~1 prompt · 28 calls"]),
        ("05-compacting", vec!["Compacting · 12s of ~45s"]),
        (
            "06-after",
            vec![
                "Compacted. 2 by CSR. Saved ~118k so far.",
                "refreshed 1m ago",
            ],
        ),
        (
            "07-away",
            vec!["asked while you were away", "looked 4×. 3 new what-ifs"],
        ),
        ("08-offer", vec!["dreamer 9k of 40k", "Dreaming."]),
        (
            "08b-nothing",
            vec![
                "The dreamer looks as you work.",
                "The last look found nothing.",
            ],
        ),
    ] {
        let (_home, paths) = state_home(state);
        let mut pane = Pane::new(paths.clone(), &args("main", "sess-now", 76, 67));
        pane.pixels = false;
        let shown = pane
            .frame(76, 67, NOW)
            .iter()
            .map(|l| text::plain(l))
            .collect::<Vec<_>>()
            .join("\n");
        for phrase in expected {
            assert!(shown.contains(phrase), "{state}: missing {phrase}: {shown}");
        }
        assert!(!shown.contains("STS") && !shown.contains("♻A") && !shown.contains("d dream"));
        if matches!(state, "01-empty" | "08b-nothing") {
            let before = fs::read(paths.keep.join("dreams.jsonl")).unwrap();
            pane.key("d");
            pane.frame(76, 67, NOW);
            assert_eq!(fs::read(paths.keep.join("dreams.jsonl")).unwrap(), before);
            assert!(!shown.contains("d queue"));
        }
        if state == "02-running" {
            let row = *pane
                .acts
                .iter()
                .find(|(_, a)| a["key"] == "instrument:dreams")
                .unwrap()
                .0;
            assert!(pane.click(row, 0, NOW));
            let detail = pane.frame(76, 67, NOW).join("\n");
            assert!(detail.contains("at most 30%"));
        }
    }
}
