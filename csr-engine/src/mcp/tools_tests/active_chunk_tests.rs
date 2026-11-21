use super::*;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn kill_switch_matches_s5_output() {
    let _guard = crate::search::episodes::ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("CSR_NO_EPISODES");
    std::env::set_var("CSR_EPISODES", "1");
    let (storage, search) = episode_fixture();
    // Pre-change path: no episodes at all.
    let before = episode_fixture_page(&storage, &search).await;
    assert!(!before.contains("session=\""), "{before}");

    put_episode(&storage, "sess-c", 0, "sessionword episode only detail");
    put_episode(&storage, "sess-a", 0, "sessionword first note");

    std::env::set_var("CSR_NO_EPISODES", "1");
    let killed = episode_fixture_page(&storage, &search).await;
    std::env::remove_var("CSR_NO_EPISODES");
    assert_eq!(killed, before, "kill switch must equal the S5 path");

    // Channel on: the episode-only session appears with its eid, and a
    // session with chunks keeps its best chunk id.
    let on = episode_fixture_page(&storage, &search).await;
    assert!(
        on.contains("session=\"sess-c\" eid=\"ep:sess-c:0\""),
        "{on}"
    );
    assert!(on.contains("session=\"sess-a\""), "{on}");
    assert!(on.contains("<id>sess-a-1</id>"), "{on}");
    assert_eq!(on.matches("session=\"sess-a\"").count(), 1, "{on}");

    // An empty episodes table is the S5 path as well.
    storage
        .with_connection(|conn| {
            conn.execute("DELETE FROM episodes", [])?;
            Ok(())
        })
        .unwrap();
    let emptied = episode_fixture_page(&storage, &search).await;
    assert_eq!(
        emptied, before,
        "an empty episodes table must equal the S5 path"
    );
}

#[tokio::test]
async fn fts_mode_keeps_bm25_order_for_valid_results_with_active_forgetting_on_and_off() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let make_chunk = |id: &str, conversation_id: &str, timestamp: &str, content: &str| {
        crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conversation_id.into(),
            project_name: "test".into(),
            timestamp: timestamp.into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        }
    };
    let bm25_first = make_chunk(
        "bm25-first",
        "bm25-first-conversation",
        "2020-01-01T00:00:00Z",
        "bm25_order_identifier bm25_order_identifier bm25_order_identifier",
    );
    let newest_but_bm25_second = make_chunk(
        "bm25-second",
        "bm25-second-conversation",
        "2099-01-01T00:00:00Z",
        "bm25_order_identifier with several unrelated filler words around it",
    );
    storage.insert_chunk(&bm25_first, &[0.0; 4]).unwrap();
    storage
        .insert_chunk(&newest_but_bm25_second, &[0.0; 4])
        .unwrap();
    let stored_order = storage
        .fts5_search("bm25_order_identifier", 2, Some("test"))
        .unwrap();
    assert_eq!(stored_order[0].0.id, "bm25-first");
    assert_eq!(stored_order[1].0.id, "bm25-second");

    for active_forgetting in [false, true] {
        let output = reflect_on_past_with_vec_mode(
            &storage,
            &Arc::new(RwLock::new(SearchEngine::new(8))),
            &[1.0, 0.0, 0.0, 0.0],
            "bm25_order_identifier",
            2,
            0.5,
            Some("test"),
            0,
            true,
            ConsumptionMode::Full,
            active_forgetting,
            "other",
            SearchMode::Fts,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();

        assert!(
            pos_of(&output, "<id>bm25-first</id>")
                < pos_of(&output, "<id>bm25-second</id>"),
            "FTS valid results must retain BM25 order with active_forgetting={active_forgetting}:\n{output}"
        );
    }
}

#[test]
fn active_forgetting_flag_parsing_is_opt_in_only() {
    assert!(active_forgetting_enabled_from(Some("1")));
    for value in [None, Some("0"), Some("true"), Some("yes"), Some("")] {
        assert!(
            !active_forgetting_enabled_from(value),
            "value {value:?} must leave active forgetting off"
        );
    }
}

#[test]
fn search_mode_parses_known_values_and_defaults_to_hybrid() {
    assert_eq!(search_mode_from(None), SearchMode::Hybrid);
    assert_eq!(search_mode_from(Some("hybrid")), SearchMode::Hybrid);
    assert_eq!(search_mode_from(Some("vector")), SearchMode::Vector);
    assert_eq!(search_mode_from(Some("fts")), SearchMode::Fts);
    for value in [Some(""), Some("unknown"), Some("HYBRID"), Some(" fts ")] {
        assert_eq!(
            search_mode_from(value),
            SearchMode::Hybrid,
            "value {value:?} must fall back to hybrid"
        );
    }
}

#[test]
fn rrf_combines_rankings_and_breaks_ties_by_semantic_order() {
    let semantic = ["a", "b", "c"].map(str::to_string);
    let fts = ["c", "b", "d"].map(str::to_string);
    assert_eq!(fuse_rrf(&semantic, &fts, 60), ["c", "b", "a", "d"]);

    let semantic_tie = ["semantic-first", "fts-first"].map(str::to_string);
    let fts_tie = ["fts-first", "semantic-first"].map(str::to_string);
    assert_eq!(
        fuse_rrf(&semantic_tie, &fts_tie, 60),
        ["semantic-first", "fts-first"]
    );
}

