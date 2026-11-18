use super::*;
use crate::hooks::prompt_log::DecisionReason;

/// Restore the process policy even when an assertion fails.
pub(super) struct Policy(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl Policy {
    /// Search and selection only. The send gate is off here: it has its own
    /// tests in `send_gate_tests`.
    pub(super) fn content_only() -> Self {
        Self::set(&[
            ("CSR_NO_ANTI_PATTERNS", "1"),
            ("CSR_NO_GRAPH_SLICES", "1"),
            ("CSR_SEARCH_MODE", "vector"),
            (crate::injection::send_gate::OFF_SWITCH, "1"),
        ])
    }

    pub(super) fn set(vars: &[(&'static str, &str)]) -> Self {
        let mut saved = Vec::new();
        for &(name, value) in vars {
            saved.push((name, std::env::var_os(name)));
            std::env::set_var(name, value);
        }
        Self(saved)
    }
}

impl Drop for Policy {
    fn drop(&mut self) {
        for (name, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

/// Fixed-vector calls enter the same content search, filtering and formatting
/// used after intent classification. No model classification is mocked as a
/// decision, and index occupancy alone cannot supply the expected reason.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn content_decisions_follow_the_gate_that_actually_rejected_the_candidates() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let _policy = Policy::content_only();
    for (prompt, seed, expected, delivered) in [
        (
            "fix the queue serialization race",
            None,
            DecisionReason::NoCandidates,
            false,
        ),
        (
            "fix the queue serialization race",
            Some((0.2, "The queue now serializes writes safely.", false)),
            DecisionReason::BelowFloor,
            false,
        ),
        (
            "fix the queue serialization race",
            Some((0.9, "Bash, Read, Glob", false)),
            DecisionReason::LookalikeOnly,
            false,
        ),
        (
            "fix the queue serialization race",
            Some((0.9, "The queue now serializes writes safely.", true)),
            DecisionReason::NoCandidates,
            false,
        ),
        (
            "fix the queue serialization race",
            Some((0.9, "The queue now serializes writes safely.", false)),
            DecisionReason::OnTopic,
            true,
        ),
        (
            "thanks",
            Some((0.9, "The queue now serializes writes safely.", false)),
            DecisionReason::TooShort,
            false,
        ),
        (
            "The queue now serializes writes safely and prevents concurrent workers from corrupting pending jobs.",
            Some((0.9, "The queue now serializes writes safely and prevents concurrent workers from corrupting pending jobs.", false)),
            DecisionReason::NoCandidates,
            false,
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
        let mut search = crate::search::SearchEngine::new(8);
        if let Some((cosine, content, own)) = seed {
            let cosine: f32 = cosine;
            let vector = [cosine, (1.0 - cosine * cosine).sqrt()];
            let tags = if own {
                // Reflection provenance uses conv_<session>; the real gate strips conv_.
                vec!["conv_current".into()]
            } else {
                Vec::new()
            };
            storage
                .insert_reflection("candidate", content, &tags, &vector)
                .unwrap();
            search.insert_reflection("candidate".into(), vector.to_vec());
        }
        let engine = Engine::from_parts(
            storage.clone(),
            std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
            std::sync::Arc::new(tokio::sync::RwLock::new(search)),
            root.path().to_path_buf(),
        );
        let input = HookInput {
            session_id: Some("current".into()),
            prompt: Some(prompt.into()),
            ..Default::default()
        };
        let mut effects = PendingEffects {
            reason: None,
            storage: storage.clone(),
            receipts: storage,
            impressions: Vec::new(),
            retrievals: Vec::new(),
            dream_claims: Vec::new(),
            withheld_claims: Vec::new(),
            prompt_id: None,
            serve_only: false,
        };
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
        assert_eq!(effects.reason, Some(expected), "{prompt}: {seed:?}");
        assert_eq!(!output.is_empty(), delivered, "{expected:?}: {output}");
        assert!(!engine.embeddings().is_loaded());
    }
}
