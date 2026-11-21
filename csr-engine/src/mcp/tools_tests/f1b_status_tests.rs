use super::*;

/// CE order [live:9, other:1] with a page of one: the sink puts `other`
/// first, so the summary must be read from that order, not keep the "high"
/// label the live chat's scores earned.
#[test]
fn ce_summary_follows_the_order_the_sinks_leave() {
    let scores: HashMap<String, f32> = [("conv-live".to_string(), 9.0), ("conv-o".into(), 1.0)]
        .into_iter()
        .collect();
    let inputs = CeInputs {
        session_scores: scores.clone(),
        session_of_chunk: [
            ("live".to_string(), "conv-live".to_string()),
            ("o".into(), "conv-o".into()),
        ]
        .into_iter()
        .collect(),
    };
    let before = final_ce_summary(&["conv-live", "conv-o"], &scores);
    assert_eq!(
        before.unwrap().label_with(&format::CeBands::SHIPPED),
        "high",
        "premise: the live chat's scores earn high"
    );
    let ids_before = vec!["live".to_string(), "o".to_string()];
    let unchanged = vec![
        enriched_result_conv("live", "conv-live", 0.9, 0),
        enriched_result_conv("o", "conv-o", 0.8, 1),
    ];
    assert_eq!(
        refresh_ce(before, Some(&inputs), &ids_before, &unchanged),
        before,
        "no reorder: the summary is the pass's own"
    );
    let sunk = vec![
        enriched_result_conv("o", "conv-o", 0.8, 1),
        enriched_result_conv("live", "conv-live", 0.9, 0),
    ];
    let after = refresh_ce(before, Some(&inputs), &ids_before, &sunk).unwrap();
    assert_eq!(after.top, 1.0);
    assert_eq!(after.runner_up, Some(9.0));
    assert_ne!(after.label_with(&format::CeBands::SHIPPED), "high");
    // Nothing to read it from: the pass's summary stands.
    assert_eq!(refresh_ce(before, None, &ids_before, &sunk), before);
}

/// The pipeline, not just the map: with the dream switches unset (annotate
/// only) and with the validity kill switch (Off), a logged CSR emission in a
/// conversation is dropped before ranking and the clean result stays; the
/// same text in a plan document is not an emission and both come back.
#[tokio::test]
async fn search_drops_logged_csr_emissions_and_keeps_plan_text() {
    // CSR text in a plan document is kept. The same text in a user row of a
    // conversation is a logged emission and the echo filter drops it. The
    // `[CSR text]` label and sink for the tools that keep such rows are
    // covered by the quick-check, recent-work and file-search tests below.
    for (csr_conversation, partition, mode) in [
        ("plan:conv-csr", true, ConsumptionMode::AnnotateOnly),
        ("plan:conv-csr", false, ConsumptionMode::Off),
        ("conv-csr", true, ConsumptionMode::AnnotateOnly),
    ] {
        let kept = csr_conversation.starts_with("plan:");
        let storage = Arc::new(Storage::open_memory().unwrap());
        let mut search = SearchEngine::new(8);
        for (id, conversation, content, vector, author) in [
            (
                "csr-chunk",
                csr_conversation,
                format!("shared recall text\n{CSR_RECAP_TEXT}"),
                [1.0, 0.0, 0.0, 0.0],
                crate::provenance::Speaker::User,
            ),
            (
                "clean-chunk",
                "conv-clean",
                "shared recall text about ordinary work".to_string(),
                [0.9, 0.1, 0.0, 0.0],
                crate::provenance::Speaker::User,
            ),
        ] {
            let chunk = crate::import::ConversationChunk {
                id: id.into(),
                conversation_id: conversation.into(),
                project_name: "test".into(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                content,
                message_count: 1,
                summary: None,
                author,
                seq: 0,
                is_sidechain: false,
            };
            storage.insert_chunk(&chunk, &vector).unwrap();
            storage
                .insert_chunk_provenance(
                    &chunk.id,
                    &crate::provenance::ChunkProvenance {
                        author,
                        source_conv_id: conversation.into(),
                        supersedes: None,
                    },
                )
                .unwrap();
            search.insert_chunk(chunk.id, vector.to_vec());
        }
        let search = Arc::new(RwLock::new(search));
        let out = reflect_on_past_with_vec(
            &storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            "recall",
            5,
            0.1,
            Some("all"),
            0,
            partition,
            mode,
            false,
        )
        .await
        .unwrap();
        if !kept {
            assert!(
                !out.contains("<id>csr-chunk</id>"),
                "{mode:?}: the logged emission is dropped\n{out}"
            );
            assert!(out.contains("<id>clean-chunk</id>"), "{out}");
            continue;
        }
        assert!(
            out.contains("<id>csr-chunk</id>"),
            "{mode:?}: the plan row is kept\n{out}"
        );
        assert!(out.contains("<id>clean-chunk</id>"), "{out}");
    }
}

/// Every chunk-returning tool labels a chunk holding CSR text and never
/// drops its row: quick check, recent work, the file search fallback.
#[tokio::test]
async fn the_other_chunk_returning_tools_label_csr_text_and_keep_the_row() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut index = SearchEngine::new(8);
    store_csr_text_chunk(
        &storage,
        &mut index,
        "csr-chunk",
        "conv-csr",
        CSR_RULE_TEXT,
        [1.0, 0.0, 0.0, 0.0],
    );
    let search = Arc::new(RwLock::new(index));

    let quick = quick_check_with_vec(
        &storage,
        &search,
        &[1.0, 0.0, 0.0, 0.0],
        "q",
        0.1,
        Some("all"),
    )
    .await
    .unwrap();
    assert!(quick.contains("<found>true</found>"), "{quick}");
    assert!(quick.contains("[CSR text]"), "quick check: {quick}");

    let recent = get_recent_work(&storage, 5, None, "conversation")
        .await
        .unwrap();
    assert!(recent.contains("conv-csr"), "the row is kept: {recent}");
    assert!(recent.contains("[CSR text]"), "recent work: {recent}");

    let by_file = search_by_file(&storage, "libfile", 5, None).await.unwrap();
    assert!(by_file.contains("conv-csr"), "the row is kept: {by_file}");
    assert!(by_file.contains("[CSR text]"), "file search: {by_file}");
}