#[test]
fn active_forgetting_off_output_is_byte_identical_to_current_behavior() {
    let now = "2026-04-15T10:00:00Z"
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap();
    let timestamp = "2026-01-15T10:00:00Z"
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap();
    let validity: HashMap<String, ConvValidity> = [("stale".to_string(), demote_validity("stale"))]
        .into_iter()
        .collect();
    let score = 0.91_f32;

    let actual_score = apply_chunk_decay(
        score,
        &timestamp,
        &now,
        &[],
        &decay::DecayConfig::for_search(),
        &validity,
        "stale",
        None,
        false,
    );

    assert_eq!(actual_score.to_bits(), score.to_bits());

    let mut actual = vec![enriched_result_conv(
        "stale",
        "conv-demoted",
        actual_score,
        0,
    )];
    let mut expected = vec![enriched_result_conv("stale", "conv-demoted", score, 0)];
    apply_validity_partition(&mut actual, &validity, false);
    apply_validity_partition(&mut expected, &validity, false);

    assert_eq!(
        format::format_search_results(&actual, "q", "all", 5, 3),
        format::format_search_results(&expected, "q", "all", 5, 3)
    );
}

#[test]
fn active_forgetting_demote_decays_faster_than_identical_clean_chunk() {
    let now = chrono::Utc::now();
    let timestamp = now - chrono::Duration::days(90);
    let validity: HashMap<String, ConvValidity> =
        [("chunk-demoted".to_string(), demote_validity("stale"))]
            .into_iter()
            .collect();
    let config = decay::DecayConfig::for_search();

    let demoted = apply_chunk_decay(
        1.0,
        &timestamp,
        &now,
        &[],
        &config,
        &validity,
        "chunk-demoted",
        None,
        true,
    );
    let clean = apply_chunk_decay(
        1.0,
        &timestamp,
        &now,
        &[],
        &config,
        &validity,
        "conv-clean",
        None,
        true,
    );

    assert!(demoted < clean, "demoted={demoted}, clean={clean}");
}

#[test]
fn verdict_partitioned_chunk_does_not_stack_ancestry_decay() {
    use crate::storage::ancestry::{AncestryLabel, AncestryState};

    let now = chrono::Utc::now();
    let past = now - chrono::Duration::days(30);
    let config = decay::DecayConfig::for_search();
    let validity: HashMap<String, ConvValidity> =
        [("chunk-demoted".to_string(), demote_validity("stale"))]
            .into_iter()
            .collect();
    let label = AncestryLabel {
        conversation_id: "conv-demoted".into(),
        state: AncestryState::Shipped,
        release_tag: Some("v1.0.0".into()),
        releases_behind: 5,
        repository: "/repo".into(),
        refreshed_at: "2026-08-06T12:00:00Z".into(),
    };

    let expected = decay::apply_tad_with_age_multiplier(
        0.9,
        &past,
        &now,
        &[],
        &config,
        ACTIVE_FORGETTING_DECAY_FACTOR,
    );
    let actual = apply_chunk_decay(
        0.9,
        &past,
        &now,
        &[],
        &config,
        &validity,
        "chunk-demoted",
        Some(&label),
        true,
    );

    assert_eq!(actual.to_bits(), expected.to_bits());
}

#[test]
fn semantic_scaffold_suppresses_ancestry_and_neutral_paths_are_bit_identical() {
    use crate::storage::ancestry::{AncestryLabel, AncestryState};

    let now = chrono::Utc::now();
    let mut chunk = enriched_result_conv("scaffold", "conv-scaffold", 0.9, 0).chunk;
    chunk.timestamp = (now - chrono::Duration::days(30)).to_rfc3339();
    chunk.content = "<command-message>quoted workflow</command-message>".into();
    let config = decay::DecayConfig::for_search();
    let label = |releases_behind| AncestryLabel {
        conversation_id: "conv-scaffold".into(),
        state: AncestryState::Shipped,
        release_tag: Some("v1.0.0".into()),
        releases_behind,
        repository: "/repo".into(),
        refreshed_at: now.to_rfc3339(),
    };
    let timestamp = chunk.timestamp.parse().unwrap();
    let pre_change = decay::apply_tad(0.9, &timestamp, &now, &[], &config);
    let missing = score_chunk_candidate(
        0.9,
        &chunk,
        &now,
        &[],
        &config,
        &HashMap::new(),
        None,
        false,
        &unscoped_search_scope(),
        RetrievalPolicy::Legacy,
    );
    let current_release = label(0);
    let current = score_chunk_candidate(
        0.9,
        &chunk,
        &now,
        &[],
        &config,
        &HashMap::new(),
        Some(&current_release),
        false,
        &unscoped_search_scope(),
        RetrievalPolicy::Legacy,
    );
    let shipped_release = label(5);
    let shipped = score_chunk_candidate(
        0.9,
        &chunk,
        &now,
        &[],
        &config,
        &HashMap::new(),
        Some(&shipped_release),
        false,
        &unscoped_search_scope(),
        RetrievalPolicy::Legacy,
    );

    assert_eq!(missing.0.to_bits(), pre_change.to_bits());
    assert_eq!(current.0.to_bits(), pre_change.to_bits());
    assert_eq!(shipped.0.to_bits(), pre_change.to_bits());
    assert!(!missing.1 && !current.1 && !shipped.1);

    chunk.content = "organic conversation".into();
    let organic_pre_change = decay::apply_tad(0.9, &timestamp, &now, &[], &config);
    let organic_missing = score_chunk_candidate(
        0.9,
        &chunk,
        &now,
        &[],
        &config,
        &HashMap::new(),
        None,
        false,
        &unscoped_search_scope(),
        RetrievalPolicy::Legacy,
    );
    let organic_current = score_chunk_candidate(
        0.9,
        &chunk,
        &now,
        &[],
        &config,
        &HashMap::new(),
        Some(&current_release),
        false,
        &unscoped_search_scope(),
        RetrievalPolicy::Legacy,
    );
    assert_eq!(organic_missing.0.to_bits(), organic_pre_change.to_bits());
    assert_eq!(organic_current.0.to_bits(), organic_pre_change.to_bits());
}

