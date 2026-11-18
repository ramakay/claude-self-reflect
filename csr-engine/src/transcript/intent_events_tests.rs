use super::*;
use crate::hooks::reaction::Reaction;

#[test]
fn correction_receipt_reslices_to_the_exact_raw_quote() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    let raw = concat!(
        "{\"type\":\"user\",\"timestamp\":\"2026-09-01T10:00:00Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Build the first approach\"}]}}\n",
        "{\"type\":\"assistant\",\"timestamp\":\"2026-09-01T10:01:00Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"I changed `src/old.rs`.\"}]}}\n",
        "{\"type\":\"user\",\"timestamp\":\"2026-09-01T10:02:00Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"No, change `src/new.rs` instead\"}]}}\n",
    );
    std::fs::write(&transcript, raw).unwrap();

    let events = extract_with_classifier(
        &transcript,
        "parent-session",
        "project-a",
        |text, _prior_user| {
            (text == "No, change `src/new.rs` instead").then_some(Reaction::Correction)
        },
    )
    .unwrap();

    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.kind, IntentEventKind::Correction);
    assert_eq!(event.turn, 3);
    assert_eq!(event.prior_claim, "I changed `src/old.rs`.");
    assert_eq!(event.file.as_deref(), Some("src/new.rs"));
    assert_eq!(event.symbol, None);
    assert_eq!(event.session_id, "parent-session");
    let bytes = std::fs::read(&transcript).unwrap();
    assert_eq!(
        &bytes[event.byte_start..event.byte_end],
        event.quote.as_bytes()
    );
    assert_eq!(event.quote, "No, change `src/new.rs` instead");
}

#[test]
fn abandoned_marker_is_sentence_scoped_and_unmarked_prose_abstains() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    let raw = concat!(
        "{\"type\":\"assistant\",\"timestamp\":\"2026-09-01T10:00:00Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"The first idea may work. We are dropping `OldParser` instead of changing it. The final design is ready.\"}]}}\n",
        "{\"type\":\"assistant\",\"timestamp\":\"2026-09-01T10:01:00Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"This is ordinary progress without a marker.\"}]}}\n",
        "{\"type\":\"assistant\",\"timestamp\":\"2026-09-01T10:02:00Z\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"The network is dropping packets. The operation cannot continue so the caller receives an error.\"}]}}\n",
    );
    std::fs::write(&transcript, raw).unwrap();

    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, IntentEventKind::Abandoned);
    assert_eq!(
        events[0].quote,
        "We are dropping `OldParser` instead of changing it."
    );
    assert_eq!(events[0].symbol.as_deref(), Some("OldParser"));
    let bytes = std::fs::read(&transcript).unwrap();
    assert_eq!(
        &bytes[events[0].byte_start..events[0].byte_end],
        events[0].quote.as_bytes()
    );
}

#[test]
fn a_pasted_block_is_never_the_persons_correction() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    // After the opening prompt, a reaction in a pasted block (another agent's words), then the person's own.
    let raw = concat!(
        "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Build the first approach\"}]}}\n",
        "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"check this <pasted_content id=\\\"3b47\\\">\\nNo, do not send this to grok-advisor.\\n</pasted_content>\"}]}}\n",
        "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"No, use OtherThing\"}]}}\n",
    );
    std::fs::write(&transcript, raw).unwrap();

    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| {
        Some(Reaction::Correction)
    })
    .unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].quote, "No, use OtherThing");
}

/// A pasted handoff draft carries CSR's stored summary, whose sentences can open "No," or
/// "Don't ...": those are not the person's corrections. What they type around it still is.
#[test]
fn a_pasted_handoff_drafts_summary_is_never_the_persons_correction() {
    let sid = "3002ab28-7ba1-4c61-9d2e-0a1b2c3d4e5f";
    let item = serde_json::json!({
        "key": "k1", "text": "Retry the upload", "state": "carried", "times": 0,
        "last_seen": 1_761_990_000., "why": "quiet", "detail": [],
        "handoff": {
            "conv": sid, "project": "p", "path": "/x/s.jsonl", "path_exists": true,
            "db": "/x/db", "mirror": null,
            "summary": {"request": "Fix the upload",
                        "completed": "Wrote the retry. Don't retry the upload. No, the hub is down. Stop, it failed.",
                        "outcome": "partial", "files": []},
            "stats": null, "stats_note": "",
        },
    });
    let draft = crate::companion::handoff::draft(&item).unwrap();
    // the premise: read as the person's words, these sentences are corrections
    assert!(lexical_reaction_sentence("Wrote the retry. Don't retry the upload.").is_some());
    assert!(lexical_reaction_sentence(&draft).is_none(), "{draft}");
    let row = |text: &str| {
        serde_json::json!({"type": "user", "message": {"content": [{"type": "text", "text": text}]}})
            .to_string()
    };
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    let raw = format!(
        "{}\n{}\n{}\n",
        row("Build the first approach"),
        row(&draft),
        row(&format!("{draft}No, use OtherThing")),
    );
    std::fs::write(&transcript, raw).unwrap();
    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
    let quotes: Vec<&str> = events.iter().map(|e| e.quote.as_str()).collect();
    assert_eq!(quotes, ["No, use OtherThing"], "{events:?}");
}