/// A handoff draft in a stored chunk is labeled CSR text the way a dream card's judgment
/// is, and sinks through the same path (`add_csr_text_labels`), never dropped.
#[tokio::test]
async fn a_stored_handoff_draft_is_labeled_csr_text_and_its_row_kept() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut index = SearchEngine::new(8);
    let draft = crate::companion::handoff::canary_draft();
    store_csr_text_chunk(
        &storage,
        &mut index,
        "handoff-chunk",
        "conv-handoff",
        &draft,
        [1.0, 0.0, 0.0, 0.0],
    );
    let search = Arc::new(RwLock::new(index));
    let quick = quick_check_with_vec(
        &storage,
        &search,
        &[1.0, 0.0, 0.0, 0.0],
        "q",
        0.1,
        Some("all"),
    )
    .await
    .unwrap();
    assert!(quick.contains("<found>true</found>"), "{quick}");
    assert!(quick.contains("[CSR text]"), "quick check: {quick}");
    assert!(
        quick.contains("handoff_draft"),
        "the label names the formatter: {quick}"
    );
    let recent = get_recent_work(&storage, 5, None, "conversation")
        .await
        .unwrap();
    assert!(recent.contains("conv-handoff"), "the row is kept: {recent}");
    assert!(recent.contains("[CSR text]"), "recent work: {recent}");
}

/// With no ledger for the file, the fallback finds a chunk that names the
/// path, and not one that only holds the path's words apart.
#[tokio::test]
async fn file_search_fallback_finds_a_chunk_that_names_a_slash_path() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut index = SearchEngine::new(8);
    store_csr_text_chunk(
        &storage,
        &mut index,
        "names-it",
        "conv-names",
        "changed csr-engine/src/storage/mod.rs to add a column",
        [1.0, 0.0, 0.0, 0.0],
    );
    store_csr_text_chunk(
        &storage,
        &mut index,
        "scattered",
        "conv-scattered",
        "the storage engine in src of csr, a mod for rs files",
        [0.0, 1.0, 0.0, 0.0],
    );
    let out = search_by_file(&storage, "csr-engine/src/storage/mod.rs", 5, None)
        .await
        .unwrap();
    assert!(out.contains("conv-names"), "{out}");
    assert!(!out.contains("conv-scattered"), "{out}");
}

/// The file search fallback sinks a CSR-text row below the clean rows, order
/// kept inside each group, nothing dropped.
#[tokio::test]
async fn file_search_sinks_a_csr_text_row_below_the_clean_ones() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut index = SearchEngine::new(8);
    store_csr_text_chunk(
        &storage,
        &mut index,
        "csr-chunk",
        "conv-csr",
        "A standing rule from 10-12: “Keep receipts”\nlibfile libfile libfile libfile",
        [1.0, 0.0, 0.0, 0.0],
    );
    store_csr_text_chunk(
        &storage,
        &mut index,
        "clean-chunk",
        "conv-clean",
        "notes about the tree, see libfile once among much other unrelated text here",
        [0.0, 1.0, 0.0, 0.0],
    );
    let rows = storage.fts5_search("libfile", 5, None).unwrap();
    assert_eq!(
        rows[0].0.id, "csr-chunk",
        "the CSR row ranks first before the sink"
    );
    let out = search_by_file(&storage, "libfile", 5, None).await.unwrap();
    let csr = out.find("conv-csr").expect("kept");
    let clean = out.find("conv-clean").expect("kept");
    assert!(clean < csr, "clean first\n{out}");
}