#[test]
fn fts_scoring_none_and_current_release_match_pre_ancestry_bits() {
    use crate::storage::ancestry::{AncestryLabel, AncestryState};

    let now = chrono::Utc::now();
    let mut chunk = enriched_result_conv("fts", "conv-fts", 0.45, 0).chunk;
    chunk.timestamp = (now - chrono::Duration::days(30)).to_rfc3339();
    chunk.content = "zebraquark organic conversation".into();
    let timestamp = chunk.timestamp.parse().unwrap();
    let current_release = AncestryLabel {
        conversation_id: "conv-fts".into(),
        state: AncestryState::Shipped,
        release_tag: Some("v-current".into()),
        releases_behind: 0,
        repository: "/repo".into(),
        refreshed_at: now.to_rfc3339(),
    };

    let pre_change = decay::apply_decay(0.45, &timestamp, &now, None, None);
    let missing = score_fts_candidate(
        &chunk,
        &now,
        &HashMap::new(),
        None,
        false,
        RetrievalPolicy::Legacy,
    );
    let current = score_fts_candidate(
        &chunk,
        &now,
        &HashMap::new(),
        Some(&current_release),
        false,
        RetrievalPolicy::Legacy,
    );

    assert_eq!(missing.0.to_bits(), pre_change.to_bits());
    assert_eq!(current.0.to_bits(), pre_change.to_bits());
    assert!(!missing.1 && !current.1);
}

#[test]
fn validity_read_failure_disables_ancestry_score_effect() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let refreshed_at = chrono::Utc::now().to_rfc3339();
    storage
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO conversation_ancestry_cache
                     (conversation_id, state, release_tag, releases_behind, repository, refreshed_at)
                     VALUES
                       ('conv-demoted', 'shipped', 'v1.0.0', 5, '/repo', ?1),
                       ('conv-fts', 'shipped', 'v1.0.0', 5, '/repo', ?1)",
                [&refreshed_at],
            )?;
            Ok(())
        })
        .unwrap();
    let chunk = enriched_result_conv("candidate", "conv-demoted", 0.9, 0).chunk;
    let conversation_ids = vec!["conv-demoted".to_string()];
    let mut signals = CandidateSignals::load(
        &storage,
        std::slice::from_ref(&chunk),
        true,
        ConsumptionMode::Full,
    );
    assert!(signals.ancestry.contains_key("conv-demoted"));
    let now = "2026-08-06T12:00:00Z"
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap();
    let mut chunk = chunk;
    chunk.timestamp = (now - chrono::Duration::days(30)).to_rfc3339();
    let config = decay::DecayConfig::for_search();
    let (ancestry_score, ancestry_applied) = score_chunk_candidate(
        0.9,
        &chunk,
        &now,
        &[],
        &config,
        &signals.validity,
        signals.ancestry.get("conv-demoted"),
        false,
        &unscoped_search_scope(),
        RetrievalPolicy::Legacy,
    );
    assert!(ancestry_applied);

    storage
        .with_connection(|conn| {
            // The validity resolver starts from code_nodes. Removing it
            // gives us a real SQLite read failure while leaving the
            // independently cached ancestry label readable.
            conn.execute_batch("DROP TABLE code_nodes")?;
            Ok(())
        })
        .unwrap();
    assert!(storage
        .witness_verdicts_for_conversations(&conversation_ids)
        .is_err());

    let fts_chunk = enriched_result_conv("fts", "conv-fts", 0.8, 0).chunk;
    let ancestry_revoked = signals.extend(&storage, &[fts_chunk], true, ConsumptionMode::Full);
    assert!(ancestry_revoked);
    assert!(
        signals.ancestry.is_empty(),
        "an FTS validity failure must clear semantic ancestry for the whole pass"
    );

    let (actual, ancestry_applied) = score_chunk_candidate(
        0.9,
        &chunk,
        &now,
        &[],
        &config,
        &signals.validity,
        signals.ancestry.get("conv-demoted"),
        false,
        &unscoped_search_scope(),
        RetrievalPolicy::Legacy,
    );
    let timestamp = chunk
        .timestamp
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap();
    let expected = decay::apply_tad(0.9, &timestamp, &now, &[], &config);
    assert!(!ancestry_applied);
    assert!(ancestry_score < actual);
    assert_eq!(actual.to_bits(), expected.to_bits());

    let failed_initial = CandidateSignals::load(
        &storage,
        std::slice::from_ref(&chunk),
        true,
        ConsumptionMode::Full,
    );
    assert!(failed_initial.validity.is_empty());
    assert!(failed_initial.ancestry.is_empty());
}