#[test]
fn claude_briefing_a_subagent_or_a_headless_prompt_is_never_a_correction() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    // The person's opening prompt first: a reaction needs one before it.
    let raw = concat!(
        "{\"type\":\"user\",\"isSidechain\":false,\"userType\":\"external\",\"entrypoint\":\"cli\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Build the first approach\"}]}}\n",
        "{\"type\":\"user\",\"isSidechain\":true,\"userType\":\"external\",\"entrypoint\":\"cli\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Do not push.\"}]}}\n",
        "{\"type\":\"user\",\"entrypoint\":\"sdk-cli\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"No, summarize only.\"}]}}\n",
        "{\"type\":\"assistant\",\"isSidechain\":true,\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"We are dropping `OldParser` instead of changing it.\"}]}}\n",
        "{\"type\":\"user\",\"isSidechain\":false,\"userType\":\"external\",\"entrypoint\":\"cli\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"No, use OtherThing\"}]}}\n",
    );
    std::fs::write(&transcript, raw).unwrap();

    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| {
        Some(Reaction::Correction)
    })
    .unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].quote, "No, use OtherThing");
}

#[test]
fn csr_emission_is_never_classified_as_a_correction() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    let raw = format!(
        "{{\"type\":\"user\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{} no, use OtherThing\"}}]}}}}\n",
        crate::extraction::provenance::RECAP_SENTINEL
    );
    std::fs::write(&transcript, raw).unwrap();

    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| {
        Some(Reaction::Correction)
    })
    .unwrap();

    assert!(events.is_empty());
}

#[test]
fn multiple_text_blocks_in_one_user_entry_are_one_logical_turn() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-09-01T10:00:00Z","message":{"content":[{"type":"text","text":"Implement parser"},{"type":"text","text":"No, use NewParser"}]}}"#,
            "\n",
        ),
    )
    .unwrap();

    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| {
        Some(Reaction::Correction)
    })
    .unwrap();

    assert!(
        events.is_empty(),
        "one opening entry is not a reaction turn"
    );
}

#[test]
fn meta_user_entries_are_not_corrections_or_prior_user_context() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
            "\n",
            r#"{"type":"user","uuid":"u2","isMeta":true,"message":{"content":"Never --no-verify."}}"#,
            "\n",
        ),
    )
    .unwrap();

    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
    assert!(events.is_empty());

    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","uuid":"u1","isMeta":true,"message":{"content":"Injected setup"}}"#,
            "\n",
            r#"{"type":"user","uuid":"u2","message":{"content":"Never --no-verify."}}"#,
            "\n",
        ),
    )
    .unwrap();
    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
    assert!(
        events.is_empty(),
        "metadata must not establish prior-user context"
    );
}

#[test]
fn non_meta_negative_command_is_a_correction_with_its_marker() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
            "\n",
            r#"{"type":"user","uuid":"u2","message":{"content":"Never --no-verify."}}"#,
            "\n",
        ),
    )
    .unwrap();

    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, IntentEventKind::Correction);
    assert_eq!(events[0].marker.as_deref(), Some("negative-command"));
}

#[test]
fn compact_summary_is_not_a_correction_but_the_same_human_turn_is() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
            "\n",
            r#"{"type":"user","uuid":"u2","isCompactSummary":true,"message":{"content":"Never --no-verify."}}"#,
            "\n",
        ),
    )
    .unwrap();

    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
    assert!(events.is_empty());

    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
            "\n",
            r#"{"type":"user","uuid":"u2","message":{"content":"Never --no-verify."}}"#,
            "\n",
        ),
    )
    .unwrap();
    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, IntentEventKind::Correction);
}

