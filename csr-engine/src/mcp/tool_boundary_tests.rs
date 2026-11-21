//! The tool boundary: ids that name nothing are errors, a session with no
//! stored learnings still answers with its entries, scratch checkouts stay out
//! of the code graph, and csr_search_insights carries aggregates.

use super::*;
use crate::storage::codegraph::{EdgeRow, NodeRow};

fn server(dir: &std::path::Path, storage: Arc<Storage>) -> CsrServer {
    CsrServer::new(
        storage,
        Arc::new(EmbeddingEngine::disabled()),
        Arc::new(RwLock::new(SearchEngine::new(8))),
        dir.to_path_buf(),
        dir.join("index-v3"),
        None,
        None,
    )
}

fn invalid_params(result: Result<CallToolResult, rmcp::ErrorData>, what: &str) {
    let error = result.expect_err(what);
    assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS, "{what}");
}

const UNKNOWN: &str = "00000000-0000-4000-8000-000000000000";
const TRAVERSAL: &str = "../../../../etc/passwd";

#[tokio::test]
async fn unknown_and_traversal_ids_are_errors_for_the_conversation_tools() {
    let dir = tempfile::tempdir().unwrap();
    let server = server(dir.path(), Arc::new(Storage::open_memory().unwrap()));
    for id in [UNKNOWN, TRAVERSAL] {
        let params: GetFullConversationParams =
            serde_json::from_value(serde_json::json!({ "conversation_id": id })).unwrap();
        invalid_params(
            server.get_full_conversation(Parameters(params)).await,
            "get_full_conversation",
        );
        let params: GetSessionLearningsParams =
            serde_json::from_value(serde_json::json!({ "session_id": id })).unwrap();
        invalid_params(
            server.get_session_learnings(Parameters(params)).await,
            "get_session_learnings",
        );
        let params: TranscriptParams =
            serde_json::from_value(serde_json::json!({ "session": id, "view": "receipt" }))
                .unwrap();
        invalid_params(server.transcript(Parameters(params)).await, "transcript");
    }
}

#[tokio::test]
async fn a_session_without_stored_learnings_lists_its_indexed_entries() {
    use crate::transcript::chunks::test_support::{add_chunk, temp_storage};
    let dir = tempfile::tempdir().unwrap();
    let storage = temp_storage(dir.path());
    add_chunk(
        &storage,
        "c1",
        "live-session-1",
        "proj",
        "2026-11-09T10:00:00Z",
        0,
        "user",
        1,
        "add a cap to the search by file output",
    );
    let server = server(dir.path(), Arc::new(storage));
    let params: GetSessionLearningsParams =
        serde_json::from_value(serde_json::json!({ "session_id": "live-session-1" })).unwrap();
    let result = server.get_session_learnings(Parameters(params)).await;
    let text = result
        .unwrap()
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect::<String>();
    assert!(
        text.contains("add a cap to the search by file output"),
        "{text}"
    );
    assert!(!text.contains("No learnings stored"), "{text}");
}

#[test]
fn tool_results_are_capped_at_twenty_kilobytes() {
    let big = "a line of tool output that repeats\n".repeat(2_000);
    let result = tool_result(Ok(big)).unwrap();
    let text: String = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect();
    assert!(text.len() <= 20 * 1024, "{} bytes", text.len());
    assert!(text.contains("<truncated omitted_bytes="), "{text}");
}

#[test]
fn code_graph_leaves_out_sources_under_tmp() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let project = "demo";
    let family = vec![project.to_string()];
    for (id, file) in [
        ("target-real", "src/a.rs"),
        ("target-tmp", "/private/tmp/sbx/src/a.rs"),
    ] {
        storage
            .upsert_code_node(&NodeRow {
                id: id.into(),
                project: project.into(),
                file: file.into(),
                kind: "function".into(),
                name: "shared_symbol".into(),
                ..NodeRow::default()
            })
            .unwrap();
    }
    for (id, name, file, target) in [
        ("caller-real", "real_caller", "src/b.rs", "target-real"),
        (
            "caller-tmp",
            "scratch_caller",
            "/tmp/rcperf/src/b.rs",
            "target-tmp",
        ),
    ] {
        storage
            .upsert_code_node(&NodeRow {
                id: id.into(),
                project: project.into(),
                file: file.into(),
                kind: "function".into(),
                name: name.into(),
                ..NodeRow::default()
            })
            .unwrap();
        storage
            .replace_code_file_edges(
                project,
                file,
                &[EdgeRow {
                    src_id: id.into(),
                    dst_id: target.into(),
                    kind: "calls".into(),
                    src_file: file.into(),
                    resolved: 1,
                    weight: 1.0,
                    ..EdgeRow::default()
                }],
            )
            .unwrap();
    }
    for mode in ["callers", "neighbors"] {
        let rendered = tools::code_graph_with_map(
            &storage,
            Some("shared_symbol"),
            None,
            mode,
            5,
            project,
            &family,
            true,
        )
        .unwrap();
        assert!(!rendered.contains("/tmp/"), "{mode}: {rendered}");
        assert!(!rendered.contains("scratch_caller"), "{mode}: {rendered}");
    }
    let callers = tools::code_graph_with_map(
        &storage,
        Some("shared_symbol"),
        None,
        "callers",
        5,
        project,
        &family,
        true,
    )
    .unwrap();
    assert!(callers.contains("real_caller"), "{callers}");
}

