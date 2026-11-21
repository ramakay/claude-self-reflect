#[test]
fn stored_thread_inherits_its_episode_floor_or_unknown_without_one() {
    use crate::provenance::TrustTier;
    use crate::storage::artifact_provenance::{ArtifactKind, InputEnvelope};
    use crate::storage::Storage;
    let storage = Storage::open_memory().unwrap();
    // Episode on record for sess-a, derived from an External observation.
    storage
        .insert_derived_reflection(
            "ep-a",
            r#"{"schema":"v2","session_id":"sess-a","project":"p"}"#,
            &[
                "session_episode".into(),
                "conv_sess-a".into(),
                "project_p".into(),
            ],
            &[0.0; 4],
            &InputEnvelope::new(vec![
                crate::storage::artifact_provenance::test_observed_input(
                    "tool_result:WebFetch",
                    TrustTier::External,
                    "source",
                ),
            ]),
        )
        .unwrap();
    storage
        .with_connection(|conn| {
            store_thread(
                conn,
                "hash-a",
                "sess-a",
                "p",
                "a thread",
                "quoted evidence",
                &[],
                ReceiptTier::Unverified,
                &[],
                "sonnet-5",
            )?;
            store_thread(
                conn,
                "hash-b",
                "sess-none",
                "p",
                "orphan thread",
                "quoted evidence",
                &[],
                ReceiptTier::Unverified,
                &[],
                "sonnet-5",
            )?;
            Ok(())
        })
        .unwrap();
    let rows: Vec<(String, i64)> = storage
        .with_connection(|c| {
            Ok(
                c.prepare("SELECT session_id, min_trust FROM dream_threads ORDER BY id")?
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?,
            )
        })
        .unwrap();
    assert_eq!(
        rows,
        vec![("sess-a".to_string(), 1), ("sess-none".to_string(), 0)],
        "a thread carries its episode's floor; no episode on record is Unknown"
    );
    assert_eq!(
        storage
            .get_artifact_min_trust(ArtifactKind::DreamThread, "1")
            .unwrap(),
        TrustTier::External
    );
}

use super::*;
use crate::storage::witness_ledger::WitnessLedgerRow;
use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};

/// Serializes tests that touch process-global env vars this module
/// reads (`CSR_DREAM_THREADS`, `CSR_DREAM_THREAD_MODEL`,
/// `CSR_NIGHT_ACTOR_CMD`, `CSR_NO_AI_NARRATIVES`, `CSR_NO_DREAMING`,
/// `CSR_DREAM_THREADS_CAP`). It must be the crate-wide guard: `clear_env`
/// removes `CSR_NO_DREAMING`, which the backfill `disabled_*` tests set and
/// read under `dream_cadence::env_test_guard`, and a module-local lock does
/// not exclude them.
use crate::daemon::dream_cadence::env_test_guard as env_guard;

fn clear_env() {
    std::env::remove_var("CSR_DREAM_THREADS");
    std::env::remove_var("CSR_DREAM_THREAD_MODEL");
    std::env::remove_var("CSR_NIGHT_ACTOR_CMD");
    std::env::remove_var("CSR_NO_AI_NARRATIVES");
    std::env::remove_var("CSR_NO_DREAMING");
    std::env::remove_var("CSR_DREAM_THREADS_CAP");
}

fn open() -> Storage {
    Storage::open_memory().unwrap()
}

fn insert_episode(storage: &Storage, json: &str, timestamp: &str) {
    storage
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, '[]', ?3)",
                params![uuid::Uuid::new_v4().to_string(), json, timestamp],
            )?;
            Ok(())
        })
        .unwrap();
}

fn insert_chunk(storage: &Storage, id: &str, conversation_id: &str, content: &str) {
    storage
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, message_count)
                     VALUES (?1, ?2, 'proj', '2026-01-01T00:00:00Z', ?3, 1)",
                params![id, conversation_id, content],
            )?;
            Ok(())
        })
        .unwrap();
}

