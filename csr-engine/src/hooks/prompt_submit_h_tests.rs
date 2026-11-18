use super::*;

const NOTE: &str = "Here is a note offered by a side agent:\n\n> Heads up · The export queue drops the last frame.\n> - Check the mp4 before you post.";

#[test]
fn machine_forms_never_enter_h() {
    for prompt in [
        "Another Claude session sent a message:\n<cross-session-message from=\"peer\">Build finished.</cross-session-message>",
        "Another Claude session sent a message:\n<agent-message from=\"peer\">Build finished.</agent-message>",
        "[Cross-session idle notice] a task is idle.",
        "<task-notification>completed</task-notification>",
        "<system-reminder>retry</system-reminder>",
        "The coordinator sent a message while you were working:\nTry the build.",
        NOTE,
    ] {
        assert!(is_machine_prompt(None, prompt), "{prompt}");
        assert_eq!(early_exit_for(None, prompt), Some(EarlyExit::Silent));
    }
    assert_eq!(
        early_exit_for(Some("system"), "fix the export queue"),
        Some(EarlyExit::Silent)
    );
}

#[test]
fn human_and_mixed_prompts_keep_hs_original_query() {
    let tail = "move the reel launch to tomorrow and update the release notes";
    let mixed = format!("{NOTE}\n\n{tail}");
    for source in [None, Some("typed"), Some("")] {
        assert!(!is_machine_prompt(source, &mixed));
        assert_eq!(early_exit_for(source, &mixed), None);
        assert!(!is_machine_prompt(source, tail));
        assert_eq!(early_exit_for(source, tail), None);
    }
    // H's query preparation removes CSR emissions, not forwarded notes.
    assert_eq!(
        crate::extraction::provenance::strip_csr_emissions(&mixed),
        mixed
    );
}

#[test]
fn release_veto_passes_unknown_sources_to_hs_older_source_gate() {
    let ask = "please refactor the export queue so the reel renders once";
    for source in [Some("sdk"), Some("poll_event")] {
        assert!(!is_machine_prompt(source, ask));
        assert_eq!(early_exit_for(source, ask), Some(EarlyExit::Silent));
    }
    assert!(!is_machine_prompt(None, ask));
    assert_eq!(early_exit_for(None, ask), None);
}

#[test]
fn h_continuation_route_and_no_session_entry_remain_available() {
    assert_eq!(
        early_exit_for(None, "continue"),
        Some(EarlyExit::LineagePickup)
    );
    assert_eq!(
        early_exit_for(None, "resume"),
        Some(EarlyExit::LineagePickup)
    );
    // No session id is an input to neither the H early exit nor the transport
    // pre-dispatch check; the H pipeline can still choose an episode.
    assert_eq!(
        early_exit_for(None, "resume the export queue"),
        Some(EarlyExit::LineagePickup)
    );
}

#[test]
fn h_keeps_multiple_chunks_but_not_a_repeated_wrapper() {
    let mut handed = HandedOnce::default();
    let session = "earlier".to_string();
    assert!(handed.admit(
        Some(&session),
        false,
        "The queue fix used a bounded writer lock."
    ));
    assert!(handed.admit(
        Some(&session),
        false,
        "The release passed after the queue fix."
    ));
    assert!(!handed.admit(Some(&session), true, "The earlier work finished."));
    let other = "another".to_string();
    assert!(handed.admit(
        Some(&other),
        true,
        "A separate session fixed the tokenizer."
    ));
    assert!(!handed.admit(Some(&other), false, "The tokenizer changed again."));
}

async fn keyword_fixture(vector_count: usize) -> (Engine, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let mut search = crate::search::SearchEngine::new(8);
    let fresh = chrono::Utc::now().to_rfc3339();
    for i in 0..vector_count + 2 {
        let keyword = i >= vector_count;
        let id = if keyword {
            format!("kw-{}", i - vector_count)
        } else {
            format!("vec-{i}")
        };
        let vector = if keyword {
            [0.4, 0.916_515_1]
        } else {
            [1.0, 0.0]
        };
        let chunk = crate::import::ConversationChunk {
            id: id.clone(),
            conversation_id: format!("conv-{id}"),
            project_name: "project".into(),
            timestamp: fresh.clone(),
            content: if keyword {
                format!("implementation of parse_widget_identifier case {i}")
            } else {
                format!("unrelated conceptual match {i}")
            },
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: i,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &vector).unwrap();
        storage
            .insert_chunk_provenance(
                &id,
                &crate::provenance::ChunkProvenance {
                    author: crate::provenance::Speaker::User,
                    source_conv_id: chunk.conversation_id.clone(),
                    supersedes: None,
                },
            )
            .unwrap();
        search.insert_chunk(id, vector.to_vec());
    }
    let engine = Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );
    (engine, root)
}

#[tokio::test]
async fn h_keyword_hits_fill_only_unoccupied_slots() {
    for (vectors, keyword_count) in [(3, 2), (4, 1), (5, 0)] {
        let (engine, _root) = keyword_fixture(vectors).await;
        let hits = search_chunks_with_vec_core(
            &engine,
            "parse_widget_identifier",
            crate::search::hybrid::SearchMode::Hybrid,
            &[1.0, 0.0],
            10,
            0.5,
            "project",
            Some("current-session"),
            |_, _| Default::default(),
        )
        .await;
        let keyword_hits = hits
            .iter()
            .filter(|hit| {
                hit.memory_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("kw-"))
            })
            .count();
        assert_eq!(hits.len(), 5, "{vectors} vector hits");
        assert_eq!(keyword_hits, keyword_count, "{vectors} vector hits");
    }
}