#[test]
fn active_forgetting_does_not_change_annotate_channel_decay() {
    let now = chrono::Utc::now();
    let timestamp = now - chrono::Duration::days(90);
    let validity: HashMap<String, ConvValidity> =
        [("chunk-annotated".to_string(), annotate_validity("evolved"))]
            .into_iter()
            .collect();
    let config = decay::DecayConfig::for_search();

    let annotated = apply_chunk_decay(
        1.0,
        &timestamp,
        &now,
        &[],
        &config,
        &validity,
        "chunk-annotated",
        None,
        true,
    );
    let clean = apply_chunk_decay(
        1.0,
        &timestamp,
        &now,
        &[],
        &config,
        &validity,
        "conv-clean",
        None,
        true,
    );

    assert_eq!(annotated.to_bits(), clean.to_bits());
}

#[test]
fn demoted_chunk_sinks_below_non_demoted_stable_order_preserved() {
    let mut enriched = vec![
        enriched_result_conv("a", "conv-demoted", 0.95, 0),
        enriched_result_conv("b", "conv-clean-1", 0.90, 1),
        enriched_result_conv("c", "conv-demoted", 0.85, 2),
        enriched_result_conv("d", "conv-clean-2", 0.80, 3),
    ];
    let validity: HashMap<String, ConvValidity> = [
        (
            "a".to_string(),
            demote_validity("[stale anchor] foo no longer in current code (receipt abc1234)"),
        ),
        (
            "c".to_string(),
            demote_validity("[stale anchor] foo no longer in current code (receipt abc1234)"),
        ),
    ]
    .into_iter()
    .collect();

    apply_validity_partition(&mut enriched, &validity, false);

    let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
    // Non-demoted (b, d) keep their relative order, ahead of demoted
    // (a, c) which ALSO keep their relative order among themselves.
    assert_eq!(ids, ["b", "d", "a", "c"]);
}

#[test]
fn validity_demote_verdict_only_sinks_its_own_chunk_in_shared_conversation() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut enriched = vec![
        enriched_result_conv("chunk-stale", "conv-shared", 0.95, 0),
        enriched_result_conv("chunk-live", "conv-shared", 0.90, 1),
    ];
    // The stored verdict already owns its chunk identity. Neither chunk
    // needs to repeat the symbol/filename text for binding to work.
    enriched[0].chunk.content = "historical implementation discussion".into();
    enriched[1].chunk.content = "unrelated live design discussion".into();
    let live_score = enriched[1].score;
    for result in &enriched {
        storage.insert_chunk(&result.chunk, &[1.0, 0.0]).unwrap();
    }
    let witness_id = install_demote_verdict_for(
        &storage,
        "/repo/src/lib.rs",
        "old_fn",
        "conv-shared",
        "conv-shared",
    );
    assert!(
        resolve_validity_with(
            &storage,
            std::slice::from_ref(&enriched[1].chunk),
            ConsumptionMode::Full,
        )
        .is_empty(),
        "legacy conversation-only identity must abstain when storage contains siblings"
    );
    storage
        .with_connection(|conn| {
            crate::storage::witness_verdicts::bind_witness_to_chunk(
                conn,
                witness_id,
                "chunk-stale",
            )?;
            crate::storage::witness_ledger::insert_witness(
                conn,
                &crate::storage::witness_ledger::WitnessLedgerRow {
                    id: 0,
                    project: "proj".into(),
                    file: "/repo/src/lib.rs".into(),
                    symbol: Some("old_fn".into()),
                    span_start: Some(1),
                    span_end: Some(3),
                    stamp: "b3:2".into(),
                    tier: "committed".into(),
                    at_oid: Some("bbb".into()),
                    source_kind: "backfill".into(),
                    source_id: Some("bbb:old_fn".into()),
                },
            )?;
            let newest_witness = crate::storage::witness_ledger::latest_witness_for_symbol(
                conn,
                "proj",
                "/repo/src/lib.rs",
                Some("old_fn"),
            )?
            .expect("second witness must exist");
            crate::storage::witness_verdicts::insert_verdict_if_changed(
                conn,
                &crate::storage::witness_verdicts::WitnessVerdictRow {
                    witness_id: newest_witness.id,
                    verdict: crate::storage::witness_verdicts::VerdictKind::AnchorObsolete,
                    successor_witness_id: None,
                    receipt_oid: Some("cafebabe".into()),
                    observed_head_oid: "cafebabe".into(),
                },
            )?;
            Ok(())
        })
        .unwrap();
    let chunks: Vec<_> = enriched.iter().map(|result| result.chunk.clone()).collect();
    let validity = resolve_validity_with(&storage, &chunks, ConsumptionMode::Full);

    assert!(is_demote_channel(&validity, "chunk-stale"));
    assert!(!validity.contains_key("chunk-live"));
    assert!(
        resolve_validity_with(
            &storage,
            std::slice::from_ref(&chunks[1]),
            ConsumptionMode::Full,
        )
        .is_empty(),
        "a retrieval window containing only the clean sibling must not inherit the verdict"
    );

    let rerank_ids: Vec<&str> = enriched
        .iter()
        .filter(|result| !is_demote_channel(&validity, &result.chunk.id))
        .map(|result| result.chunk.id.as_str())
        .collect();
    assert_eq!(rerank_ids, ["chunk-live"]);

    let now = chrono::Utc::now();
    let label = crate::storage::ancestry::AncestryLabel {
        conversation_id: "conv-shared".into(),
        state: crate::storage::ancestry::AncestryState::Shipped,
        release_tag: Some("v1.0.0".into()),
        releases_behind: 5,
        repository: "/repo".into(),
        refreshed_at: now.to_rfc3339(),
    };
    let config = decay::DecayConfig::for_search();
    let (_, stale_ancestry_applied) = score_chunk_candidate(
        enriched[0].score,
        &enriched[0].chunk,
        &now,
        &[],
        &config,
        &validity,
        Some(&label),
        false,
        &unscoped_search_scope(),
        RetrievalPolicy::Legacy,
    );
    let (_, live_ancestry_applied) = score_chunk_candidate(
        enriched[1].score,
        &enriched[1].chunk,
        &now,
        &[],
        &config,
        &validity,
        Some(&label),
        false,
        &unscoped_search_scope(),
        RetrievalPolicy::Legacy,
    );
    assert!(!stale_ancestry_applied);
    assert!(live_ancestry_applied);

    apply_validity_partition(&mut enriched, &validity, false);

    assert_eq!(enriched[0].chunk.id, "chunk-live");
    assert_eq!(enriched[0].score.to_bits(), live_score.to_bits());
    assert_eq!(enriched[0].resolution, None);
    assert!(!enriched[0].validity_demoted);
    assert_eq!(enriched[1].chunk.id, "chunk-stale");
    assert!(enriched[1].validity_demoted);
    assert_eq!(
        enriched[1].resolution.as_deref(),
        Some("[stale anchor] old_fn no longer in current code (receipt cafebab)")
    );
}