/// A pasted handoff draft is stripped before the tail is cut, with the person's own line kept
/// and the chunk id (what the episode hash reads) untouched.
#[test]
fn the_tail_strips_a_pasted_handoff_draft_before_the_cut() {
    let storage = Storage::open_memory().unwrap();
    let draft = crate::companion::handoff::canary_draft();
    insert_chunk(
        &storage,
        "c-draft",
        "sess-h",
        &format!("{draft}also fix the footer"),
    );
    insert_chunk(&storage, "c-plain", "sess-h", "we moved the index load");
    let tail = storage
        .with_connection(|conn| load_tail_chunks(conn, "sess-h"))
        .unwrap();
    let ids: Vec<&str> = tail.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["c-draft", "c-plain"]);
    let draft_text = &tail[0].1;
    eprintln!("TAIL: {draft_text:?}");
    assert!(draft_text.starts_with("### The ask"), "{draft_text}");
    assert!(draft_text.contains("also fix the footer"), "{draft_text}");
    for csr in [
        "went quiet",
        "Wrote the retry",
        "csr-engine.db",
        "csr-handoff",
    ] {
        assert!(!draft_text.contains(csr), "{csr}: {draft_text}");
    }
    assert_eq!(tail[1].1, "we moved the index load");
}

#[allow(clippy::too_many_arguments)]
fn insert_witness_with_verdict(
    storage: &Storage,
    project: &str,
    file: &str,
    symbol: Option<&str>,
    at_oid: &str,
    stamp: &str,
    verdict: VerdictKind,
    receipt_oid: &str,
) {
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(
                conn,
                &WitnessLedgerRow {
                    id: 0,
                    project: project.into(),
                    file: file.into(),
                    symbol: symbol.map(|s| s.to_string()),
                    span_start: Some(1),
                    span_end: Some(3),
                    stamp: stamp.into(),
                    tier: "committed".into(),
                    at_oid: Some(at_oid.into()),
                    source_kind: "backfill".into(),
                    source_id: Some(at_oid.into()),
                },
            )?;
            let witness_id: i64 = conn.query_row(
                "SELECT id FROM witness_ledger WHERE project = ?1 AND file = ?2 AND stamp = ?3",
                params![project, file, stamp],
                |r| r.get(0),
            )?;
            witness_verdicts::insert_verdict_if_changed(
                conn,
                &WitnessVerdictRow {
                    witness_id,
                    verdict,
                    successor_witness_id: None,
                    receipt_oid: Some(receipt_oid.into()),
                    observed_head_oid: receipt_oid.into(),
                },
            )?;
            Ok(())
        })
        .unwrap();
}

fn episode_json(session_id: &str, outcome: &str, files: &[&str]) -> String {
    serde_json::json!({
        "schema": "v2",
        "session_id": session_id,
        "project": "proj",
        "timestamp": "2026-01-01T00:00:00Z",
        "request": "fix the thing",
        "outcome": outcome,
        "completed": "partly fixed the thing",
        "files_modified": files,
        "investigated": [],
    })
    .to_string()
}

// ---- kill switches --------------------------------------------------

#[test]
fn threads_enabled_requires_exact_1() {
    let _g = env_guard();
    clear_env();
    assert!(!threads_enabled());
    std::env::set_var("CSR_DREAM_THREADS", "true");
    assert!(!threads_enabled(), "only the literal '1' turns this on");
    std::env::set_var("CSR_DREAM_THREADS", "1");
    assert!(threads_enabled());
    clear_env();
}

#[test]
fn kill_switch_off_skips_without_invocation() {
    let _g = env_guard();
    clear_env(); // CSR_DREAM_THREADS unset -> disabled
    let storage = open();
    insert_episode(
        &storage,
        &episode_json("sess-1", "failed", &["/repo/src/a.rs"]),
        "2026-01-01T00:00:00Z",
    );

    let invoked = std::cell::Cell::new(0u32);
    let actor = |_model: Option<&str>, _prompt: &str| {
        invoked.set(invoked.get() + 1);
        ActorAttempt::Failed("must not be called".into())
    };
    // Directly assert the top-level gate rather than routing a closure
    // through run_thread_extraction (which always uses ProcessActor).
    assert!(threads_disabled());
    let stats = run_thread_extraction(&storage);
    assert!(stats.skipped);
    assert_eq!(stats.threads_stored, 0);
    let _ = actor.invoke(None, ""); // never reached in the real pass
    clear_env();
}

// ---- quote gate -------------------------------------------------------

