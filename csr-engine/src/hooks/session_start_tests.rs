use super::*;

/// A fixture session: 1 user, 2 assistant, 3 boundary, 4 user, 5 assistant, 6 user.
fn pointer_fixture(dir: &Path, sid: &str) -> std::path::PathBuf {
    let project = dir.join("projects").join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let path = project.join(format!("{sid}.jsonl"));
    let user = |n: usize| {
        format!(
            r#"{{"type":"user","uuid":"u{n}","message":{{"role":"user","content":"question {n}"}}}}"#
        )
    };
    let assistant = |n: usize| {
        format!(
            r#"{{"type":"assistant","uuid":"a{n}","message":{{"role":"assistant","content":[{{"type":"text","text":"answer {n}"}}]}}}}"#
        )
    };
    let boundary =
        r#"{"type":"system","subtype":"compact_boundary","uuid":"b3","content":"compacted"}"#;
    let body = [
        user(1),
        assistant(2),
        boundary.to_string(),
        user(4),
        assistant(5),
        user(6),
    ]
    .join("\n");
    std::fs::write(&path, format!("{body}\n")).unwrap();
    path
}

fn pointer_input(source: &str, sid: &str, path: Option<&Path>) -> HookInput {
    HookInput {
        source: Some(source.to_string()),
        session_id: Some(sid.to_string()),
        transcript_path: path.map(|p| p.to_string_lossy().into_owned()),
        ..HookInput::default()
    }
}

/// An engine over `storage` whose in-memory index is empty, as a
/// daemon-proxied hook's is.
fn engine_with_empty_index(storage: crate::storage::Storage) -> Engine {
    Engine::from_parts(
        std::sync::Arc::new(storage),
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(crate::search::SearchEngine::new(
            100,
        ))),
        std::path::PathBuf::from("/tmp"),
    )
}

