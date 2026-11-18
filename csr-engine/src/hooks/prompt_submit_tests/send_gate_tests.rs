use super::*;
use crate::hooks::prompt_log::DecisionReason;

/// A claim is a receipt only when its line went out: an unacknowledged render
/// and a line the send gate withheld both give it back.
#[test]
fn undelivered_or_withheld_dream_claim_is_released_but_a_delivered_one_survives() {
    use crate::storage::dream_delivery::{already_delivered, claim_delivery, DeliveryChannel};
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    // A delivered claim stays, so it goes last: no later case could claim again.
    for (acknowledged, withheld, delivered) in [
        (false, false, false),
        (true, true, false),
        (true, false, true),
    ] {
        assert!(claim_delivery(
            &storage,
            "dream",
            DeliveryChannel::Prompt,
            Some("session")
        ));
        let mut pending = PendingEffects {
            reason: None,
            storage: storage.clone(),
            receipts: storage.clone(),
            impressions: Vec::new(),
            retrievals: Vec::new(),
            dream_claims: vec![("dream".into(), Some("session".into()))],
            withheld_claims: Vec::new(),
            prompt_id: None,
            serve_only: false,
        };
        let held = || {
            storage
                .with_connection(|conn| {
                    Ok(already_delivered(conn, "dream", DeliveryChannel::Prompt))
                })
                .unwrap()
        };
        if withheld {
            pending.withhold_dream_claims(0);
        }
        if acknowledged {
            pending.commit_exposures();
            pending.commit_retrievals();
        }
        // Nothing is deleted while the render may still hold admission.
        assert!(held(), "acknowledged={acknowledged} withheld={withheld}");
        drop(pending);
        assert_eq!(
            held(),
            delivered,
            "acknowledged={acknowledged} withheld={withheld}"
        );
    }
}

const PROMPT: &str = "fix the queue race in src/queue/writer.rs please";

/// Fresh chunks from one earlier session at the given cosines to the query
/// vector `[1, 0]`, plus a code-evolution row for the file the prompt names.
fn engine(seeds: &[(f32, &str)]) -> (Engine, tempfile::TempDir) {
    let aged: Vec<(f32, &str, i64)> = seeds.iter().map(|&(c, t)| (c, t, 0)).collect();
    engine_aged(&aged)
}