#[test]
fn search_insights_carries_bounded_aggregates() {
    use crate::format::EnrichedResult;
    use crate::import::ConversationChunk;
    let row = |id: &str, conversation: &str, content: &str| EnrichedResult {
        score: 0.6,
        chunk: ConversationChunk {
            id: id.into(),
            conversation_id: conversation.into(),
            project_name: "p".into(),
            timestamp: "2026-11-03T10:00:00Z".into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        },
        resolution: None,
        validity_demoted: false,
        trust: crate::provenance::TrustTier::Unknown,
    };
    let out = crate::format::format_search_insights(
        &[
            row(
                "1",
                "conv-a",
                "timeline grouping lives in src/temporal/mod.rs",
            ),
            row(
                "2",
                "conv-b",
                "timeline buckets use src/temporal/mod.rs too",
            ),
        ],
        "timeline",
    );
    assert!(out.contains("<top_concepts>"), "{out}");
    assert!(
        out.contains("<concept count=\"2\">timeline</concept>"),
        "{out}"
    );
    assert!(
        out.contains("<file count=\"2\">src/temporal/mod.rs</file>"),
        "{out}"
    );
    assert!(out.contains("<week count=\"2\">2026-W45</week>"), "{out}");
    assert!(out.contains("<cid>conv-a</cid>"), "{out}");
}

fn text_of(result: CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect()
}

#[test]
fn code_graph_finds_the_real_caller_behind_many_scratch_callers() {
    // 45 scratch callers sort ahead of the one real caller. A fetch of a few
    // times `limit` rows holds only scratch rows, so nothing real would show.
    let storage = Arc::new(Storage::open_memory().unwrap());
    let project = "demo";
    let family = vec![project.to_string()];
    let node = |id: &str, name: &str, file: &str| NodeRow {
        id: id.into(),
        project: project.into(),
        file: file.into(),
        kind: "function".into(),
        name: name.into(),
        ..NodeRow::default()
    };
    storage
        .upsert_code_node(&node("target", "shared_symbol", "src/a.rs"))
        .unwrap();
    let mut callers: Vec<(String, String, String)> = (0..45)
        .map(|i| {
            (
                format!("a-scratch-{i:02}"),
                format!("scratch_caller_{i}"),
                format!("/tmp/rcperf/{i}.rs"),
            )
        })
        .collect();
    callers.push(("z-real".into(), "real_caller".into(), "src/b.rs".into()));
    for (id, name, file) in &callers {
        storage.upsert_code_node(&node(id, name, file)).unwrap();
        storage
            .replace_code_file_edges(
                project,
                file,
                &[EdgeRow {
                    src_id: id.clone(),
                    dst_id: "target".into(),
                    kind: "calls".into(),
                    src_file: file.clone(),
                    resolved: 1,
                    weight: 1.0,
                    ..EdgeRow::default()
                }],
            )
            .unwrap();
    }
    for mode in ["callers", "neighbors"] {
        let rendered = tools::code_graph_with_map(
            &storage,
            Some("shared_symbol"),
            None,
            mode,
            5,
            project,
            &family,
            true,
        )
        .unwrap();
        assert!(rendered.contains("real_caller"), "{mode}: {rendered}");
        assert!(!rendered.contains("/tmp/"), "{mode}: {rendered}");
    }
}

#[tokio::test]
async fn get_full_conversation_of_a_long_session_stays_under_the_cap_and_says_how_to_continue() {
    use crate::transcript::chunks::test_support::{add_chunk, temp_storage};
    let dir = tempfile::tempdir().unwrap();
    let storage = temp_storage(dir.path());
    for i in 0..40 {
        add_chunk(
            &storage,
            &format!("long-chunk-{i}"),
            "long-session-1",
            "proj",
            "2026-11-09T10:00:00Z",
            i,
            "user",
            1,
            &"word ".repeat(180),
        );
    }
    let server = server(dir.path(), Arc::new(storage));
    let params: GetFullConversationParams =
        serde_json::from_value(serde_json::json!({ "conversation_id": "long-session-1" })).unwrap();
    let text = text_of(
        server
            .get_full_conversation(Parameters(params))
            .await
            .unwrap(),
    );
    assert!(text.len() <= 20 * 1024, "{} bytes", text.len());
    assert!(
        text.contains("more chunks not shown"),
        "{}",
        &text[text.len() - 400..]
    );
    assert!(
        text.contains("csr_transcript(session=\"long-session-1\""),
        "{text}"
    );
}

