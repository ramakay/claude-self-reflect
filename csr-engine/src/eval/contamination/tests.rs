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
    assert_eq!(report.labeled_rows + report.unlabeled_rows, report.total);
    // Label, never delete: no row expects the text back different, except the
    // one structural cut (a complete hook-labelled wrapper in the person's turn).
    assert!(cases()
        .iter()
        .all(|case| case.expected == expected_stored(&case.channel, &case.input)));
    assert!(cases()
        .iter()
        .filter(|case| case.channel != "user_text")
        .all(|case| case.expected == case.input));
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
    assert_eq!(report.generated, report.correctly_detected);
    assert_eq!(report.protected, report.correctly_unlabeled);
    assert_eq!(report.protected, report.generated * 6);
    require_green().unwrap();
    let result = emitter_canary();
    assert!(result.passed, "{}", result.detail);
    assert_eq!(result.name, "Emitter Contamination Canary");
    assert!(result.detail.contains("detected"), "{}", result.detail);
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

/// A formatter that drifts from its registered grammar is not found.
#[test]
fn a_drifted_output_is_not_detected() {
    assert!(crate::import::contamination::detect("CSR engine ready (degraded mode)!").is_empty());
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
fn the_census_result_reports_labeled_chunks_and_conversations_of_one_population() {
    let storage = crate::storage::Storage::open_memory().unwrap();
    let seed = |id: &str, conversation: &str, source: &str, content: &str| {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, message_count, source)
                     VALUES (?1, ?2, 'project', '2026-09-01T00:00:00Z', ?3, 1, ?4)",
                    rusqlite::params![id, conversation, content, source],
                )?;
                Ok(())
            })
            .unwrap();
    };
    let degraded = "CSR engine ready (degraded mode).";
    seed("a1", "labeled", "conversation", degraded);
    seed(
        "a2",
        "labeled",
        "conversation",
        "recap [2m ago]: Fixed it. [[CSR:RECAP]]",
    );
    seed("b1", "labeled-too", "sidechain", degraded);
    seed("c1", "clean-1", "conversation", "ordinary work");
    seed("p1", "plan:x", "plan", degraded); // a plan quotes CSR; it is not CSR
    seed(
        "m1",
        "mention",
        "conversation",
        &format!("see `{degraded}`"),
    );
    let result = census_result(&storage);
    assert_eq!(result.name, "Contamination Census");
    assert!(result.passed, "{}", result.detail);
    assert_eq!(
        result.detail, "3 CSR-text chunks labeled in 2 of 5 conversations",
        "{}",
        result.detail
    );
}

#[test]
fn stored_chunks_must_be_the_expected_text_byte_for_byte() {
    let chunks = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect::<Vec<_>>();
    // exact, edge whitespace trimmed, split at whitespace
    assert!(chunks_are_exactly(&chunks(&["a b"]), "a b"));
    assert!(chunks_are_exactly(&chunks(&["a b"]), "  a b\n"));
    assert!(chunks_are_exactly(&chunks(&["aaa", "bbb"]), "aaa \n bbb"));
    // inner whitespace collapsed: a whitespace-stripped comparison would pass this
    assert!(!chunks_are_exactly(&chunks(&["a b"]), "a  b"));
    // a changed, dropped, or reordered piece
    assert!(!chunks_are_exactly(&chunks(&["a c"]), "a b"));
    assert!(!chunks_are_exactly(&chunks(&["aaa"]), "aaa bbb"));
    assert!(!chunks_are_exactly(&chunks(&["bbb", "aaa"]), "aaa bbb"));
    assert!(!chunks_are_exactly(&[], "aaa"));
    // a non-text channel has nothing to compare
    let tool = Case {
        id: "t".into(),
        source: "conversation".into(),
        channel: "non_csr_tool_result".into(),
        input: "x".into(),
        expected: "x".into(),
        reason: None,
    };
    assert_eq!(stored_by_import(&tool), Ok(None));
}