/// As [`engine`], each seed also giving its age in days.
fn engine_aged(seeds: &[(f32, &str, i64)]) -> (Engine, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let mut search = crate::search::SearchEngine::new(8);
    let project = resolve_project_from_cwd(&root.path().to_string_lossy()).unwrap();
    for (i, &(cosine, content, age_days)) in seeds.iter().enumerate() {
        let vector = [cosine, (1.0 - cosine * cosine).sqrt()];
        let chunk = crate::import::ConversationChunk {
            id: format!("candidate-{i}"),
            conversation_id: "previous".into(),
            project_name: project.clone(),
            timestamp: (chrono::Utc::now() - chrono::Duration::days(age_days)).to_rfc3339(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: i,
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
    storage
        .insert_code_evolution(
            "previous",
            &project,
            "src/queue/writer.rs",
            "rust",
            "Edit",
            "[\"append_locked\"]",
            "",
            "",
            "",
            "",
            "",
            None,
        )
        .unwrap();
    let engine = Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::disabled()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );
    (engine, root)
}

/// Renders the content path with an optional correlated episode, the way
/// `handle_classified_intent` hands one over.
async fn render(
    engine: &Engine,
    root: &std::path::Path,
    pickup: bool,
) -> (Option<DecisionReason>, String, PendingEffects) {
    let input = HookInput {
        session_id: Some("current".into()),
        prompt: Some(PROMPT.into()),
        ..Default::default()
    };
    let memo = pickup.then(|| {
        let mut ep = super::code_map_episode();
        ep.session_id = "earlier".into();
        ep.request = "Rework the queue writer lock".into();
        (ep, "2 days ago".to_string(), 0.6)
    });
    let mut effects = super::route_decision_tests::effects(engine);
    let mut output = String::new();
    handle_content(
        &input,
        engine,
        root,
        &mut output,
        &mut effects,
        PROMPT,
        &normalized_words(PROMPT),
        &[1.0, 0.0],
        "other",
        Some(memo),
    )
    .await
    .unwrap();
    (effects.reason, output, effects)
}

/// One strong chunk, a PICKUP and a code-evolution line: before the gate all
/// three went out. Now the pool is thin and nothing goes out, not even the
/// lines that were never search results.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_thin_pool_sends_nothing_not_even_its_pickup_or_review_lines() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let _policy = super::decision_tests::Policy::set(&[("CSR_SEARCH_MODE", "vector")]);
    let (engine, root) = engine(&[(0.9, "Lock taken before the append.")]);
    {
        let _gate = crate::injection::send_gate::Switch::off();
        let (reason, output, _) = render(&engine, root.path(), true).await;
        assert_eq!(reason, Some(DecisionReason::OnTopic), "{output}");
        for line in ["CSR PICKUP", "Lock taken", "src/queue/writer.rs"] {
            assert!(output.contains(line), "gate off sends {line}: {output}");
        }
    }
    let _gate = crate::injection::send_gate::Switch::on();
    let (reason, output, effects) = render(&engine, root.path(), true).await;
    assert_eq!(reason, Some(DecisionReason::ThinPool), "{output}");
    assert!(output.is_empty(), "{output}");
    assert!(effects.impressions.iter().all(|i| i.items.is_empty()));
    assert!(effects.retrievals.is_empty());
}

/// Three strong chunks clear both conditions; the block goes out whole, its
/// PICKUP and review lines included.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn three_strong_items_send_the_whole_block() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let _policy = super::decision_tests::Policy::set(&[("CSR_SEARCH_MODE", "vector")]);
    let _gate = crate::injection::send_gate::Switch::on();
    let (engine, root) = engine(&[
        (0.92, "Lock taken before the append."),
        (0.9, "Writer test runs two producers."),
        (0.88, "One fsync per batch now."),
    ]);
    let (reason, output, _) = render(&engine, root.path(), true).await;
    assert_eq!(reason, Some(DecisionReason::OnTopic), "{output}");
    for line in [
        "CSR PICKUP",
        "Lock taken",
        "two producers",
        "One fsync",
        "src/queue/writer.rs",
    ] {
        assert!(output.contains(line), "{line}: {output}");
    }
}

/// With nothing to send, the gate stays out of the way and the search's own
/// reason stands.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn an_empty_search_keeps_its_own_reason_under_the_gate() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let _policy = super::decision_tests::Policy::set(&[
        ("CSR_SEARCH_MODE", "vector"),
        ("CSR_NO_GRAPH_SLICES", "1"),
    ]);
    let _gate = crate::injection::send_gate::Switch::on();
    let root = tempfile::tempdir().unwrap();
    let search = crate::search::SearchEngine::new(8);
    let engine = Engine::from_parts(
        std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap()),
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::disabled()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );
    let (reason, output, _) = render(&engine, root.path(), false).await;
    assert_eq!(reason, Some(DecisionReason::NoCandidates), "{output}");
    assert!(output.is_empty());
}

/// A strong result that a hook filter drops cannot vouch for the three weak
/// ones that render. The noisy chunk ranks first (about 0.56) and is skipped as
/// CSR dev-history; the survivors score about 0.42, under the floor. Reading
/// the floor off the ranked list before the filters sent this block.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_filtered_strong_result_does_not_authorize_three_weak_items() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let _policy = super::decision_tests::Policy::set(&[("CSR_SEARCH_MODE", "vector")]);
    let (engine, root) = engine_aged(&[
        (0.95, "hook success: lock taken before the append.", 10),
        (0.7, "Writer test runs two producers.", 20),
        (0.7, "One fsync per batch now.", 20),
        (0.7, "Queue rotation keeps the lock.", 20),
    ]);
    {
        let _gate = crate::injection::send_gate::Switch::off();
        let (reason, output, _) = render(&engine, root.path(), false).await;
        assert_eq!(reason, Some(DecisionReason::OnTopic), "{output}");
        for line in ["two producers", "One fsync", "rotation keeps"] {
            assert!(output.contains(line), "gate off sends {line}: {output}");
        }
        assert!(!output.contains("hook success"), "{output}");
    }
    let _gate = crate::injection::send_gate::Switch::on();
    let (reason, output, _) = render(&engine, root.path(), false).await;
    assert_eq!(reason, Some(DecisionReason::BelowFloor), "{output}");
    assert!(output.is_empty(), "{output}");
}