/// The printed csr_why output keeps every clean row above every CSR-text
/// row even when they share a conversation: the formatter's grouping by
/// conversation must not lift the CSR row back up.
#[test]
fn printed_why_output_keeps_clean_evidence_above_csr_text_evidence() {
    use crate::search::reinstatement::{EvidenceItem, ReinstateTrace, Via};
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut index = SearchEngine::new(8);
    for (id, conversation, content) in [
        ("c", "A", CSR_RULE_TEXT),
        ("a1", "A", "we moved the index load out of the hook"),
        ("b1", "B", "we capped the retry loop"),
    ] {
        store_csr_text_chunk(
            &storage,
            &mut index,
            id,
            conversation,
            content,
            [1.0, 0.0, 0.0, 0.0],
        );
    }
    let item = |id: &str, conversation: &str, score: f32| EvidenceItem {
        chunk_id: id.into(),
        conversation_id: conversation.into(),
        score,
        best_route: Via::Keyword,
        routes: [Via::Keyword].into_iter().collect(),
        route_scores: [(Via::Keyword, score)].into_iter().collect(),
        timestamp: "2026-10-13T00:00:00Z".into(),
        excerpt: format!("EXCERPT-{id}"),
        ratification: None,
        trust: crate::provenance::TrustTier::Unknown,
    };
    // ranked: C (0.95) in A, then A1 (0.90) in A, then B1 (0.85) in B
    let rendered = render_why_evidence(
        &storage,
        "q",
        vec![
            item("c", "A", 0.95),
            item("a1", "A", 0.90),
            item("b1", "B", 0.85),
        ],
        &HashSet::new(),
        &ReinstateTrace::default(),
    )
    .unwrap();
    let at = |needle: &str| {
        rendered
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} dropped\n{rendered}"))
    };
    assert!(
        at("EXCERPT-a1") < at("EXCERPT-b1") && at("EXCERPT-b1") < at("EXCERPT-c"),
        "printed order must be A1, B1, C\n{rendered}"
    );
    assert!(rendered.contains("[CSR text]"), "{rendered}");
}

#[test]
fn why_evidence_sinks_a_csr_text_chunk_below_the_clean_ones() {
    use crate::search::reinstatement::{EvidenceItem, Via};
    let item = |id: &str, score: f32| EvidenceItem {
        chunk_id: id.into(),
        conversation_id: format!("conv-{id}"),
        score,
        best_route: Via::Keyword,
        routes: [Via::Keyword].into_iter().collect(),
        route_scores: [(Via::Keyword, score)].into_iter().collect(),
        timestamp: "2026-10-13T00:00:00Z".into(),
        excerpt: "x".into(),
        ratification: None,
        trust: crate::provenance::TrustTier::Unknown,
    };
    let items = vec![item("csr", 0.9), item("a", 0.8), item("b", 0.7)];
    let labels: HashMap<String, String> = [("csr".to_string(), "[CSR text] x".to_string())].into();
    let sunk = sink_labeled_evidence(items, &labels);
    let ids: Vec<&str> = sunk.iter().map(|i| i.chunk_id.as_str()).collect();
    assert_eq!(ids, ["a", "b", "csr"]);
}

/// A failed dream-verdict read must not lose the CSR-text labels (recency,
/// concept and keyword fallback all use this wrapper).
#[test]
fn a_failed_verdict_read_keeps_the_csr_text_labels() {
    use crate::provenance::Speaker;
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut index = SearchEngine::new(8);
    store_csr_text_chunk(
        &storage,
        &mut index,
        "csr-chunk",
        "conv-csr",
        CSR_RULE_TEXT,
        [1.0, 0.0, 0.0, 0.0],
    );
    let chunk = csr_text_result(
        "csr-chunk",
        "conv-csr",
        Speaker::User,
        CSR_RULE_TEXT,
        0.9,
        0,
    )
    .chunk;
    // a real SQLite read failure: the validity resolver starts from code_nodes
    storage
        .with_connection(|conn| {
            conn.execute_batch("DROP TABLE code_nodes")?;
            Ok(())
        })
        .unwrap();
    assert!(resolve_validity_checked(
        &storage,
        std::slice::from_ref(&chunk),
        ConsumptionMode::AnnotateOnly
    )
    .is_err());
    for mode in [ConsumptionMode::AnnotateOnly, ConsumptionMode::Full] {
        let validity = resolve_validity_with(&storage, std::slice::from_ref(&chunk), mode);
        let entry = validity
            .get("csr-chunk")
            .unwrap_or_else(|| panic!("{mode:?}: label lost"));
        assert!(entry.note.starts_with("[CSR text]"), "{}", entry.note);
        assert!(entry.demote);
    }
}

#[test]
fn why_evidence_from_a_csr_text_chunk_is_labeled_and_kept() {
    use crate::search::reinstatement::{EvidenceItem, ReinstateTrace, Via};
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut index = SearchEngine::new(8);
    store_csr_text_chunk(
        &storage,
        &mut index,
        "csr-chunk",
        "conv-csr",
        CSR_RULE_TEXT,
        [1.0, 0.0, 0.0, 0.0],
    );
    let item = EvidenceItem {
        chunk_id: "csr-chunk".into(),
        conversation_id: "conv-csr".into(),
        score: 0.9,
        best_route: Via::Keyword,
        routes: [Via::Keyword].into_iter().collect(),
        route_scores: [(Via::Keyword, 0.9)].into_iter().collect(),
        timestamp: "2026-10-13T00:00:00Z".into(),
        excerpt: "A standing rule".into(),
        ratification: None,
        trust: crate::provenance::TrustTier::Unknown,
    };
    let labels = why_csr_text_labels(&storage, std::slice::from_ref(&item));
    let rendered = format_why_labeled(
        "q",
        &[item],
        &HashSet::new(),
        &ReinstateTrace::default(),
        &labels,
    );
    assert!(rendered.contains("conv_conv-csr"), "kept: {rendered}");
    assert!(rendered.contains("[CSR text]"), "{rendered}");
}