#[test]
fn quote_gate_rejects_non_verbatim_and_retries() {
    let _g = env_guard();
    clear_env();
    let storage = open();
    let prompt = "RECORD: the quick brown fox jumps over the lazy dog. FILES: [\"/repo/src/a.rs\"]";
    let calls = std::cell::Cell::new(0u32);
    let actor = |_model: Option<&str>, p: &str| {
        calls.set(calls.get() + 1);
        if calls.get() == 1 {
            // First reply's quote is NOT verbatim (paraphrased).
            ActorAttempt::Parsed(ParsedNarrative {
                text: r#"[{"thread":"finish the fox jump","evidence_quote":"the fast brown fox jumped","files":["/repo/src/a.rs"]}]"#.to_string(),
                model: "sonnet-5".into(),
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            })
        } else {
            assert!(
                p.contains("CORRECTION"),
                "retry prompt must carry the correction line"
            );
            // Retry's quote IS verbatim.
            ActorAttempt::Parsed(ParsedNarrative {
                text: r#"[{"thread":"finish the fox jump","evidence_quote":"the quick brown fox jumps over the lazy dog","files":["/repo/src/a.rs"]}]"#.to_string(),
                model: "sonnet-5".into(),
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            })
        }
    };

    let allowlist = vec!["/repo/src/a.rs".to_string()];
    let chain = vec![Some("sonnet-5".to_string())];
    let result = verify_reply(&actor, &chain, prompt, &allowlist, &storage, None);
    let (threads, _model) = result.expect("actor produced a reply");
    assert_eq!(
        threads.len(),
        1,
        "the retried, verbatim thread must survive: {threads:?}"
    );
    assert_eq!(calls.get(), 2, "exactly one retry must have fired");
}