// --- the companion's agents envelope, from the real Python formatter ---

#[test]
fn plugin_agents_context_and_handoff_fixtures_are_detected() {
    let document = agents_document();
    assert_eq!(document.formatter, "plugin_agents_v1");
    let mut seen = Vec::new();
    for (name, fixture) in &document.fixtures {
        seen.push(name.as_str());
        let hits = detect(&fixture.text);
        assert_eq!(hits.len(), 1, "{name}: marker, JSON and marker are one hit");
        assert_eq!(hits[0].reason, ContaminationReason::PluginAgentsEnvelope);
        assert_eq!(
            (hits[0].start_byte, hits[0].end_byte),
            (0, fixture.text.len()),
            "{name}"
        );
    }
    assert_eq!(seen, ["context", "handoff"]);
    // the canary reports both
    let report = run_canary();
    assert_eq!((report.agents_generated, report.agents_correct), (2, 2));
    assert!(report.failures.is_empty(), "{:#?}", report.failures);
}

#[test]
fn plugin_agents_fixtures_import_byte_identical() {
    for (name, fixture) in &agents_document().fixtures {
        for channel in ["user_text", "assistant_text"] {
            let case = Case {
                id: format!("agents-{name}-{channel}"),
                source: "conversation".to_string(),
                channel: channel.to_string(),
                input: fixture.text.clone(),
                expected: fixture.text.clone(),
                reason: Some(ContaminationReason::PluginAgentsEnvelope),
            };
            run_case(&case).unwrap();
            let stored = stored_by_import(&case).unwrap().unwrap();
            assert!(
                chunks_are_exactly(&stored, &fixture.text),
                "{name} via {channel} was not stored byte for byte: {stored:?}"
            );
        }
    }
}

#[test]
fn plugin_agents_production_fixture_digest_matches() {
    use sha2::{Digest, Sha256};
    let hex = |bytes: &[u8]| -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    };
    let document = agents_document();
    assert!(agents_failures().is_empty(), "{:#?}", agents_failures());
    for (name, fixture) in &document.fixtures {
        assert_eq!(hex(fixture.text.as_bytes()), fixture.sha256, "{name}");
    }
    assert_eq!(document.source_digest.len(), 64);

    // With the plugin beside the crate, the engine's copy is the plugin's file
    // and the plugin's own check passes: the fixture is what bin/agents.py
    // writes now.
    let plugin = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugins/csr-companion");
    let copy = plugin.join("tests/fixtures/emissions/agents-v1.json");
    let Ok(plugin_text) = std::fs::read_to_string(&copy) else {
        return; // packaged without the plugin beside it
    };
    assert_eq!(
        plugin_text,
        include_str!("../../../tests/fixtures/contamination/agents-v1.json"),
        "the engine's copy of agents-v1.json differs from the plugin's"
    );
    let output = std::process::Command::new("python3")
        .args(["-I", "-B"])
        .arg(plugin.join("tests/export-agent-emissions.py"))
        .arg("--check")
        .output();
    match output {
        Ok(output) => assert!(
            output.status.success(),
            "agents-v1.json is not what bin/agents.py writes now: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => eprintln!("python3 unavailable, digest check skipped: {error}"),
    }
}

#[test]
fn the_canary_generates_every_dream_card_emitter() {
    let outputs = engine_outputs();
    let cards: Vec<&Output> = outputs
        .iter()
        .filter(|output| output.formatter == "dream_card_judgment")
        .collect();
    let ids: Vec<&str> = cards.iter().map(|output| output.id.as_str()).collect();
    // the dreams CLI's unfinished card, the strategy card, the two backfill cards
    assert_eq!(
        ids,
        [
            "dream-card-unfinished",
            "dream-card-strategy",
            "dream-card-supersession",
            "dream-card-unfinished-backfill"
        ]
    );
    let before = run_canary();
    assert!(before.failures.is_empty(), "{:#?}", before.failures);
    assert!(before.generated >= 4 && before.generated == before.correctly_detected);
}