/// Label, never delete: a result whose chunk holds CSR's own output carries
/// a `[CSR text]` note in every mode (the dream switch does not turn it
/// off), is kept whole, and sinks below the clean results in every mode.
#[test]
fn a_result_holding_csr_text_is_labeled_kept_and_sunk_in_every_mode() {
    use crate::provenance::Speaker;
    let storage = Arc::new(Storage::open_memory().unwrap());
    let results = || {
        vec![
            csr_text_result("csr", "conv-a", Speaker::User, CSR_RECAP_TEXT, 0.95, 0),
            csr_text_result("clean", "conv-b", Speaker::User, "ordinary work", 0.90, 1),
            csr_text_result(
                "tool",
                "conv-c",
                Speaker::ToolResult,
                CSR_RECAP_TEXT,
                0.85,
                2,
            ),
            csr_text_result("plan", "plan:x", Speaker::User, CSR_RECAP_TEXT, 0.80, 3),
        ]
    };
    for (mode, order) in [
        (ConsumptionMode::Full, ["clean", "tool", "plan", "csr"]),
        (
            ConsumptionMode::AnnotateOnly,
            ["clean", "tool", "plan", "csr"],
        ),
        (ConsumptionMode::Off, ["clean", "tool", "plan", "csr"]),
    ] {
        let mut enriched = results();
        let chunks: Vec<_> = enriched.iter().map(|e| e.chunk.clone()).collect();
        // stored with their authors, as the importer writes them: a chunk
        // with no provenance row at all would be a legacy chunk of unknown author
        for chunk in &chunks {
            let _ = storage.insert_chunk(chunk, &[0.0; 4]);
            let _ = storage.insert_chunk_provenance(
                &chunk.id,
                &crate::provenance::ChunkProvenance {
                    author: chunk.author,
                    source_conv_id: chunk.conversation_id.clone(),
                    supersedes: None,
                },
            );
        }
        let validity = resolve_validity_with(&storage, &chunks, mode);
        apply_validity_partition(&mut enriched, &validity, false);

        let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
        assert_eq!(ids, order, "{mode:?}");
        for e in &enriched {
            let labeled = e
                .resolution
                .as_deref()
                .is_some_and(|r| r.starts_with("[CSR text]"));
            assert_eq!(labeled, e.chunk.id == "csr", "{mode:?} {}", e.chunk.id);
            // never counted as a dream verdict, never edited
            assert!(!e.validity_demoted);
        }
        assert_eq!(
            enriched
                .iter()
                .find(|e| e.chunk.id == "csr")
                .unwrap()
                .chunk
                .content,
            CSR_RECAP_TEXT
        );
    }
}

#[test]
fn a_csr_text_label_joins_a_dream_note_instead_of_replacing_it() {
    use crate::provenance::Speaker;
    let storage = Arc::new(Storage::open_memory().unwrap());
    let chunk = csr_text_result("c", "conv", Speaker::Assistant, CSR_RECAP_TEXT, 0.9, 0).chunk;
    let mut validity: HashMap<String, ConvValidity> = [(
        "c".to_string(),
        demote_validity("[stale anchor] foo no longer in current code (receipt abc1234)"),
    )]
    .into_iter()
    .collect();
    add_csr_text_labels(&mut validity, &storage, &[chunk]);
    let note = &validity["c"].note;
    assert!(note.starts_with("[stale anchor]"), "{note}");
    assert!(note.contains("[CSR text]"), "{note}");
}

#[test]
fn demoted_chunk_still_returned_if_it_fits_the_limit() {
    let mut enriched = vec![
        enriched_result_conv("a", "conv-demoted", 0.95, 0),
        enriched_result_conv("b", "conv-clean", 0.90, 1),
    ];
    let validity: HashMap<String, ConvValidity> = [(
        "a".to_string(),
        demote_validity("[stale anchor] foo no longer in current code (receipt abc1234)"),
    )]
    .into_iter()
    .collect();

    apply_validity_partition(&mut enriched, &validity, false);
    enriched.truncate(2); // both fit — demoted is sunk, not dropped

    let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
    assert_eq!(ids, ["b", "a"]);
}