/// 20% of a real corpus has no chunk_provenance row. Such a conversation
/// chunk has an unknown author, not a tool author: it is labeled and sunk
/// like any other.
#[tokio::test]
async fn a_legacy_chunk_with_no_provenance_row_is_labeled_and_sunk() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut search = SearchEngine::new(8);
    for (id, conversation, content, vector) in [
        (
            "legacy-csr",
            "conv-legacy",
            "A standing rule from 10-12: “Keep receipts”\nshared recall text".to_string(),
            [1.0, 0.0, 0.0, 0.0],
        ),
        (
            "legacy-clean",
            "conv-clean",
            "shared recall text about ordinary work".to_string(),
            [0.9, 0.1, 0.0, 0.0],
        ),
    ] {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conversation.into(),
            project_name: "test".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content,
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        // no insert_chunk_provenance: a legacy row
        storage.insert_chunk(&chunk, &vector).unwrap();
        search.insert_chunk(chunk.id, vector.to_vec());
    }
    let search = Arc::new(RwLock::new(search));
    let out = reflect_on_past_with_vec(
        &storage,
        &search,
        &[1.0, 0.0, 0.0, 0.0],
        "recall",
        5,
        0.1,
        Some("all"),
        0,
        true,
        ConsumptionMode::AnnotateOnly,
        false,
    )
    .await
    .unwrap();
    let csr = out.find("<id>legacy-csr</id>").expect("kept");
    let clean = out.find("<id>legacy-clean</id>").expect("kept");
    assert!(clean < csr, "clean first\n{out}");
    assert!(out.contains("[CSR text]"), "{out}");
}

#[tokio::test]
async fn full_fts_window_refills_slots_opened_by_sidechain_dedupe() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let make_chunk = |id: &str, conversation_id: &str, is_sidechain: bool, content: &str| {
        crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conversation_id.into(),
            project_name: "test".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::Assistant,
            seq: 0,
            is_sidechain,
        }
    };
    let parent = make_chunk(
        "fts-parent",
        "fts-parent-conversation",
        false,
        "refilltoken refilltoken refilltoken",
    );
    storage.insert_chunk(&parent, &[0.0; 4]).unwrap();
    for index in 0..5 {
        let child = make_chunk(
            &format!("fts-child-{index}"),
            &format!("agent-fts-child-{index}"),
            true,
            "refilltoken refilltoken refilltoken",
        );
        storage.insert_chunk(&child, &[0.0; 4]).unwrap();
        storage
            .insert_chunk_provenance(
                &child.id,
                &crate::provenance::ChunkProvenance {
                    author: crate::provenance::Speaker::Assistant,
                    source_conv_id: "fts-parent-conversation".into(),
                    supersedes: None,
                },
            )
            .unwrap();
    }
    for index in 0..2 {
        let extra = make_chunk(
            &format!("fts-extra-{index}"),
            &format!("fts-extra-conversation-{index}"),
            false,
            "refilltoken with lower density filler words for an additional result",
        );
        storage.insert_chunk(&extra, &[0.0; 4]).unwrap();
    }

    let search = Arc::new(RwLock::new(SearchEngine::new(8)));
    let output = reflect_on_past_with_vec(
        &storage,
        &search,
        &[1.0, 0.0, 0.0, 0.0],
        "refilltoken",
        2,
        0.1,
        Some("all"),
        0,
        false,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();

    assert!(output.contains("<count>2</count>"), "{output}");
    assert!(output.contains("<id>fts-parent</id>"), "{output}");
    assert!(output.contains("<id>fts-extra-"), "{output}");
    assert!(!output.contains("<id>fts-child-"), "{output}");
}

#[tokio::test]
async fn f1b_dedupe_codex_and_claude_identical_text_collapse_to_origin() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let codex = f1b_chunk(
        "f1b-codex",
        "codex-session",
        "Dedupetoken the   rollout NOTES say the gate is green",
    );
    storage
        .insert_chunk_with_source(&codex, &[0.0; 4], "codex_rollout")
        .unwrap();
    let claude = f1b_chunk(
        "f1b-claude",
        "claude-session",
        "dedupetoken the rollout notes say\nthe gate is green",
    );
    storage
        .insert_chunk_with_source(&claude, &[0.0; 4], "conversation")
        .unwrap();

    let output = f1b_fts_search(&storage, "dedupetoken", 5).await;
    assert!(output.contains("<count>1</count>"), "{output}");
    assert!(output.contains("<cid>claude-session</cid>"), "{output}");
    assert!(!output.contains("codex-session"), "{output}");
    assert!(
        output.contains("<cross-source-collapsed>1</cross-source-collapsed>"),
        "{output}"
    );
}