fn add_chunk(storage: &crate::storage::Storage) {
    storage
        .with_connection(|c| {
            c.execute(
                "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, message_count)
                     VALUES ('c1', 'conv1', 'p', '2026-10-01T00:00:00Z', 'some history', 2)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
}

fn add_reflection(storage: &crate::storage::Storage) {
    storage
        .with_connection(|c| {
            c.execute(
                "INSERT INTO reflections (id, content, tags, timestamp)
                     VALUES ('r1', 'a note', '[]', '2026-10-01T00:00:00Z')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
}

// --- episode_carries_state tests (Tier-0 anchor selection) ---

fn minimal_episode(request: &str, completed: &str, next_steps: Option<&str>) -> Episode {
    use crate::hooks::stop::Episode;
    Episode {
        schema: "v2".into(),
        session_id: "s".into(),
        project: "p".into(),
        timestamp: "2026-07-07T12:00:00Z".into(),
        request: request.into(),
        investigated: vec![],
        completed: completed.into(),
        next_steps: next_steps.map(String::from),
        blockers: None,
        outcome: "success".into(),
        error_signatures: vec![],
        tools_used: vec![],
        files_modified: vec![],
        message_count: 10,
        duration_minutes: 5,
        todos: vec![],
        approved_plan: None,
        prev_episode_id: None,
        error_count: None,
        top_errors: vec![],
        steer_count: None,
        steers: vec![],
        instrumentation_version: None,
        correction_count: None,
        recap_evidence_version: None,
        anchor_request: None,
        completions: vec![],
        completions_untied: false,
        anchor_receipts: None,
        compact_anchor: None,
        late_errors: false,
        closing_success: false,
        closing_ended: false,
        final_failed: false,
        closing_receipt: true,
        outcome_version: Some(1),
        latest_ask: None,
        watermark: None,
        recap_extras: Default::default(),
        anchors: vec![],
    }
}

fn seed_recap_feeds(storage: &crate::storage::Storage, ep: &Episode) {
    storage
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO chunks
                        (id, conversation_id, project_name, timestamp, content, message_count)
                     VALUES ('recap-chunk', ?1, ?2, ?3, 'fixture', 1)",
                rusqlite::params![ep.session_id, ep.project, ep.timestamp],
            )?;
            conn.execute(
                "INSERT INTO chunks
                        (id, conversation_id, project_name, timestamp, content, message_count)
                     VALUES ('proposal-chunk', ?1, ?2, ?3, 'fixture', 1)",
                rusqlite::params![ep.session_id, ep.project, ep.timestamp],
            )?;
            Ok(())
        })
        .unwrap();
    storage
        .insert_resolutions(
            &["recap-chunk".to_string()],
            "resolved",
            "verified at abcdef123456",
            Some("seeded fact"),
            "user_confirmed",
        )
        .unwrap();
    storage
        .insert_resolution_proposal(
            "proposal-chunk",
            Some("open proposal"),
            "proposal evidence",
            "proposal-session",
        )
        .unwrap();
}

const NATIVE_LINE: &str = "Claude's 300k auto-compact window beat CSR 2\u{d7} this week. \
         Remove autoCompactWindow from settings.json to let CSR lead.";

const COMPACT_TASKS: [&str; 3] = [
    "Design the GitHub discussion side pane for users.",
    "Show the subscription cost as a percentage of usage.",
    "Drive release readiness and verify all candidate checks.",
];

fn compact_episode(extra_prompts: &[serde_json::Value]) -> Episode {
    let mut rows = Vec::new();
    for task in COMPACT_TASKS {
        rows.push(serde_json::json!({
            "type": "user", "promptSource": "typed", "message": {"content": task}
        }));
        rows.push(serde_json::json!({
            "type": "assistant", "message": {"content": "Work finished.", "stop_reason": "end_turn"}
        }));
    }
    rows.extend_from_slice(extra_prompts);
    let lines: Vec<String> = rows.iter().map(serde_json::Value::to_string).collect();
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    // Round-trip through the stored episode representation, as SessionStart does.
    with_session_commit(
        serde_json::from_str(
            &serde_json::to_string(&crate::hooks::stop::extract_episode(&refs, "s", "p")).unwrap(),
        )
        .unwrap(),
    )
}

/// These tests are about which task the recap names. A recap that names only
/// the task abstains (the fragment block runs instead), so the episode carries
/// one commit the session made outside every task: the recap then keeps its
/// head clause and adds `Settled:`, and the head is what the tests read.
fn with_session_commit(mut ep: Episode) -> Episode {
    ep.completions = vec![crate::hooks::stop::CompletionReceipt {
        claim: "fix(auth): keep form errors".into(),
        receipt: "abc1234".into(),
    }];
    ep.completions_untied = true;
    ep
}

fn compact_block(ep: &Episode, source: &str, session: Option<&str>) -> String {
    let storage = crate::storage::Storage::open_memory().unwrap();
    format_session_start_tier0_for_session(
        &storage,
        &ep.project,
        ep,
        &[],
        "1m ago",
        session,
        Some(source),
    )
}

fn lineage_episodes(ids: &[&str]) -> Vec<(Episode, String)> {
    ids.iter()
        .map(|id| {
            let mut ep = minimal_episode("Fix the auth middleware regression", "", None);
            ep.session_id = (*id).into();
            (ep, format!("{id} ago"))
        })
        .collect()
}

fn session_ids(episodes: &[(Episode, String)]) -> Vec<&str> {
    episodes
        .iter()
        .map(|(e, _)| e.session_id.as_str())
        .collect()
}

fn learnt_input(source: Option<&str>, session_id: &str) -> HookInput {
    HookInput {
        source: source.map(String::from),
        session_id: Some(session_id.into()),
        ..Default::default()
    }
}

fn seed_learnt_note(storage: &crate::storage::Storage, session: &str, slug: &str) {
    storage
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO memory_registry
                        (file_path, project, slug, description, mem_type, origin_session_id,
                         modified_ts, file_mtime, content_hash, links_json, last_seen_scan)
                     VALUES (?1, 'p', ?2, 'd', 'project', ?3, '2026-10-16T09:00:00Z', 0, 'h', '[]', 1)",
                rusqlite::params![format!("/m/{slug}.md"), slug, session],
            )?;
            Ok(())
        })
        .unwrap();
}

fn insert_row(
    storage: &crate::storage::Storage,
    id: &str,
    content: &str,
    tags: &[String],
    timestamp: &str,
) {
    storage
        .insert_reflection(id, content, tags, &[0.0; 4])
        .unwrap();
    storage
        .with_connection(|conn| {
            conn.execute(
                "UPDATE reflections SET timestamp = ?1 WHERE id = ?2",
                rusqlite::params![timestamp, id],
            )?;
            Ok(())
        })
        .unwrap();
}

fn insert_episode(storage: &crate::storage::Storage, project: &str, session: &str, ts: &str) {
    let mut ep = minimal_episode("Fix the auth middleware regression", "", None);
    ep.session_id = session.into();
    ep.project = project.into();
    let tags = crate::hooks::stop::episode_tags(&ep);
    insert_row(
        storage,
        &format!("ep-{project}-{session}"),
        &serde_json::to_string(&ep).unwrap(),
        &tags,
        ts,
    );
}

const RULE_TEXT: &str = "A standing rule from 10-12: “Keep receipts”";