#[test]
fn quote_gate_drops_thread_still_failing_after_retry() {
    let _g = env_guard();
    clear_env();
    let storage = open();
    let prompt = "RECORD: alpha beta gamma. FILES: [\"/repo/src/a.rs\"]";
    let actor = |_model: Option<&str>, _p: &str| {
        // Always paraphrased — never verbatim, even on retry.
        ActorAttempt::Parsed(ParsedNarrative {
            text: r#"[{"thread":"finish alpha","evidence_quote":"alpha, beta, and gamma","files":["/repo/src/a.rs"]}]"#.to_string(),
            model: "sonnet-5".into(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        })
    };
    let allowlist = vec!["/repo/src/a.rs".to_string()];
    let chain = vec![Some("sonnet-5".to_string())];
    let (threads, _model) =
        verify_reply(&actor, &chain, prompt, &allowlist, &storage, None).unwrap();
    assert!(
        threads.is_empty(),
        "a quote that never becomes verbatim must be dropped: {threads:?}"
    );
}

// ---- effort tier + budget cap (Journal v4 P5) ---------------------------

#[test]
fn candidate_cap_follows_the_effort_tier_unless_explicitly_overridden() {
    use crate::dream::policy::EffortTier;
    assert_eq!(candidate_cap_for(None, EffortTier::Less), 8);
    assert_eq!(candidate_cap_for(None, EffortTier::Balanced), 20);
    assert_eq!(candidate_cap_for(None, EffortTier::Max), 40);
    assert_eq!(
        candidate_cap_for(Some("5"), EffortTier::Max),
        5,
        "an explicit cap wins over the tier"
    );
    assert_eq!(
        candidate_cap_for(Some("0"), EffortTier::Less),
        8,
        "a zero cap falls back to the tier, not to no work at all"
    );
    assert_eq!(
        candidate_cap_for(Some("9999"), EffortTier::Max),
        MAX_CANDIDATE_CAP,
        "nothing may exceed the absolute ceiling"
    );
}

#[test]
fn the_model_chain_takes_the_tier_model_unless_the_env_overrides_it() {
    use crate::dream::policy::EffortTier;
    let _g = env_guard();
    clear_env();
    assert_eq!(
        thread_model_candidates_for(EffortTier::Balanced),
        vec![Some(DEFAULT_THREAD_MODEL.to_string()), None],
        "the default tier must reproduce the pre-tier chain exactly"
    );
    assert_eq!(
        thread_model_candidates_for(EffortTier::Less)[0],
        Some(EffortTier::Less.model().to_string())
    );
    assert_eq!(
        thread_model_candidates_for(EffortTier::Max)[0],
        Some(EffortTier::Max.model().to_string())
    );
    std::env::set_var("CSR_DREAM_THREAD_MODEL", "my-model");
    let overridden = thread_model_candidates_for(EffortTier::Max);
    assert_eq!(
        overridden[0],
        Some("my-model".to_string()),
        "an explicit model choice is never overridden by a tier"
    );
    clear_env();
}

#[test]
fn the_pass_budget_holds_across_the_verifier_retry() {
    use crate::dream::policy::{Budget, BudgetedActor};
    let _g = env_guard();
    clear_env();
    let storage = open();
    let prompt = "RECORD: alpha beta gamma. FILES: [\"/repo/src/a.rs\"]";
    let calls = std::cell::Cell::new(0_usize);
    // Always paraphrased, so the verifier ALWAYS wants its one retry.
    let inner = |_model: Option<&str>, _p: &str| {
        calls.set(calls.get() + 1);
        ActorAttempt::Parsed(ParsedNarrative {
            text: r#"[{"thread":"finish alpha","evidence_quote":"alpha, beta, and gamma","files":["/repo/src/a.rs"]}]"#.to_string(),
            model: "sonnet-5".into(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        })
    };
    let budget = Budget::new(1);
    let actor = BudgetedActor::new(&inner, &budget);
    let allowlist = vec!["/repo/src/a.rs".to_string()];
    let chain = vec![Some("sonnet-5".to_string())];
    let (threads, _model) =
        verify_reply(&actor, &chain, prompt, &allowlist, &storage, None).unwrap();
    assert_eq!(
        calls.get(),
        1,
        "the retry must be refused by the budget, not merely discouraged"
    );
    assert_eq!(budget.used(), 1);
    assert!(budget.exhausted());
    assert!(
        threads.is_empty(),
        "an unverifiable quote is still dropped, never softened: {threads:?}"
    );
}

#[test]
fn an_exhausted_budget_produces_no_reply_at_all_so_nothing_is_cached() {
    use crate::dream::policy::{Budget, BudgetedActor};
    let _g = env_guard();
    clear_env();
    let storage = open();
    let calls = std::cell::Cell::new(0_usize);
    let inner = |_m: Option<&str>, _p: &str| {
        calls.set(calls.get() + 1);
        ActorAttempt::Parsed(ParsedNarrative {
            text: "[]".to_string(),
            model: "sonnet-5".into(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        })
    };
    let budget = Budget::new(0);
    let actor = BudgetedActor::new(&inner, &budget);
    let chain = vec![Some("sonnet-5".to_string())];
    assert!(
        verify_reply(&actor, &chain, "RECORD: x", &[], &storage, None).is_none(),
        "no reply means the episode is retried next pass, not cached as empty"
    );
    assert_eq!(calls.get(), 0);
}

// ---- usage accounting cannot fail open ---------------------------------

fn reply_actor(text: &'static str) -> impl Fn(Option<&str>, &str) -> ActorAttempt {
    move |_m: Option<&str>, _p: &str| {
        ActorAttempt::Parsed(ParsedNarrative {
            text: text.to_string(),
            model: "sonnet-5".into(),
            input_tokens: 11,
            output_tokens: 7,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        })
    }
}

#[test]
fn each_invocation_is_reserved_before_the_call_and_settled_against_its_usage_row() {
    use crate::dream::policy::{Budget, BudgetedActor};
    use crate::storage::usage_reservation;
    let _g = env_guard();
    clear_env();
    let storage = open();
    let budget = Budget::for_night_with_cap(&storage, "2026-08-11", 4);
    let inner = reply_actor("[]");
    let actor = BudgetedActor::new(&inner, &budget);
    let chain = vec![Some("sonnet-5".to_string())];
    let (threads, _model) =
        verify_reply(&actor, &chain, "RECORD: x", &[], &storage, Some("hash-a")).unwrap();
    assert!(threads.is_empty());

    assert_eq!(
        storage
            .with_connection(usage_reservation::unaccounted_count)
            .unwrap(),
        0,
        "an invocation that returned must not stay unaccounted"
    );
    let (usage_rows, finalised, usage_id): (i64, i64, Option<i64>) = storage
        .with_connection(|conn| {
            let rows: i64 = conn.query_row(
                "SELECT COUNT(*) FROM narrative_usage WHERE call_site = 'dream_threads'
                       AND ref_id = 'hash-a' AND input_tokens = 11",
                [],
                |row| row.get(0),
            )?;
            let finalised: i64 = conn.query_row(
                "SELECT COUNT(*) FROM narrative_reservations WHERE state = 'finalised'",
                [],
                |row| row.get(0),
            )?;
            let usage_id: Option<i64> = conn.query_row(
                "SELECT usage_id FROM narrative_reservations WHERE state = 'finalised'",
                [],
                |row| row.get(0),
            )?;
            Ok((rows, finalised, usage_id))
        })
        .unwrap();
    assert_eq!(usage_rows, 1, "the call must be measured exactly once");
    assert_eq!(finalised, 1, "its reservation must be settled");
    let measured: i64 = storage
        .with_connection(|conn| {
            Ok(conn.query_row(
                "SELECT input_tokens FROM narrative_usage WHERE id = ?1",
                rusqlite::params![usage_id.expect("usage id")],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(
        measured, 11,
        "the reservation must point at the row that measured THIS call"
    );
}

#[test]
fn a_usage_accounting_failure_is_counted_and_left_unaccounted_never_swallowed() {
    use crate::dream::policy::{self, Budget, BudgetedActor};
    use crate::storage::usage_reservation;
    let _g = env_guard();
    clear_env();
    let storage = open();
    let budget = Budget::for_night_with_cap(&storage, "2026-08-11", 4);
    // The call happens and is reserved; the measurement then cannot be
    // written. That must be visible, not discarded.
    storage
        .with_connection(|conn| {
            conn.execute("DROP TABLE narrative_usage", [])?;
            Ok(())
        })
        .unwrap();
    let inner = reply_actor("[]");
    let actor = BudgetedActor::new(&inner, &budget);
    let chain = vec![Some("sonnet-5".to_string())];
    let _ = verify_reply(&actor, &chain, "RECORD: x", &[], &storage, Some("hash-a"));

    assert_eq!(
        policy::accounting_failure_count(&storage),
        Some(1),
        "a discarded accounting error is exactly the fail-open under review"
    );
    assert_eq!(
        storage
            .with_connection(usage_reservation::unaccounted_count)
            .unwrap(),
        1,
        "an unmeasured call must remain a known unknown, never zero spend"
    );
    assert_eq!(
        policy::unaccounted_invocations(&storage),
        Some(1),
        "status must be able to read the unaccounted count"
    );
}

#[test]
fn a_missing_reservation_during_finalisation_is_visible_in_status() {
    use crate::dream::policy;
    let storage = open();
    let attempts = [ChainAttempt {
        model_label: "sonnet-5".into(),
        success: true,
        input_tokens: 11,
        output_tokens: 7,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        reservation: Some("reservation-that-does-not-exist".into()),
    }];

    let failures = record_attempts(
        &storage,
        &attempts,
        "dream_threads",
        Some("hash-missing-reservation"),
    );

    assert_eq!(
        failures, 1,
        "updating zero reservation rows is a finalisation failure"
    );
    assert_eq!(
        policy::accounting_failure_count(&storage),
        Some(1),
        "status must surface a finalisation that matched no reserved attempt"
    );
}

#[test]
fn a_disabled_pass_reports_skipped_and_spends_nothing() {
    use crate::dream::policy::Budget;
    let _g = env_guard();
    clear_env(); // CSR_DREAM_THREADS unset — the opt-in gate is off.
    let storage = open();
    let budget = Budget::new(25);
    let stats = run_thread_extraction_with_budget(&storage, &budget);
    assert!(stats.skipped);
    assert_eq!(budget.used(), 0);
    assert_eq!(stats.budget_queued, 0);
}

// ---- file allowlist -----------------------------------------------------

#[test]
fn file_allowlist_rejects_foreign_path() {
    let _g = env_guard();
    clear_env();
    let storage = open();
    let prompt = "RECORD: fix the parser bug. FILES: [\"/repo/src/a.rs\"]";
    let actor = |_model: Option<&str>, _p: &str| {
        ActorAttempt::Parsed(ParsedNarrative {
            text: r#"[{"thread":"fix parser","evidence_quote":"fix the parser bug","files":["/repo/src/OUTSIDE.rs"]}]"#.to_string(),
            model: "sonnet-5".into(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        })
    };
    let allowlist = vec!["/repo/src/a.rs".to_string()];
    let chain = vec![Some("sonnet-5".to_string())];
    let (threads, _model) =
        verify_reply(&actor, &chain, prompt, &allowlist, &storage, None).unwrap();
    assert!(
        threads.is_empty(),
        "a thread naming a file outside the allowlist must be dropped: {threads:?}"
    );
}

// ---- tier assignment ----------------------------------------------------

#[test]
fn tier_assignment_verdict_beats_witnessed_beats_unverified() {
    let _g = env_guard();
    let storage = open();

    // Verdict tier: a witness_verdicts row matches by symbol token.
    insert_witness_with_verdict(
        &storage,
        "proj",
        "/repo/src/parser.rs",
        Some("parse_thing"),
        "aaa",
        "b3:1",
        VerdictKind::SupersededBy,
        "aaa",
    );
    let (tier, receipts) = storage
        .with_connection(|conn| {
            Ok(compute_receipts(
                conn,
                "proj",
                "fix `parse_thing` bug",
                &["/repo/src/other.rs".to_string()],
            ))
        })
        .unwrap();
    assert_eq!(tier, ReceiptTier::Verdict);
    assert!(!receipts.is_empty());

    // Witnessed tier: ledger row matches by file, no verdict for that file.
    storage
        .with_connection(|conn| {
            witness_ledger::insert_witness(
                conn,
                &WitnessLedgerRow {
                    id: 0,
                    project: "proj".into(),
                    file: "/repo/src/lib.rs".into(),
                    symbol: None,
                    span_start: None,
                    span_end: None,
                    stamp: "b3:2".into(),
                    tier: "committed".into(),
                    at_oid: Some("bbb".into()),
                    source_kind: "backfill".into(),
                    source_id: Some("bbb".into()),
                },
            )
        })
        .unwrap();
    let (tier, receipts) = storage
        .with_connection(|conn| {
            Ok(compute_receipts(
                conn,
                "proj",
                "no code tokens here",
                &["/repo/src/lib.rs".to_string()],
            ))
        })
        .unwrap();
    assert_eq!(tier, ReceiptTier::Witnessed);
    assert!(!receipts.is_empty());

    // Unverified: nothing matches.
    let (tier, receipts) = storage
        .with_connection(|conn| {
            Ok(compute_receipts(
                conn,
                "proj",
                "no code tokens here",
                &["/repo/src/never_seen.rs".to_string()],
            ))
        })
        .unwrap();
    assert_eq!(tier, ReceiptTier::Unverified);
    assert!(receipts.is_empty());
}

// ---- convergence --------------------------------------------------------

#[test]
fn convergence_second_run_zero_invocations_zero_usage_rows() {
    let _g = env_guard();
    clear_env();
    std::env::set_var("CSR_DREAM_THREADS", "1");

    let storage = open();
    insert_episode(
        &storage,
        &episode_json("sess-conv", "failed", &["/repo/src/a.rs"]),
        "2026-01-01T00:00:00Z",
    );
    insert_chunk(
        &storage,
        "chunk-1",
        "sess-conv",
        "some transcript content here",
    );

    let invocations = std::cell::Cell::new(0u32);
    let actor = |_model: Option<&str>, _p: &str| {
        invocations.set(invocations.get() + 1);
        ActorAttempt::Parsed(ParsedNarrative {
            text: "[]".to_string(),
            model: "sonnet-5".into(),
            input_tokens: 3,
            output_tokens: 2,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        })
    };

    let episodes = storage
        .with_connection(|conn| load_candidate_episodes(conn, 40))
        .unwrap();
    assert_eq!(episodes.len(), 1);
    let mut stats = ThreadExtractionStats::default();
    extract_for_episode(&storage, &actor, &episodes[0], &mut stats).unwrap();
    assert_eq!(
        invocations.get(),
        1,
        "first run must invoke the actor exactly once"
    );
    assert_eq!(
        stats.sentinels_stored, 1,
        "an empty reply converges via a sentinel row"
    );

    let usage_count_1: i64 = storage
        .with_connection(|conn| {
            conn.query_row("SELECT COUNT(*) FROM narrative_usage", [], |r| r.get(0))
                .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(usage_count_1, 1);

    // Second run over the SAME corpus: already converged, zero invocations.
    let mut stats2 = ThreadExtractionStats::default();
    extract_for_episode(&storage, &actor, &episodes[0], &mut stats2).unwrap();
    assert_eq!(
        invocations.get(),
        1,
        "convergence: second run must add zero invocations"
    );
    assert_eq!(stats2.sentinels_stored, 0);
    assert_eq!(stats2.threads_stored, 0);

    let usage_count_2: i64 = storage
        .with_connection(|conn| {
            conn.query_row("SELECT COUNT(*) FROM narrative_usage", [], |r| r.get(0))
                .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        usage_count_2, usage_count_1,
        "convergence: second run must add zero usage rows"
    );
    clear_env();
}

// ---- sentinel caching for empty replies ----------------------------------

#[test]
fn sentinel_caching_for_empty_replies() {
    let _g = env_guard();
    clear_env();
    let storage = open();
    insert_episode(
        &storage,
        &episode_json("sess-empty", "partial", &["/repo/src/a.rs"]),
        "2026-01-01T00:00:00Z",
    );
    let actor = |_model: Option<&str>, _p: &str| {
        ActorAttempt::Parsed(ParsedNarrative {
            text: "[]".to_string(),
            model: "sonnet-5".into(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        })
    };
    let episodes = storage
        .with_connection(|conn| load_candidate_episodes(conn, 40))
        .unwrap();
    let mut stats = ThreadExtractionStats::default();
    extract_for_episode(&storage, &actor, &episodes[0], &mut stats).unwrap();
    assert_eq!(stats.sentinels_stored, 1);

    let sentinel_count: i64 = storage
        .with_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM dream_threads WHERE thread = '' AND receipt_tier = 'unverified'",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(sentinel_count, 1);

    // load_dream_threads must never surface the sentinel.
    let loaded = storage.with_connection(load_dream_threads).unwrap();
    assert!(loaded.is_empty());
}

// ---- kill-switch off = no invocation (via extract_for_episode is N/A; test the gate directly) ----

#[test]
fn run_thread_extraction_disabled_never_touches_storage_candidates() {
    let _g = env_guard();
    clear_env();
    let storage = open();
    insert_episode(
        &storage,
        &episode_json("sess-x", "failed", &["/repo/src/a.rs"]),
        "2026-01-01T00:00:00Z",
    );
    let stats = run_thread_extraction(&storage); // CSR_DREAM_THREADS unset
    assert!(stats.skipped);
    assert_eq!(stats.candidates, 0);
    let total: i64 = storage
        .with_connection(|conn| {
            conn.query_row("SELECT COUNT(*) FROM dream_threads", [], |r| r.get(0))
                .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(total, 0);
}

// ---- malformed JSON -> sentinel + usage row ------------------------------

#[test]
fn malformed_json_yields_sentinel_and_usage_row() {
    let _g = env_guard();
    clear_env();
    let storage = open();
    insert_episode(
        &storage,
        &episode_json("sess-bad", "failed", &["/repo/src/a.rs"]),
        "2026-01-01T00:00:00Z",
    );
    let actor = |_model: Option<&str>, _p: &str| {
        ActorAttempt::Parsed(ParsedNarrative {
            text: "not json at all, sorry".to_string(),
            model: "sonnet-5".into(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        })
    };
    let episodes = storage
        .with_connection(|conn| load_candidate_episodes(conn, 40))
        .unwrap();
    let mut stats = ThreadExtractionStats::default();
    extract_for_episode(&storage, &actor, &episodes[0], &mut stats).unwrap();
    assert_eq!(
        stats.sentinels_stored, 1,
        "malformed JSON must converge to a sentinel"
    );

    let usage_count: i64 = storage
        .with_connection(|conn| {
            conn.query_row("SELECT COUNT(*) FROM narrative_usage", [], |r| r.get(0))
                .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(
        usage_count, 1,
        "the attempt that produced the malformed reply must still be recorded"
    );
}

// ---- fence-stripping ------------------------------------------------------

#[test]
fn fence_stripping_parses_fenced_json() {
    let text = "```json\n[{\"thread\":\"t\",\"evidence_quote\":\"q\",\"files\":[]}]\n```";
    let threads = parse_threads(text);
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].thread, "t");
}

// ---- prompt cap -----------------------------------------------------------

#[test]
fn prompt_is_capped_at_8kb() {
    let ep = EpisodeCandidate {
        session_id: "sess".into(),
        project: "proj".into(),
        request: Some("x".repeat(1000)),
        completed: Some("y".repeat(1000)),
        outcome: "failed".into(),
        files: vec!["/repo/a.rs".to_string()],
    };
    let huge_tail = vec![("c1".to_string(), "z".repeat(20_000))];
    let prompt = build_prompt(&ep, &huge_tail, &["/repo/a.rs".to_string()]);
    assert!(
        prompt.len() <= PROMPT_CAP_BYTES,
        "prompt must be capped: {} bytes",
        prompt.len()
    );
}

// ---- session-strip argv (string-building only, never executes) -----------

#[test]
fn claude_p_argv_strips_session_scaffolding() {
    let args = claude_p_argv(
        Some("sonnet-5"),
        "EXTRACT",
        std::path::Path::new("/tmp/dream-mcp.json"),
    );

    // Prompt is a -p argument, not a trailing positional — --mcp-config
    // is variadic and would swallow the episode text otherwise.
    let dash_p = args.iter().position(|a| a == "-p").expect("-p");
    assert_eq!(args[dash_p + 1], "EXTRACT");
    let mcp = args.iter().position(|a| a == "--mcp-config").expect("mcp");
    assert!(dash_p < mcp, "prompt must precede --mcp-config");
    assert_eq!(args[mcp + 1], "/tmp/dream-mcp.json");
    let model = args.iter().position(|a| a == "--model").expect("model");
    assert_eq!(args[model + 1], "sonnet-5");

    let sys = args
        .iter()
        .position(|a| a == "--system-prompt")
        .expect("system-prompt");
    assert_eq!(args[sys + 1], "Output only raw JSON. No prose.");

    assert!(
        args.iter()
            .any(|a| a == "--exclude-dynamic-system-prompt-sections"),
        "keep the measured flag even though the CLI ignores it with --system-prompt"
    );

    let sources = args
        .iter()
        .position(|a| a == "--setting-sources")
        .expect("setting-sources");
    assert_eq!(
        args[sources + 1],
        "",
        "empty sources is what drops CLAUDE.md"
    );

    let deny = args
        .iter()
        .position(|a| a == "--disallowedTools")
        .expect("disallowedTools");
    for tool in [
        "Bash",
        "Read",
        "Edit",
        "Write",
        "Glob",
        "Grep",
        "Task",
        "WebFetch",
        "WebSearch",
        "NotebookEdit",
        "TodoWrite",
        "BashOutput",
        "KillShell",
        "SlashCommand",
        "ExitPlanMode",
        "TaskCreate",
        "TaskUpdate",
        "Skill",
    ] {
        assert!(
            args[deny + 1].split(',').any(|t| t == tool),
            "disallowedTools must include {tool}"
        );
    }
}

#[test]
fn claude_p_argv_empties_the_builtin_tool_set() {
    let args = claude_p_argv(None, "p", std::path::Path::new("/tmp/m.json"));
    let tools = args.iter().position(|a| a == "--tools").expect("--tools");
    assert_eq!(
        args[tools + 1],
        "",
        "--tools must be followed by the empty list"
    );
    assert_eq!(
        args.last().map(String::as_str),
        Some(""),
        "--tools \"\" is variadic and must be the final argument so nothing is swallowed"
    );
}

#[test]
fn claude_p_argv_omits_model_flag_when_unset() {
    let args = claude_p_argv(None, "p", std::path::Path::new("/tmp/m.json"));
    assert!(!args.iter().any(|a| a == "--model"));
}

// ---- actor template substitution (string-building only, never executes) --

#[test]
fn actor_template_substitution_builds_command_string_only() {
    let prompt_file = std::path::Path::new("/tmp/x/prompt-abc.txt");
    let out_file = std::path::Path::new("/tmp/x/out-abc.txt");
    let cmd = substitute_actor_template(
        "codex exec -m {model} {prompt_file} > {out_file}",
        "sonnet-5",
        prompt_file,
        out_file,
    );
    assert_eq!(
        cmd,
        "codex exec -m sonnet-5 /tmp/x/prompt-abc.txt > /tmp/x/out-abc.txt"
    );
}

// ---- model chain ------------------------------------------------------------

#[test]
fn thread_model_candidates_chain_env_default_none() {
    let _g = env_guard();
    clear_env();
    let chain = thread_model_candidates();
    assert_eq!(chain, vec![Some(DEFAULT_THREAD_MODEL.to_string()), None]);
    assert_eq!(primary_thread_model(), DEFAULT_THREAD_MODEL);

    std::env::set_var("CSR_DREAM_THREAD_MODEL", "opus");
    let chain = thread_model_candidates();
    assert_eq!(chain[0], Some("opus".to_string()));
    assert_eq!(chain[1], Some(DEFAULT_THREAD_MODEL.to_string()));
    assert_eq!(chain[2], None);
    assert_eq!(primary_thread_model(), "opus");
    clear_env();
}

// ---- episode candidate filter (outcome + files gate) -----------------------

#[test]
fn candidate_episodes_require_partial_or_failed_and_files() {
    let storage = open();
    insert_episode(
        &storage,
        &episode_json("sess-ok", "failed", &["/repo/a.rs"]),
        "2026-01-01T00:00:00Z",
    );
    insert_episode(
        &storage,
        &episode_json("sess-success", "success", &["/repo/b.rs"]),
        "2026-01-01T00:00:00Z",
    );
    insert_episode(
        &storage,
        &episode_json("sess-nofiles", "partial", &[]),
        "2026-01-01T00:00:00Z",
    );

    let episodes = storage
        .with_connection(|conn| load_candidate_episodes(conn, 40))
        .unwrap();
    assert_eq!(
        episodes.len(),
        1,
        "{:?}",
        episodes.iter().map(|e| &e.session_id).collect::<Vec<_>>()
    );
    assert_eq!(episodes[0].session_id, "sess-ok");
}

// ---- files_json/scratchpad filtering ---------------------------------------

#[test]
fn scratchpad_and_memory_paths_filtered_from_allowlist() {
    let files = vec![
        "/repo/src/a.rs".to_string(),
        "/tmp/scratchpad/notes.md".to_string(),
        "/home/user/memory/MEMORY.md".to_string(),
    ];
    let filtered = filtered_files(&files);
    assert_eq!(filtered, vec!["/repo/src/a.rs".to_string()]);
}