#[tokio::test]
async fn f1b_dedupe_different_text_not_collapsed() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let codex = f1b_chunk(
        "f1b-codex",
        "codex-session",
        "dedupetoken the gate is green",
    );
    storage
        .insert_chunk_with_source(&codex, &[0.0; 4], "codex_rollout")
        .unwrap();
    let claude = f1b_chunk(
        "f1b-claude",
        "claude-session",
        "dedupetoken the gate is red",
    );
    storage
        .insert_chunk_with_source(&claude, &[0.0; 4], "conversation")
        .unwrap();

    let output = f1b_fts_search(&storage, "dedupetoken", 5).await;
    assert!(output.contains("<count>2</count>"), "{output}");
    assert!(output.contains("codex-session"), "{output}");
    assert!(output.contains("claude-session"), "{output}");
    assert!(!output.contains("cross-source-collapsed"), "{output}");
}

#[test]
fn f1b_dedupe_origin_row_takes_the_best_rank_of_the_text() {
    let row = |id: &str, text: &str, score: f32| EnrichedResult {
        score,
        chunk: f1b_chunk(id, id, text),
        resolution: None,
        trust: crate::provenance::TrustTier::Unknown,
        validity_demoted: false,
    };
    let source = |id: &str| match id {
        "codex-a" | "codex-b" => Some("codex_rollout".to_string()),
        "claude-a" => Some("conversation".to_string()),
        "other" => Some("sidechain".to_string()),
        _ => None,
    };
    let mut rows = vec![
        row("codex-a", "the Answer  text", 0.9),
        row("other", "something else", 0.8),
        row("claude-a", "the answer text", 0.7),
        row("codex-b", "unmatched codex text", 0.6),
    ];
    let collapsed = collapse_cross_source_duplicates(&mut rows, source);
    assert_eq!(collapsed, 1);
    let ids: Vec<&str> = rows.iter().map(|r| r.chunk.id.as_str()).collect();
    assert_eq!(ids, ["claude-a", "other", "codex-b"]);
    assert_eq!(rows[0].score, 0.9);
    // A Codex copy with no Claude twin stays.
    let mut alone = vec![row("codex-b", "unmatched codex text", 0.6)];
    assert_eq!(collapse_cross_source_duplicates(&mut alone, source), 0);
    assert_eq!(alone.len(), 1);
}

#[tokio::test]
async fn f1b_dedupe_refills_slot() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let text = "refilltoken refilltoken refilltoken";
    let codex = f1b_chunk("f1b-codex", "codex-session", text);
    storage
        .insert_chunk_with_source(&codex, &[0.0; 4], "codex_rollout")
        .unwrap();
    let claude = f1b_chunk("f1b-claude", "claude-session", text);
    storage
        .insert_chunk_with_source(&claude, &[0.0; 4], "conversation")
        .unwrap();
    for index in 0..2 {
        let extra = f1b_chunk(
            &format!("f1b-extra-{index}"),
            &format!("f1b-extra-conversation-{index}"),
            "refilltoken with lower density filler words for an additional result",
        );
        storage.insert_chunk(&extra, &[0.0; 4]).unwrap();
    }

    let output = f1b_fts_search(&storage, "refilltoken", 2).await;
    assert!(output.contains("<count>2</count>"), "{output}");
    assert!(output.contains("<id>f1b-claude</id>"), "{output}");
    assert!(!output.contains("<id>f1b-codex</id>"), "{output}");
    assert!(output.contains("<id>f1b-extra-"), "{output}");
    assert!(
        output.contains("<cross-source-collapsed>1</cross-source-collapsed>"),
        "{output}"
    );
}

