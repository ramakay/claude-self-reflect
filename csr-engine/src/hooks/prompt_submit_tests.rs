use super::*;

fn handed(conversation_id: Option<&str>) -> crate::injection::predictor::ScoredResult {
    crate::injection::predictor::ScoredResult {
        content: "the import reuses the upload queue".into(),
        raw_score: 0.8,
        final_score: 0.8,
        source: "conversation".into(),
        signals: Vec::new(),
        memory_id: Some("chunk-1".into()),
        conversation_id: conversation_id.map(str::to_owned),
        timestamp: None,
        author: None,
        min_trust: crate::provenance::TrustTier::Unknown,
    }
}

/// Forty-five dumps, alternately under the asker's label and another, then
/// three reports under the asker's: reaching a report takes the whole
/// refill. `conversation_of` names the session of each dump filed under the
/// other label.
async fn refill_engine(conversation_of: impl Fn(usize) -> String) -> (Engine, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let mut search = crate::search::SearchEngine::new(128);
    let mut rows: Vec<(String, String, String, f32)> = (0..45)
        .map(|i| {
            (
                format!("dump-{i:02}"),
                if i % 2 == 1 {
                    conversation_of(i)
                } else {
                    format!("conv-home-{i}")
                },
                format!("{i}: let value_{i} = table[{i}];"),
                1.0 - 0.002 * i as f32,
            )
        })
        .collect();
    for r in 0..3 {
        rows.push((
            format!("report-{r}"),
            format!("conv-report-{r}"),
            format!("Cleanup {r} is done. The simulator folder dropped from 284 GB to 67 GB."),
            0.7,
        ));
    }
    for (seq, (id, conversation, content, cosine)) in rows.iter().enumerate() {
        let is_dump = id.starts_with("dump");
        let vec = [*cosine, (1.0 - cosine * cosine).sqrt()];
        let chunk = crate::import::ConversationChunk {
            id: id.clone(),
            conversation_id: conversation.clone(),
            project_name: if is_dump && seq % 2 == 1 {
                "other"
            } else {
                "project"
            }
            .into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: content.clone(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &vec).unwrap();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET min_trust=1, tool_result_share=?2 WHERE id=?1",
                    rusqlite::params![id, if is_dump { 1.0 } else { 0.0 }],
                )?;
                Ok(())
            })
            .unwrap();
        if is_dump && seq % 2 == 1 {
            storage
                .upsert_import_state_explicit(
                    &format!("/cc/projects/-u-projects-other/{conversation}.jsonl"),
                    conversation,
                    1,
                    "0",
                )
                .unwrap();
        }
        search.insert_chunk(id.clone(), vec.to_vec());
    }
    let engine = crate::engine::Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );
    (engine, root)
}

/// A check that ran out of time judged nothing: its conversations are not
/// "not this repository", and a later pass asks again.
#[tokio::test]
async fn a_conversation_the_check_never_reached_is_asked_again() {
    let (engine, _root) = refill_engine(|_| "conv-shared".into()).await;
    let calls = std::cell::Cell::new(0usize);
    let _ = search_chunks_with_vec_core(
        &engine,
        "",
        crate::search::hybrid::SearchMode::Vector,
        &[1.0, 0.0],
        15,
        0.55,
        "project",
        None,
        |paths, _| {
            calls.set(calls.get() + 1);
            super::super::scope_folder::RepoCheck {
                widened: Default::default(),
                unchecked: paths.keys().cloned().collect(),
            }
        },
    )
    .await;
    assert!(calls.get() >= 2, "{} checks", calls.get());
}

