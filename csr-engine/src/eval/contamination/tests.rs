use super::*;

#[test]
fn cases_jsonl_expected_bytes_and_reasons_hold() {
    let report = run_cases();
    assert!(
        report.failures.is_empty(),
        "{} of {} rows failed:\n{}",
        report.failures.len(),
        report.total,
        report.failures.join("\n")
    );
    assert_eq!(report.passed, report.total);
    assert!(report.total >= 62, "the Appendix C rows are all present");
    assert_eq!(report.removal_rows + report.kept_rows, report.total);
}

#[test]
fn every_appendix_c_case_id_is_present() {
    let ids: std::collections::HashSet<String> = cases().into_iter().map(|c| c.id).collect();
    for id in [
        "recap-raw",
        "dream-injection-raw",
        "lessons-raw",
        "manifest-raw",
        "briefing-raw",
        "continuity-raw",
        "continuum-basic",
        "continuum-next-todos",
        "episode-index-raw",
        "recent-sessions-raw",
        "empty-status-raw",
        "degraded-status-raw",
        "pickup-raw",
        "injection-stuck",
        "injection-dont-retry",
        "injection-error-solutions",
        "injection-relevant",
        "injection-past",
        "injection-iteration",
        "code-map-raw",
        "anchor-notice-raw",
        "memory-feedback-report",
        "plugin-band-dream",
        "plugin-pane-dream",
        "plugin-compaction-card",
        "plugin-session-story",
        "plugin-side-note",
        "plugin-keep-list",
        "plugin-compact-notice",
        "plugin-carry",
        "plugin-recall",
        "plugin-session-recall",
        "plugin-reading",
        "plugin-rule",
        "plugin-recap-forward",
        "plugin-last-session",
        "plugin-leftover",
        "plugin-code-shift",
        "plugin-lost-side",
        "mixed-dream-reply",
        "mixed-side-note-reply",
        "assistant-verbatim-echo",
        "assistant-analysis-kept",
        "recap-crlf",
        "recap-bom",
        "json-escaped-literal-kept",
        "rust-fence-kept",
        "typescript-fence-kept",
        "grep-output-kept",
        "assertion-kept",
        "analysis-paragraph-kept",
        "blockquote-kept",
        "indented-code-kept",
        "non-csr-tool-kept",
        "codex-user-kept",
        "codex-tool-kept",
        "plan-kept",
        "bare-sentinel-kept",
        "unproven-stored-kept",
        "csr-tool-use-structural",
        "csr-tool-result-structural",
        "harness-row-structural",
    ] {
        assert!(ids.contains(id), "cases.jsonl lost {id}");
    }
}

/// The matrix rows are generated from the production formatters. If this
/// fails, a formatter changed: regenerate with
/// `cargo test --release --lib regenerate_fixture_matrix -- --ignored`
/// and review the diff (the registry grammar may need to follow).
#[test]
fn stored_matrix_matches_the_production_formatters() {
    let stored: Vec<Case> = cases()
        .into_iter()
        .filter(|case| case.id.starts_with(MATRIX_PREFIX))
        .collect();
    let current = expected_matrix();
    assert_eq!(stored.len(), current.len(), "matrix row count");
    for (stored, current) in stored.iter().zip(&current) {
        assert_eq!(stored, current, "row {}", current.id);
    }
}

#[test]
#[ignore = "rewrites tests/fixtures/contamination/cases.jsonl from the production formatters"]
fn regenerate_fixture_matrix() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/contamination/cases.jsonl");
    std::fs::write(path, render_fixture()).unwrap();
}

#[test]
fn canary_is_green_and_counts_every_output_and_mention() {
    let report = run_canary();
    assert!(report.failures.is_empty(), "{:#?}", report.failures);
    assert_eq!(report.generated, report.correctly_removed);
    assert_eq!(report.protected, report.correctly_kept);
    assert_eq!(report.protected, report.generated * 6);
    require_green().unwrap();
    let result = emitter_canary();
    assert!(result.passed, "{}", result.detail);
    assert_eq!(result.name, "Emitter Contamination Canary");
}

