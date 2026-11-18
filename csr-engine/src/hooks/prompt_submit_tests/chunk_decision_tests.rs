use super::*;
use crate::hooks::prompt_log::DecisionReason;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn chunk_decisions_follow_search_and_selection_gates() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let _policy = super::decision_tests::Policy::content_only();
    for (seed, expected) in [
        (None, DecisionReason::NoCandidates),
        (
            Some((0.2, "The queue now serializes writes safely.")),
            DecisionReason::BelowFloor,
        ),
        (
            Some((
                0.9,
                "prompt_submit_hook now serializes queue writes safely.",
            )),
            DecisionReason::LookalikeOnly,
        ),
        (
            Some((0.9, "The queue now serializes writes safely.")),
            DecisionReason::OnTopic,
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
        let mut search = crate::search::SearchEngine::new(8);
        let project = resolve_project_from_cwd(&root.path().to_string_lossy()).unwrap();
        if let Some((cosine, content)) = seed {
            let cosine: f32 = cosine;
            let vector = [cosine, (1.0 - cosine * cosine).sqrt()];
            let chunk = crate::import::ConversationChunk {
                id: "candidate".into(),
                conversation_id: "previous".into(),
                project_name: project,
                timestamp: chrono::Utc::now().to_rfc3339(),
                content: content.into(),
                message_count: 1,
                summary: None,
                author: crate::provenance::Speaker::User,
                seq: 0,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &vector).unwrap();
            storage
                .insert_chunk_provenance(
                    &chunk.id,
                    &crate::provenance::ChunkProvenance {
                        author: crate::provenance::Speaker::User,
                        source_conv_id: chunk.conversation_id.clone(),
                        supersedes: None,
                    },
                )
                .unwrap();
            search.insert_chunk(chunk.id, vector.to_vec());
        }
        let engine = Engine::from_parts(
            storage.clone(),
            std::sync::Arc::new(crate::embeddings::EmbeddingEngine::disabled()),
            std::sync::Arc::new(tokio::sync::RwLock::new(search)),
            root.path().to_path_buf(),
        );
        let prompt = "fix the queue serialization race";
        let input = HookInput {
            session_id: Some("current".into()),
            prompt: Some(prompt.into()),
            ..Default::default()
        };
        let mut effects = super::route_decision_tests::effects(&engine);
        let mut output = String::new();
        handle_content(
            &input,
            &engine,
            root.path(),
            &mut output,
            &mut effects,
            prompt,
            &normalized_words(prompt),
            &[1.0, 0.0],
            "other",
            Some(None),
        )
        .await
        .unwrap();
        assert_eq!(effects.reason, Some(expected), "{seed:?}: {output}");
        assert_eq!(!output.is_empty(), expected == DecisionReason::OnTopic);
        assert!(!engine.embeddings().is_loaded());
    }
}