/// Hybrid fixture: one strong semantic chunk, and several chunks that only the
/// keyword channel can reach (their vectors sit below any cosine floor), each
/// failing a different hook filter except `kw-ok`.
#[allow(clippy::type_complexity)]
async fn hybrid_engine() -> (Engine, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let mut search = crate::search::SearchEngine::new(8);
    let fresh = chrono::Utc::now().to_rfc3339();
    let old = (chrono::Utc::now() - chrono::Duration::days(60)).to_rfc3339();
    let keyword = "implementation of parse_widget_identifier";
    let csr_rule = format!(
        "A standing rule from 10-12: \u{201c}{} parse_widget_identifier\u{201d}",
        "x".repeat(400)
    );
    let rows: [(&str, &str, &str, &str, &str, [f32; 2]); 7] = [
        (
            "sem-high",
            "conv-sem",
            "project",
            &fresh,
            "unrelated conceptual match",
            [1.0, 0.0],
        ),
        (
            "kw-ok",
            "conv-ok",
            "project",
            &fresh,
            keyword,
            [0.4, 0.916_515_1],
        ),
        (
            "kw-other-project",
            "conv-other",
            "elsewhere",
            &fresh,
            keyword,
            [0.4, 0.916_515_1],
        ),
        (
            "kw-self",
            "current-session",
            "project",
            &fresh,
            keyword,
            [0.4, 0.916_515_1],
        ),
        (
            "kw-old",
            "conv-old",
            "project",
            &old,
            keyword,
            [0.4, 0.916_515_1],
        ),
        (
            "kw-csr",
            "conv-csr",
            "project",
            &fresh,
            &csr_rule,
            [0.4, 0.916_515_1],
        ),
        (
            "kw-unrelated",
            "conv-un",
            "project",
            &fresh,
            "nothing to match here",
            [0.4, 0.916_515_1],
        ),
    ];
    for (seq, (id, conv, project, ts, content, vec)) in rows.iter().enumerate() {
        let chunk = crate::import::ConversationChunk {
            id: (*id).into(),
            conversation_id: (*conv).into(),
            project_name: (*project).into(),
            timestamp: (*ts).into(),
            content: (*content).into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, vec).unwrap();
        storage
            .insert_chunk_provenance(
                id,
                &crate::provenance::ChunkProvenance {
                    author: crate::provenance::Speaker::User,
                    source_conv_id: chunk.conversation_id.clone(),
                    supersedes: None,
                },
            )
            .unwrap();
        search.insert_chunk(chunk.id.clone(), vec.to_vec());
    }
    let engine = crate::engine::Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );
    (engine, root)
}

async fn hybrid_ids(
    engine: &Engine,
    mode: crate::search::hybrid::SearchMode,
) -> Vec<(String, f32)> {
    search_chunks_with_vec_core(
        engine,
        "parse_widget_identifier",
        mode,
        &[1.0, 0.0],
        10,
        0.5,
        "project",
        Some("current-session"),
        |_, _| Default::default(),
    )
    .await
    .into_iter()
    .map(|r| (r.memory_id.unwrap(), r.score))
    .collect()
}

/// `n_vector` chunks that match by vector, and two that only the keyword
/// channel reaches.
async fn fill_engine(n_vector: usize) -> (Engine, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let mut search = crate::search::SearchEngine::new(8);
    let fresh = chrono::Utc::now().to_rfc3339();
    let mut rows: Vec<(String, String, [f32; 2])> = (0..n_vector)
        .map(|i| {
            (
                format!("vec-{i}"),
                format!("unrelated conceptual match {i}"),
                [1.0, 0.0],
            )
        })
        .collect();
    for i in 0..2 {
        rows.push((
            format!("kw-{i}"),
            format!("implementation {i} of parse_widget_identifier"),
            [0.4, 0.916_515_1],
        ));
    }
    for (seq, (id, content, vec)) in rows.iter().enumerate() {
        let chunk = crate::import::ConversationChunk {
            id: id.clone(),
            conversation_id: format!("conv-{id}"),
            project_name: "project".into(),
            timestamp: fresh.clone(),
            content: content.clone(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, vec).unwrap();
        storage
            .insert_chunk_provenance(
                id,
                &crate::provenance::ChunkProvenance {
                    author: crate::provenance::Speaker::User,
                    source_conv_id: chunk.conversation_id.clone(),
                    supersedes: None,
                },
            )
            .unwrap();
        search.insert_chunk(chunk.id.clone(), vec.to_vec());
    }
    let engine = crate::engine::Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );
    (engine, root)
}

