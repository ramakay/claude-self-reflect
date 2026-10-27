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
    // Production gives each frame's compaction scan 5 ms. The fixture's three transcripts finish
    // inside that on an idle host, so the goldens show the finished scan ("of ~45s"); an
    // unbounded budget makes that the result on any CPU speed or load instead of a race.
    csr_engine::companion::sources::set_compaction_scan_budget(std::time::Duration::from_secs(60));
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
const STATES: [&str; 14] = [
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
    "11-last-time",
    "12-fresh",
    "13-flat",
];
const QUESTION:&str="What if one of Fable's three prototypes made the what-if question the star of the pane, as you wanted on 10-09?";
fn write_json(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value.to_string()).unwrap();
}
fn state_home(state: &str) -> (tempfile::TempDir, Paths) {
    let (home, paths) = rich_home();
    let empty = matches!(state, "01-empty" | "11-last-time");
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
        con.execute(
            "DELETE FROM session_registry WHERE project='pane-wp2' AND session_id!='sess-now'",
            [],
        )
        .unwrap();
        con.execute("delete from witness_verdicts", []).unwrap();
        con.execute("delete from reflections where id='story'", [])
            .unwrap();
        con.execute("delete from meta where key like 'story_map:%'", [])
            .unwrap();
    }
    if state == "11-last-time" {
        let episode = json!({"schema":"v2","session_id":"prior-pane-session","project":"pane-wp2",
            "timestamp":"2026-10-18T09:00:00Z","request":"Make the companion readable","investigated":[],
            "completed":"","next_steps":"Check the live narrow pane","blockers":null,"outcome":"done",
            "error_signatures":[],"tools_used":[],"files_modified":[],"message_count":8,"duration_minutes":30,
            "recap_evidence_version":1,"anchor_request":"Make the companion readable",
            "completions":[{"claim":"Restore footer labels","receipt":"abcdef1"}]});
        con.execute("INSERT INTO reflections VALUES('prior-pane-session',?1,'[\"session_episode\",\"project_pane-wp2\"]','2026-10-18T09:00:00Z')",[episode.to_string()]).unwrap();
        con.execute_batch("CREATE INDEX idx_chunks_conversation ON chunks(conversation_id);
            CREATE INDEX idx_chunk_provenance_source_conv ON chunk_provenance(source_conv_id);
            CREATE INDEX idx_resolution_open_recent ON resolution_ledger(status,source,id);
            INSERT INTO chunks VALUES('prior-settled','prior-pane-session','2026-10-18T09:00:00Z',NULL,'pane-wp2','fixture');
            INSERT INTO chunks VALUES('prior-open','prior-pane-session','2026-10-18T09:00:00Z',NULL,'pane-wp2','fixture');
            INSERT INTO resolution_ledger VALUES(11,'prior-settled','Keep one layout owner','commit fedcba9','resolved','user_confirmed',NULL);
            INSERT INTO resolution_ledger VALUES(12,'prior-open','Check device readability','commit acdc123','still_open','user_confirmed',NULL);").unwrap();
        for i in 0..60 {
            let sid = format!("earlier-{i:02}");
            let at = chrono::DateTime::from_timestamp((NOW - 7200. - i as f64 * 3600.) as i64, 0)
                .unwrap()
                .to_rfc3339();
            let intent = format!("Verify companion history case {}", i + 1);
            con.execute(
                "INSERT INTO session_registry VALUES(?1,'pane-wp2',?2,?3,?3)",
                rusqlite::params![sid, intent, at],
            )
            .unwrap();
            if i % 3 != 2 {
                let ep = json!({"schema":"v2","session_id":sid,"project":"pane-wp2","timestamp":at,
                    "request":"An unrelated first prompt","investigated":[],"completed":"",
                    "next_steps":if i%3==1{json!("Check the live pane")}else{Value::Null},
                    "blockers":null,"outcome":"unknown","error_signatures":[],"tools_used":[],"files_modified":[],
                    "message_count":8,"duration_minutes":30,"recap_evidence_version":1,"anchor_request":intent,
                    "completions":if i%3==0{json!([{"claim":"Keep history readable","receipt":format!("abc{i:04x}")}])}else{json!([])}});
                con.execute("INSERT INTO reflections VALUES(?1,?2,'[\"session_episode\",\"project_pane-wp2\"]',?3)",rusqlite::params![sid,ep.to_string(),at]).unwrap();
            }
        }
    }
    let ctx = match state {
        "01-empty" | "11-last-time" => 0,
        "12-fresh" | "13-flat" => 190000,
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
            if matches!(state, "12-fresh" | "13-flat") {
                let fresh = if state == "13-flat" {
                    2000
                } else {
                    2000 + i as i64 * 38000 / 23
                };
                lines.last_mut().unwrap()["message"]["usage"] = json!({"input_tokens":1000,
                    "cache_creation_input_tokens":fresh-1000,"cache_read_input_tokens":190000-fresh,
                    "cache_creation":{"ephemeral_1h_input_tokens":fresh-1000},"output_tokens":400});
            }
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
    if !matches!(
        state,
        "01-empty" | "11-last-time" | "08-offer" | "08b-nothing"
    ) {
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
            "fresh_plots":metrics.fresh_plots.iter().map(|p| json!({
                "region":[format!("{:.3}",p.region.0),format!("{:.3}",p.region.1),format!("{:.3}",p.region.2),format!("{:.3}",p.region.3)],
                "heights":p.heights.iter().map(|h|format!("{h:.3}")).collect::<Vec<_>>(),"compactions":p.compactions})).collect::<Vec<_>>(),
            "layout_height":format!("{:.3}",metrics.height),
            "history_rows":metrics.history_rows,
            "history_bottom":metrics.history_bottom.map(|n|format!("{n:.3}")),
            "figure_region":[format!("{:.3}",metrics.figure_region.0),format!("{:.3}",metrics.figure_region.1)],
            "help_region":metrics.help_region.map(|(y,h)|[format!("{y:.3}"),format!("{h:.3}")]),
            "dream_height":format!("{:.3}",metrics.dream),
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
fn pixel_number_keys_answer_their_own_dream_choice() {
    let sizes = (34..=80).map(|cols| (cols, 30)).chain([(76, 67)]);
    for ((cols, rows), (key, word)) in sizes.flat_map(|size| {
        [
            ("1", "helpful"),
            ("2", "thought_of_it"),
            ("3", "not_relevant"),
            ("4", "ask_claude"),
        ]
        .map(|choice| (size, choice))
    }) {
        let (_home, paths) = state_home("02-running");
        let mut pane = Pane::new(paths.clone(), &args("main", "sess-now", cols, rows));
        pane.pixels = true;
        pane.enable_cards();
        pane.frame(cols, rows, NOW);
        let spans = pane
            .acts
            .values()
            .find(|a| a["dream"].is_string())
            .map(|a| a["spans"].as_array().unwrap().clone())
            .unwrap_or_else(|| panic!("{cols}x{rows}: no dream answers"));
        for pair in spans.windows(2) {
            assert!(pair[0][1].as_u64() <= pair[1][0].as_u64(), "{spans:?}");
        }
        pane.key(key);
        let text = fs::read_to_string(paths.keep.join("dreams.jsonl")).unwrap();
        let answer: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(answer["answer"], word, "{cols}x{rows} key {key}: {spans:?}");
    }
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
        let (_home, paths) = state_home(state);
        let mut source = Pane::new(paths, &args("main", "sess-now", 76, 67));
        source.pixels = false;
        let golden = source
            .frame(76, 67, NOW)
            .iter()
            .map(|row| text::plain(row).trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n");
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
            let short_content = content(shorter);
            let long_content = lines[..66]
                .iter()
                .filter(|line| !line.is_empty())
                .cloned()
                .collect::<Vec<_>>();
            if state == "11-last-time" {
                // More height adds older history while keeping the recap and quota facts whole.
                let is_history = |line: &&String| line.contains(" ago · Verify companion history");
                // Spare height past the last history row holds the starter questions.
                let mut starters = false;
                let long_content = long_content
                    .into_iter()
                    .filter(|line| {
                        starters |= line.as_str() == "Try a question";
                        starters &= !line.starts_with("7D ");
                        !starters
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    short_content
                        .iter()
                        .filter(|line| !is_history(line))
                        .collect::<Vec<_>>(),
                    long_content
                        .iter()
                        .filter(|line| !is_history(line))
                        .collect::<Vec<_>>()
                );
                let short_history = short_content.iter().filter(is_history).collect::<Vec<_>>();
                let long_history = long_content.iter().filter(is_history).collect::<Vec<_>>();
                assert!(!short_history.is_empty() && long_history.len() > short_history.len());
                assert_eq!(short_history, long_history[..short_history.len()]);
            } else {
                assert_eq!(short_content, long_content);
            }
        }
        if !matches!(state, "01-empty" | "11-last-time" | "08b-nothing") {
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
                "Confused Claude",
                "Should be a skill / memory / tool",
                "hook: Check the card height after each layout edit.",
                "turns 4, 8",
            ],
        ),
        ("04-band", vec!["♻ ~1 prompt · 28 calls"]),
        ("05-compacting", vec!["Compacting · 10s of ~45s"]),
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
        if matches!(state, "01-empty" | "11-last-time" | "08b-nothing") {
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
            // The findings live in their own cards now; the dreamer's drill-down holds dreams only.
            assert!(detail.contains("You asked to make the question the star"));
            assert!(!detail.contains("at most 30%"));
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
fn expanded_fresh_bars_use_their_own_scale_and_empty_quotas_grow() {
    use csr_engine::companion::render::instrument::{self, Composition};
    let (_home, paths) = state_home("03-agents");
    let mut pane = Pane::new(paths, &args("main", "sess-now", 76, 67));
    pane.pixels = true;
    pane.enable_cards();
    pane.key("i");
    pane.frame(76, 67, NOW);
    let metrics = pane.instrument_metrics.as_ref().unwrap();
    assert!(
        metrics.ceiling_y.is_none(),
        "context ceiling has different units from fresh input"
    );
    assert_eq!(metrics.fresh_plots.len(), 1);
    assert_eq!(metrics.fresh_plots[0].compactions, [12]);

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
        empty_metrics.figure_region.1 > 154. && empty_metrics.figure_region.1 < 300.,
        "quotas card height {}: want natural enlarged quota gauges with their labels",
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
        pane.rings = true;
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
                pane.rings = true;
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
                let expected = mid.radius_inner.clamp(25., mid.radius_max);
                assert!(
                    (r - expected).abs() < 0.05,
                    "{state} {cols}x{rows}: r={r} != clamp(25, {}, 64)={expected}",
                    mid.radius_inner
                );
                assert!(
                    mid.gauge_r >= 16. - 0.01 && mid.gauge_r <= 30. + 0.01,
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
    assert_eq!(quota_caption(&data, &q), "7D · CSR 0.9 pts");
    data.csr_cost.share = Some(0.);
    assert_eq!(quota_caption(&data, &q), "7D · CSR 0 pts");
    data.csr_cost.share = None;
    assert_eq!(quota_caption(&data, &q), "7D · CSR — pts");
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
                pane.rings = true;
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
    assert_eq!(rows_checked, STATES.len() * 8);
    assert!(arcs >= 56);
}

#[test]
fn a_tap_on_each_quota_circle_opens_that_circles_page() {
    use csr_engine::companion::render::instrument::{Gauge, LAYOUT_WIDTH};
    // The row and the span of cells the circle `code` is a target in, on the frame as drawn now.
    fn circle(pane: &Pane, code: &str) -> Option<(usize, (u64, u64))> {
        pane.acts.iter().find_map(|(row, act)| {
            act["gauges"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|s| s[2] == code)
                .map(|s| (*row, (s[0].as_u64().unwrap(), s[1].as_u64().unwrap())))
        })
    }
    for (cols, rows) in [(60, 40), (76, 67)] {
        let (_home, paths) = state_home("02-running");
        let mut pane = Pane::new(paths, &args("main", "sess-now", cols, rows));
        pane.pixels = true;
        pane.rings = true;
        pane.enable_cards();
        pane.frame(cols, rows, NOW);
        let m = pane.instrument_metrics.clone().unwrap();
        let drawn: Vec<_> = m
            .quota_gauges
            .iter()
            .filter(|g| g.tap.is_some())
            .map(|g| (g.code.clone(), g.center.0))
            .collect();
        assert!(drawn.len() >= 3, "{cols}x{rows}: {drawn:?}");
        let mut seen = vec![];
        for (code, centre_x) in &drawn {
            let gauge = Gauge::from_code(code).unwrap_or_else(|| panic!("{code}"));
            let centre = (m.content_left + centre_x / LAYOUT_WIDTH * m.content_width) * cols as f32;
            let (row, span) = circle(&pane, code)
                .unwrap_or_else(|| panic!("{cols}x{rows}: no tap row for {code}"));
            assert!(
                span.0 as f32 <= centre && centre <= span.1 as f32,
                "{code}: centre {centre} outside {span:?}"
            );
            let col = ((span.0 + span.1) / 2) as usize;
            assert!(pane.click(row, col, NOW));
            assert_eq!(pane.instrument_open.gauge, Some(gauge), "{code}");
            assert!(pane.instrument_open.figure, "{code}");
            let text = pane.frame(cols, rows, NOW).join("\n");
            let png = pane
                .placed
                .first()
                .map(|p| p.png.to_vec())
                .unwrap_or_default();
            seen.push((code.clone(), (text, png)));
            // The same circle again closes its page. The page moved the rows: find it again.
            let (row, span) = circle(&pane, code)
                .unwrap_or_else(|| panic!("{cols}x{rows}: {code} not a target while open"));
            assert!(pane.click(row, ((span.0 + span.1) / 2) as usize, NOW));
            assert!(
                !pane.instrument_open.figure && pane.instrument_open.gauge.is_none(),
                "{code}"
            );
            pane.frame(cols, rows, NOW);
        }
        // Each circle's page differs from the others'.
        for (i, (ca, a)) in seen.iter().enumerate() {
            for (cb, b) in &seen[i + 1..] {
                assert_ne!(a, b, "{cols}x{rows}: {ca} and {cb} opened the same page");
            }
        }
        // A cell on the figure that is on no circle still opens the plain list.
        let row = pane
            .acts
            .iter()
            .find(|(_, a)| a["key"] == "instrument:figure" && a["gauges"].is_null())
            .map(|(r, _)| *r)
            .expect("a figure row outside the circles");
        assert!(pane.click(row, 0, NOW));
        assert!(pane.instrument_open.figure && pane.instrument_open.gauge.is_none());
    }
}

#[test]
fn the_outlook_speaks_from_the_first_reading_and_blends_in_a_finished_window() {
    use csr_engine::companion::paths::epoch;
    let resets = epoch("2026-10-18T14:08:00Z").unwrap();
    let line = |at: f64, used: f64, resets: f64| {
        format!("{{\"at\":{at},\"kind\":\"five_hour\",\"used\":{used},\"resets\":{resets}}}\n")
    };
    let frame = |paths: &Paths, cols: usize, rows: usize| {
        let mut pane = Pane::new(paths.clone(), &args("main", "sess-now", cols, rows));
        pane.pixels = true;
        pane.enable_cards();
        pane.frame(cols, rows, NOW);
        pane.instrument_metrics.clone().unwrap()
    };
    for (cols, rows) in [(76, 67), (44, 30)] {
        let (_home, paths) = state_home("02-running");
        // No history file at all: the dashboard still has its headline and a line for the window.
        let m = frame(&paths, cols, rows);
        let before = m.dash.expect("the dashboard");
        assert!(before.headline.is_some(), "{cols}x{rows}");
        let five = before.limits.iter().find(|l| l.code == "5H").expect("5H");
        assert!(
            five.line.contains("resets in"),
            "{cols}x{rows}: {}",
            five.line
        );
        assert!(!before.segments.is_empty(), "{cols}x{rows}: the bar");
        // A finished window that peaked at 60% is in the history: it is blended in.
        let history = paths.keep.join("usage-history.jsonl");
        let past = resets - 18000.;
        let body: String = (0..6)
            .map(|i| line(past - 14400. + 2400. * i as f64, 10. + 10. * i as f64, past))
            .collect();
        fs::write(&history, &body).unwrap();
        let after = frame(&paths, cols, rows).dash.expect("the dashboard");
        let five_after = after.limits.iter().find(|l| l.code == "5H").unwrap();
        assert_ne!(
            five_after.tick, five.tick,
            "{cols}x{rows}: the past window moves the forecast tick"
        );
        // The breakdown bar keeps its segments with a forecast, and every one has its legend.
        assert_eq!(after.segments.len(), before.segments.len(), "{cols}x{rows}");
        assert!(after.segments.iter().all(|s| s.legend.is_some()));
    }
}

#[test]
fn real_shaped_fresh_input_varies_and_flat_history_abstains() {
    for theme in [Theme::Dark, Theme::Light] {
        for expanded in [false, true] {
            // The legend under the quota circles outranks the plot under the ring: the closed
            // pane needs the taller 76x72 for both.
            let rows = if expanded { 67 } else { 72 };
            let (_home, paths) = state_home("12-fresh");
            let mut pane = Pane::new(paths, &args("main", "sess-now", 76, rows));
            pane.pixels = true;
            pane.enable_cards();
            pane.theme = theme;
            if expanded {
                pane.key("i");
            }
            pane.frame(76, rows, NOW);
            let plots = &pane.instrument_metrics.as_ref().unwrap().fresh_plots;
            // The dashboard draws its chart under the rows when `i` opens it, and not before.
            assert_eq!(plots.len(), usize::from(expanded), "expanded={expanded}");
            if !expanded {
                continue;
            }
            let p = &plots[0];
            assert_eq!(p.heights.len(), 24);
            let min = p.heights.iter().copied().fold(f32::INFINITY, f32::min);
            let max = p.heights.iter().copied().fold(0., f32::max);
            assert!((max - min) / p.region.3 >= 0.4);
            assert!(
                (min / max - 0.05).abs() < 0.001,
                "cache read or TTL metadata leaked into fresh height"
            );
            assert_eq!(p.compactions, [12]);
            let (_home, paths) = state_home("13-flat");
            let mut pane = Pane::new(paths, &args("main", "sess-now", 76, 67));
            pane.pixels = true;
            pane.enable_cards();
            pane.theme = theme;
            if expanded {
                pane.key("i");
            }
            pane.frame(76, 67, NOW);
            let m = pane.instrument_metrics.as_ref().unwrap();
            assert!(m.fresh_plots.is_empty());
            assert!(
                m.dash.as_ref().is_some_and(|d| !d.chart),
                "flat history draws no chart, and the dashboard keeps its room"
            );
        }
    }
}

#[test]
fn narrow_footer_keeps_every_key_labeled() {
    for state in ["01-empty", "02-running", "11-last-time"] {
        let (_home, paths) = state_home(state);
        let mut pane = Pane::new(paths, &args("main", "sess-now", 38, 30));
        pane.pixels = false;
        let shown = pane.frame(38, 30, NOW);
        let foot = text::plain(&shown[29]);
        assert!(foot.contains("i fig"), "{state}: {foot}");
        assert!(
            foot.contains("l chg") && foot.contains("t thr"),
            "{state}: {foot}"
        );
        assert!(
            foot.contains("b th") && foot.contains("q close"),
            "{state}: {foot}"
        );
        assert!(text::len(&foot) <= 38);
        if state == "11-last-time" {
            let content = shown.join("\n");
            assert!(
                content.contains("Last time here") && content.contains("abcdef1"),
                "{content}"
            );
        }
    }
}

#[test]
fn repository_history_fills_the_last_card_without_expanding_card_gaps() {
    for theme in [Theme::Dark, Theme::Light] {
        for (cols, rows) in [(38, 30), (44, 30), (60, 40), (76, 67)] {
            let (_home, paths) = state_home("11-last-time");
            let mut pane = Pane::new(paths, &args("main", "sess-now", cols, rows));
            pane.pixels = true;
            pane.enable_cards();
            pane.theme = theme;
            pane.frame(cols, rows, NOW);
            let m = pane.instrument_metrics.as_ref().unwrap();
            assert!((m.figure_region.0 - (16. + m.dream) - 12.).abs() < 0.1);
            let (help_y, help_h) = m.help_region.unwrap();
            assert!((help_y - (m.figure_region.0 + m.figure_region.1) - 12.).abs() < 0.1);
            assert!(m.history_rows >= 5, "{cols}x{rows}: {}", m.history_rows);
            let slack = help_y + help_h - 16. - m.history_bottom.unwrap();
            assert!(
                (0. ..18.1).contains(&slack),
                "{cols}x{rows}: unused content height {slack}"
            );
            assert!((m.last_card_gap_px - 12.).abs() <= 2.);
        }
    }
}

#[test]
fn native_weekly_quota_keeps_7d_and_magenta_csr_points() {
    for state in ["01-empty", "02-running"] {
        for theme in [Theme::Dark, Theme::Light] {
            let (_home, paths) = state_home(state);
            let mut pane = Pane::new(paths, &args("main", "sess-now", 38, 30));
            pane.pixels = false;
            pane.theme = theme;
            let shown = pane.frame(38, 30, NOW).join("\n");
            let t = theme.tokens();
            let colour = text::fg([t.csr_share[0], t.csr_share[1], t.csr_share[2]]);
            assert!(shown.contains(&format!("{colour}CSR ")), "{shown}");
            let plain = text::plain(&shown);
            assert!(plain.contains("7D ") && plain.contains(" pts"), "{plain}");
        }
    }
}

/// A session whose dreamer found nothing to ask but left observations: three places Claude went
/// wrong and two things that should be a skill or a hook. No what-if is open, so the number keys
/// are free of the dream answers.
fn observed_home() -> (tempfile::TempDir, Paths) {
    let (home, paths) = state_home("08b-nothing");
    let log = paths.keep.join("dreams.jsonl");
    let mut text = fs::read_to_string(&log).unwrap();
    text.push_str(
        &json!({"kind":"look","session":"sess-now","at":NOW - 20.,"outcome":"none",
            "confusing_claude":[
                {"line":"Misread which pane should change.","evidence":"Turn 4: 'the companion pane'"},
                {"line":"Repeated the rejected approach.","evidence":"Turns 12 and 15: 'keep it local'",
                    "explain":"Claude tried the local cache again after you ruled it out.",
                    "next":"Tell Claude once, in CLAUDE.md, that the local cache is out."},
                {"line":"Said the test passed without running it.","evidence":"'it should pass now'"}],
            "should_be":[
                {"kind":"hook","line":"Check the card height after each layout edit.","evidence":"Turns 4 and 8: 'at most 30%'"},
                {"kind":"skill","line":"Bump the version in both files.","evidence":"Turns 9 and 20: 'both files'"}]})
        .to_string(),
    );
    text.push('\n');
    fs::write(&log, text).unwrap();
    (home, paths)
}
fn plain_rows(pane: &mut Pane, cols: usize, rows: usize) -> Vec<String> {
    pane.frame(cols, rows, NOW)
        .iter()
        .map(|l| text::plain(l).trim_end().to_owned())
        .collect()
}
/// The first row with anything on it: text mode sits its content above the keys.
fn first_row(rows: &[String]) -> &str {
    rows.iter()
        .map(String::as_str)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}
fn row_of(rows: &[String], starts: &str) -> usize {
    rows.iter()
        .position(|l| l.starts_with(starts))
        .unwrap_or_else(|| panic!("no row starts with {starts:?}: {rows:#?}"))
}

#[test]
fn number_keys_open_an_observation_item_and_the_popup_is_modal() {
    let (_home, paths) = observed_home();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 100, 60));
    let rows = plain_rows(&mut pane, 100, 60);
    assert!(rows.iter().any(|l| l == "Confused Claude"), "{rows:#?}");
    assert!(
        rows.last().unwrap().contains("o details"),
        "{:?}",
        rows.last()
    );
    // 2 opens the second item of the first card, in full, with the dreamer's own words.
    assert!(pane.key("2"));
    let rows = plain_rows(&mut pane, 100, 60);
    let top = row_of(&rows, "Confused Claude · 2 of 3");
    assert_eq!(rows[top + 1], "Repeated the rejected approach.");
    assert!(rows[top + 2].starts_with("Evidence: Turns 12 and 15"));
    assert_eq!(
        rows[top + 3],
        "What happened: Claude tried the local cache again after you ruled it out."
    );
    assert!(rows[top + 4].starts_with("Next step for you: Tell Claude once, in CLAUDE.md"));
    let footer = rows.last().unwrap();
    assert!(
        footer.contains("c copy") && footer.contains("esc close") && !footer.contains("o details"),
        "{footer}"
    );
    // Modal: the keys of the pane underneath do nothing while it is up.
    for key in ["l", "t", "i", "d", "a"] {
        pane.key(key);
    }
    let after = plain_rows(&mut pane, 100, 60);
    assert_eq!(after, rows);
    // The item without turn numbers still gets a composed explanation.
    pane.key("3");
    let rows = plain_rows(&mut pane, 100, 60);
    assert!(
        rows.iter()
            .any(|l| l.starts_with("What happened: The dreamer flagged")),
        "{rows:#?}"
    );
    assert!(rows
        .iter()
        .any(|l| l.starts_with("Next step for you: Re-read the quoted evidence")));
    // j and k step through every item of the cards in the order they are drawn.
    pane.key("j");
    assert_eq!(
        first_row(&plain_rows(&mut pane, 100, 60)),
        "Should be a skill / memory / tool · 1 of 2 · hook"
    );
    pane.key("k");
    assert!(first_row(&plain_rows(&mut pane, 100, 60)).starts_with("Confused Claude · 3 of 3"));
    // A number past the card's last item changes nothing.
    pane.key("5");
    assert!(first_row(&plain_rows(&mut pane, 100, 60)).starts_with("Confused Claude · 3 of 3"));
    // Tab goes to the next item, which here is the first of the other card.
    pane.key("tab");
    let rows = plain_rows(&mut pane, 100, 60);
    assert_eq!(
        first_row(&rows),
        "Should be a skill / memory / tool · 1 of 2 · hook"
    );
    // esc closes the popup and the pane is as it was.
    pane.key("escape");
    let rows = plain_rows(&mut pane, 100, 60);
    assert!(rows.iter().any(|l| l == "Confused Claude"), "{rows:#?}");
    // q closes an open popup first; the next q closes the pane.
    pane.key("o");
    assert!(first_row(&plain_rows(&mut pane, 100, 60)).starts_with("Confused Claude · 1 of 3"));
    assert!(pane.key("q"));
    assert!(plain_rows(&mut pane, 100, 60)
        .iter()
        .any(|l| l == "Confused Claude"));
    assert!(!pane.key("q"));
}

#[test]
fn c_copies_the_item_with_its_explanation_or_a_draft_for_a_should_be_item() {
    let (_home, paths) = observed_home();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 100, 60));
    plain_rows(&mut pane, 100, 60);
    // Nothing is copied until asked.
    pane.key("o");
    plain_rows(&mut pane, 100, 60);
    assert_eq!(pane.take_copy(), None);
    pane.key("c");
    let copied = pane.take_copy().expect("c copies");
    assert!(
        copied.starts_with("Confused Claude: Misread which pane should change.\nEvidence: Turn 4: 'the companion pane'\nWhat happened: The dreamer flagged"),
        "{copied}"
    );
    assert!(copied.contains("Next step: Re-read turn 4"), "{copied}");
    assert_eq!(pane.take_copy(), None, "taken once");
    assert!(plain_rows(&mut pane, 100, 60)
        .iter()
        .any(|l| l == "Copied."));
    // A should-be skill copies a draft, ready to paste, and writes nothing itself.
    for _ in 0..3 {
        pane.key("tab");
    }
    pane.key("2");
    plain_rows(&mut pane, 100, 60);
    assert!(plain_rows(&mut pane, 100, 60)
        .last()
        .unwrap()
        .contains("c copy draft"));
    pane.key("c");
    let draft = pane.take_copy().expect("c copies the draft");
    assert!(
        draft.starts_with("---\nname: bump-the-version-in-both\ndescription: Bump the version in both files.\n---\n"),
        "{draft}"
    );
    assert!(
        draft.contains("Seen: Turns 9 and 20: 'both files'"),
        "{draft}"
    );
    // Closing and reopening forgets that it was copied.
    pane.key("escape");
    pane.key("o");
    assert!(!plain_rows(&mut pane, 100, 60)
        .iter()
        .any(|l| l == "Copied."));
}

#[test]
fn a_ready_dream_keeps_the_number_keys_and_o_opens_the_cards() {
    let (_home, paths) = state_home("02-running");
    let mut pane = Pane::new(paths, &args("main", "sess-now", 100, 60));
    let rows = plain_rows(&mut pane, 100, 60);
    assert!(rows.iter().any(|l| l.starts_with("1 helpful")), "{rows:#?}");
    // o opens the first observation item.
    pane.key("o");
    let rows = plain_rows(&mut pane, 100, 60);
    assert!(
        first_row(&rows).starts_with("Confused Claude · 1 of 1"),
        "{rows:#?}"
    );
    pane.key("escape");
    plain_rows(&mut pane, 100, 60);
    // 1 answers the dream, as before; it opens nothing.
    pane.key("1");
    let rows = plain_rows(&mut pane, 100, 60);
    assert!(
        !rows.iter().any(|l| l.starts_with("Confused Claude ·")),
        "{rows:#?}"
    );
}

#[test]
fn clicking_an_observation_row_opens_it_and_any_click_closes_it() {
    let (_home, paths) = observed_home();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 100, 60));
    let rows = plain_rows(&mut pane, 100, 60);
    let row = row_of(&rows, "Repeated the rejected approach.");
    assert!(pane.acts.contains_key(&row), "the row is a click target");
    assert!(pane.click(row, 10, NOW));
    let shown = plain_rows(&mut pane, 100, 60);
    assert!(
        first_row(&shown).starts_with("Confused Claude · 2 of 3"),
        "{shown:#?}"
    );
    // The popup's footer answers: a click on c copies.
    let footer = shown.len() - 1;
    let at = shown[footer].find("c copy").expect("footer c");
    assert!(pane.click(footer, at, NOW));
    assert!(pane.take_copy().is_some());
    // Any other click closes.
    assert!(pane.click(5, 5, NOW));
    let rows = plain_rows(&mut pane, 100, 60);
    assert!(rows.iter().any(|l| l == "Confused Claude"), "{rows:#?}");
    // A should-be row opens its own card.
    let row = row_of(&rows, "skill: Bump the version in both files.");
    pane.click(row, 3, NOW);
    let shown = plain_rows(&mut pane, 100, 60);
    assert!(
        first_row(&shown).starts_with("Should be a skill / memory / tool · 2 of 2 · skill"),
        "{shown:#?}"
    );
}