#[test]
fn demoted_chunk_annotation_string_exact() {
    let mut enriched = vec![enriched_result_conv("a", "conv-demoted", 0.95, 0)];
    let validity: HashMap<String, ConvValidity> = [(
        "a".to_string(),
        demote_validity("[stale anchor] old_fn no longer in current code (receipt abc1234)"),
    )]
    .into_iter()
    .collect();

    apply_validity_partition(&mut enriched, &validity, false);

    assert_eq!(
        enriched[0].resolution.as_deref(),
        Some("[stale anchor] old_fn no longer in current code (receipt abc1234)")
    );
}

#[test]
fn annotate_channel_does_not_move_rank_only_annotates() {
    let mut enriched = vec![
        enriched_result_conv("a", "conv-evolved", 0.70, 0),
        enriched_result_conv("b", "conv-clean", 0.95, 1),
    ];
    let validity: HashMap<String, ConvValidity> = [(
        "a".to_string(),
        annotate_validity("[evolved] foo changed since this conversation (as of abc1234)"),
    )]
    .into_iter()
    .collect();

    apply_validity_partition(&mut enriched, &validity, false);

    // Order untouched — "a" (lower score) was already below "b" and
    // stays there; Annotate never moves rank.
    let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(
        enriched[0].resolution.as_deref(),
        Some("[evolved] foo changed since this conversation (as of abc1234)")
    );
    assert_eq!(enriched[1].resolution.as_deref(), None);
}

#[test]
fn annotate_note_merges_with_existing_resolution_ledger_note() {
    let mut enriched = vec![enriched_result_conv("a", "conv-evolved", 0.70, 0)];
    enriched[0].resolution = Some("still_open: earlier verdict".to_string());
    let validity: HashMap<String, ConvValidity> = [(
        "a".to_string(),
        annotate_validity("[evolved] foo changed since this conversation (as of abc1234)"),
    )]
    .into_iter()
    .collect();

    apply_validity_partition(&mut enriched, &validity, false);

    assert_eq!(
        enriched[0].resolution.as_deref(),
        Some("still_open: earlier verdict; [evolved] foo changed since this conversation (as of abc1234)")
    );
}

#[test]
fn empty_validity_map_is_a_no_op() {
    let mut enriched = vec![
        enriched_result_conv("a", "conv-1", 0.95, 0),
        enriched_result_conv("b", "conv-2", 0.90, 1),
    ];
    apply_validity_partition(&mut enriched, &HashMap::new(), false);
    let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
    assert_eq!(ids, ["a", "b"]);
    assert!(enriched.iter().all(|e| e.resolution.is_none()));
}

#[test]
fn kill_switch_disables_validity_partition() {
    // `enabled = false` (what `validity_partition_enabled` returns when
    // CSR_NO_VALIDITY_PARTITION=1) must make resolution return an empty
    // map regardless of what storage would otherwise report. Driven by
    // parameter, not by mutating the real env var — see
    // `resolve_validity_with`'s doc on why (parallel test races).
    let storage = Arc::new(Storage::open_memory().unwrap());
    let refreshed_at = chrono::Utc::now().to_rfc3339();
    storage
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO conversation_ancestry_cache
                     (conversation_id, state, release_tag, releases_behind, repository, refreshed_at)
                     VALUES ('conv-1', 'shipped', 'v1.0.0', 5, '/repo', ?1)",
                [&refreshed_at],
            )?;
            Ok(())
        })
        .unwrap();
    let chunk = enriched_result("chunk-1", 0.9, 0).chunk;
    let out = resolve_validity_with(&storage, std::slice::from_ref(&chunk), ConsumptionMode::Off);
    assert!(out.is_empty());
    let signals = CandidateSignals::load(&storage, &[chunk], false, ConsumptionMode::Off);
    assert!(signals.validity.is_empty());
    assert!(
        signals.ancestry.is_empty(),
        "without a validity batch, the kill switch must also disable ancestry"
    );
    assert!(!signals.ancestry_allowed);
}

#[test]
fn validity_partition_enabled_parses_env_var() {
    // The one place this suite touches the real process env var for
    // this feature — every other test drives `resolve_validity_with`
    // directly by parameter to avoid racing with this test under
    // cargo's parallel test runner. The guard serializes us against the
    // other env-sensitive tests (status, eval gate) that also hold it.
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    let restore = std::env::var("CSR_NO_VALIDITY_PARTITION").ok();
    std::env::set_var("CSR_NO_VALIDITY_PARTITION", "1");
    assert!(!validity_partition_enabled());
    std::env::set_var("CSR_NO_VALIDITY_PARTITION", "0");
    assert!(validity_partition_enabled());
    std::env::remove_var("CSR_NO_VALIDITY_PARTITION");
    assert!(validity_partition_enabled());
    match restore {
        Some(v) => std::env::set_var("CSR_NO_VALIDITY_PARTITION", v),
        None => std::env::remove_var("CSR_NO_VALIDITY_PARTITION"),
    }
}