#[test]
fn f1b_status_recency_stage_reads_stored_author_and_subject() {
    use crate::provenance::Speaker;
    let storage = Storage::open_memory().unwrap();
    let mut rows: Vec<EnrichedResult> = Vec::new();
    let mut keys: HashMap<String, String> = HashMap::new();
    for (id, session, when, author, text) in [
        (
            "old-report",
            "s-old",
            "2026-09-13T10:00:00Z",
            Speaker::Assistant,
            "Vidhi maestro hardening workflow: launched, sweeps pending",
        ),
        (
            "new-tool",
            "s-tool",
            "2026-10-01T10:00:00Z",
            Speaker::ToolResult,
            "vidhi maestro hardening workflow log tail",
        ),
        (
            "new-report",
            "s-new",
            "2026-09-20T10:00:00Z",
            Speaker::Assistant,
            "Vidhi maestro hardening workflow: done, two flows left",
        ),
        (
            "new-offtopic",
            "s-off",
            "2026-10-02T10:00:00Z",
            Speaker::Assistant,
            "unrelated note about radio stations",
        ),
    ] {
        let mut chunk = f1b_chunk(id, session, text);
        chunk.timestamp = when.into();
        chunk.author = author;
        storage.insert_chunk(&chunk, &[0.0; 4]).unwrap();
        storage
            .insert_chunk_provenance(
                id,
                &crate::provenance::ChunkProvenance {
                    author,
                    source_conv_id: String::new(),
                    supersedes: None,
                },
            )
            .unwrap();
        keys.insert(id.into(), session.into());
        rows.push(EnrichedResult {
            score: 0.5,
            chunk,
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        });
    }
    let ce: HashMap<String, f32> = [
        ("s-old", 10.0),
        ("s-tool", 9.9),
        ("s-new", 9.9),
        ("s-off", 9.9),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let order: Vec<String> = ["old-report", "new-tool", "new-report", "new-offtopic"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let status =
        "what is the current state of the Vidhi maestro hardening workflow and what is left";

    let out = apply_status_recency(&storage, status, order.clone(), &rows, &keys, &ce, 4);
    // The newer report takes the older report's slot; the tool row and the
    // off-topic row hold theirs.
    assert_eq!(
        out,
        ["new-report", "new-tool", "old-report", "new-offtopic"]
    );
    // Not a status query: untouched.
    let plain = "vidhi maestro hardening workflow notes";
    assert_eq!(
        apply_status_recency(&storage, plain, order.clone(), &rows, &keys, &ce, 4),
        order
    );
    // No cross-encoder scores: untouched.
    assert_eq!(
        apply_status_recency(
            &storage,
            status,
            order.clone(),
            &rows,
            &keys,
            &HashMap::new(),
            4
        ),
        order
    );
}

#[test]
fn f3b_status_query_admits_a_newer_same_session_report_outside_the_page() {
    let (storage, mut enriched) = f3b_session();
    let admission = f3b_admit(&storage, F3B_STATUS, &mut enriched);
    // The newer report was outside the first stage and joins the pool. The
    // tool row is never read; the harness tail is read but dropped.
    assert_eq!(admission.admitted, ["new-report"], "{admission:?}");
    let ids: HashSet<&str> = enriched.iter().map(|e| e.chunk.id.as_str()).collect();
    assert!(ids.contains("new-report") && !ids.contains("harness-tail"));
    // Admitted rows are keyword-lane rows with a row of their own.
    let row = enriched
        .iter()
        .find(|e| e.chunk.id == "new-report")
        .unwrap();
    assert!(row.chunk.content.starts_with("[keyword] "));
    // The pool takes them on top of the window.
    let mut pool = ce_pool_ids(&["old-report".into()], &[], CE_POOL);
    pool.extend(admission.admitted.iter().cloned());
    assert_eq!(pool, ["old-report", "new-report"]);
}

#[test]
fn f3b_status_newest_eligible_chunk_represents_its_session() {
    let (storage, mut enriched) = f3b_session();
    let admission = f3b_admit(&storage, F3B_STATUS, &mut enriched);
    // The harness tail is newer, but the harness wrote it: the newest
    // eligible report is the assistant chunk.
    assert_eq!(
        admission.picks.get("s1").map(String::as_str),
        Some("new-report")
    );
    let keys: HashMap<String, String> = enriched
        .iter()
        .map(|e| (e.chunk.id.clone(), "s1".to_string()))
        .collect();
    let swapped = apply_status_picks(
        vec!["old-report".into()],
        &keys,
        &admission.picks,
        &HashSet::new(),
    );
    assert_eq!(swapped, ["new-report"]);
    // A superseded pick never stands.
    let sunk: HashSet<String> = ["new-report".to_string()].into();
    assert_eq!(
        apply_status_picks(vec!["old-report".into()], &keys, &admission.picks, &sunk),
        ["old-report"]
    );
}

#[test]
fn f3b_status_non_status_query_changes_nothing() {
    let (storage, mut enriched) = f3b_session();
    let before: Vec<String> = enriched.iter().map(|e| e.chunk.id.clone()).collect();
    let plain = "vidhi android maestro hardening workflow notes";
    let admission = f3b_admit(&storage, plain, &mut enriched);
    assert!(admission.admitted.is_empty() && admission.picks.is_empty());
    let after: Vec<String> = enriched.iter().map(|e| e.chunk.id.clone()).collect();
    assert_eq!(before, after);
    let order = vec!["old-report".to_string()];
    let keys: HashMap<String, String> = [("old-report".to_string(), "s1".to_string())].into();
    assert_eq!(
        apply_status_picks(order.clone(), &keys, &admission.picks, &HashSet::new()),
        order
    );
    // A status query under the legacy policy admits nothing either.
    let fused = before.clone();
    let mut signals = CandidateSignals::load(&storage, &[], false, ConsumptionMode::Off);
    let legacy = status_pool_admission(
        &storage,
        F3B_STATUS,
        &mut enriched,
        &fused,
        &HashMap::new(),
        &mut signals,
        &unscoped_search_scope(),
        false,
        RetrievalPolicy::Legacy,
        false,
        ConsumptionMode::Off,
        &chrono::Utc::now(),
    );
    assert!(legacy.admitted.is_empty() && legacy.picks.is_empty());
    assert_eq!(enriched.len(), before.len());
}

#[test]
fn f3b_status_harness_or_tool_result_row_is_never_the_representative() {
    use crate::provenance::Speaker;
    let storage = Arc::new(Storage::open_memory().unwrap());
    let first = f3b_put(
        &storage,
        "report",
        "s1",
        "2026-09-15T02:00:00Z",
        Speaker::Assistant,
        "Vidhi Android maestro hardening: status",
    );
    // Newer, same subject, but the harness wrote one and the other is tool
    // output: neither may stand for the session.
    f3b_put(
        &storage,
        "harness-only",
        "s1",
        "2026-09-15T05:00:30Z",
        Speaker::User,
        "<task-notification> Vidhi Android maestro PASS </task-notification>",
    );
    let tool = f3b_put(
        &storage,
        "tool-newer",
        "s1",
        "2026-09-15T06:00:00Z",
        Speaker::ToolResult,
        "Vidhi Android maestro log",
    );
    let mut enriched = vec![first, tool];
    let admission = f3b_admit(&storage, F3B_STATUS, &mut enriched);
    assert!(admission.admitted.is_empty(), "{admission:?}");
    assert_eq!(
        admission.picks.get("s1").map(String::as_str),
        Some("report")
    );
    assert!(!enriched.iter().any(|e| e.chunk.id == "harness-only"));
}

#[tokio::test]
async fn bench_refetches_past_a_dominant_conversations_initial_chunk_window() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    for index in 0..20 {
        let chunk = crate::import::ConversationChunk {
            id: format!("a-dominant-{index:02}"),
            conversation_id: "dominant-conversation".into(),
            project_name: "csr-bench".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            content: "windowtoken".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::Assistant,
            seq: index,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[0.0; 4]).unwrap();
    }
    let gold = crate::import::ConversationChunk {
        id: "z-gold".into(),
        conversation_id: "gold-conversation".into(),
        project_name: "csr-bench".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        content: "windowtoken".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::User,
        seq: 0,
        is_sidechain: false,
    };
    storage.insert_chunk(&gold, &[0.0; 4]).unwrap();
    let search = Arc::new(RwLock::new(SearchEngine::new(32)));

    let hits = reflect_for_bench_with_vec(
        &storage,
        &search,
        &[0.0; 4],
        "windowtoken",
        5,
        SearchMode::Fts,
    )
    .await
    .unwrap();
    assert!(
        hits.iter()
            .take(5)
            .any(|hit| hit.conversation_id == "gold-conversation"),
        "adaptive refetch must find a distinct conversation below the initial 20 chunks: {hits:?}"
    );
}

#[tokio::test]
async fn hybrid_surfaces_exact_identifier_below_semantic_threshold() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let now = chrono::Utc::now().to_rfc3339();
    let semantic = crate::import::ConversationChunk {
        id: "semantic-high".into(),
        conversation_id: "semantic-conversation".into(),
        project_name: "test".into(),
        timestamp: now.clone(),
        content: "unrelated conceptual match".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::User,
        seq: 0,
        is_sidechain: false,
    };
    let identifier = crate::import::ConversationChunk {
        id: "identifier-low".into(),
        conversation_id: "identifier-conversation".into(),
        project_name: "test".into(),
        timestamp: now,
        content: "implementation of parse_widget_identifier".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::User,
        seq: 1,
        is_sidechain: false,
    };
    let semantic_vec = vec![1.0, 0.0, 0.0, 0.0];
    let identifier_vec = vec![0.4, 0.916_515_1, 0.0, 0.0];
    storage.insert_chunk(&semantic, &semantic_vec).unwrap();
    storage.insert_chunk(&identifier, &identifier_vec).unwrap();
    let mut engine = SearchEngine::new(8);
    engine.insert_chunk(semantic.id.clone(), semantic_vec);
    engine.insert_chunk(identifier.id.clone(), identifier_vec);
    let search = Arc::new(RwLock::new(engine));

    let output = reflect_on_past_with_vec(
        &storage,
        &search,
        &[1.0, 0.0, 0.0, 0.0],
        "parse_widget_identifier",
        5,
        0.5,
        Some("all"),
        0,
        false,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();

    assert!(
        output.contains("<id>identifier-low</id>"),
        "hybrid search must include an exact FTS identifier even when another semantic hit exceeds 0.5:\n{output}"
    );
}

#[tokio::test]
async fn vector_mode_excludes_fts_only_hits() {
    let output = render_ablation_mode(SearchMode::Vector).await;
    assert!(output.contains("<id>mode-semantic</id>"), "{output}");
    assert!(!output.contains("<id>mode-keyword</id>"), "{output}");
}

#[tokio::test]
async fn fts_mode_excludes_semantic_only_hits() {
    let output = render_ablation_mode(SearchMode::Fts).await;
    assert!(output.contains("<id>mode-keyword</id>"), "{output}");
    assert!(!output.contains("<id>mode-semantic</id>"), "{output}");
}

#[tokio::test]
async fn hybrid_limit_one_does_not_double_count_fts_only_hit() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let now = chrono::Utc::now().to_rfc3339();
    let make_chunk =
        |id: &str, conversation_id: &str, content: &str| crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conversation_id.into(),
            project_name: "test".into(),
            timestamp: now.clone(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
    let semantic = make_chunk(
        "rrf-semantic",
        "rrf-semantic-conversation",
        "true semantic result without the identifier",
    );
    let keyword = make_chunk(
        "rrf-keyword-only",
        "rrf-keyword-conversation",
        "rrfconflictword keyword-only result",
    );
    let semantic_vec = vec![1.0, 0.0, 0.0, 0.0];
    let keyword_vec = vec![0.0, 1.0, 0.0, 0.0];
    storage.insert_chunk(&semantic, &semantic_vec).unwrap();
    storage.insert_chunk(&keyword, &keyword_vec).unwrap();
    let mut engine = SearchEngine::new(8);
    engine.insert_chunk(semantic.id.clone(), semantic_vec);
    engine.insert_chunk(keyword.id.clone(), keyword_vec);
    let search = Arc::new(RwLock::new(engine));

    for active_forgetting in [false, true] {
        let output = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &[1.0, 0.0, 0.0, 0.0],
            "rrfconflictword",
            1,
            0.5,
            Some("all"),
            0,
            true,
            ConsumptionMode::Full,
            active_forgetting,
            "other",
            SearchMode::Hybrid,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();

        assert!(output.contains("<id>rrf-semantic</id>"), "{output}");
        assert!(!output.contains("<id>rrf-keyword-only</id>"), "{output}");
    }
}

#[tokio::test]
async fn hybrid_identifier_query_lifts_the_exact_name_through_the_postings_channel() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let now = chrono::Utc::now().to_rfc3339();
    let make_chunk =
        |id: &str, conversation_id: &str, content: &str| crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conversation_id.into(),
            project_name: "test".into(),
            timestamp: now.clone(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
    let semantic = make_chunk(
        "id-semantic",
        "id-semantic-conversation",
        "true semantic result without the name",
    );
    let named = make_chunk(
        "id-named",
        "id-named-conversation",
        "rrf_conflict_identifier keyword-only result",
    );
    let semantic_vec = vec![1.0, 0.0, 0.0, 0.0];
    let named_vec = vec![0.0, 1.0, 0.0, 0.0];
    storage.insert_chunk(&semantic, &semantic_vec).unwrap();
    storage.insert_chunk(&named, &named_vec).unwrap();
    let mut engine = SearchEngine::new(8);
    engine.insert_chunk(semantic.id.clone(), semantic_vec);
    engine.insert_chunk(named.id.clone(), named_vec);
    let search = Arc::new(RwLock::new(engine));

    let output = reflect_on_past_with_vec_mode(
        &storage,
        &search,
        &[1.0, 0.0, 0.0, 0.0],
        "rrf_conflict_identifier",
        1,
        0.5,
        Some("all"),
        0,
        true,
        ConsumptionMode::Full,
        false,
        "other",
        SearchMode::Hybrid,
        RecallRerankMode::Baseline,
        false,
    )
    .await
    .unwrap();

    assert!(output.contains("<id>id-named</id>"), "{output}");
    assert!(!output.contains("<id>id-semantic</id>"), "{output}");
}

#[tokio::test]
async fn hybrid_returns_one_row_per_session_with_its_best_chunk() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let now = chrono::Utc::now().to_rfc3339();
    let make_chunk =
        |id: &str, conversation_id: &str, content: &str| crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conversation_id.into(),
            project_name: "test".into(),
            timestamp: now.clone(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
    let chunks = [
        make_chunk(
            "sess-a-1",
            "sess-a",
            "sessionword first note about sessionword",
        ),
        make_chunk("sess-a-2", "sess-a", "sessionword second note elsewhere"),
        make_chunk("sess-b-1", "sess-b", "sessionword third note"),
    ];
    let vectors = [
        vec![1.0, 0.0, 0.0, 0.0],
        vec![0.9, 0.1, 0.0, 0.0],
        vec![0.8, 0.2, 0.0, 0.0],
    ];
    let mut engine = SearchEngine::new(8);
    for (chunk, vector) in chunks.iter().zip(vectors.iter()) {
        storage.insert_chunk(chunk, vector).unwrap();
        engine.insert_chunk(chunk.id.clone(), vector.clone());
    }
    let search = Arc::new(RwLock::new(engine));

    let output = reflect_on_past_with_vec_mode(
        &storage,
        &search,
        &[1.0, 0.0, 0.0, 0.0],
        "sessionword",
        10,
        0.1,
        Some("all"),
        0,
        true,
        ConsumptionMode::Full,
        false,
        "other",
        SearchMode::Hybrid,
        RecallRerankMode::Baseline,
        false,
    )
    .await
    .unwrap();

    assert_eq!(output.matches("<cid>sess-a</cid>").count(), 1, "{output}");
    assert_eq!(output.matches("<cid>sess-b</cid>").count(), 1, "{output}");
    assert!(output.contains("<id>sess-a-1</id>"), "{output}");
    assert!(!output.contains("<id>sess-a-2</id>"), "{output}");
}
