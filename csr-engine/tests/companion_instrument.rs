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
const STATES: [&str; 11] = [
    "01-empty",
    "02-running",
    "03-agents",
    "04-band",
    "05-compacting",
    "06-after",
    "07-away",
    "08-offer",
    "08b-nothing",
    "09-api-cost",
    "10-unknown-cost",
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
            // Dreams are off: prior answered batch only, no open what-if and no in-flight look.
            log.push(json!({"kind":"batch","session":"sess-now","at":NOW-700.,"budget":{"spent":9000,"cap":40000},"dreams":[
                {"id":"prev-one","line":"What if the budget had a label?","title":"Label the dream budget","confidence":0.8},
                {"id":"prev-two","line":"What if the figure opened in place?","title":"Keep the figure in place","confidence":0.7}]}));
            log.push(json!({"kind":"answer","id":"prev-one","answer":"helpful","at":NOW-650.}));
            log.push(
                json!({"kind":"answer","id":"prev-two","answer":"thought_of_it","at":NOW-640.}),
            );
            log.push(json!({"kind":"look","session":"sess-now","at":NOW-30.,"outcome":"none","budget":{"spent":9000,"cap":40000}}));
            let settings_dir = paths.home.join(".claude");
            fs::create_dir_all(&settings_dir).unwrap();
            write_json(
                &settings_dir.join("settings.json"),
                &json!({"pluginConfigs":{"csr-companion@local":{"options":{"dreams":false}}}}),
            );
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
    // Instrument states use a live daemon except 08b-nothing, which keeps the down fixture.
    let lock = paths.home.join(".claude-self-reflect/daemon.lock");
    if state == "08b-nothing" {
        fs::write(&lock, "").unwrap();
    } else {
        fs::create_dir_all(lock.parent().unwrap()).unwrap();
        fs::write(&lock, std::process::id().to_string()).unwrap();
    }
    if empty {
        con.execute("DELETE FROM narrative_usage", []).unwrap();
    }
    if state == "09-api-cost" {
        fs::remove_dir_all(paths.keep.join("usage")).unwrap();
        write_json(
            &paths.keep.join("usage/sess-now.json"),
            &json!({
                "at":NOW,"context":{"tokens":ctx,"window":1000000},
                "rateLimits":[{"kind":"five_hour","percentUsed":2,"resetsAt":"2026-10-18T14:08:00Z"}]
            }),
        );
        con.execute("DELETE FROM narrative_usage", []).unwrap();
        let rate =
            csr_engine::companion::value::cost_of("claude-haiku-4-5", [1_000_000, 0, 0, 0, 0])
                .unwrap();
        for call in 0..37 {
            con.execute("INSERT INTO narrative_usage(ts,call_site,model,input_tokens) VALUES('2026-10-18T10:38:00','story','claude-haiku-4-5',?)",
                [if call == 0 { (1.20 / rate * 1e6).round() as i64 } else { 0 }]).unwrap();
        }
    }
    if state == "10-unknown-cost" {
        con.execute("DROP TABLE narrative_usage", []).unwrap();
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
    let lines = pane.frame(cols, rows, NOW);
    assert_eq!(
        pane.placed.len(),
        1,
        "{state} did not draw the actual main frame"
    );
    let metrics = pane.instrument_metrics.as_ref().unwrap();
    assert!(
        metrics.font_fallbacks.is_empty(),
        "{state} {cols}x{rows}: font fallback {:?}",
        metrics.font_fallbacks
    );
    let expected_size = (24. + (cols as f32 * 16. - 704.) / 256. * 6.).clamp(22., 30.);
    assert!((metrics.question_size - expected_size).abs() < 0.01);
    assert_eq!(metrics.question_line_height, (expected_size * 1.25).round());
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
    if state == "08-offer" {
        assert_eq!(metrics.question_lines.join(" "), "Turn dreams on.");
        assert!(text.iter().any(|l| l.contains("Ask about past work.")));
        let cta = "Press 1 to turn dreams on in all your sessions.";
        assert_eq!(
            text.iter()
                .flat_map(|l| l.split_whitespace())
                .collect::<Vec<_>>()
                .join(" ")
                .matches(cta)
                .count(),
            1,
            "{state}: CTA must appear once: {text:?}"
        );
        assert!(!text.iter().any(|l| l.contains("1 Turn dreams on")));
        assert!(!text
            .iter()
            .any(|l| l.contains("Press 1: dreams on in all your sessions.")));
    }
    let cell = csr_engine::companion::render::kitty::cell_size();
    assert_eq!(
        pane.placed[0].rows,
        rows.saturating_sub(1),
        "{state}: instrument must fill every row above the keys"
    );
    let frame = csr_engine::companion::png::compose(&lines, &pane.placed, cols, rows, cell, theme)
        .expect("cell-grid PNG");
    assert_eq!(frame.width(), cols as u32 * cell.0);
    assert_eq!(frame.height(), rows as u32 * cell.1);
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
        json!({"text":text,"png_sha256":hash,"key_gap_px":format!("{:.2}",metrics.last_card_gap_px),"acts":pane.acts,"dream_percent":format!("{:.2}",100.*metrics.dream/metrics.height),
            "csr_cost":{"share":metrics.csr_cost.share_label(),"usd":metrics.csr_cost.usd,"calls":metrics.csr_cost.calls}}),
        // Cell-grid export for contact sheets; placement hash stays in the golden JSON.
        {
            let mut out = Vec::new();
            let mut cursor = std::io::Cursor::new(&mut out);
            image::DynamicImage::ImageRgba8(frame)
                .write_to(&mut cursor, image::ImageFormat::Png)
                .unwrap();
            out
        },
    )
}
#[test]
fn all_nine_states_and_both_themes_have_honest_top_frame_goldens() {
    for (cols, rows) in [(76, 67), (38, 30), (44, 30), (60, 40)] {
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
                assert_eq!(value, expected, "{name}/{state} {cols}x{rows}");
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
            if cols >= 32 {
                let metrics = pane.instrument_metrics.as_ref().expect("whole Instrument");
                assert!(metrics.dream / metrics.height <= 0.3);
                assert!(metrics.story_region.is_some());
                assert_eq!(pane.placed[0].row, 0);
                assert!(pane.placed[0].rows <= 29);
            } else {
                assert!(pane.instrument_metrics.is_none());
                assert!(pane.placed[0].row + pane.placed[0].rows <= 29);
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
fn all_states_draw_the_whole_instrument_from_32_columns_in_short_panes() {
    for state in STATES {
        let (_home, paths) = state_home(state);
        for cols in [32, 38, 40, 41, 42, 43, 44, 45, 60, 76, 100] {
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
                if let Some(row) = status {
                    assert!(row > ctx, "{state}: CSR displaced CTX");
                }
                if state == "08b-nothing" {
                    assert!(
                        status.is_some_and(|row| lines[row].contains("● CSR: daemon down")),
                        "{state}: expected daemon down"
                    );
                } else {
                    assert!(
                        status.is_none_or(|row| !lines[row].contains("daemon down")),
                        "{state}: running daemon still shows down"
                    );
                }
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
fn narrow_pixels_keep_native_text_and_strip_contiguous() {
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
        assert!(strip.row + strip.rows <= 39);
        assert!(strip.row > 0);
        assert!(
            !text::plain(&rows[strip.row - 1]).trim().is_empty(),
            "blank band before strip"
        );
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
        (
            "08-offer",
            vec![
                "Turn dreams on.",
                "Press 1 to turn dreams on in all your sessions.",
                "Ask about past work. CSR finds it.",
                "dreamer 9k of 40k",
            ],
        ),
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

#[test]
fn offer_hotkey_enables_dreams_on_one_press_in_private_home() {
    let (_home, paths) = state_home("08-offer");
    let settings = paths.home.join(".claude/settings.json");
    write_json(
        &settings,
        &json!({
            "pluginConfigs": {
                "csr-companion@local": {"options": {"dreams": false, "keep": 1}},
                "csr-companion@other": {"options": {"dreams": false, "keep": 2}},
                "unrelated@x": {"options": {"dreams": false}}
            }
        }),
    );
    let mut pane = Pane::new(paths.clone(), &args("main", "sess-now", 76, 67));
    pane.pixels = false;
    let shown = pane
        .frame(76, 67, NOW)
        .iter()
        .map(|l| text::plain(l))
        .collect::<Vec<_>>()
        .join("\n");
    let shown = shown.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        shown.contains("Press 1 to turn dreams on in all your sessions."),
        "{shown}"
    );
    assert!(!shown.contains("1 Turn dreams on"), "{shown}");
    assert!(!shown.contains("Press 1: dreams on"), "{shown}");
    assert_eq!(
        shown
            .matches("Press 1 to turn dreams on in all your sessions.")
            .count(),
        1,
        "CTA once: {shown}"
    );
    assert!(pane
        .acts
        .values()
        .any(|a| a["key"] == "instrument:dreams-on"));
    let mode = fs::metadata(&settings).unwrap().permissions();
    assert!(pane.key("1"));
    let after: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(
        after["pluginConfigs"]["csr-companion@local"]["options"]["dreams"],
        true
    );
    assert_eq!(
        after["pluginConfigs"]["csr-companion@other"]["options"]["dreams"],
        true
    );
    assert_eq!(
        after["pluginConfigs"]["csr-companion@local"]["options"]["keep"],
        1
    );
    assert_eq!(
        after["pluginConfigs"]["csr-companion@other"]["options"]["keep"],
        2
    );
    assert_eq!(
        after["pluginConfigs"]["unrelated@x"]["options"]["dreams"],
        false
    );
    assert_eq!(fs::metadata(&settings).unwrap().permissions(), mode);
    let on = pane
        .frame(76, 67, NOW + 1.)
        .iter()
        .map(|l| text::plain(l))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!on.contains("Turn dreams on"), "{on}");
    assert!(!on.contains("Couldn't turn dreams on"), "{on}");
}

#[test]
fn offer_refuses_malformed_settings_and_surfaces_the_notice() {
    let (_home, paths) = state_home("08-offer");
    let settings = paths.home.join(".claude/settings.json");
    let mut pane = Pane::new(paths.clone(), &args("main", "sess-now", 76, 67));
    pane.pixels = false;
    pane.frame(76, 67, NOW);
    assert!(pane
        .acts
        .values()
        .any(|a| a["key"] == "instrument:dreams-on"));
    // Corrupt after the offer is on screen so key 1 still routes to the enable path.
    fs::write(&settings, "not-json{").unwrap();
    assert!(pane.key("1"));
    assert_eq!(fs::read_to_string(&settings).unwrap(), "not-json{");
    let shown = pane.frame(76, 67, NOW + 1.).join("\n");
    assert!(shown.contains("Couldn't turn dreams on"), "{shown}");
    assert!(
        shown.contains("malformed") || shown.contains("settings.json"),
        "{shown}"
    );
}

#[test]
fn offer_refuses_write_failure_without_claiming_enabled() {
    let (_home, paths) = state_home("08-offer");
    let settings = paths.home.join(".claude/settings.json");
    let before = fs::read_to_string(&settings).unwrap();
    let claude = paths.home.join(".claude");
    let mut perms = fs::metadata(&claude).unwrap().permissions();
    let restored = perms.clone();
    perms.set_readonly(true);
    fs::set_permissions(&claude, perms).unwrap();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 76, 67));
    pane.pixels = false;
    pane.frame(76, 67, NOW);
    assert!(pane.key("1"));
    let shown = pane.frame(76, 67, NOW + 1.).join("\n");
    let _ = fs::set_permissions(&claude, restored);
    assert_eq!(fs::read_to_string(&settings).unwrap(), before);
    assert!(shown.contains("Couldn't turn dreams on"), "{shown}");
    assert!(shown.contains("Turn dreams on"), "{shown}");
}

#[test]
fn daemon_down_is_only_the_08b_fixture_state() {
    for state in STATES {
        let (_home, paths) = state_home(state);
        let mut pane = Pane::new(paths, &args("main", "sess-now", 76, 67));
        pane.pixels = false;
        let shown = pane.frame(76, 67, NOW).join("\n");
        let down = shown.contains("daemon down");
        assert_eq!(down, state == "08b-nothing", "{state}: {shown}");
    }
}

#[test]
fn expanded_bars_report_a_ceiling_on_the_chart_and_quotas_stay_compact() {
    use csr_engine::companion::render::instrument::{self, Composition};
    let (_home, paths) = state_home("03-agents");
    let mut pane = Pane::new(paths, &args("main", "sess-now", 76, 67));
    pane.pixels = true;
    pane.enable_cards();
    pane.key("i");
    pane.frame(76, 67, NOW);
    let metrics = pane.instrument_metrics.as_ref().unwrap();
    let ceiling = metrics.ceiling_y.expect("expanded ceiling");
    let (fig_y, fig_h) = metrics.figure_region;
    assert!(
        ceiling >= fig_y + 90. && ceiling <= fig_y + 200.,
        "ceiling {ceiling} outside figure {fig_y}+{fig_h}"
    );

    let empty = Composition::build(
        &json!({
            "dreams_on": true,
            "claude_fields": {"context_window": {}},
            "learnt": []
        }),
        &json!({"id":"empty"}),
        NOW,
    );
    let (_, empty_metrics) = instrument::draw(
        &empty,
        44 * 16,
        29 * 34,
        Theme::Dark,
        Default::default(),
        false,
    )
    .unwrap();
    // Reserve a second caption line for CSR's weekly cost below the quota label.
    assert!(
        empty_metrics.figure_region.1 >= 152. && empty_metrics.figure_region.1 <= 154.,
        "quotas card height {}: want 152..=154 so labels fit and stay compact",
        empty_metrics.figure_region.1
    );
    let help = empty_metrics.help_region.expect("help card");
    assert!(
        (empty_metrics.last_card_gap_px - 12.).abs() <= 2.,
        "last card pad {}: want >= 10pt above keys",
        empty_metrics.last_card_bottom_pad
    );
    assert!(
        help.0 + help.1 + empty_metrics.last_card_bottom_pad <= empty_metrics.height + 0.5,
        "help card overruns pixmap: help={help:?} pad={} height={}",
        empty_metrics.last_card_bottom_pad,
        empty_metrics.height
    );
}

#[test]
fn footer_abbreviates_only_what_must_fit_and_keeps_q_close() {
    let (_home, paths) = state_home("02-running");
    let mut pane = Pane::new(paths, &args("main", "sess-now", 44, 30));
    pane.pixels = false;
    let footer = text::plain(&pane.frame(44, 30, NOW)[29]);
    assert!(footer.contains("q close"), "{footer}");
    assert!(
        footer.contains("l changes") && footer.contains("t threads"),
        "{footer}"
    );
    assert!(!footer.contains("q quit"), "{footer}");
    assert!(
        footer.starts_with(' '),
        "key row needs col-1 inset: {footer:?}"
    );
    let legend = pane.acts.get(&29).expect("legend act");
    let spans = legend["keys"].as_array().unwrap();
    assert!(
        spans[0][0].as_u64().unwrap() >= 1,
        "first key span starts at col >= 1: {spans:?}"
    );
    assert!(
        spans.last().unwrap()[1].as_u64().unwrap() <= 43,
        "keys fit cols-2: {spans:?}"
    );
    for cols in [48, 60] {
        let wide = text::plain(&pane.frame(cols, 30, NOW)[29]);
        assert!(
            wide.contains("l changes") && wide.contains("t threads") && wide.contains("q close"),
            "{wide}"
        );
        if cols == 60 {
            assert!(wide.contains("figure") && wide.contains("theme"), "{wide}");
        }
    }
}

#[test]
fn collapsed_context_hugs_exact_group_with_bottom_clearance() {
    for (cols, rows) in [(44, 40), (60, 54)] {
        let (_home, paths) = state_home("02-running");
        // Drop request history so collapsed layout is independent of per-turn bars.
        let usage = paths.keep.join("usage/sess-now.json");
        if usage.exists() {
            let mut u: Value = serde_json::from_slice(&fs::read(&usage).unwrap()).unwrap();
            if let Some(obj) = u.as_object_mut() {
                obj.remove("requests");
            }
            write_json(&usage, &u);
        }
        let mut pane = Pane::new(paths, &args("main", "sess-now", cols, rows));
        pane.pixels = true;
        pane.enable_cards();
        pane.frame(cols, rows, NOW);
        let metrics = pane.instrument_metrics.as_ref().unwrap();
        let (cx, cy, r) = metrics.ring_bounds.expect("collapsed ring");
        let (group_top, group_bottom) = metrics.ring_group.expect("ring group");
        let avail = metrics
            .ring_formula_avail
            .expect("ring formula available_height");
        let expected = (avail / 2.8).clamp(78., 118.);
        assert!(
            (r - expected).abs() < 0.05,
            "{cols}x{rows}: r={r} != clamp(78, {avail}/2.8, 118)={expected}"
        );
        let (fig_y, fig_h) = metrics.figure_region;
        let card_bottom = fig_y + fig_h;
        assert!(
            group_bottom <= card_bottom - 16. + 0.5,
            "{cols}x{rows}: group_bottom {group_bottom} needs >=16pt above card bottom {card_bottom}"
        );
        let title_band = fig_y + 16. + 18. + 8. + 14. + 12.;
        assert!(group_top >= title_band - 0.5);
        assert!(
            cy > group_top && cy < group_bottom,
            "{cols}x{rows}: ring centre outside group"
        );
        assert_eq!(
            pane.placed[0].rows,
            rows.saturating_sub(1),
            "picture ends one row above keys"
        );
        assert!((metrics.last_card_gap_px - 12.).abs() <= 2.);
        let _ = cx;
    }
}

#[test]
fn mid_height_context_uses_side_ring_and_small_gauges() {
    for state in ["06-after", "07-away"] {
        for (cols, rows) in [(44, 30), (60, 40)] {
            for theme in [Theme::Dark, Theme::Light] {
                let (_home, paths) = state_home(state);
                let mut pane = Pane::new(paths, &args("main", "sess-now", cols, rows));
                pane.pixels = true;
                pane.enable_cards();
                pane.theme = theme;
                pane.frame(cols, rows, NOW);
                let metrics = pane.instrument_metrics.as_ref().unwrap();
                let mid = metrics
                    .mid_layout
                    .as_ref()
                    .unwrap_or_else(|| panic!("{state} {cols}x{rows} {theme:?}: expected mid layout, got ring={:?} fig={:?}", metrics.ring_bounds, metrics.figure_region));
                assert!(
                    metrics.ring_bounds.is_none(),
                    "{state} {cols}x{rows}: mid must not use full scaled ring"
                );
                let (fig_y, fig_h) = metrics.figure_region;
                assert!(
                    fig_h > 110.5,
                    "{state} {cols}x{rows}: card {fig_h} should be above strip height"
                );
                let (_, _, r) = mid.ring;
                let expected = mid.radius_inner.clamp(25., 64.);
                assert!(
                    (r - expected).abs() < 0.05,
                    "{state} {cols}x{rows}: r={r} != clamp(25, {}, 64)={expected}",
                    mid.radius_inner
                );
                assert!(
                    mid.gauge_r >= 16. - 0.01 && mid.gauge_r <= 20. + 0.01,
                    "{state}: gauge_r {}",
                    mid.gauge_r
                );
                let (gt, gb) = mid.group;
                let title_bottom = fig_y + 16. + 18. + 8.; // padding + title line + gap
                let card_bottom = fig_y + fig_h;
                let top_band = gt - title_bottom;
                let bottom_band = (card_bottom - 16.) - gb;
                assert!(
                    top_band <= 24. + 0.5 && bottom_band <= 24. + 0.5,
                    "{state} {cols}x{rows}: empty bands top={top_band} bottom={bottom_band}"
                );
                assert!(
                    gb <= card_bottom - 16. + 0.5,
                    "{state}: group bottom {gb} vs card {card_bottom}"
                );
                let ring_right = mid.ring.0 + mid.ring.2;
                assert!(
                    mid.gauges_left >= ring_right + 8. - 0.5,
                    "{state}: gauges overlap ring: left={} ring_right={ring_right}",
                    mid.gauges_left
                );
                assert!(
                    mid.gauge_label_y + 2. <= mid.details_y,
                    "{state}: gauge labels overlap details"
                );
                assert!(mid.details_y <= gb + 0.5, "{state}: details below group");
                let slot = (card_bottom - 16. - title_bottom).max(0.);
                let group_h = gb - gt;
                assert!(
                    group_h >= slot * 0.45 || group_h >= 2. * r - 0.5,
                    "{state} {cols}x{rows}: mid group underuses card: group_h={group_h} slot={slot}"
                );
            }
        }
    }
}

#[test]
fn offer_cta_appears_once_without_legacy_phrases() {
    let (_home, paths) = state_home("08-offer");
    let mut pane = Pane::new(paths, &args("main", "sess-now", 44, 30));
    pane.pixels = false;
    let shown = pane
        .frame(44, 30, NOW)
        .iter()
        .map(|l| text::plain(l))
        .collect::<Vec<_>>()
        .join("\n");
    let shown = shown.split_whitespace().collect::<Vec<_>>().join(" ");
    let cta = "Press 1 to turn dreams on in all your sessions.";
    assert_eq!(shown.matches(cta).count(), 1, "{shown}");
    assert!(shown.contains("Turn dreams on."), "{shown}");
    assert!(!shown.contains("1 Turn dreams on"), "{shown}");
    assert!(!shown.contains("Press 1: dreams on"), "{shown}");
}

#[test]
fn collapsed_ring_formula_holds_for_all_token_fixtures() {
    for state in STATES {
        for (cols, rows) in [(44, 30), (60, 40)] {
            let (_home, paths) = state_home(state);
            let mut pane = Pane::new(paths, &args("main", "sess-now", cols, rows));
            pane.pixels = true;
            pane.enable_cards();
            pane.frame(cols, rows, NOW);
            let metrics = pane.instrument_metrics.as_ref().unwrap();
            if let (Some((_, _, r)), Some(avail), Some((gt, gb))) = (
                metrics.ring_bounds,
                metrics.ring_formula_avail,
                metrics.ring_group,
            ) {
                let expected = (avail / 2.8).clamp(78., 118.);
                assert!(
                    (r - expected).abs() < 0.05,
                    "{state} {cols}x{rows}: r={r} != {expected} from avail={avail}"
                );
                let card_bottom = metrics.figure_region.0 + metrics.figure_region.1;
                assert!(
                    gb <= card_bottom - 16. + 0.5,
                    "{state} {cols}x{rows}: group bottom {gb} vs card {card_bottom}"
                );
                assert!(gt < gb, "{state}: empty group");
            }
        }
    }
}

#[test]
fn every_fixture_ends_one_s3_gap_above_the_key_row() {
    let mut measurements = Vec::new();
    for state in STATES {
        for (cols, rows) in [(76, 67), (38, 30), (44, 30), (60, 40)] {
            for theme in [Theme::Dark, Theme::Light] {
                let (_home, paths) = state_home(state);
                let mut pane = Pane::new(paths, &args("main", "sess-now", cols, rows));
                pane.pixels = true;
                pane.enable_cards();
                pane.theme = theme;
                if state == "03-agents" {
                    pane.key("i");
                }
                pane.frame(cols, rows, NOW);
                let metrics = pane.instrument_metrics.as_ref().unwrap();
                let placement = &pane.placed[0];
                let png = image::load_from_memory(&placement.png).unwrap();
                let (y, h) = metrics
                    .help_region
                    .or(metrics.agents_region)
                    .unwrap_or(metrics.figure_region);
                // Convert the actual final card's logical bottom through the encoded PNG's
                // scale, then measure against the terminal key row's pixel origin.
                let card_bottom =
                    placement.row as f32 * 34. + (y + h) * png.height() as f32 / metrics.height;
                let key_top = (rows - 1) as f32 * 34.;
                let gap = key_top - card_bottom;
                assert!(
                    (gap - 12.).abs() <= 2.,
                    "{state} {cols}x{rows} {theme:?}: key gap {gap}px"
                );
                assert!((metrics.last_card_gap_px - gap).abs() <= 0.1);
                println!("key gap {state} {cols}x{rows} {theme:?}: {gap:.3}px");
                measurements.push(json!({"state":state,"cols":cols,"rows":rows,"theme":format!("{theme:?}"),"gap_px":gap}));
            }
        }
    }
    if let Some(dir) = std::env::var_os("CSR_INSTRUMENT_PNG") {
        let dir = PathBuf::from(dir);
        fs::create_dir_all(&dir).unwrap();
        write_json(&dir.join("key-gaps.json"), &json!(measurements));
    }
}

#[test]
fn long_story_cuts_only_on_a_clause_boundary_with_ellipsis() {
    use csr_engine::companion::render::instrument::{self, story_text_lines, Composition};
    // Mirrors the live cut: "...so the thread finder has not yet been shown to find them"
    let story = "Work stands at: the pane redesign is nearly done, so the thread finder has not yet been shown to find them. The next pass still needs the live export and contact sheets before the owner can review.";
    let lines = story_text_lines(story, 44, 4);
    let joined = lines.join(" ");
    assert!(
        !joined.contains("find them The next"),
        "must not join mid-sentence without a boundary: {joined}"
    );
    assert!(
        joined.ends_with('…')
            || joined.ends_with('.')
            || joined.ends_with(';')
            || joined.ends_with(',')
            || joined.ends_with(':'),
        "long story must end on a clause or ellipsis: {joined}"
    );
    assert!(
        !joined.contains("contact sheets before the owner"),
        "overflowing tail must be dropped: {joined}"
    );

    let data = json!({
        "dreams_on": true,
        "story": {"now": story, "at": NOW - 30.},
        "claude_fields": {"context_window": {"total_input_tokens": 192000, "context_window_size": 1000000}},
        "learnt": []
    });
    let c = Composition::build(
        &data,
        &json!({"id":"sess-now","main":[1],"prompts":[1]}),
        NOW,
    );
    let (_, metrics) =
        instrument::draw(&c, 44 * 16, 29 * 34, Theme::Dark, Default::default(), false).unwrap();
    assert!(
        metrics.story_line_count >= 1 && metrics.story_line_count <= 4,
        "story lines {}",
        metrics.story_line_count
    );
}

#[test]
fn empty_38_column_pane_is_a_complete_instrument() {
    for theme in [Theme::Dark, Theme::Light] {
        let (_home, paths) = state_home("01-empty");
        let mut pane = Pane::new(paths, &args("main", "sess-now", 38, 30));
        pane.pixels = true;
        pane.enable_cards();
        pane.theme = theme;
        let rows = pane.frame(38, 30, NOW);
        let metrics = pane
            .instrument_metrics
            .as_ref()
            .expect("full Instrument at 38 columns");
        assert_eq!(pane.placed.len(), 1);
        assert_eq!(pane.placed[0].row, 0);
        assert_eq!(pane.placed[0].rows, 29);
        assert!(metrics.help_region.is_some());
        assert!(metrics.figure_region.1 > 100.);
        assert!(rows
            .iter()
            .take(29)
            .all(|line| text::plain(line).trim().is_empty()));
        assert!(
            metrics.font_fallbacks.is_empty(),
            "{:?}",
            metrics.font_fallbacks
        );
    }
}

fn fixture_week(paths: &Paths) -> Value {
    use csr_engine::companion::{db::Db, masthead, value};
    let fields = masthead::kept_status(paths, "sess-now", NOW);
    let plan = value::plan_of(
        &fields["fields"],
        &masthead::account_limits(paths, NOW),
        NOW,
    );
    let db = Db::open(paths).unwrap();
    let narrative = value::narrative_week(&db, plan["start"].as_f64().unwrap());
    let mut spent = Value::Null;
    if plan["mode"] == "plan" {
        let mut spend = value::Spend::new(paths, false);
        assert!(spend.step(
            plan["start"].as_f64().unwrap(),
            NOW,
            Duration::from_secs(30)
        ));
        spent = spend.total();
    }
    value::week_value(paths, &plan, &narrative, &spent)
}

#[test]
fn instrument_and_info_use_the_same_week_figure_from_one_fixture() {
    use csr_engine::companion::{
        render::instrument::{self, Composition},
        value,
    };
    for state in [
        "02-running",
        "03-agents",
        "09-api-cost",
        "10-unknown-cost",
        "01-empty",
    ] {
        let (_home, paths) = state_home(state);
        let week = fixture_week(&paths);
        let composition =
            Composition::build(&json!({"value":week}), &json!({"id":"sess-now"}), NOW);
        let mut pane = Pane::new(paths, &args("main", "sess-now", 44, 30));
        pane.pixels = true;
        pane.enable_cards();
        pane.frame(44, 30, NOW);
        let actual = &pane.instrument_metrics.as_ref().unwrap().csr_cost;
        assert_eq!(actual.usd, week["figure"]["usd"].as_f64(), "{state}");
        assert_eq!(actual.share, composition.csr_cost.share, "{state}");
        let info = value::fuel_text(&week);
        if week["mode"] == "plan" && actual.share.is_some_and(|n| n > 0.) {
            assert_eq!(format!("≈{}%", actual.share_label()), info);
            let q = json!({"code":"7D","used":52});
            assert!(instrument::quota_caption(&composition, &q)
                .ends_with(&format!("CSR {} pts", actual.share_label())));
            assert!(actual.segment(52.).is_some());
        } else if week["mode"] == "api" {
            assert_eq!(format!("{info} this week"), actual.money_line());
            assert_eq!(actual.api_line(), "$1.20 this week · 37 calls");
        }
        if state == "10-unknown-cost" {
            assert_eq!(actual.share_label(), "—");
        }
        if state == "01-empty" {
            assert_eq!(actual.share_label(), "0");
        }
    }
}

#[test]
fn csr_cost_distinguishes_unknown_zero_and_bounds_without_a_second_cost_path() {
    use csr_engine::companion::value::{self, InstrumentCost};
    for figure in [
        Value::Null,
        json!({"usd":null,"share":0}),
        json!({"usd":null,"unpriced":["future-model"]}),
    ] {
        let cost = InstrumentCost::from_week(&json!({"figure":figure}));
        assert_eq!(cost.share_label(), "—");
        assert_eq!(cost.money_line(), "CSR —");
        assert_eq!(cost.segment(56.), None);
    }
    let zero = InstrumentCost::from_week(&json!({"figure":{"usd":0,"share":null}}));
    assert_eq!(zero.share_label(), "0");
    assert_eq!(zero.money_line(), "$0 this week");
    assert_eq!(zero.api_line(), "CSR 0 · 0 calls");
    assert_eq!(zero.segment(56.), None);
    let cost = InstrumentCost::from_week(
        &json!({"figure":{"usd":1.2,"share":0.4,"bound":true},"calls":35,"looks":2}),
    );
    assert_eq!(
        cost.money_line(),
        format!("{} this week", value::money_text(Some(1.2), true))
    );
    assert_eq!(cost.share_label(), "≤0.4");
    assert_eq!(cost.api_line(), "≤$1.20 this week · 37 calls");
    let (_, end) = cost.segment(56.).unwrap();
    assert!((end - 0.004).abs() < 1e-7);
    assert!(end < 0.56);
    assert_eq!(cost.segment(0.), Some((0., 0.)));
    let (_, end) = cost.segment(0.1).unwrap();
    assert!((end - 0.001).abs() < 1e-7);
}

#[test]
fn unpriced_narrative_and_unreadable_table_stay_unknown_in_the_actual_pane() {
    for unpriced in [false, true] {
        let (_home, paths) = state_home("02-running");
        let con = rusqlite::Connection::open(&paths.db).unwrap();
        if unpriced {
            con.execute(
                "UPDATE narrative_usage SET model='unpriced-future-model'",
                [],
            )
            .unwrap();
        } else {
            con.execute("DROP TABLE narrative_usage", []).unwrap();
        }
        let week = fixture_week(&paths);
        assert!(week["figure"].is_null());
        let mut pane = Pane::new(paths, &args("main", "sess-now", 44, 30));
        pane.pixels = false;
        let shown = pane.frame(44, 30, NOW).join("\n");
        assert!(shown.contains("CSR —"), "{shown}");
        assert!(!shown.contains("CSR 0"), "{shown}");
    }
}

#[test]
fn weekly_caption_names_only_csrs_percentage_points() {
    use csr_engine::companion::render::instrument::{quota_caption, Composition};
    let (_home, paths) = state_home("02-running");
    let week = fixture_week(&paths);
    let mut data = Composition::build(&json!({"value":week}), &json!({"id":"sess-now"}), NOW);
    let q = json!({"code":"7D", "used":57});
    data.csr_cost.share = Some(0.9);
    assert_eq!(quota_caption(&data, &q), "CSR 0.9 pts");
    data.csr_cost.share = Some(0.);
    assert_eq!(quota_caption(&data, &q), "CSR 0 pts");
    data.csr_cost.share = None;
    assert_eq!(quota_caption(&data, &q), "CSR — pts");
}

#[test]
fn gauge_centres_are_even_and_positive_csr_arcs_have_a_visible_separator() {
    let mut arcs = 0;
    let mut rows_checked = 0;
    for state in STATES {
        for (cols, rows) in [(38, 30), (44, 30), (60, 40), (76, 67)] {
            for theme in [Theme::Dark, Theme::Light] {
                let (_home, paths) = state_home(state);
                let mut pane = Pane::new(paths, &args("main", "sess-now", cols, rows));
                pane.pixels = true;
                pane.enable_cards();
                pane.theme = theme;
                if state == "03-agents" {
                    pane.key("i");
                }
                pane.frame(cols, rows, NOW);
                let m = pane.instrument_metrics.as_ref().unwrap();
                let gauges = &m.quota_gauges;
                assert!(gauges.len() >= 2, "{state} {cols}: missing quota row");
                let scale = (rows - 1) as f32 * 34. / m.height;
                let steps: Vec<_> = gauges
                    .windows(2)
                    .map(|p| p[1].center.0 - p[0].center.0)
                    .collect();
                for step in &steps {
                    assert!((step - steps[0]).abs() < 0.01, "{state} {cols}: {steps:?}");
                }
                for gauge in gauges {
                    assert!((gauge.center.1 - gauges[0].center.1).abs() < 0.01);
                    if let Some((rest, from, to)) = gauge.csr_segment {
                        assert!(
                            (to - from) * 360. >= 6. - 0.001,
                            "{state}: sweep {}",
                            (to - from) * 360.
                        );
                        assert!(from >= rest && to <= 1.);
                        if rest > 0. {
                            let gap_px =
                                (from - rest) * std::f32::consts::TAU * gauge.radius * scale;
                            assert!((gap_px - 1.).abs() < 0.01, "{state}: separator {gap_px}px");
                        }
                        arcs += 1;
                    } else if gauge.code == "7D" {
                        assert!(m.csr_cost.share.is_none_or(|v| v <= 0.));
                    }
                }
                let token = theme.tokens();
                // Distinct hue: CSR is magenta (R>B>G), quota usage is blue (B>R).
                assert!(
                    token.csr_share[0] > token.csr_share[2]
                        && token.csr_share[2] > token.csr_share[1]
                );
                assert!(token.sky[2] > token.sky[0]);
                rows_checked += 1;
            }
        }
    }
    assert_eq!(rows_checked, 88);
    assert!(arcs >= 56);
}