#[test]
fn synthetic_db_demoted_symbol_partitions_end_to_end() {
    // Full DB-backed exercise of the v10 validity partition: a real
    // code_nodes + witness_ledger + witness_verdicts fixture producing a
    // genuine Demote-channel verdict, resolved through `resolve_validity`
    // (the same storage round-trip `reflect_on_past` makes) — not a
    // fabricated ConvValidity map like the tests above. Covers:
    // partition order, exact annotation string, no-stacking (the shared
    // `is_demote_channel` predicate agreeing with the partition), and
    // the kill switch, all against one synthetic DB with a
    // demoted-symbol conversation plus two untouched "valid" ones.
    use crate::storage::codegraph::{upsert_node, NodeRow};
    use crate::storage::witness_ledger::{self, WitnessLedgerRow};
    use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};

    let storage = Arc::new(Storage::open_memory().unwrap());
    storage
        .with_connection(|conn| {
            upsert_node(
                conn,
                &NodeRow {
                    id: crate::extraction::codegraph::node_id(
                        "proj",
                        "/repo/src/lib.rs",
                        "function",
                        "old_fn",
                    ),
                    repo: "proj".into(),
                    project: "proj".into(),
                    file: "/repo/src/lib.rs".into(),
                    lang: "rust".into(),
                    kind: "function".into(),
                    name: "old_fn".into(),
                    fqname: String::new(),
                    body_hash: String::new(),
                    span_start: 1,
                    span_end: 3,
                    first_conv_id: "conv-demoted".into(),
                    last_conv_id: "conv-demoted".into(),
                    last_session_id: "sess".into(),
                    repo_root: None,
                    name_only: false,
                    attribution: String::new(),
                },
            )?;
            witness_ledger::insert_witness(
                conn,
                &WitnessLedgerRow {
                    id: 0,
                    project: "proj".into(),
                    file: "/repo/src/lib.rs".into(),
                    symbol: Some("old_fn".into()),
                    span_start: Some(1),
                    span_end: Some(3),
                    stamp: "b3:1".into(),
                    tier: "committed".into(),
                    at_oid: Some("aaa".into()),
                    source_kind: "backfill".into(),
                    source_id: Some("aaa".into()),
                },
            )?;
            let wid = witness_ledger::latest_witness_for_symbol(
                conn,
                "proj",
                "/repo/src/lib.rs",
                Some("old_fn"),
            )?
            .unwrap()
            .id;
            witness_verdicts::insert_verdict_if_changed(
                conn,
                &WitnessVerdictRow {
                    witness_id: wid,
                    verdict: VerdictKind::AnchorObsolete,
                    successor_witness_id: None,
                    receipt_oid: Some("deadbeef00".into()),
                    observed_head_oid: "deadbeef00".into(),
                },
            )?;
            Ok(())
        })
        .unwrap();

    let mut enriched = vec![
        // Highest raw score of the three — if the partition were
        // stacking with (or losing to) score-based ranking, this would
        // stay #1. It must not.
        enriched_result_conv("stale", "conv-demoted", 0.95, 0),
        enriched_result_conv("valid-1", "conv-clean-1", 0.90, 1),
        enriched_result_conv("valid-2", "conv-clean-2", 0.80, 2),
    ];
    enriched[0].chunk.content = "old_fn stale claim".into();
    for result in &enriched {
        storage.insert_chunk(&result.chunk, &[1.0, 0.0]).unwrap();
    }
    let chunks: Vec<_> = enriched.iter().map(|e| e.chunk.clone()).collect();
    let validity = resolve_validity_with(&storage, &chunks, ConsumptionMode::Full);

    // NO STACKING: the shared skip predicate agrees with what the
    // partition below will do — this is the exact check
    // `reflect_on_past`'s TAD loop and rerank filter make.
    assert!(is_demote_channel(&validity, "stale"));
    assert!(!is_demote_channel(&validity, "valid-1"));
    assert!(!is_demote_channel(&validity, "valid-2"));

    apply_validity_partition(&mut enriched, &validity, false);

    let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
    assert_eq!(
        ids,
        ["valid-1", "valid-2", "stale"],
        "highest-scored chunk still sinks below both lower-scored valid \
             chunks — the partition, not the score, decides rank"
    );
    assert_eq!(
        enriched[2].resolution.as_deref(),
        Some("[stale anchor] old_fn no longer in current code (receipt deadbee)")
    );
    assert!(enriched[0].resolution.is_none());
    assert!(enriched[1].resolution.is_none());

    // Kill switch, same populated DB and same real storage round-trip
    // (via `resolve_validity_with(..., false)` — see that function's
    // doc on why tests drive this by parameter, never by mutating the
    // real env var): resolution must come back empty and the partition
    // must be a total no-op.
    let validity_off = resolve_validity_with(&storage, &chunks, ConsumptionMode::Off);
    assert!(validity_off.is_empty());

    let mut enriched_off = vec![
        enriched_result_conv("stale", "conv-demoted", 0.95, 0),
        enriched_result_conv("valid-1", "conv-clean-1", 0.90, 1),
    ];
    apply_validity_partition(&mut enriched_off, &validity_off, false);
    let ids_off: Vec<&str> = enriched_off.iter().map(|e| e.chunk.id.as_str()).collect();
    assert_eq!(
        ids_off,
        ["stale", "valid-1"],
        "kill switch: original order preserved, nothing demoted/annotated"
    );
    assert!(enriched_off.iter().all(|e| e.resolution.is_none()));
}