#[test]
fn registry_formatters_all_have_a_matrix_case() {
    let produced: std::collections::HashSet<&str> = engine_outputs()
        .iter()
        .map(|o| o.formatter)
        .chain(plugin_synthetic().iter().map(|o| o.formatter))
        .collect();
    for formatter in REGISTRY.iter().filter(|f| f.is_component_formatter()) {
        assert!(
            produced.contains(formatter.id),
            "{} is registered but no canary output exercises it",
            formatter.id
        );
    }
}

/// A formatter that drifts from its registered grammar turns the canary red.
#[test]
fn a_drifted_output_turns_the_canary_red() {
    let drifted = Output {
        id: "drifted".into(),
        formatter: "degraded_status",
        reason: ContaminationReason::DegradedStatus,
        text: "CSR engine ready (degraded mode)!".into(),
        assistant_relay: true,
        mixed: Mixed::Removed,
    };
    let removal = remove_csr_emissions(OwnedText {
        source: SourceKind::Conversation,
        channel: ContentChannel::UserText,
        text: &drifted.text,
    });
    assert_eq!(
        removal.kept, drifted.text,
        "an unregistered wording is kept"
    );
}

#[test]
fn matrix_covers_every_engine_branch() {
    let ids: Vec<String> = engine_outputs().into_iter().map(|o| o.id).collect();
    for needed in [
        "continuum-basic",
        "continuum-next-todos",
        "continuum-anchors-intact",
        "continuum-anchors-modified",
        "tier0-recap",
        "tier0-recap-anchors",
        "injection-stuck",
        "injection-dont-retry",
        "injection-error-solutions",
        "injection-relevant",
        "injection-past",
        "injection-iteration",
        "code-map",
        "code-map-no-anchors",
        "pickup",
        "pickup-semantic",
        "recap-minimal",
        "recap-full",
        "session-start-blob",
    ] {
        assert!(ids.iter().any(|id| id == needed), "{needed}");
    }
}

#[test]
fn the_census_result_reports_proven_unjudged_and_clean_over_one_population() {
    let storage = crate::storage::Storage::open_memory().unwrap();
    let seed = |id: &str, conversation: &str, content: &str, owned: bool| {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, message_count, source)
                     VALUES (?1, ?2, 'project', '2026-09-01T00:00:00Z', ?3, 1, 'conversation')",
                    rusqlite::params![id, conversation, content],
                )?;
                Ok(())
            })
            .unwrap();
        if owned {
            let event = crate::provenance::ProvenanceEvent {
                event_id: format!("event:{id}"),
                conversation_id: conversation.to_string(),
                message_key: id.to_string(),
                seq: 0,
                channel: "user_message".to_string(),
                trust_tier: crate::provenance::TrustTier::UserHistory,
                parent_event_id: None,
                receipt_kind: "jsonl".to_string(),
                receipt_ref: None,
                observed_at: "2026-09-01T00:00:00Z".to_string(),
            };
            storage
                .replace_chunk_evidence(&crate::provenance::ChunkEvidence {
                    chunk_id: id.to_string(),
                    min_trust: event.trust_tier,
                    tool_result_share: Some(0.0),
                    spans: vec![crate::provenance::ChunkSpan {
                        chunk_id: id.to_string(),
                        event_id: event.event_id.clone(),
                        start_char: 0,
                        end_char: content.chars().count(),
                        content_hash: crate::provenance::content_hash(content),
                    }],
                    events: vec![event],
                })
                .unwrap();
        }
    };
    let degraded = "CSR engine ready (degraded mode).";
    seed("p", "proven", degraded, true);
    seed("u", "unjudged", degraded, false);
    seed("c1", "clean-1", "ordinary work", false);
    seed("c2", "clean-2", "more ordinary work", true);
    let result = census_result(&storage);
    assert_eq!(result.name, "Contamination Census");
    assert!(result.passed, "{}", result.detail);
    assert!(
        result.detail.contains("proven 1")
            && result.detail.contains("unjudged 1")
            && result.detail.contains("clean 2"),
        "{}",
        result.detail
    );
    assert!(
        result.detail.contains("of 4 conversations"),
        "{}",
        result.detail
    );
}