#[test]
fn legacy_continuation_summary_is_not_a_correction_or_prior_user_context() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    let summary =
        "This session is being continued from a previous conversation\nNever --no-verify.";
    let first = serde_json::json!({
        "type": "user",
        "uuid": "u1",
        "message": { "content": "Build parser" }
    });
    let second = serde_json::json!({
        "type": "user",
        "uuid": "u2",
        "message": { "content": summary }
    });
    std::fs::write(&transcript, format!("{first}\n{second}\n")).unwrap();

    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
    assert!(events.is_empty());

    let first = serde_json::json!({
        "type": "user",
        "uuid": "u1",
        "message": { "content": summary }
    });
    let second = serde_json::json!({
        "type": "user",
        "uuid": "u2",
        "message": { "content": "Never --no-verify." }
    });
    std::fs::write(&transcript, format!("{first}\n{second}\n")).unwrap();
    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
    assert!(
        events.is_empty(),
        "a continuation summary must not establish prior-user context"
    );
}

#[test]
fn wrapper_tagged_correction_marker_is_ignored() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
            "\n",
            r#"{"type":"user","uuid":"u2","message":{"content":"Pasted context:\n<system-reminder>\nNever --no-verify.\n</system-reminder>\nEnd context."}}"#,
            "\n",
        ),
    )
    .unwrap();

    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
    assert!(events.is_empty());
}

#[test]
fn bulleted_negative_command_is_ignored_but_plain_command_fires() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    for bullet in ["- do not push", "* do not push", "1. do not push"] {
        let first = serde_json::json!({
            "type": "user",
            "uuid": "u1",
            "message": { "content": "Build parser" }
        });
        let second = serde_json::json!({
            "type": "user",
            "uuid": "u2",
            "message": { "content": bullet }
        });
        std::fs::write(&transcript, format!("{first}\n{second}\n")).unwrap();
        let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
        assert!(events.is_empty(), "bullet {bullet:?} must abstain");
    }

    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
            "\n",
            r#"{"type":"user","uuid":"u2","message":{"content":"do not push"}}"#,
            "\n",
        ),
    )
    .unwrap();
    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, IntentEventKind::Correction);
}

#[test]
fn fenced_and_quoted_correction_markers_are_ignored() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
            "\n",
            r#"{"type":"user","uuid":"u2","message":{"content":"Example:\n```text\nNever --no-verify.\n```\n> Do not forward this task.\nDone."}}"#,
            "\n",
        ),
    )
    .unwrap();

    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
    assert!(events.is_empty());
}

#[test]
fn long_inline_code_correction_markers_are_ignored() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    let inline = format!("`Never {}`", "x".repeat(64));
    let first = serde_json::json!({
        "type": "user",
        "uuid": "u1",
        "message": { "content": "Build parser" }
    });
    let second = serde_json::json!({
        "type": "user",
        "uuid": "u2",
        "message": { "content": inline }
    });
    std::fs::write(&transcript, format!("{first}\n{second}\n")).unwrap();

    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
    assert!(events.is_empty());
}

#[test]
fn prior_claim_joins_all_text_blocks_from_the_preceding_assistant_entry() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a1","message":{"content":[{"type":"text","text":"I chose OldParser."},{"type":"text","text":"It lives in old.rs."}]}}"#,
            "\n",
            r#"{"type":"user","uuid":"u2","message":{"content":"No, use NewParser."}}"#,
            "\n",
        ),
    )
    .unwrap();

    let events = extract_with_classifier(&transcript, "s", "p", |text, _prior| {
        (text == "No, use NewParser.").then_some(Reaction::Correction)
    })
    .unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].prior_claim,
        "I chose OldParser.\nIt lives in old.rs."
    );
}

#[test]
fn meta_assistant_entry_does_not_replace_prior_claim() {
    let temp = tempfile::tempdir().unwrap();
    let transcript = temp.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","uuid":"u1","message":{"content":"Build parser"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a1","message":{"content":"I chose OldParser."}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a2","isMeta":true,"message":{"content":"Injected hook output."}}"#,
            "\n",
            r#"{"type":"user","uuid":"u2","message":{"content":"No, use NewParser."}}"#,
            "\n",
        ),
    )
    .unwrap();

    let events = extract_with_classifier(&transcript, "s", "p", |_text, _prior| None).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].prior_claim, "I chose OldParser.");
}