#[tokio::test]
async fn csr_why_keyword_evidence_skips_a_logged_copy_of_the_question_at_limit_one() {
    use crate::transcript::chunks::test_support::{add_chunk, temp_storage};
    let dir = tempfile::tempdir().unwrap();
    let storage = temp_storage(dir.path());
    let query = "How does the retry backoff work for outgoing hook calls?";
    add_chunk(
        &storage,
        "copy",
        "conv-probe",
        "p",
        "2026-11-01T00:00:00Z",
        0,
        "tool_result",
        1,
        &format!("{{\"query\": \"{query}\"}}"),
    );
    add_chunk(
        &storage,
        "answer",
        "conv-real",
        "p",
        "2026-11-01T00:00:00Z",
        0,
        "assistant",
        1,
        "The hook client retries outgoing calls and doubles the backoff after each failure, \
         up to a cap of 30 seconds, and it logs every attempt that it makes along the way.",
    );
    let storage = Arc::new(storage);
    let search = Arc::new(RwLock::new(SearchEngine::new(8)));
    let out = tools::keyword_fallback(&storage, &search, query, 1, None, tools::KeywordTool::Why)
        .await
        .unwrap();
    assert!(out.contains("conv-real"), "{out}");
    assert!(!out.contains("conv-probe"), "{out}");
}

#[tokio::test]
async fn csr_why_keyword_evidence_reaches_an_answer_behind_many_logged_copies() {
    use crate::transcript::chunks::test_support::{add_chunk, temp_storage};
    let dir = tempfile::tempdir().unwrap();
    let storage = temp_storage(dir.path());
    let query = "How does the retry backoff work for outgoing hook calls?";
    for i in 0..8 {
        add_chunk(
            &storage,
            &format!("copy-{i}"),
            &format!("conv-probe-{i}"),
            "p",
            "2026-11-01T00:00:00Z",
            0,
            "tool_result",
            1,
            &format!("{{\"query\": \"{query}\"}}"),
        );
    }
    add_chunk(
        &storage,
        "answer",
        "conv-real",
        "p",
        "2026-11-01T00:00:00Z",
        0,
        "assistant",
        1,
        "The hook client retries outgoing calls and doubles the backoff after each failure, \
         up to a cap of 30 seconds, and it logs every attempt that it makes along the way.",
    );
    let storage = Arc::new(storage);
    let search = Arc::new(RwLock::new(SearchEngine::new(8)));
    let out = tools::keyword_fallback(&storage, &search, query, 1, None, tools::KeywordTool::Why)
        .await
        .unwrap();
    assert!(out.contains("conv-real"), "{out}");
    assert!(!out.contains("conv-probe"), "{out}");
}

#[tokio::test]
async fn get_full_conversation_of_non_ascii_text_keeps_its_closing_envelope_and_hint() {
    use crate::transcript::chunks::test_support::{add_chunk, temp_storage};
    let dir = tempfile::tempdir().unwrap();
    let storage = temp_storage(dir.path());
    for i in 0..40 {
        add_chunk(
            &storage,
            &format!("wide-chunk-{i}"),
            "wide-session-1",
            "proj",
            "2026-11-09T10:00:00Z",
            i,
            "user",
            1,
            &"語".repeat(180),
        );
    }
    let server = server(dir.path(), Arc::new(storage));
    let params: GetFullConversationParams =
        serde_json::from_value(serde_json::json!({ "conversation_id": "wide-session-1" })).unwrap();
    let text = text_of(
        server
            .get_full_conversation(Parameters(params))
            .await
            .unwrap(),
    );
    assert!(text.len() <= 20 * 1024, "{} bytes", text.len());
    assert!(!text.contains("<truncated"), "cut by the byte cap");
    assert!(text.contains("csr_transcript(session=\"wide-session-1\""));
    assert!(text.ends_with("</conversation_file>"));
}

#[tokio::test]
async fn a_long_error_message_is_bounded_and_scrubbed_on_the_way_out() {
    let key = format!("ghp_{}", "C".repeat(36));
    let error = tool_result(Err(anyhow::anyhow!(
        "bad argument {key} {}",
        "x".repeat(50_000)
    )))
    .unwrap_err();
    assert!(
        error.message.len() < 4 * 1024,
        "{} bytes",
        error.message.len()
    );
    assert!(!error.message.contains(&key[..8]), "{}", error.message);
}
