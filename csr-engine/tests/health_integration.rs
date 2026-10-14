//! Memory-health integration: the real binary against synthetic databases in
//! throwaway homes. Nothing here touches the live database, the live mod or
//! `~/.codex`; the only outside input is a clone-copy of the local embedding
//! model cache, so the binary can embed queries without a network.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use csr_engine::health::{self, ProbeCase, ProbeSample};
use csr_engine::import::ConversationChunk;
use csr_engine::provenance::{ChunkProvenance, Speaker};
use csr_engine::storage::Storage;

struct World {
    home: tempfile::TempDir,
    db: PathBuf,
    projects: PathBuf,
}

impl World {
    fn new() -> Self {
        let home = tempfile::tempdir_in("/private/tmp").unwrap();
        // The binary resolves its model cache from $HOME; give it a clone of
        // the one this test process resolves. No download, no credentials.
        let source = dirs::cache_dir()
            .expect("a cache dir")
            .join("csr-engine")
            .join("fastembed");
        let target = home.path().join("Library/Caches/csr-engine");
        std::fs::create_dir_all(&target).unwrap();
        if source.exists() {
            let status = Command::new("cp")
                .args(["-Rc"])
                .arg(&source)
                .arg(target.join("fastembed"))
                .status()
                .unwrap();
            assert!(status.success(), "could not clone the local model cache");
        }
        let projects = home.path().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        let db = home.path().join("data").join("csr-engine.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        Self { home, db, projects }
    }

    fn csr(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_csr-engine"))
            .args(["--db-path"])
            .arg(&self.db)
            .arg("--projects-dir")
            .arg(&self.projects)
            .args(args)
            .env("HOME", self.home.path())
            .env("CSR_NO_DREAMING", "1")
            .env("CSR_NO_AI_NARRATIVES", "1")
            .env_remove("CSR_TRAINED_RERANK")
            .env_remove("CSR_SEARCH_MODE")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn storage(&self) -> Storage {
        Storage::open(&self.db).unwrap()
    }
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn recent() -> String {
    (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339()
}

fn body(i: usize) -> String {
    format!(
        "Investigating the migration planner behaviour number {i}. The scheduler widget{i} keeps \
         stalling after zebra{i} deployments finish. Please check quokka{i} handling carefully \
         before shipping anything to production today for everyone involved here."
    )
}

/// A user-authored, tool-free chunk with a fixed unit vector (384 dims).
fn seed_chunk(storage: &Storage, id: &str, conversation: &str, content: &str) {
    let chunk = ConversationChunk {
        id: id.into(),
        conversation_id: conversation.into(),
        project_name: "synthetic".into(),
        timestamp: recent(),
        content: content.into(),
        message_count: 1,
        summary: None,
        author: Speaker::User,
        seq: 0,
        is_sidechain: false,
    };
    let mut vector = vec![0.0_f32; 384];
    vector[0] = 1.0;
    storage.insert_chunk(&chunk, &vector).unwrap();
    storage
        .insert_chunk_provenance(
            id,
            &ChunkProvenance {
                author: Speaker::User,
                source_conv_id: conversation.into(),
                supersedes: None,
            },
        )
        .unwrap();
}

/// The import step records each chunk's tool share; synthetic chunks carry none.
fn mark_tool_free(db: &Path) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute("UPDATE chunks SET tool_result_share = 0.0", [])
        .unwrap();
}

/// Every row of every table, as text, keyed by table name.
fn dump(db: &Path) -> std::collections::BTreeMap<String, Vec<String>> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE '%_fts%' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut out = std::collections::BTreeMap::new();
    for table in tables {
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let width = stmt.column_count();
        let mut rows: Vec<String> = stmt
            .query_map([], |row| {
                let mut cells = Vec::new();
                for i in 0..width {
                    cells.push(format!("{:?}", row.get::<_, rusqlite::types::Value>(i)?));
                }
                Ok(cells.join("|"))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        rows.sort();
        out.insert(table, rows);
    }
    out
}

/// The exit status of `eval --full` mirrors its FAIL lines: it never fails
/// for a report-only or abstaining result, and always fails for a FAIL line.
fn assert_exit_mirrors_failures(output: &Output) {
    let stdout = text(output);
    let has_fail = stdout.contains("[FAIL]");
    assert_eq!(
        output.status.success(),
        !has_fail,
        "exit {:?} but report has FAIL lines = {has_fail}\n{stdout}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn status_json_flag_matches_default_json() {
    let world = World::new();
    drop(world.storage());
    let default = world.csr(&["status"]);
    let explicit = world.csr(&["status", "--json"]);
    assert!(
        default.status.success(),
        "{}",
        String::from_utf8_lossy(&default.stderr)
    );
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    let a: serde_json::Value = serde_json::from_slice(&default.stdout).unwrap();
    let b: serde_json::Value = serde_json::from_slice(&explicit.stdout).unwrap();
    assert_eq!(a, b);
    assert!(a["memory_health"]["probe"]["state"] == "not_run");
    // --json is the default format, so it cannot be combined with another one
    for other in ["--compact", "--swiftbar", "--stale"] {
        let conflict = world.csr(&["status", "--json", other]);
        assert!(
            !conflict.status.success(),
            "--json with {other} must be rejected"
        );
    }
}

#[test]
fn full_eval_red_probe_exits_nonzero() {
    let world = World::new();
    {
        let storage = world.storage();
        let mut cases = Vec::new();
        for i in 0..12 {
            let content = body(i);
            seed_chunk(&storage, &format!("u{i}"), &format!("conv-{i}"), &content);
            cases.push(ProbeCase {
                chunk_id: format!("u{i}"),
                conversation_id: format!("conv-{i}"),
                parent_conversation_id: None,
                content_hash: blake3::hash(content.as_bytes()).to_hex().to_string(),
                // words no chunk contains: the keyword channel cannot rescue it
                query: format!("zzqxv{i} wwkpl{i} yyhgt{i}"),
            });
        }
        mark_tool_free(&world.db);
        // Every vector is identical and no query shares a word with any chunk,
        // so at most five of twelve conversations can ever sit in a
        // five-result page: 12 cases cannot reach 70%.
        let snapshot = health::HealthSnapshot {
            sample: Some(ProbeSample {
                version: health::probe::SAMPLE_VERSION,
                sample_id: "ps-integration".into(),
                created_at: recent(),
                cutoff_at: recent(),
                cases,
            }),
            ..Default::default()
        };
        health::save_health(&storage, &snapshot).unwrap();
    }
    let output = world.csr(&["eval", "--full"]);
    let stdout = text(&output);
    assert!(stdout.contains("[FAIL] Known-Item Recall"), "{stdout}");
    assert!(stdout.contains("RED:"), "{stdout}");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    // the whole report was printed before the exit
    assert!(stdout.contains("Result: "), "{stdout}");
    // and the status a later reader sees is the stored red
    let status: serde_json::Value = serde_json::from_slice(&world.csr(&["status"]).stdout).unwrap();
    assert_eq!(status["memory_health"]["probe"]["state"], "red");
    assert!(status["memory_health"]["warning"]
        .as_str()
        .unwrap()
        .starts_with("⚠ mem: recall"));
}

#[test]
fn full_eval_duplicate_rate_never_fails_gate() {
    let world = World::new();
    {
        let storage = world.storage();
        for i in 0..30 {
            seed_chunk(&storage, &format!("d{i}"), &format!("dup-{i}"), &body(0));
        }
        mark_tool_free(&world.db);
    }
    let output = world.csr(&["eval", "--full"]);
    let stdout = text(&output);
    let line = stdout
        .lines()
        .find(|line| line.contains("Duplicate Rate (Report Only)"))
        .unwrap_or_else(|| panic!("no duplicate result\n{stdout}"));
    assert!(line.contains("[PASS]"), "{line}");
    assert!(
        line.contains("REPORT ONLY: 29 duplicate rows of 30 chunks (96.67%)"),
        "{line}"
    );
    assert_exit_mirrors_failures(&output);
    let status: serde_json::Value = serde_json::from_slice(&world.csr(&["status"]).stdout).unwrap();
    assert_eq!(status["memory_health"]["duplicates"]["state"], "measured");
    assert!(
        status["memory_health"]["warning"].is_null(),
        "duplicates never raise a chip"
    );
}

#[test]
fn full_eval_abstention_is_explicit() {
    let world = World::new();
    let output = world.csr(&["eval", "--full"]);
    let stdout = text(&output);
    let recall = stdout
        .lines()
        .find(|line| line.contains("Known-Item Recall"))
        .unwrap_or_else(|| panic!("no recall result\n{stdout}"));
    assert!(
        recall.contains("[PASS]") && recall.contains("ABSTAIN:"),
        "{recall}"
    );
    assert!(!stdout.contains("100%"), "no data is never 100%\n{stdout}");
    assert!(stdout.contains("/24 passed"), "{stdout}");
    // a fresh install has never trained a re-ranker: that abstains, it does not fail
    let gate = stdout
        .lines()
        .find(|line| line.contains("Trained Re-ranker Gate"))
        .unwrap_or_else(|| panic!("no re-ranker gate line\n{stdout}"));
    assert!(
        gate.contains("[PASS]") && gate.contains("ABSTAIN: never_run"),
        "{gate}"
    );
    assert!(stdout.contains("Contamination Census"), "{stdout}");
    assert!(stdout.contains("REPORT ONLY: no chunks"), "{stdout}");
    assert_exit_mirrors_failures(&output);
    let status: serde_json::Value = serde_json::from_slice(&world.csr(&["status"]).stdout).unwrap();
    assert_eq!(status["memory_health"]["probe"]["state"], "abstain");
    assert_eq!(status["memory_health"]["probe"]["n"], 0);
    assert!(status["memory_health"]["warning"].is_null());
}

#[test]
fn probe_only_writes_health_meta() {
    let world = World::new();
    {
        let storage = world.storage();
        for i in 0..12 {
            seed_chunk(&storage, &format!("u{i}"), &format!("conv-{i}"), &body(i));
        }
        mark_tool_free(&world.db);
    }
    let before = dump(&world.db);
    let first = world.csr(&["eval", "--full"]);
    assert!(
        text(&first).contains("Known-Item Recall"),
        "{}",
        text(&first)
    );
    let after_first = dump(&world.db);

    let mut changed: Vec<String> = Vec::new();
    for (table, rows) in &after_first {
        if before.get(table) != Some(rows) {
            changed.push(table.clone());
        }
    }
    assert_eq!(
        changed,
        ["meta"],
        "only the meta table may change: {changed:?}"
    );
    let meta_new: Vec<&String> = after_first["meta"]
        .iter()
        .filter(|row| !before["meta"].contains(row))
        .collect();
    assert!(
        meta_new.iter().all(|row| row.contains("memory_health_")),
        "unexpected meta writes: {meta_new:?}"
    );
    assert!(meta_new
        .iter()
        .any(|row| row.contains("memory_health_probe_sample_v1")));

    // a second full eval keeps the frozen sample byte for byte
    let sample = |dump: &std::collections::BTreeMap<String, Vec<String>>| {
        dump["meta"]
            .iter()
            .find(|row| row.contains("memory_health_probe_sample_v1"))
            .cloned()
    };
    let second = world.csr(&["eval", "--full"]);
    assert!(text(&second).contains("Known-Item Recall"));
    let after_second = dump(&world.db);
    assert_eq!(sample(&after_first), sample(&after_second));
    for (table, rows) in &after_second {
        if table != "meta" {
            assert_eq!(
                Some(rows),
                after_first.get(table),
                "{table} changed on the second run"
            );
        }
    }
}
