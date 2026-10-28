//! The rich private HOME the pane tests draw from: the instrument states and the owner's session.
#![allow(dead_code)]
use csr_engine::companion::{cli::PaneArgs, paths::Paths};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Once,
    time::{Duration, SystemTime},
};

pub fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/companion")
}
pub fn rich() -> PathBuf {
    fixture().join("rich")
}
pub fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}
/// Copy a fixture tree, writing `<REPO>` and `<HOME>` into every text file the way the oracle does.
pub fn copy_tree(from: &Path, to: &Path, home: &str) {
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
pub fn fill(text: &str, home: &str) -> String {
    text.replace("<REPO>", &repo().to_string_lossy())
        .replace("<HOME>", home)
}
/// The goldens carry local times (10:37), so the clock is UTC for every test in this file.
pub fn utc() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::env::set_var("TZ", "UTC");
        // The API credit row reads this folder when it is set; the goldens read the private HOME's.
        std::env::remove_var("API_LANES_HOME");
        // No keychain read and no network from a test: the goldens show the ledger row.
        std::env::set_var("CSR_ADMIN_COST", "off");
    });
}
pub fn rich_home() -> (tempfile::TempDir, Paths) {
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
    con.execute_batch(include_str!("../fixtures/companion/home.sql"))
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
pub fn args(page: &str, sid: &str, cols: usize, rows: usize) -> PaneArgs {
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

pub const NOW: f64 = 1792320000.;
pub const QUESTION:&str="What if one of Fable's three prototypes made the what-if question the star of the pane, as you wanted on 10-09?";
pub fn write_json(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value.to_string()).unwrap();
}
pub fn state_home(state: &str) -> (tempfile::TempDir, Paths) {
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
    if state == "14-soak" {
        soak_files(&paths);
    }
    (home, paths)
}
/// A soak two hours in, ten minutes between samples, and API credit with about a day left.
pub fn soak_files(paths: &Paths) {
    let at = |secs: f64| {
        chrono::DateTime::from_timestamp(secs as i64, 0)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    };
    let check = |id: &str,
                 label: &str,
                 metric: &str,
                 op: &str,
                 threshold: Value,
                 kind: &str,
                 unit: &str,
                 warmup: f64| {
        json!({"id":id,"label":label,"metric":metric,"op":op,"threshold":threshold,"kind":kind,
            "unit":unit,"warmup_hours":warmup})
    };
    let trials = paths.home.join(".claude-self-reflect/trials");
    write_json(
        &trials.join("combo-soak.json"),
        &json!({"name":"Combo soak","started_at":at(NOW - 7200.),"planned_end":at(NOW + 6. * 3600.),
        "candidate":{"label":"combo","sha":"ac9c24c3f0a1"},"source":"combo-soak.ndjson",
        "checks":[
            check("daemon_errors","Daemon errors","daemon_errors","==",json!(0),"latest","",0.),
            check("rss","Memory trend","rss_mb","<=",json!(150),"slope_per_hour","MB/h",0.5),
            check("prompt","Prompt hook p95, worst","per_hook_p50_p95_max.prompt-submit.1","<=",json!(800),"max","ms",0.),
            check("post","Post-tool hook p95, worst","per_hook_p50_p95_max.post-tool-use.1","<=",json!(100),"max","ms",0.),
            check("replay","Replay finished","replay_finished","==",json!(true),"at_least_by_end","",0.),
            check("dreams","Dream cycles","dream_cycles",">=",json!(1),"at_least_by_end","",0.),
        ]}),
    );
    let rows: Vec<String> = (0..11)
        .map(|i| {
            json!({"t":at(NOW - 6600. + i as f64 * 600.),"daemon_errors":0,
                "rss_mb":4400. - i as f64 * 20.,"dream_cycles":0,"replay_finished":false,
                "per_hook_p50_p95_max":{"prompt-submit":[16.4, 481.1, 491.5],
                    "post-tool-use":[10.2, 21.7 - i as f64 * 0.1, 128.8]}})
            .to_string()
        })
        .collect();
    fs::write(trials.join("combo-soak.ndjson"), rows.join("\n") + "\n").unwrap();
    // Credit 200 from 10-01; $76.14 spent ten days ago, $94.63 within the week: $29.23 left,
    // $13.5 a day, about two days.
    let lanes = paths.home.join(".agents/api-lanes");
    write_json(
        &lanes.join("credit.json"),
        &json!({"amount":200.0,"since":"2026-10-01"}),
    );
    fs::write(
        lanes.join("ledger.jsonl"),
        format!(
            "{}\n{}\n",
            json!({"ts":NOW - 10. * 86400.,"run":"r","id":"a","cost":76.14}),
            json!({"ts":NOW - 3. * 86400.,"run":"r","id":"b","cost":94.63})
        ),
    )
    .unwrap();
}

/// The owner's session at 21:47: a performance trial, five items on each observation card, a long
/// story and a monologue of many rows.
pub fn owner_home() -> (tempfile::TempDir, Paths) {
    let (home, paths) = state_home("14-soak");
    let log = paths.keep.join("dreams.jsonl");
    let mut text = fs::read_to_string(&log).unwrap();
    let confusing: Vec<Value> = (0..5)
        .map(|i| {
            json!({"line":format!("Claude misread which pane the owner meant, case {i}."),
                "evidence":format!("Turn {}: 'the companion pane'", i + 3)})
        })
        .collect();
    let should_be: Vec<Value> = ["skill", "hook", "memory", "tool", "skill"]
        .iter()
        .enumerate()
        .map(|(i, kind)| {
            json!({"kind":kind,"line":format!("Check the card height after each layout edit, rule {i}."),
                "evidence":format!("Turns {} and {}: 'at most 30%'", i + 4, i + 9)})
        })
        .collect();
    text.push_str(
        &json!({"kind":"look","session":"sess-now","at":NOW - 20.,"outcome":"none",
            "confusing_claude":confusing,"should_be":should_be})
        .to_string(),
    );
    text.push('\n');
    fs::write(&log, text).unwrap();
    (home, paths)
}