/// Engine over three chunks filed under three labels, with the transcript
/// paths `import_state` would hold for them. Returns the recalled ids and
/// the path map `same_repo` was handed (`None` when it was never called).
async fn recall_across_labels(
    project: &str,
    import_rows: &[(&str, &str)],
    widen: &[(&str, &str)],
) -> (Vec<String>, Option<HashMap<String, Vec<String>>>) {
    let root = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let mut search = crate::search::SearchEngine::new(8);
    for (id, conversation_id, label) in [
        ("root-chunk", "root-session", "repo"),
        ("sub-chunk", "sub-session", "repo-sub"),
        ("other-chunk", "other-session", "other"),
    ] {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conversation_id.into(),
            project_name: label.into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: format!("work recorded under {label}"),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
        search.insert_chunk(chunk.id.clone(), vec![1.0, 0.0]);
    }
    for (file_path, conversation_id) in import_rows {
        storage
            .upsert_import_state_explicit(file_path, conversation_id, 1, "0")
            .unwrap();
    }
    let engine = crate::engine::Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );
    let seen = std::cell::RefCell::new(None);
    let mut ids: Vec<String> = search_chunks_with_vec_core(
        &engine,
        "",
        crate::search::hybrid::SearchMode::Vector,
        &[1.0, 0.0],
        10,
        0.0,
        project,
        None,
        |paths, _| {
            *seen.borrow_mut() = Some(paths.clone());
            super::super::scope_folder::RepoCheck {
                widened: widen
                    .iter()
                    .map(|(id, label)| (id.to_string(), HashSet::from([label.to_string()])))
                    .collect(),
                unchecked: HashSet::new(),
            }
        },
    )
    .await
    .into_iter()
    .filter_map(|result| result.memory_id)
    .collect();
    ids.sort();
    (ids, seen.into_inner())
}

const IMPORT_ROWS: [(&str, &str); 3] = [
    (
        "/cc/projects/-u-projects-repo/root-session.jsonl",
        "root-session",
    ),
    (
        "/cc/projects/-u-projects-repo-sub/sub-session.jsonl",
        "sub-session",
    ),
    (
        "/cc/projects/-u-projects-other/other-session.jsonl",
        "other-session",
    ),
];

// --- format_code_map (Feature B, exploration-intent injection) ---

fn code_map_episode() -> crate::hooks::stop::Episode {
    crate::hooks::stop::Episode {
        schema: "csr_episode_v1".into(),
        session_id: "abc123".into(),
        project: "test-project".into(),
        timestamp: "2026-05-17T00:00:00Z".into(),
        request: "test request".into(),
        investigated: vec![],
        completed: "done".into(),
        next_steps: None,
        blockers: None,
        outcome: "partial".into(),
        error_signatures: vec![],
        tools_used: vec![],
        files_modified: vec![
            "src/radio/RadioSheet.swift".into(),
            "src/radio/ChannelRing.swift".into(),
        ],
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
        closing_receipt: false,
        outcome_version: Some(1),
        latest_ask: None,
        watermark: None,
        recap_extras: Default::default(),
        anchors: vec![crate::extraction::anchors::FunctionAnchor {
            file: "src/radio/RadioSheet.swift".into(),
            node_kind: "file".into(),
            name: "RadioSheet.swift".into(),
            body_hash: "h".into(),
        }],
    }
}

/// The prompt pasted live on 2026-10-10: the pane's hand-off send text plus
/// the person's question (session id inside replaced by a dummy).
const PASTED_HANDOFF: &str = "CSR handed you these this session: Corpora ingestion and watcher setup · 2 weeks ago · csr-engine · 5×; Hairline skill retrieval and agent proposal · 2 days ago · csr-engine · 3×; Implement the following plan: · 7 months ago · 2×; Implement the following plan: · 7 months ago; When claude is killed or closed it generates a session e.g: claude --resume 00000000-0000-4000-8000-000000000000 <- the session is generated, can CSR retreive it - what enrichment is needed to catch \" · 5 months ago <<< This Handed to Claude seems not useful thoughts?";

#[path = "prompt_submit_tests/mixed_tests.rs"]
mod mixed_tests;

#[path = "prompt_submit_tests/prompt_age_tests.rs"]
mod prompt_age_tests;

#[path = "prompt_submit_tests/decision_tests.rs"]
mod decision_tests;

#[path = "prompt_submit_tests/route_decision_tests.rs"]
mod route_decision_tests;

#[path = "prompt_submit_tests/chunk_decision_tests.rs"]
mod chunk_decision_tests;

#[path = "prompt_submit_tests/probe_latency_tests.rs"]
mod probe_latency_tests;

#[path = "prompt_submit_tests/send_gate_tests.rs"]
mod send_gate_tests;