#[test]
#[ignore = "needs the external B2 probe transcript at /tmp/csr-b2/probe-projects/-tmp-probe/probe-1.jsonl; run with --ignored when it exists"]
fn textbook_probe_turns_produce_four_corrections_and_one_redirect() {
    let probe = Path::new("/tmp/csr-b2/probe-projects/-tmp-probe/probe-1.jsonl");
    assert!(probe.is_file(), "B2 probe transcript must be present");
    let events =
        extract_with_classifier(probe, "probe-1", "-tmp-probe", |_text, _prior| None).unwrap();
    let user_events: Vec<_> = events
        .iter()
        .filter(|event| event.kind != IntentEventKind::Abandoned)
        .collect();
    assert_eq!(
        user_events
            .iter()
            .filter(|event| event.kind == IntentEventKind::Correction)
            .count(),
        4
    );
    assert_eq!(
        user_events
            .iter()
            .filter(|event| event.kind == IntentEventKind::Redirect)
            .count(),
        1
    );
    assert_eq!(
        user_events
            .iter()
            .find(|event| event.kind == IntentEventKind::Redirect)
            .map(|event| event.turn),
        Some(15)
    );
    assert!(user_events
        .iter()
        .all(|event| !event.quote.contains("thanks") && event.quote != "continue"));
}

#[test]
fn two_child_transcripts_with_identical_coordinates_both_persist() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("agent-first.jsonl");
    let second = temp.path().join("agent-second.jsonl");
    let line = concat!(
        r#"{"type":"assistant","timestamp":"2026-09-01T10:00:00Z","message":{"content":"I am dropping OldParser."}}"#,
        "\n"
    );
    std::fs::write(&first, line).unwrap();
    std::fs::write(&second, line).unwrap();
    let mut events = extract_with_classifier(&first, "parent", "p", |_text, _prior| None).unwrap();
    events.extend(extract_with_classifier(&second, "parent", "p", |_text, _prior| None).unwrap());
    let storage = crate::storage::Storage::open_memory().unwrap();

    assert_eq!(storage.insert_intent_events(&events).unwrap(), 2);
    assert_eq!(storage.list_intent_events(None, None).unwrap().len(), 2);
}

#[test]
fn backfill_dry_run_counts_events_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let projects = temp.path().join("projects");
    let project_dir = projects.join("project-a");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("session-1.jsonl"),
        concat!(
            r#"{"type":"assistant","timestamp":"2026-09-01T10:00:00Z","message":{"content":"We are abandoning `OldParser`."}}"#,
            "\n",
        ),
    )
    .unwrap();
    let storage = crate::storage::Storage::open_memory().unwrap();

    let stats =
        backfill_with_classifier(&storage, &projects, None, None, true, |_text, _prior| None)
            .unwrap();

    assert_eq!(stats.files_scanned, 1);
    assert_eq!(stats.total_events(), 1);
    assert_eq!(stats.by_kind.get("abandoned"), Some(&1));
    assert_eq!(stats.by_project.get("project-a"), Some(&1));
    assert_eq!(stats.inserted, 0);
    assert!(storage.list_intent_events(None, None).unwrap().is_empty());
}

#[test]
fn agreement_is_whitespace_normalized_and_limited_to_shared_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = temp.path().join("ledger");
    std::fs::create_dir(&ledger).unwrap();
    std::fs::write(
        ledger.join("records.jsonl"),
        concat!(
            r#"{"session_id":"shared","narration":{"abandoned":[{"quote":"--bare was rejected because auth failed","verified":true}]}}"#,
            "\n",
            r#"{"session_id":"ledger-only","narration":{"abandoned":[{"quote":"never seen","verified":true}]}}"#,
            "\n",
        ),
    )
    .unwrap();
    let event = IntentEvent {
        session_id: "shared".into(),
        project: "p".into(),
        turn: 2,
        kind: IntentEventKind::Abandoned,
        quote: "--bare  was rejected because auth failed.".into(),
        transcript_path: temp.path().join("shared.jsonl"),
        byte_start: 0,
        byte_end: 40,
        prior_claim: String::new(),
        symbol: None,
        file: None,
        classifier_hash: abandonment_lexicon_hash(),
        detector: Some(IntentDetector::Lexical),
        classifier_score: None,
        marker: Some("was rejected".into()),
        ts: "2026-09-01T00:00:00Z".into(),
    };

    let report = agreement_report(&ledger, &[event]).unwrap();
    assert_eq!(report.shared_sessions, 1);
    assert_eq!(
        report.by_kind.get("abandoned"),
        Some(&AgreementCounts {
            ledger_total: 1,
            ledger_matched: 1,
            event_total: 1,
            event_matched: 1,
        })
    );
}