#[test]
fn dream_consumption_off_never_queries_witness_verdicts_but_preserves_ancestry() {
    // The regression this proves: `CandidateSignals::load`'s ancestry gate
    // (`ancestry_enabled`, the pre-existing `CSR_NO_VALIDITY_PARTITION` kill
    // switch) must stay independent from the NEW dream-consumption gate
    // (`consumption_enabled`) — exactly the bug the rejected first T2
    // attempt shipped by folding both into one boolean. Proven two ways:
    // (1) dropping `witness_verdicts` entirely and confirming `load` still
    // succeeds (never touches the table when consumption is off — not just
    // "happens to return an empty result the same way an error would");
    // (2) the release-ancestry cache row for `conv-ancestry` still loads.
    let (storage, _enriched) = dream_consumption_fixture();
    storage
        .with_connection(|conn| {
            conn.execute_batch("DROP TABLE witness_verdicts")?;
            Ok(())
        })
        .unwrap();

    let chunks = vec![
        enriched_result_conv("stale", "conv-demoted", 0.95, 0).chunk,
        enriched_result_conv("evolved", "conv-annotated", 0.92, 1).chunk,
        enriched_result_conv("ancestry", "conv-ancestry", 0.90, 2).chunk,
    ];
    let consumption_mode = dream_consumption_mode_from(Some("off"));
    assert_eq!(consumption_mode, ConsumptionMode::Off);

    let signals = CandidateSignals::load(&storage, &chunks, true, consumption_mode);
    assert!(
        signals.validity.is_empty(),
        "no witness_verdicts query means no verdict can have been read"
    );
    assert!(
        signals.ancestry.contains_key("conv-ancestry"),
        "ancestry must load even though witness_verdicts is gone and consumption \
             is off — the two signals are independent"
    );
    assert!(signals.ancestry_allowed);
}

#[test]
fn dream_consumption_off_suppresses_all_verdict_text_end_to_end() {
    let (storage, mut enriched) = dream_consumption_fixture();
    let chunks: Vec<_> = enriched.iter().map(|e| e.chunk.clone()).collect();

    let consumption_mode = dream_consumption_mode_from(Some("0"));
    assert_eq!(consumption_mode, ConsumptionMode::Off);

    let signals = CandidateSignals::load(&storage, &chunks, true, consumption_mode);
    assert!(signals.validity.is_empty());

    apply_validity_partition(&mut enriched, &signals.validity, false);
    let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
    assert_eq!(
        ids,
        ["stale", "evolved", "valid-1", "valid-2"],
        "dream consumption OFF: original score order preserved, nothing demoted \
             or annotated"
    );
    assert!(
        enriched.iter().all(|e| e.resolution.is_none()),
        "no [stale anchor]/[evolved] text may render when consumption is off"
    );
}

#[test]
fn dream_consumption_annotate_only_renders_notes_without_sinking() {
    let (storage, mut enriched) = dream_consumption_fixture();
    let chunks: Vec<_> = enriched.iter().map(|e| e.chunk.clone()).collect();

    let consumption_mode = dream_consumption_mode_from(None);
    assert_eq!(consumption_mode, ConsumptionMode::AnnotateOnly);

    let signals = CandidateSignals::load(&storage, &chunks, true, consumption_mode);
    assert!(!is_demote_channel(&signals.validity, "stale"));
    assert!(!is_demote_channel(&signals.validity, "evolved"));

    apply_validity_partition(&mut enriched, &signals.validity, false);
    let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
    assert_eq!(ids, ["stale", "evolved", "valid-1", "valid-2"]);
    assert_eq!(
        enriched[0].resolution.as_deref(),
        Some("[stale anchor] old_fn no longer in current code (receipt deadbee)")
    );
    assert_eq!(
        enriched[1].resolution.as_deref(),
        Some("[evolved] evolved_fn changed since this conversation (as of bbb)")
    );
    assert!(enriched.iter().all(|result| !result.validity_demoted));
}

#[test]
fn dream_consumption_full_reproduces_demote_and_annotate_channels_byte_for_byte() {
    let (storage, mut enriched) = dream_consumption_fixture();
    enriched[0].chunk.content = "old_fn stale claim".into();
    enriched[1].chunk.content = "evolved_fn evolved claim".into();
    let chunks: Vec<_> = enriched.iter().map(|e| e.chunk.clone()).collect();

    let consumption_mode = dream_consumption_mode_from(Some("full"));
    assert_eq!(consumption_mode, ConsumptionMode::Full);

    let signals = CandidateSignals::load(&storage, &chunks, true, consumption_mode);
    assert!(is_demote_channel(&signals.validity, "stale"));
    assert!(!is_demote_channel(&signals.validity, "evolved"));
    assert!(
        signals.validity.contains_key("evolved"),
        "the Annotate channel must still be present in the map, just not demoted"
    );
    assert!(!is_demote_channel(&signals.validity, "valid-1"));
    assert!(!is_demote_channel(&signals.validity, "valid-2"));

    apply_validity_partition(&mut enriched, &signals.validity, false);
    let ids: Vec<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
    assert_eq!(
        ids,
        ["evolved", "valid-1", "valid-2", "stale"],
        "Demote sinks below everything; Annotate stays in place"
    );
    assert_eq!(
        enriched[0].resolution.as_deref(),
        Some("[evolved] evolved_fn changed since this conversation (as of bbb)")
    );
    assert!(enriched[1].resolution.is_none());
    assert!(enriched[2].resolution.is_none());
    assert_eq!(
        enriched[3].resolution.as_deref(),
        Some("[stale anchor] old_fn no longer in current code (receipt deadbee)")
    );
}

#[test]
fn short_oid_truncates_to_seven_chars() {
    assert_eq!(short_oid("abcdef1234567890"), "abcdef1");
}

#[test]
fn short_oid_never_panics_on_short_fixture_oid() {
    assert_eq!(short_oid("bbb"), "bbb");
}