#[test]
fn the_popup_repaints_once_on_open_and_once_on_close() {
    use csr_engine::companion::render::kitty;
    let (_home, paths) = observed_home();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 76, 67));
    pane.pixels = true;
    pane.enable_cards();
    let (cols, rows) = (76, 67);
    let shot = |pane: &mut Pane| {
        let written = pane.frame(cols, rows, NOW);
        (written, kitty::bands(&pane.placed))
    };
    let (written, closed) = shot(&mut pane);
    let written_closed = written.clone();
    let drawn = kitty::Drawn {
        cols,
        rows,
        written: written.clone(),
        placed: closed.clone(),
    };
    // Every observation row is a click target in the picture.
    let places = pane
        .instrument_metrics
        .as_ref()
        .unwrap()
        .observation_rows
        .len();
    assert_eq!(places, 5);
    // Five items, seven rows: every drawn row of an item answers a click.
    let keys: Vec<&str> = pane
        .acts
        .values()
        .map(|a| string(&a["key"]))
        .filter(|k| k.starts_with("observe:"))
        .collect();
    assert_eq!(keys.len(), 7, "{:?}", pane.acts);
    let mut distinct = keys.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 5, "{:?}", pane.acts);
    // Opening changes the picture once; the same popup frame again changes nothing.
    pane.key("o");
    let (written, open) = shot(&mut pane);
    assert_ne!(open, closed);
    let opened = kitty::repaint_pictures(Some(&drawn), cols, rows, &written, &open);
    assert!(!opened.contains("\x1b[2J"), "no full clear on open");
    assert!(opened.contains("\x1b_G"), "the picture is sent");
    let drawn_open = kitty::Drawn {
        cols,
        rows,
        written: written.clone(),
        placed: open.clone(),
    };
    let (written2, open2) = shot(&mut pane);
    assert_eq!(
        kitty::repaint_pictures(Some(&drawn_open), cols, rows, &written2, &open2),
        ""
    );
    // Closing puts the pane back exactly, in one more repaint.
    pane.key("escape");
    let (written3, back) = shot(&mut pane);
    assert_eq!(back, closed);
    assert!(!kitty::repaint_pictures(Some(&drawn_open), cols, rows, &written3, &back).is_empty());
    assert_eq!(written3, written_closed);
    // A click on a card row in the picture opens its popup.
    let (row, _) = pane
        .acts
        .iter()
        .find(|(_, a)| a["key"] == "observe:0:1")
        .map(|(r, a)| (*r, a.clone()))
        .expect("second confusing row is a target");
    pane.click(row, 20, NOW);
    let (_, popup) = shot(&mut pane);
    assert_ne!(popup, closed);
    let metrics = pane.instrument_metrics.as_ref().unwrap();
    assert!(metrics.observation_rows.iter().any(|r| r.1 == 1));
}

