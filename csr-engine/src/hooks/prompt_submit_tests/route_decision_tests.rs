use super::*;
use crate::hooks::intent::Intent;
use crate::hooks::prompt_log::DecisionReason;

pub(super) fn effects(engine: &Engine) -> PendingEffects {
    PendingEffects {
        reason: None,
        storage: engine.storage().clone(),
        receipts: engine.storage().clone(),
        impressions: Vec::new(),
        retrievals: Vec::new(),
        dream_claims: Vec::new(),
        withheld_claims: Vec::new(),
        prompt_id: None,
        serve_only: false,
    }
}

fn fixture(seed: bool, files: bool) -> (Engine, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let mut search = crate::search::SearchEngine::new(8);
    if seed {
        let mut ep = super::code_map_episode();
        ep.session_id = "previous".into();
        ep.project = resolve_project_from_cwd(&root.path().to_string_lossy()).unwrap();
        ep.timestamp = chrono::Utc::now().to_rfc3339();
        ep.request = "Repair token validation in the authentication middleware".into();
        ep.completed = "Fixed token validation and verified the regression".into();
        ep.next_steps = Some("Deploy to staging".into());
        if !files {
            ep.files_modified.clear();
            ep.anchors.clear();
        }
        storage
            .insert_reflection(
                "episode",
                &serde_json::to_string(&ep).unwrap(),
                &crate::hooks::stop::episode_tags(&ep),
                &[1.0, 0.0],
            )
            .unwrap();
        search.insert_reflection("episode".into(), vec![1.0, 0.0]);
    }
    let engine = Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::disabled()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );
    (engine, root)
}

fn assert_undecided_delivery() {
    let at = chrono::Utc::now();
    let line = format!(
        "{} CSR hook prompt-submit [project]: transport=socket prompt_id=real injected=0",
        at.to_rfc3339()
    );
    let report = crate::telemetry::prompt_delivery::report_at(&line, at);
    let p = &report.last_72h["project"];
    assert_eq!((p.n, p.answered, p.delivered, p.undecided), (1, 0, 0, 1));
}

#[tokio::test]
async fn machine_notices_are_named_and_other_silent_returns_stay_undecided() {
    let (engine, root) = fixture(false, false);
    for (source, prompt, reason) in [
        (None, "", None),
        (None, "/help", None),
        (
            None,
            "<task-notification>finished</task-notification>",
            Some(DecisionReason::MachinePrompt),
        ),
        (Some("sdk"), "fix the queue serialization race", None),
        // Disabled embeddings deterministically reach the embed-error return.
        (None, "fix the queue serialization race", None),
    ] {
        let input = HookInput {
            source: source.map(str::to_owned),
            prompt: Some(prompt.into()),
            ..Default::default()
        };
        let mut pending = effects(&engine);
        let mut output = String::new();
        handle_inner(&input, &engine, root.path(), &mut output, &mut pending)
            .await
            .unwrap();
        assert_eq!(pending.reason, reason, "{source:?}: {prompt}");
        assert!(output.is_empty());
        assert_undecided_delivery();
    }
    assert!(!engine.embeddings().is_loaded());
}

#[tokio::test]
async fn literal_lineage_returns_decide_with_and_without_an_episode() {
    for seed in [false, true] {
        let (engine, root) = fixture(seed, true);
        let input = HookInput {
            session_id: Some("current".into()),
            prompt: Some("continue".into()),
            ..Default::default()
        };
        let mut pending = effects(&engine);
        let mut output = String::new();
        handle_inner(&input, &engine, root.path(), &mut output, &mut pending)
            .await
            .unwrap();
        assert_eq!(
            pending.reason,
            Some(if seed {
                DecisionReason::OnTopic
            } else {
                DecisionReason::NoCandidates
            })
        );
        assert_eq!(!output.is_empty(), seed);
        assert!(!engine.embeddings().is_loaded());
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn every_classified_intent_return_sets_a_reason() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let _policy = super::decision_tests::Policy::content_only();
    for intent in [Intent::Continue, Intent::StateRecall, Intent::Explore] {
        for (seed, files) in [(false, false), (true, true), (true, false)] {
            let (engine, root) = fixture(seed, files);
            let prompt = "locate the authentication implementation files";
            let input = HookInput {
                session_id: Some("current".into()),
                prompt: Some(prompt.into()),
                ..Default::default()
            };
            let mut pending = effects(&engine);
            let mut output = String::new();
            handle_classified_intent(
                &input,
                &engine,
                root.path(),
                &mut output,
                &mut pending,
                prompt,
                &normalized_words(prompt),
                &[1.0, 0.0],
                Some(intent),
            )
            .await
            .unwrap();
            assert_eq!(
                pending.reason,
                Some(if seed {
                    DecisionReason::OnTopic
                } else {
                    DecisionReason::NoCandidates
                }),
                "{intent:?} seed={seed} files={files}: {output}"
            );
            if seed {
                let marker = match intent {
                    Intent::Explore if files => "CSR CODE MAP",
                    _ => "CSR PICKUP",
                };
                assert!(output.contains(marker), "{intent:?}: {output}");
            } else {
                assert!(output.is_empty());
            }
            assert!(!engine.embeddings().is_loaded());
        }
    }
}