/// A project transcript dir holding a session whose compact summary lists one pending item.
fn session_with_summary(sid: &str) -> (tempfile::TempDir, HookInput) {
    let tmp = tempfile::TempDir::new().unwrap();
    let summary = "6. Pending Tasks:\n   - Still need to check receipt:true in the compact log\n\nCompact receipt: turns 1..2. [[CSR:COMPACT_RECEIPT]]\n- request \"wait for the build\" -> csr_transcript(x)\n";
    let row =
        serde_json::json!({"type":"user","isCompactSummary":true,"message":{"content":summary}});
    std::fs::write(tmp.path().join(format!("{sid}.jsonl")), format!("{row}\n")).unwrap();
    let input = HookInput {
        session_id: Some("new-session".into()),
        transcript_path: Some(
            tmp.path()
                .join("new-session.jsonl")
                .to_string_lossy()
                .into_owned(),
        ),
        source: Some("resume".into()),
        ..Default::default()
    };
    (tmp, input)
}

fn ep_with_newer_ask() -> Episode {
    let mut ep = minimal_episode("spawn a worker to fix the old feature", "Fixed it", None);
    ep.recap_evidence_version = Some(crate::hooks::stop::RECAP_EVIDENCE_VERSION);
    ep.anchor_request = Some("spawn a worker to fix the old feature".into());
    ep.compact_anchor = Some(crate::hooks::stop::CompactAnchor {
        request: "what are we waiting for do we have a ready product?".into(),
        completions_untied: false,
        receipts: Some(Vec::new()),
    });
    ep
}

fn at(text: &str) -> DateTime<Utc> {
    temporal::parse_timestamp(text).unwrap()
}

fn snapshot_at(
    watermark: Option<&str>,
    anchor_request: Option<&str>,
    ask: Option<&str>,
) -> super::super::precompact::CompactSnapshot {
    use crate::hooks::stop::{CompactAnchor, CompletionReceipt};
    super::super::precompact::CompactSnapshot {
        anchor_request: anchor_request.map(str::to_string),
        latest_ask: ask.map(str::to_string),
        watermark: watermark.map(str::to_string),
        anchor: Some(CompactAnchor {
            request: "Now wire the staging endpoint.".into(),
            completions_untied: true,
            receipts: None,
        }),
        completions: vec![CompletionReceipt {
            claim: "fresh".into(),
            receipt: "2222222".into(),
        }],
        completions_untied: true,
        anchor_receipts: Some(vec![0]),
    }
}

fn stored_episode(watermark: Option<&str>, ask: Option<&str>) -> Episode {
    let mut ep = minimal_episode("Fix the auth middleware regression", "Fixed it", None);
    ep.anchor_request = Some("Fix the auth middleware regression.".into());
    ep.completions = vec![crate::hooks::stop::CompletionReceipt {
        claim: "stored".into(),
        receipt: "1111111".into(),
    }];
    ep.watermark = watermark.map(str::to_string);
    ep.latest_ask = ask.map(str::to_string);
    ep
}

const EARLIER: &str = "2026-07-08T09:00:00Z";
const LATER: &str = "2026-07-08T13:00:00Z";
const ASK_OLD: &str = "Why did the second worker finish so early?";
const ASK_NEW: &str = "Which crate pulls in the old parser?";

// ---- batch 4a: dirty tree, off-branch receipts, ANCHORS scope, briefing clock ----

fn git_in(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repo on `main` with one commit, plus a commit on a side branch. Returns
/// (dir, main sha, side sha); `main` is checked out.
fn repo_with_side_commit() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().unwrap();
    git_in(dir.path(), &["init", "-q", "-b", "main"]);
    for name in ["a.rs", "b.rs", "c.rs", "d.rs"] {
        std::fs::write(dir.path().join(name), name).unwrap();
    }
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "-q", "-m", "base"]);
    let on_main = git_in(dir.path(), &["rev-parse", "HEAD"]);
    git_in(dir.path(), &["checkout", "-q", "-b", "side"]);
    std::fs::write(dir.path().join("side.rs"), "s").unwrap();
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "-q", "-m", "side"]);
    let on_side = git_in(dir.path(), &["rev-parse", "HEAD"]);
    git_in(dir.path(), &["checkout", "-q", "main"]);
    (dir, on_main, on_side)
}

#[path = "session_start_tests/mixed_tests.rs"]
mod mixed_tests;

#[path = "session_start_tests/episode_relative_tests.rs"]
mod episode_relative_tests;

#[path = "session_start_tests/session_tests.rs"]
mod session_tests;
