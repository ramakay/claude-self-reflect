//! End-to-end: the whole answer order (JSONL, mirror, chunks, registry, plain
//! not-found) through both surfaces, on a temp database and temp directories.

use super::chunks::test_support::{add_chunk, temp_storage};
use super::chunks::MAX_REBUILT_CHUNKS;
use super::{run_with_sources, RoleFilter, TranscriptRequest, ViewKind};
use crate::import::{CHUNK_CHAR_BUDGET, MAX_TOOL_RESULT_CHARS};
use crate::mcp::tools::get_full_conversation_with_db;
use crate::storage::Storage;
use rusqlite::params;
use std::path::{Path, PathBuf};

const CONV: &str = "edf72267-357f-438c-a230-8bd1a210eb4c";
const OTHER: &str = "edf72267-0000-4000-8000-000000000001";
const NOT_FOUND_TEXT: &str = "may not have been imported yet";

struct Rig {
    _dir: tempfile::TempDir,
    projects: PathBuf,
    data_dir: PathBuf,
    storage: Storage,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();
        let storage = temp_storage(&data_dir);
        Rig {
            _dir: dir,
            projects,
            data_dir,
            storage,
        }
    }

    fn seed_chunks(&self, conv: &str) {
        // Inserted out of order on purpose: the rebuild must sort them.
        add_chunk(
            &self.storage,
            "k3",
            conv,
            "anukriti",
            "2026-08-12T15:27:59Z",
            2,
            "assistant",
            1,
            "third chunk, the closing words",
        );
        add_chunk(
            &self.storage,
            "k1",
            conv,
            "anukriti",
            "2026-08-10T14:17:38Z",
            0,
            "user",
            1,
            "first chunk, the user's ask",
        );
        add_chunk(
            &self.storage,
            "k2",
            conv,
            "anukriti",
            "2026-08-10T14:17:38Z",
            1,
            "tool_result",
            2,
            "second chunk mentions StationCard and <b>markup</b>",
        );
    }

    fn seed_registry(&self, id: &str) {
        self.storage
            .with_connection(|c| {
                c.execute(
                    "INSERT INTO session_registry (session_id, project, first_prompt, first_ts, last_ts, prompt_count) \
                     VALUES (?1, 'anukriti', 'p', '2026-08-10T14:16:32.970+00:00', '2026-08-12T15:27:52.310+00:00', 73)",
                    params![id],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn write_jsonl(&self, id: &str) {
        let dir = self.projects.join("-tmp-anukriti");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{id}.jsonl")),
            "{\"type\":\"user\",\"timestamp\":\"2026-08-10T10:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"the real jsonl prompt\"}}\n",
        )
        .unwrap();
    }

    fn write_mirror(&self, id: &str) {
        let dir = crate::mirror::transcripts_dir(&self.data_dir).join("-tmp-anukriti");
        std::fs::create_dir_all(&dir).unwrap();
        let body = "{\"type\":\"user\",\"timestamp\":\"2026-08-10T10:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"the mirrored prompt\"}}\n";
        std::fs::write(
            dir.join(format!("{id}.jsonl.zst")),
            zstd::stream::encode_all(body.as_bytes(), 3).unwrap(),
        )
        .unwrap();
    }

    fn ask(&self, session: &str, view: ViewKind, grep: Option<&str>, json: bool) -> String {
        run_with_sources(
            &self.projects,
            Some(&self.data_dir),
            Some(&self.storage),
            &TranscriptRequest {
                session: session.to_string(),
                view,
                grep: grep.map(str::to_string),
                json,
                ..Default::default()
            },
        )
    }

    fn full(&self, session: &str) -> String {
        get_full_conversation_with_db(
            &self.projects,
            Some(&self.data_dir),
            Some(&self.storage),
            session,
            None,
        )
    }
}

#[test]
fn chunks_only_session_is_rebuilt_in_order_under_a_lossy_header() {
    let rig = Rig::new();
    rig.seed_chunks(CONV);
    let out = rig.ask(CONV, ViewKind::Slice, None, false);
    assert!(out.contains("NOT THE ORIGINAL TRANSCRIPT"), "got: {out}");
    assert!(out.contains("Rebuilt from 3 indexed chunks"), "got: {out}");
    assert!(
        out.contains("2026-08-10T14:17:38Z .. 2026-08-12T15:27:59Z"),
        "got: {out}"
    );
    assert!(out.contains("LOSSY"), "got: {out}");
    assert!(
        out.contains(&format!("cut to {MAX_TOOL_RESULT_CHARS} bytes")),
        "got: {out}"
    );
    assert!(
        out.contains(&format!("up to {CHUNK_CHAR_BUDGET} bytes")),
        "got: {out}"
    );
    assert!(!out.contains(NOT_FOUND_TEXT), "got: {out}");
    let first = out.find("first chunk").unwrap();
    let second = out.find("second chunk").unwrap();
    let third = out.find("third chunk").unwrap();
    assert!(first < second && second < third, "got: {out}");
    assert!(out.contains("[turn 1] user: first chunk"), "got: {out}");
    assert!(
        out.contains("[turn 2] tool_result (x2 messages)"),
        "got: {out}"
    );
    // Transcript text is escaped inside the envelope.
    assert!(out.contains("&lt;b&gt;markup&lt;/b&gt;"), "got: {out}");
}

#[test]
fn grep_finds_a_hit_in_the_rebuilt_text_and_nowhere_else() {
    let rig = Rig::new();
    rig.seed_chunks(CONV);
    let out = rig.ask(CONV, ViewKind::Grep, Some("StationCard"), false);
    assert!(out.contains("[turn 2]"), "got: {out}");
    assert!(!out.contains("[turn 1]"), "got: {out}");
    assert!(out.contains("NOT THE ORIGINAL TRANSCRIPT"), "got: {out}");
    let none = rig.ask(CONV, ViewKind::Grep, Some("NoSuchSymbol"), false);
    assert!(none.contains("(no matches)"), "got: {none}");
    let bad = rig.ask(CONV, ViewKind::Grep, Some("("), false);
    assert!(bad.contains("invalid --grep regex"), "got: {bad}");
}

#[test]
fn json_mode_names_its_source_and_what_is_lost() {
    let rig = Rig::new();
    rig.seed_chunks(CONV);
    let grep: serde_json::Value =
        serde_json::from_str(&rig.ask(CONV, ViewKind::Grep, Some("StationCard"), true)).unwrap();
    assert_eq!(grep["source"], "db_chunks");
    assert!(grep["lossy"].as_array().unwrap().len() >= 5);
    assert_eq!(grep["chunks_total"], 3);
    assert_eq!(grep["capped"], false);
    assert_eq!(grep["items"][0]["turn"], 2);
    assert_eq!(grep["items"][0]["role"], "tool_result");

    let stats: serde_json::Value =
        serde_json::from_str(&rig.ask(CONV, ViewKind::Stats, None, true)).unwrap();
    assert_eq!(stats["source"], "db_chunks");
    assert_eq!(stats["chunks_by_author"]["user"], 1);
    assert_eq!(stats["chunks_by_author"]["tool_result"], 1);
    assert!(stats["unavailable"]
        .as_str()
        .unwrap()
        .contains("not available"));
    assert!(stats.get("tool_calls").is_none());
}

#[test]
fn structural_views_abstain_instead_of_inventing_numbers() {
    let rig = Rig::new();
    rig.seed_chunks(CONV);
    for view in [ViewKind::Tools, ViewKind::Files, ViewKind::Errors] {
        let out = rig.ask(CONV, view, None, false);
        assert!(
            out.contains("not available from indexed chunks"),
            "got: {out}"
        );
        assert!(out.contains("NOT THE ORIGINAL TRANSCRIPT"), "got: {out}");
        assert!(!out.contains("[turn"), "got: {out}");
        let json: serde_json::Value =
            serde_json::from_str(&rig.ask(CONV, view, None, true)).unwrap();
        assert_eq!(json["available"], false);
        assert_eq!(json["source"], "db_chunks");
    }
    let stats = rig.ask(CONV, ViewKind::Stats, None, false);
    assert!(
        stats.contains("not available from indexed chunks"),
        "got: {stats}"
    );
    assert!(!stats.contains("<tool_calls"), "got: {stats}");
    assert!(stats.contains("total=\"3\""), "got: {stats}");
}

#[test]
fn prompts_lists_only_user_authored_chunks_and_the_role_filter_applies() {
    let rig = Rig::new();
    rig.seed_chunks(CONV);
    let prompts = rig.ask(CONV, ViewKind::Prompts, None, false);
    assert!(prompts.contains("first chunk"), "got: {prompts}");
    assert!(!prompts.contains("third chunk"), "got: {prompts}");
    assert!(!prompts.contains("second chunk"), "got: {prompts}");
    let assistant = run_with_sources(
        &rig.projects,
        Some(&rig.data_dir),
        Some(&rig.storage),
        &TranscriptRequest {
            session: CONV.to_string(),
            view: ViewKind::Slice,
            role: RoleFilter::Assistant,
            ..Default::default()
        },
    );
    assert!(assistant.contains("third chunk"), "got: {assistant}");
    assert!(!assistant.contains("first chunk"), "got: {assistant}");
}

#[test]
fn registry_only_session_gets_a_registry_aware_error() {
    let rig = Rig::new();
    rig.seed_registry(CONV);
    for out in [rig.ask(CONV, ViewKind::Stats, None, false), rig.full(CONV)] {
        assert!(
            out.contains("Session edf72267-357f-438c-a230-8bd1a210eb4c is in CSR's session registry (project anukriti, first seen 2026-08-10T14:16:32.970+00:00), but its JSONL is gone and CSR kept no transcript copy or indexed chunks for it."),
            "got: {out}"
        );
        assert!(!out.contains(NOT_FOUND_TEXT), "got: {out}");
    }
    let json: serde_json::Value =
        serde_json::from_str(&rig.ask(CONV, ViewKind::Stats, None, true)).unwrap();
    assert_eq!(json["known_to"], "session_registry");
    assert_eq!(json["project"], "anukriti");
    // A unique prefix reaches the registry row too.
    let prefixed = rig.ask("edf72267-357f", ViewKind::Stats, None, false);
    assert!(
        prefixed.contains("is in CSR's session registry"),
        "got: {prefixed}"
    );
}

#[test]
fn a_session_known_nowhere_keeps_todays_message() {
    let rig = Rig::new();
    rig.seed_chunks(CONV);
    rig.seed_registry(CONV);
    let unknown = "ffffffff-ffff-4fff-8fff-ffffffffffff";
    for out in [
        rig.ask(unknown, ViewKind::Stats, None, false),
        rig.full(unknown),
    ] {
        assert!(
            out.contains(&format!(
                "Conversation ID '{unknown}' not found in any project."
            )),
            "got: {out}"
        );
        assert!(out.contains(NOT_FOUND_TEXT), "got: {out}");
    }
    // No database at all behaves the same.
    let out = run_with_sources(
        &rig.projects,
        Some(&rig.data_dir),
        None,
        &TranscriptRequest {
            session: CONV.to_string(),
            ..Default::default()
        },
    );
    assert!(out.contains(NOT_FOUND_TEXT), "got: {out}");
}

#[test]
fn prefixes_resolve_when_unique_and_report_ambiguity_otherwise() {
    let rig = Rig::new();
    rig.seed_chunks(CONV);
    let unique = rig.ask("edf72267-357f", ViewKind::Grep, Some("StationCard"), false);
    assert!(unique.contains("[turn 2]"), "got: {unique}");
    assert!(rig
        .full("edf72267-357f")
        .contains("<source>db_chunks</source>"));

    // Under 8 characters is never a prefix match.
    let short = rig.ask("edf7226", ViewKind::Stats, None, false);
    assert!(short.contains(NOT_FOUND_TEXT), "got: {short}");

    add_chunk(
        &rig.storage,
        "o1",
        OTHER,
        "anukriti",
        "2026-08-11T00:00:00Z",
        0,
        "user",
        1,
        "another session",
    );
    let ambiguous = rig.ask("edf72267", ViewKind::Stats, None, false);
    assert!(ambiguous.contains("ambiguous"), "got: {ambiguous}");
    assert!(
        ambiguous.contains(CONV) && ambiguous.contains(OTHER),
        "got: {ambiguous}"
    );
    let ambiguous_full = rig.full("edf72267");
    assert!(
        ambiguous_full.contains("ambiguous"),
        "got: {ambiguous_full}"
    );
    // The exact id still wins.
    assert!(rig
        .ask(CONV, ViewKind::Slice, None, false)
        .contains("first chunk"));
}

#[test]
fn the_mirror_beats_chunks_and_the_jsonl_beats_both() {
    let rig = Rig::new();
    rig.seed_chunks(CONV);
    rig.write_mirror(CONV);
    let mirrored = rig.ask(CONV, ViewKind::Slice, None, false);
    assert!(mirrored.contains("the mirrored prompt"), "got: {mirrored}");
    assert!(!mirrored.contains("NOT THE ORIGINAL"), "got: {mirrored}");
    let mirror_json: serde_json::Value =
        serde_json::from_str(&rig.ask(CONV, ViewKind::Slice, None, true)).unwrap();
    assert_eq!(mirror_json["source"], "mirror");
    let full = rig.full(CONV);
    assert!(full.contains("<mirror_path>"), "got: {full}");
    assert!(!full.contains("db_chunks"), "got: {full}");

    rig.write_jsonl(CONV);
    let jsonl = rig.ask(CONV, ViewKind::Slice, None, false);
    assert!(jsonl.contains("the real jsonl prompt"), "got: {jsonl}");
    assert!(!jsonl.contains("mirrored prompt"), "got: {jsonl}");
    assert!(!jsonl.contains("NOT THE ORIGINAL"), "got: {jsonl}");
    let full = rig.full(CONV);
    assert!(full.contains("<file_path>"), "got: {full}");
    assert!(!full.contains("db_chunks"), "got: {full}");
}

#[test]
fn get_full_conversation_returns_the_rebuilt_text_with_the_same_header() {
    let rig = Rig::new();
    rig.seed_chunks(CONV);
    let out = rig.full(CONV);
    assert!(out.starts_with("<conversation_file>"), "got: {out}");
    assert!(out.contains("<source>db_chunks</source>"), "got: {out}");
    assert!(out.contains("NOT THE ORIGINAL TRANSCRIPT"), "got: {out}");
    assert!(out.contains("Rebuilt from 3 indexed chunks"), "got: {out}");
    assert!(out.contains("LOSSY"), "got: {out}");
    assert!(
        out.contains("[turn 2] tool_result (x2 messages): second chunk"),
        "got: {out}"
    );
    assert!(out.ends_with("</conversation_file>"), "got: {out}");
}

#[test]
fn a_long_session_is_read_up_to_the_cap_and_says_so() {
    let rig = Rig::new();
    rig.storage
        .with_connection(|c| {
            let tx = c.unchecked_transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, message_count, seq, source) \
                     VALUES (?1, ?2, 'anukriti', '2026-08-10T00:00:00Z', ?3, 1, ?4, 'conversation')",
                )?;
                for i in 0..(MAX_REBUILT_CHUNKS + 5) {
                    stmt.execute(params![
                        format!("big{i}"),
                        CONV,
                        format!("chunk number {i}"),
                        i as i64
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    let out = rig.ask(CONV, ViewKind::Stats, None, false);
    assert!(
        out.contains("CAPPED: only the first 5000 of 5005 chunks were read."),
        "got: {out}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&rig.ask(CONV, ViewKind::Stats, None, true)).unwrap();
    assert_eq!(json["chunks_read"], 5000);
    assert_eq!(json["capped"], true);
    let kept = rig.ask(CONV, ViewKind::Grep, Some("chunk number 4999$"), false);
    assert!(kept.contains("[turn 5000]"), "got: {kept}");
    let cut = rig.ask(CONV, ViewKind::Grep, Some("chunk number 5004$"), false);
    assert!(!cut.contains("[turn"), "got: {cut}");
}

#[test]
fn json_stays_valid_json_when_a_big_session_is_cut_at_the_budget() {
    let rig = Rig::new();
    rig.storage
        .with_connection(|c| {
            let tx = c.unchecked_transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, message_count, seq, source) \
                     VALUES (?1, ?2, 'anukriti', '2026-08-10T00:00:00Z', ?3, 1, ?4, 'conversation')",
                )?;
                for i in 0..400 {
                    stmt.execute(params![
                        format!("fat{i}"),
                        CONV,
                        format!("{i} {}", "word ".repeat(150)),
                        i as i64
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    for budget in [4_000usize, 30_000] {
        let out = run_with_sources(
            &rig.projects,
            Some(&rig.data_dir),
            Some(&rig.storage),
            &TranscriptRequest {
                session: CONV.to_string(),
                view: ViewKind::Grep,
                grep: Some("word".to_string()),
                json: true,
                budget_chars: budget,
                ..Default::default()
            },
        );
        let json: serde_json::Value = serde_json::from_str(&out)
            .unwrap_or_else(|e| panic!("budget {budget}: invalid JSON ({e}): {out:.200}"));
        assert_eq!(json["source"], "db_chunks");
        assert_eq!(json["truncated"], true);
        assert!(json["items"].as_array().unwrap().len() < 400);
        assert!(json["resume_turns"].as_str().unwrap().ends_with(".."));
    }
}

#[test]
fn a_read_only_open_answers_without_touching_the_database_file() {
    let rig = Rig::new();
    rig.seed_chunks(CONV);
    let db: &Path = &rig.data_dir.join("csr-engine.db");
    // Flush the writer so the main file holds everything.
    rig.storage
        .with_connection(|c| {
            c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
        .unwrap();
    let before = std::fs::read(db).unwrap();
    let ro = Storage::open_read_only(db).unwrap();
    let out = run_with_sources(
        &rig.projects,
        Some(&rig.data_dir),
        Some(&ro),
        &TranscriptRequest {
            session: CONV.to_string(),
            view: ViewKind::Grep,
            grep: Some("StationCard".to_string()),
            ..Default::default()
        },
    );
    assert!(out.contains("[turn 2]"), "got: {out}");
    drop(ro);
    assert_eq!(before, std::fs::read(db).unwrap());
}