#[test]
fn every_wrapped_row_of_an_observation_opens_it_in_the_text_pane() {
    let (_home, paths) = observed_home();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 30, 60));
    let rows = plain_rows(&mut pane, 30, 60);
    let first = row_of(&rows, "Repeated the rejected");
    // The item wraps onto a second row at 30 columns; that row answers too.
    assert!(!rows[first + 1].is_empty(), "{rows:#?}");
    assert_eq!(pane.acts[&first]["key"], pane.acts[&(first + 1)]["key"]);
    assert!(pane.click(first + 1, 3, NOW));
    let shown = plain_rows(&mut pane, 30, 60);
    assert!(
        first_row(&shown).starts_with("Confused Claude"),
        "{shown:#?}"
    );
}

#[test]
fn l_t_and_i_each_change_the_picture_through_the_keys_the_footer_names() {
    use csr_engine::companion::render::kitty;
    for (cols, rows) in [(40, 50), (76, 67)] {
        let (_home, paths) = observed_home();
        let mut pane = Pane::new(paths, &args("main", "sess-now", cols, rows));
        pane.pixels = true;
        pane.enable_cards();
        let shot = |pane: &mut Pane| {
            pane.frame(cols, rows, NOW);
            kitty::bands(&pane.placed)
        };
        let closed = shot(&mut pane);
        assert!(!closed.is_empty(), "{cols}x{rows}: a picture");
        for key in ["l", "t", "i"] {
            pane.key(key);
            let open = shot(&mut pane);
            assert_ne!(open, closed, "{cols}x{rows}: {key} changes the picture");
            let m = pane.instrument_metrics.as_ref().unwrap();
            match key {
                "i" if m.dash.is_some() => {
                    assert!(m.figure_detail_region.is_none(), "{cols}x{rows}")
                }
                "i" => assert!(m.figure_detail_region.is_some(), "{cols}x{rows}"),
                _ => assert!(m.drill_region.is_some(), "{cols}x{rows}: {key} got a card"),
            }
            pane.key(key);
            assert_eq!(
                shot(&mut pane),
                closed,
                "{cols}x{rows}: {key} again closes it"
            );
        }
    }
}

#[test]
fn a_row_the_picture_cut_opens_its_words_in_full_on_click() {
    let (_home, paths) = state_home("02-running");
    // One change whose words are longer than any row of the picture.
    let con = rusqlite::Connection::open(&paths.db).unwrap();
    let raw: String = con
        .query_row(
            "SELECT value FROM meta WHERE key='story_map:sess-now'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut map: Value = serde_json::from_str(&raw).unwrap();
    // First, so the story card (which shows three) draws it.
    map["current"]["entries"].as_array_mut().unwrap().insert(0, json!({
        "kind":"added","claim":"Long change",
        "now":"The pane now keeps every changed row whole in its popup, however many words the picture had to leave out of the row itself, so nothing is ever lost to the ellipsis.",
        "was":"Cut rows ended in an ellipsis and offered nothing to open.",
        "before":[],"after":[{"scope":"render/layout/a_deeply_nested_directory/instrument_drawing_and_layout_engine.rs","sym":"draw_every_card_in_the_pane_from_the_top","mark":"+"}],"receipts":["oid:7625eed7"]}));
    con.execute(
        "UPDATE meta SET value=? WHERE key='story_map:sess-now'",
        [map.to_string()],
    )
    .unwrap();
    drop(con);
    let (cols, rows) = (36, 60);
    let mut pane = Pane::new(paths, &args("main", "sess-now", cols, rows));
    pane.pixels = true;
    pane.enable_cards();
    pane.key("l");
    pane.frame(cols, rows, NOW);
    let cuts = pane.instrument_metrics.as_ref().unwrap().cuts.clone();
    assert!(!cuts.is_empty(), "a 36-column pane cuts some row");
    // Rows with the same words share one popup item, numbered in order of first appearance.
    let mut seen: Vec<(&str, &str)> = vec![];
    for cut in &cuts {
        let at = (cut.title, cut.full.as_str());
        let index = seen.iter().position(|s| *s == at).unwrap_or_else(|| {
            seen.push(at);
            seen.len() - 1
        });
        pane.frame(cols, rows, NOW);
        let key = format!("observe:2:{index}");
        let (row, _) = pane
            .acts
            .iter()
            .find(|(_, a)| a["key"] == key.as_str())
            .map(|(r, a)| (*r, a.clone()))
            .unwrap_or_else(|| panic!("no click target for {cut:?}: {:?}", pane.acts));
        assert!(pane.click(row, 4, NOW));
        pane.frame(cols, rows, NOW);
        // The popup is up: c copies the title and every word of the row.
        pane.key("c");
        let copied = pane
            .take_copy()
            .unwrap_or_else(|| panic!("no popup for {cut:?}"));
        assert!(copied.starts_with(cut.title), "{copied}");
        assert!(copied.contains(&cut.full), "{copied}");
        assert!(!copied.contains('…'), "{copied}");
        pane.key("escape");
        pane.frame(cols, rows, NOW);
    }
}

/// A herdr that logs every call and answers `process-info` with a foreground process named
/// `argv0`. HERDR_BIN_PATH is process-wide, so the tests that use it take turns.
struct FakeHerdr {
    dir: tempfile::TempDir,
    _turn: std::sync::MutexGuard<'static, ()>,
}
impl FakeHerdr {
    fn new(argv0: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        static TURNS: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let turn = TURNS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let (log, who) = (dir.path().join("log"), dir.path().join("argv0"));
        fs::write(&who, argv0).unwrap();
        let script = dir.path().join("herdr");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$@\" >> {log}\ncase \"$2\" in\nprocess-info)\n  who=$(cat {who})\n  printf '{{\"result\":{{\"process_info\":{{\"foreground_process_group_id\":7,\"foreground_processes\":[{{\"pid\":7,\"argv0\":\"%s\",\"argv\":[\"%s\"]}}]}}}}}}' \"$who\" \"$who\"\n  ;;\nesac\n",
                log = log.display(),
                who = who.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("HERDR_BIN_PATH", &script);
        std::env::remove_var("HERDR_PANE_ID");
        Self { dir, _turn: turn }
    }
    /// Every call herdr got, one per line, in order.
    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.dir.path().join("log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}
impl Drop for FakeHerdr {
    fn drop(&mut self) {
        std::env::remove_var("HERDR_BIN_PATH");
    }
}
fn agent_args(cols: usize, rows: usize) -> PaneArgs {
    let mut a = args("main", "sess-now", cols, rows);
    a.agent = Some("fake-agent".into());
    a
}
/// What the popup that is open puts on the clipboard: it names the item.
fn opened_item(pane: &mut Pane) -> Option<String> {
    // The popup is drawn before a key reaches it, as the watch loop does after every key.
    pane.frame(100, 60, NOW);
    let _ = pane.take_copy();
    pane.key("c");
    pane.take_copy()
}
/// `opened_item` for a pane of another size.
fn opened_at(pane: &mut Pane, cols: usize, rows: usize) -> Option<String> {
    pane.frame(cols, rows, NOW);
    let _ = pane.take_copy();
    pane.key("c");
    pane.take_copy()
}
const ITEMS: [&str; 6] = [
    "Misread which pane should change.",
    "Repeated the rejected approach.",
    "Said the test passed without running it.",
    "Check the card height after each layout edit.",
    "Bump the version in both files.",
    "Where the work stands\n",
];
fn which_item(copy: &str) -> Option<usize> {
    ITEMS.iter().position(|item| {
        if item.ends_with('\n') {
            copy.starts_with(item)
        } else {
            copy.contains(item)
        }
    })
}

#[test]
fn every_row_of_the_three_cards_opens_its_own_item_by_click_or_by_a_step() {
    use csr_engine::companion::render::{kitty, theme::BODY_LINE};
    let mut table = vec![];
    for (cols, rows) in [(46, 30), (40, 50)] {
        let (_home, paths) = observed_home();
        let mut pane = Pane::new(paths, &args("main", "sess-now", cols, rows));
        pane.pixels = true;
        pane.enable_cards();
        pane.frame(cols, rows, NOW);
        let m = pane.instrument_metrics.clone().expect("a picture");
        let cell = kitty::cell_size();
        let tall = pane.placed[0].rows as f32 * cell.1 as f32;
        let at = |y: f32| y / m.height * tall;
        // The oracle: of the picture rows that touch a terminal row, the one with most of it.
        let half = BODY_LINE / 2. - 0.5;
        let mut spans: Vec<(usize, f32, f32)> = vec![];
        let mut order = vec![];
        for (card, index, y) in &m.observation_rows {
            let item = match card {
                csr_engine::companion::render::observation::ObsCard::Confusing => *index,
                _ => 3 + *index,
            };
            order.push(item);
            spans.push((item, at(y - half), at(y + half)));
        }
        for (card, top, bottom) in &m.observation_titles {
            let item = match card {
                csr_engine::companion::render::observation::ObsCard::Confusing => 0,
                _ => 3,
            };
            spans.push((item, at(*top), at(*bottom)));
        }
        let (top, bottom, _, _) = m.story_open.clone().expect("a story card");
        spans.push((5, at(top), at(bottom)));
        assert_eq!(
            order,
            [0, 1, 2, 3, 4],
            "{cols}x{rows}: the cards, in drawn order"
        );
        let mut clicked: Vec<Option<usize>> = vec![];
        for row in 0..rows - 1 {
            let mut best: Option<(f32, usize)> = None;
            for (item, from, to) in &spans {
                let overlap = to.min(((row + 1) as u32 * cell.1) as f32)
                    - from.max((row as u32 * cell.1) as f32);
                if overlap > 0. && best.is_none_or(|b| overlap > b.0) {
                    best = Some((overlap, *item));
                }
            }
            pane.frame(cols, rows, NOW);
            pane.click(row, 4, NOW);
            pane.frame(cols, rows, NOW);
            let got = opened_at(&mut pane, cols, rows).and_then(|c| which_item(&c));
            pane.key("escape");
            pane.frame(cols, rows, NOW);
            // A row the observation spans do not touch belongs to something else, and opens
            // none of the three cards' items unless the oracle says so.
            assert_eq!(got, best.map(|b| b.1), "{cols}x{rows} row {row}");
            clicked.push(got);
            if let Some(item) = got {
                table.push(format!(
                    "{cols}x{rows} click row {row}: {}",
                    ITEMS[item].trim()
                ));
            }
        }
        // Every item is one key away from the ones a click can single out: o opens the first,
        // and each down steps to the next, round the three cards.
        pane.key("o");
        pane.frame(cols, rows, NOW);
        let mut walked = vec![];
        for _ in 0..ITEMS.len() {
            walked.push(opened_at(&mut pane, cols, rows).and_then(|c| which_item(&c)));
            pane.key("down");
            pane.frame(cols, rows, NOW);
        }
        assert_eq!(
            walked,
            (0..ITEMS.len()).map(Some).collect::<Vec<_>>(),
            "{cols}x{rows}: down visits every item once"
        );
        // And back: up from the first goes round to the story, shift+tab likewise.
        pane.key("escape");
        pane.key("o");
        pane.key("up");
        assert_eq!(
            opened_at(&mut pane, cols, rows).and_then(|c| which_item(&c)),
            Some(5)
        );
        pane.key("shift+tab");
        assert_eq!(
            opened_at(&mut pane, cols, rows).and_then(|c| which_item(&c)),
            Some(4)
        );
        pane.key("tab");
        assert_eq!(
            opened_at(&mut pane, cols, rows).and_then(|c| which_item(&c)),
            Some(5)
        );
        pane.key("escape");
        // Every item is reached by a click or by those steps: none is out of reach.
        for (item, words) in ITEMS.iter().enumerate() {
            let direct = clicked.contains(&Some(item));
            table.push(format!(
                "{cols}x{rows} {}: {}",
                words.trim(),
                if direct { "a click" } else { "keys only" }
            ));
        }
        // The number keys open the first card's items, as before.
        pane.key("3");
        assert_eq!(
            opened_at(&mut pane, cols, rows).and_then(|c| which_item(&c)),
            Some(2)
        );
        pane.key("escape");
    }
    println!("{}", table.join("\n"));
}

/// The real session's shape: five Confused Claude rows and five should-be rows.
fn five_and_five_home() -> (tempfile::TempDir, Paths) {
    let (home, paths) = state_home("08b-nothing");
    let log = paths.keep.join("dreams.jsonl");
    let mut text = fs::read_to_string(&log).unwrap();
    let confusing: Vec<Value> = (1..=5)
        .map(|n| json!({"line": format!("Confused row number {n}."), "evidence": format!("Turn {n}: 'quote {n}'")}))
        .collect();
    let should: Vec<Value> = (1..=5)
        .map(|n| json!({"kind": "skill", "line": format!("Should-be row number {n}."), "evidence": format!("Turns {n} and 9: 'quote'")}))
        .collect();
    text.push_str(
        &json!({"kind":"look","session":"sess-now","at":NOW - 20.,"outcome":"none",
            "confusing_claude":confusing,"should_be":should})
        .to_string(),
    );
    text.push('\n');
    fs::write(&log, text).unwrap();
    (home, paths)
}

#[test]
fn a_click_on_row_n_of_five_opens_n_of_five_in_the_text_and_the_picture() {
    // Text pane: every row answers with its own place.
    let (_home, paths) = five_and_five_home();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 100, 60));
    for n in 1..=5 {
        let rows = plain_rows(&mut pane, 100, 60);
        let row = row_of(&rows, &format!("Confused row number {n}."));
        assert!(pane.click(row, 5, NOW));
        let shown = plain_rows(&mut pane, 100, 60);
        assert!(
            first_row(&shown).starts_with(&format!("Confused Claude · {n} of 5")),
            "{n}: {shown:#?}"
        );
        pane.key("escape");
        let rows = plain_rows(&mut pane, 100, 60);
        let row = row_of(&rows, &format!("skill: Should-be row number {n}."));
        assert!(pane.click(row, 5, NOW));
        let shown = plain_rows(&mut pane, 100, 60);
        assert!(
            first_row(&shown).starts_with(&format!("Should be a skill / memory / tool · {n} of 5")),
            "{n}: {shown:#?}"
        );
        pane.key("escape");
    }
    // Picture at 46x30: a click opens the item the oracle names, never its neighbour; the rows
    // the cell grid cannot single out are a step away, and each step lands on the next number.
    let (_home, paths) = five_and_five_home();
    let (cols, rows) = (46, 30);
    let mut pane = Pane::new(paths, &args("main", "sess-now", cols, rows));
    pane.pixels = true;
    pane.enable_cards();
    pane.frame(cols, rows, NOW);
    let open = pane.hit_rows();
    let mut seen = vec![];
    for row in 0..rows - 1 {
        pane.frame(cols, rows, NOW);
        pane.click(row, 4, NOW);
        let copy = opened_at(&mut pane, cols, rows);
        pane.key("escape");
        if let Some(copy) = copy {
            if !copy.contains(" row number ") {
                assert!(open[&row].starts_with("observe:2"), "row {row}: {copy}");
                continue;
            }
            let word = if copy.starts_with("Confused Claude: ") {
                "Confused"
            } else {
                "Should-be"
            };
            let n: usize = copy
                .split(&format!("{word} row number "))
                .nth(1)
                .and_then(|r| r.split('.').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            let want = &open[&row];
            let card = usize::from(word == "Should-be");
            assert_eq!(*want, format!("observe:{card}:{}", n - 1), "row {row}");
            seen.push((word, n));
        }
    }
    // Each card's first row answers: row 1 opens "1 of 5", not "3 of 5".
    assert!(
        seen.contains(&("Confused", 1)) && seen.contains(&("Should-be", 1)),
        "{seen:?}"
    );
    pane.key("o");
    for n in 1..=10 {
        let copy = opened_at(&mut pane, cols, rows).unwrap();
        let (word, k) = if n <= 5 {
            ("Confused", n)
        } else {
            ("Should-be", n - 5)
        };
        assert!(
            copy.contains(&format!("{word} row number {k}."))
                || copy.contains(&format!("{} row number {k}.", word.to_lowercase())),
            "{n}: {copy}"
        );
        pane.key("down");
    }
}

#[test]
fn a_title_and_the_story_open_in_the_text_pane_too() {
    let (_home, paths) = observed_home();
    let mut pane = Pane::new(paths, &args("main", "sess-now", 100, 60));
    let rows = plain_rows(&mut pane, 100, 60);
    let title = row_of(&rows, "Confused Claude");
    assert!(pane.click(title, 3, NOW));
    let shown = plain_rows(&mut pane, 100, 60);
    assert!(
        first_row(&shown).starts_with("Confused Claude · 1 of 3"),
        "{shown:#?}"
    );
    pane.key("escape");
    let rows = plain_rows(&mut pane, 100, 60);
    let title = row_of(&rows, "Should be a skill / memory / tool");
    assert!(pane.click(title, 3, NOW));
    assert!(first_row(&plain_rows(&mut pane, 100, 60))
        .starts_with("Should be a skill / memory / tool · 1 of 2 · hook"));
    pane.key("escape");
    let rows = plain_rows(&mut pane, 100, 60);
    let story = row_of(&rows, "Story ·");
    assert!(pane.click(story, 3, NOW));
    let shown = plain_rows(&mut pane, 100, 60);
    assert_eq!(first_row(&shown), "Where the work stands", "{shown:#?}");
    // A story has no send or dismiss: the footer offers copy and close.
    let footer = shown.last().unwrap();
    assert!(
        footer.contains("c copy") && footer.contains("esc close"),
        "{footer}"
    );
    assert!(
        !footer.contains("s send") && !footer.contains("x "),
        "{footer}"
    );
    // The story is the last step of the cards: tab goes round to the first item.
    pane.key("tab");
    assert!(first_row(&plain_rows(&mut pane, 100, 60)).starts_with("Confused Claude · 1 of 3"));
}

#[test]
fn s_types_the_item_into_claude_unsent_and_says_so_when_it_cannot() {
    let fake = FakeHerdr::new("claude");
    let (_home, paths) = observed_home();
    let mut pane = Pane::new(paths, &agent_args(100, 60));
    plain_rows(&mut pane, 100, 60);
    // Confused Claude: the line and its evidence, quoted, then the ask.
    pane.key("o");
    plain_rows(&mut pane, 100, 60);
    pane.key("s");
    let calls = fake.calls();
    let typed: Vec<&str> = calls
        .iter()
        .filter_map(|c| c.strip_prefix("pane send-text fake-agent "))
        .collect();
    assert_eq!(
        typed,
        [
            "Here is a note offered by CSR (claude-self-reflect):",
            "> Confused Claude: Misread which pane should change.",
            "> Evidence: Turn 4: 'the companion pane'",
            "You may have gone off course here. Please say what you got wrong and correct course.",
        ],
        "{calls:#?}"
    );
    // Lines are joined with ctrl+j; nothing ever presses Enter.
    let keys: Vec<&str> = calls
        .iter()
        .filter_map(|c| c.strip_prefix("pane send-keys fake-agent "))
        .collect();
    assert!(
        !keys.is_empty() && keys.iter().all(|k| *k == "ctrl+j"),
        "{calls:#?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.to_lowercase().contains("enter") || c.contains("return")),
        "{calls:#?}"
    );
    let rows = plain_rows(&mut pane, 100, 60);
    assert!(
        rows.iter()
            .any(|l| l == "Typed into Claude's prompt. Press Enter there to send."),
        "{rows:#?}"
    );
    // A should-be skill asks Claude to make it, with the draft quoted.
    for _ in 0..4 {
        pane.key("tab");
    }
    plain_rows(&mut pane, 100, 60);
    let before = fake.calls().len();
    pane.key("s");
    let calls = fake.calls();
    let typed: Vec<&str> = calls[before..]
        .iter()
        .filter_map(|c| c.strip_prefix("pane send-text fake-agent "))
        .collect();
    assert!(
        typed.contains(&"> skill: Bump the version in both files."),
        "{typed:#?}"
    );
    assert!(
        typed.contains(&"> name: bump-the-version-in-both"),
        "{typed:#?}"
    );
    assert_eq!(
        typed.last(),
        Some(&"Please make this skill from the draft above, and show me what you wrote.")
    );
    assert!(
        !calls[before..]
            .iter()
            .any(|c| c.to_lowercase().contains("enter")),
        "{calls:#?}"
    );
    // A pane that is not Claude gets nothing, and the popup says so.
    fs::write(fake.dir.path().join("argv0"), "zsh").unwrap();
    let before = fake.calls().len();
    plain_rows(&mut pane, 100, 60);
    pane.key("s");
    assert!(fake.calls()[before..]
        .iter()
        .all(|c| !c.starts_with("pane send-text")));
    let rows = plain_rows(&mut pane, 100, 60);
    assert!(
        rows.iter()
            .any(|l| l == "Claude is not at its prompt. Nothing was typed."),
        "{rows:#?}"
    );
    // The story has nothing to send.
    pane.key("escape");
    let rows = plain_rows(&mut pane, 100, 60);
    let story = row_of(&rows, "Story ·");
    pane.click(story, 3, NOW);
    plain_rows(&mut pane, 100, 60);
    let before = fake.calls().len();
    pane.key("s");
    assert_eq!(fake.calls().len(), before);
}

#[test]
fn s_with_no_claude_pane_types_nothing_and_says_so_in_one_line() {
    let fake = FakeHerdr::new("claude");
    let (_home, paths) = observed_home();
    // No --agent, and this process is in no herdr pane to find a neighbour from.
    let mut pane = Pane::new(paths, &args("main", "sess-now", 100, 60));
    plain_rows(&mut pane, 100, 60);
    pane.key("o");
    plain_rows(&mut pane, 100, 60);
    pane.key("s");
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    let rows = plain_rows(&mut pane, 100, 60);
    assert_eq!(
        rows.iter()
            .filter(|l| l.contains("No Claude pane to type into."))
            .count(),
        1,
        "{rows:#?}"
    );
}

#[test]
fn x_hides_the_item_on_the_next_frame_and_after_a_restart_and_records_a_no() {
    let (_home, paths) = observed_home();
    let mut pane = Pane::new(paths.clone(), &args("main", "sess-now", 100, 60));
    let rows = plain_rows(&mut pane, 100, 60);
    assert!(rows.iter().any(|l| l.starts_with("Repeated the rejected")));
    pane.key("2");
    assert!(opened_item(&mut pane).is_some_and(|c| c.contains("Repeated the rejected")));
    pane.key("x");
    let rows = plain_rows(&mut pane, 100, 60);
    assert!(
        !rows.iter().any(|l| l.contains("Repeated the rejected")),
        "{rows:#?}"
    );
    assert!(
        rows.iter().any(|l| l == "Confused Claude"),
        "the popup closed: {rows:#?}"
    );
    assert!(rows.iter().any(|l| l.starts_with("Misread which pane")));
    assert!(rows.iter().any(|l| l.starts_with("Said the test passed")));
    // The no is in the likes log, under this session's name.
    let likes = fs::read_to_string(paths.keep.join("likes.jsonl")).unwrap();
    let line: Value = serde_json::from_str(likes.lines().last().unwrap()).unwrap();
    assert!(
        string(&line["key"]).starts_with("observation:sess-now:confusing_claude:"),
        "{line}"
    );
    assert_eq!(
        (line["liked"].clone(), string(&line["source"])),
        (json!(false), "observation")
    );
    assert_eq!(string(&line["title"]), "Repeated the rejected approach.");
    // A pane started afterwards still hides it; another session's pane would not.
    let mut again = Pane::new(paths.clone(), &args("main", "sess-now", 100, 60));
    let rows = plain_rows(&mut again, 100, 60);
    assert!(
        !rows.iter().any(|l| l.contains("Repeated the rejected")),
        "{rows:#?}"
    );
    // It shows the other items in the same order, numbered again from 1.
    again.key("o");
    assert!(first_row(&plain_rows(&mut again, 100, 60)).starts_with("Confused Claude · 1 of 2"));
    again.key("escape");
}

#[test]
fn the_popups_footer_keys_answer_a_click_in_the_text_pane_and_in_the_picture() {
    let _fake = FakeHerdr::new("claude");
    let (_home, paths) = observed_home();
    let mut pane = Pane::new(paths.clone(), &args("main", "sess-now", 100, 60));
    plain_rows(&mut pane, 100, 60);
    pane.key("o");
    let shown = plain_rows(&mut pane, 100, 60);
    let footer = shown.len() - 1;
    for key in ["c copy", "s send", "x dismiss", "tab next", "esc close"] {
        assert!(shown[footer].contains(key), "{key}: {:?}", shown[footer]);
    }
    // s send: no agent, so the popup says so, and stays.
    pane.click(footer, shown[footer].find("s send").unwrap(), NOW);
    let shown = plain_rows(&mut pane, 100, 60);
    assert!(
        shown.iter().any(|l| l == "No Claude pane to type into."),
        "{shown:#?}"
    );
    // tab next opens the next item.
    pane.click(footer, shown[footer].find("tab next").unwrap(), NOW);
    let shown = plain_rows(&mut pane, 100, 60);
    assert!(
        first_row(&shown).starts_with("Confused Claude · 2 of 3"),
        "{shown:#?}"
    );
    // x dismiss hides it and closes the popup.
    pane.click(footer, shown[footer].find("x dismiss").unwrap(), NOW);
    let rows = plain_rows(&mut pane, 100, 60);
    assert!(
        !rows.iter().any(|l| l.contains("Repeated the rejected")),
        "{rows:#?}"
    );
    // esc close closes.
    pane.key("o");
    let shown = plain_rows(&mut pane, 100, 60);
    pane.click(footer, shown[footer].find("esc close").unwrap(), NOW);
    assert!(plain_rows(&mut pane, 100, 60)
        .iter()
        .any(|l| l == "Confused Claude"));

    // The footer the picture draws inside the panel answers too, at 46x30 on a 16x34 cell.
    let (_home, paths) = observed_home();
    let (cols, rows) = (46, 30);
    let mut pane = Pane::new(paths, &args("main", "sess-now", cols, rows));
    pane.pixels = true;
    pane.enable_cards();
    pane.frame(cols, rows, NOW);
    pane.key("o");
    pane.frame(cols, rows, NOW);
    let hit = |pane: &Pane, key: &str| -> Option<(usize, usize)> {
        pane.acts
            .iter()
            .filter(|(r, _)| **r < rows - 1)
            .find_map(|(r, a)| {
                a["keys"]
                    .as_array()?
                    .iter()
                    .find(|s| s[2] == key)
                    .map(|s| (*r, s[0].as_u64().unwrap() as usize))
            })
    };
    for key in ["c", "s", "x", "tab"] {
        assert!(hit(&pane, key).is_some(), "{key}: {:?}", pane.acts);
    }
    let (row, col) = hit(&pane, "tab").unwrap();
    pane.click(row, col + 1, NOW);
    pane.frame(cols, rows, NOW);
    assert!(opened_at(&mut pane, cols, rows).is_some_and(|c| c.contains("Repeated the rejected")));
    pane.frame(cols, rows, NOW);
    let (row, col) = hit(&pane, "x").unwrap();
    pane.click(row, col + 1, NOW);
    pane.frame(cols, rows, NOW);
    assert!(pane.acts.values().all(|a| a["key"] != "observe:0:2"));
    pane.key("o");
    pane.key("tab");
    assert!(opened_at(&mut pane, cols, rows).is_some_and(|c| c.contains("Said the test passed")));
}
fn string(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}
