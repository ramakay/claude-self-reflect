use super::*;

#[test]
fn f2a_refetch_reuses_first_pass_ce_scores() {
    let memo = CallMemo::default();
    let texts = ["alpha", "beta", "gamma"];
    let keys: Vec<String> = ["a", "b", "c"]
        .iter()
        .zip(texts)
        .map(|(id, text)| CallMemo::ce_key(id, text))
        .collect();
    let calls = std::cell::RefCell::new(Vec::<Vec<String>>::new());
    let scorer = |fresh: &[&str]| -> Result<Vec<f32>> {
        calls
            .borrow_mut()
            .push(fresh.iter().map(|t| t.to_string()).collect());
        Ok(fresh.iter().map(|t| t.len() as f32).collect())
    };
    let first = memo.ce_scores(&keys, &texts, scorer).unwrap();
    assert_eq!(first, vec![5.0, 4.0, 5.0]);
    // Second pass: a wider pool holding the same three chunks plus one new.
    let texts2 = ["delta!", "alpha", "beta", "gamma"];
    let keys2: Vec<String> = ["d", "a", "b", "c"]
        .iter()
        .zip(texts2)
        .map(|(id, text)| CallMemo::ce_key(id, text))
        .collect();
    let second = memo.ce_scores(&keys2, &texts2, scorer).unwrap();
    assert_eq!(second, vec![6.0, 5.0, 4.0, 5.0]);
    let calls = calls.borrow();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        calls[1],
        vec!["delta!".to_string()],
        "only the new unit is scored"
    );
    // A third pass of already scored units never calls the scorer.
    let third = memo
        .ce_scores(&keys, &texts, |_| panic!("scorer must not run"))
        .unwrap();
    assert_eq!(third, first);
    // The same id with changed text is a new unit.
    assert_ne!(
        CallMemo::ce_key("a", "alpha"),
        CallMemo::ce_key("a", "alpha2")
    );
}

#[tokio::test]
async fn stage_probe_records_every_stage_and_final_rank_matches_the_returned_page() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    let query = [1.0_f32, 0.0, 0.0, 0.0];
    for i in 0..5 {
        let chunk = crate::import::ConversationChunk {
            id: format!("stage-probe-{i}"),
            conversation_id: format!("codex:stage-probe-conv-{i}"),
            project_name: "test".into(),
            timestamp: "2026-09-20T00:00:00Z".into(),
            content: format!("stage probe content {i}"),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        // Cosine similarity to the query decreases with i, so the target
        // (i=2) lands at a known, non-trivial rank.
        let vector = [1.0 - (i as f32) * 0.1, 0.05, 0.0, 0.0];
        storage.insert_chunk(&chunk, &vector).unwrap();
        engine.insert_chunk(chunk.id, vector.to_vec());
    }
    let search = Arc::new(RwLock::new(engine));

    let (output, samples) = crate::mcp::stage_probe::scope("codex:stage-probe-conv-2", || {
        reflect_on_past_with_vec(
            &storage,
            &search,
            &query,
            "stage probe content",
            6,
            0.0,
            None,
            0,
            false,
            ConsumptionMode::Off,
            false,
        )
    })
    .await;
    let output = output.unwrap();
    assert!(
        output.contains("stage-probe-2"),
        "target chunk should be in the returned page: {output}"
    );

    let stage_names: Vec<&str> = samples.iter().map(|sample| sample.stage).collect();
    assert_eq!(
        stage_names,
        vec![
            "hnsw_fetch",
            "decay_enriched",
            "rerank",
            "postings",
            "fuse_rrf",
            "dedupe",
            "final_page",
        ],
        "every documented stage must record exactly once, in pipeline order"
    );
    // The fixture query names no identifier: the postings stage is
    // recorded, and empty.
    let postings_sample = samples
        .iter()
        .find(|sample| sample.stage == "postings")
        .unwrap();
    assert_eq!(postings_sample.total_candidates, 0);
    for sample in samples.iter().filter(|sample| sample.stage != "postings") {
        assert!(
            sample.target_rank.is_some(),
            "target present at every stage with these fixtures: {} had none",
            sample.stage
        );
    }
    let final_page = samples.last().unwrap();
    assert_eq!(
        final_page.target_rank,
        Some(2),
        "target is the 3rd-most-similar of 5 chunks, 0-indexed rank 2"
    );

    let rerank_sample = samples
        .iter()
        .find(|sample| sample.stage == "rerank")
        .unwrap();
    assert!(
        !rerank_sample.rerank_breakdown.is_empty(),
        "rerank checkpoint must capture a per-term breakdown when the target survives to it"
    );
    let target_rows: Vec<_> = rerank_sample
        .rerank_breakdown
        .iter()
        .filter(|row| row.is_target)
        .collect();
    assert_eq!(
        target_rows.len(),
        1,
        "exactly one row should be flagged as the target's own best chunk"
    );
    assert_eq!(target_rows[0].chunk_id, "stage-probe-2");
    assert!(target_rows[0].breakdown.total.is_finite());
    let displacer_rows: Vec<_> = rerank_sample
        .rerank_breakdown
        .iter()
        .filter(|row| !row.is_target)
        .collect();
    assert_eq!(
        displacer_rows.len(),
        rerank_sample.target_rank.unwrap(),
        "displacer count must equal the target's own rerank rank (everyone ranked above it)"
    );

    let fuse_rrf_sample = samples
        .iter()
        .find(|sample| sample.stage == "fuse_rrf")
        .unwrap();
    assert!(
        fuse_rrf_sample.target_fts_rank.is_some(),
        "target's own content is FTS-matchable, so fuse_rrf must also \
             record its keyword-side rank, not just its fused rank"
    );
}

#[tokio::test]
async fn stage_probe_is_inert_outside_an_active_scope() {
    // `is_active()` must be false with no `scope()` wrapper — the
    // production hot path (MCP tool, hooks) never wraps calls in
    // `scope`, so every `(String, String, f32)` vector build gated on it
    // must be skipped there. Exercised indirectly here: a call outside
    // `scope` returns zero samples via the same production code path
    // that skips the vector builds.
    assert!(!crate::mcp::stage_probe::is_active());
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    let chunk = crate::import::ConversationChunk {
        id: "no-scope-0".into(),
        conversation_id: "codex:no-scope-conv".into(),
        project_name: "test".into(),
        timestamp: "2026-09-20T00:00:00Z".into(),
        content: "no scope active".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::User,
        seq: 0,
        is_sidechain: false,
    };
    let vector = [1.0_f32, 0.0, 0.0, 0.0];
    storage.insert_chunk(&chunk, &vector).unwrap();
    engine.insert_chunk(chunk.id, vector.to_vec());
    let search = Arc::new(RwLock::new(engine));

    // No `scope()` wrapper — this must succeed exactly like production
    // callers, and `is_active()` must read false throughout.
    let output = reflect_on_past_with_vec(
        &storage,
        &search,
        &vector,
        "no scope active",
        6,
        0.0,
        None,
        0,
        false,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();
    assert!(output.contains("no-scope-0"));
    assert!(!crate::mcp::stage_probe::is_active());
}

/// Two `scope()` calls interleaved via `tokio::join!` inside the SAME
/// task must not cross-contaminate: each records samples only for its
/// own target. `LocalKey::scope` sets/restores the task-local value
/// around each individual `poll()`, so sibling scoped futures polled in
/// turn never see each other's state — the property a naive
/// process-global `static` would not have.
#[tokio::test]
async fn stage_probe_scopes_do_not_cross_contaminate_concurrent_queries() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    let query = [1.0_f32, 0.0, 0.0, 0.0];
    for i in 0..3 {
        let chunk = crate::import::ConversationChunk {
            id: format!("concurrent-{i}"),
            conversation_id: format!("codex:concurrent-conv-{i}"),
            project_name: "test".into(),
            timestamp: "2026-09-20T00:00:00Z".into(),
            content: format!("concurrent probe content {i}"),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        let vector = [1.0 - (i as f32) * 0.1, 0.05, 0.0, 0.0];
        storage.insert_chunk(&chunk, &vector).unwrap();
        engine.insert_chunk(chunk.id, vector.to_vec());
    }
    let search = Arc::new(RwLock::new(engine));

    let run = |target: &'static str| {
        let storage = storage.clone();
        let search = search.clone();
        async move {
            crate::mcp::stage_probe::scope(target, || {
                reflect_on_past_with_vec(
                    &storage,
                    &search,
                    &query,
                    "concurrent probe content",
                    6,
                    0.0,
                    None,
                    0,
                    false,
                    ConsumptionMode::Off,
                    false,
                )
            })
            .await
        }
    };
    let (first, second) = tokio::join!(
        run("codex:concurrent-conv-0"),
        run("codex:concurrent-conv-1")
    );
    let (_, first_samples) = first;
    let (_, second_samples) = second;
    assert_eq!(
        first_samples.last().unwrap().target_rank,
        Some(0),
        "conv-0 probe must see its own target, not the other task's"
    );
    assert_eq!(
        second_samples.last().unwrap().target_rank,
        Some(1),
        "conv-1 probe must see its own target, not the other task's"
    );
}

#[test]
fn explicit_memory_is_told_apart_from_derived_reflections() {
    let uuid = "5eb5aebb-65ab-4fba-9917-86cb83845154";
    assert!(is_explicit_memory(
        "story_x",
        &[EXPLICIT_MEMORY_TAG.to_string()]
    ));
    assert!(is_explicit_memory(
        uuid,
        &["anukriti".into(), "memory-archive".into()]
    ));
    assert!(is_explicit_memory(uuid, &[]));
    assert!(!is_explicit_memory(
        uuid,
        &["session_story".into(), "conv_x".into()]
    ));
    assert!(!is_explicit_memory(uuid, &["session_episode".into()]));
    assert!(!is_explicit_memory(
        uuid,
        &["session_briefing".into(), "schema_v1".into()]
    ));
    assert!(!is_explicit_memory(
        uuid,
        &["narrative_extracted_v3".into(), "status_partial".into()]
    ));
    assert!(!is_explicit_memory("v3_abc", &["context_cache".into()]));
    assert!(!is_explicit_memory("extracted_1", &["free".into()]));
}

/// Regression for the live bug: `store_reflection`'s shared insert path
/// stamped `csr:memory` onto every derived narrative it wrote (stories,
/// episodes, ...), so a row could carry BOTH the stamp and a derived tag.
/// The derived tag must win — a pipeline narrative is never a deliberate
/// memory write, whatever it happens to be stamped with.
#[test]
fn derived_tag_overrides_explicit_memory_stamp() {
    let uuid = "5eb5aebb-65ab-4fba-9917-86cb83845154";
    assert!(
        !is_explicit_memory(
            uuid,
            &["session_story".into(), EXPLICIT_MEMORY_TAG.to_string()]
        ),
        "a stamped session_story must not count as an explicit memory"
    );
    assert!(!is_explicit_memory(
        uuid,
        &[
            "narrative_extracted_v3".into(),
            EXPLICIT_MEMORY_TAG.to_string()
        ]
    ));
}

/// The bug this closes: `summarizer.rs` wrote stories through the same
/// `store_reflection` helper the MCP tool uses, so every story got the
/// `csr:memory` stamp and, via `reflection_recall_standing`, the flat
/// `W_USER` boost `derived_story_keeps_no_standing_against_user_chunks`
/// exists to forbid. That test only caught it for reflections written
/// directly through storage; this one exercises the actual write path a
/// story takes (`store_reflection_internal`) and proves it does NOT
/// stamp. Revert the split in `store_reflection`/`store_reflection_internal`
/// and this fails: the row would carry `csr:memory`.
#[tokio::test]
async fn summarizer_story_write_path_does_not_stamp_explicit_memory() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
    let engine = SearchEngine::new(EmbeddingEngine::dimension());
    let search = Arc::new(RwLock::new(engine));
    let tags = vec![
        "session_story".to_string(),
        "project_test".to_string(),
        "conv_abc".to_string(),
    ];
    let result = store_reflection_internal(
        &storage,
        &embeddings,
        &search,
        "a synthesized session story",
        &tags,
    )
    .await
    .unwrap();
    let id = result
        .lines()
        .find_map(|l| l.strip_prefix("ID: "))
        .expect("id line")
        .to_string();
    let (_, stored_tags, _) = storage.get_reflection_by_id(&id).unwrap().unwrap();
    assert!(
        !stored_tags.iter().any(|t| t == EXPLICIT_MEMORY_TAG),
        "the summarizer's write path must not stamp explicit-memory standing: {stored_tags:?}"
    );
    assert!(!is_explicit_memory(&id, &stored_tags));
}

/// The production write path (`store_reflection_internal`, real embeddings)
/// for a story and a ledger row with the same words: every tool that reads
/// reflections returns the story and never the shift.
#[tokio::test]
async fn shift_ledger_rows_written_for_real_never_reach_the_search_tools() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
    let search = Arc::new(RwLock::new(SearchEngine::new(EmbeddingEngine::dimension())));
    let words = "the scheduler now retries the upload queue after a timeout";
    store_reflection_internal(
        &storage,
        &embeddings,
        &search,
        &format!("{words} STORYPROSE"),
        &[
            "session_story".to_string(),
            "project_shiftleak".to_string(),
            "conv_leak1".to_string(),
        ],
    )
    .await
    .unwrap();
    store_reflection_internal(
        &storage,
        &embeddings,
        &search,
        &format!("[shift: model-derived] {words} LEAKEDSHIFT"),
        &[
            "narrative_shift".to_string(),
            "shift_project:shiftleak".to_string(),
            "shift_conv:leak1".to_string(),
            "shift_sig_00aa11bb22cc33dd".to_string(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        storage
            .get_ledger_reflections_by_tag("narrative_shift", 5)
            .unwrap()
            .len(),
        1,
        "the ledger row is stored (never deleted)"
    );

    let replies = [
        reflect_on_past(&storage, &embeddings, &search, words, 5, 0.0, None)
            .await
            .unwrap(),
        get_more_results(&storage, &embeddings, &search, words, 0, 5, 0.0, None)
            .await
            .unwrap(),
        why(
            &storage,
            &embeddings,
            &search,
            words,
            None,
            &crate::search::reinstatement::ReinstateConfig::default(),
        )
        .await
        .unwrap(),
        quick_check(&storage, &embeddings, &search, words, 0.0, None)
            .await
            .unwrap(),
        search_insights(&storage, &embeddings, &search, words, None)
            .await
            .unwrap(),
        search_by_concept(&storage, &embeddings, &search, words, 5, None)
            .await
            .unwrap(),
    ];
    for (i, reply) in replies.iter().enumerate() {
        assert!(!reply.contains("LEAKEDSHIFT"), "tool {i} leaked:\n{reply}");
    }
    assert!(
        replies[0].contains("STORYPROSE"),
        "the story still comes back:\n{}",
        replies[0]
    );
}

#[tokio::test]
async fn shift_ledger_rows_never_reach_mcp_or_hook_outputs() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    let query_vec = [1.0_f32, 0.0, 0.0, 0.0];
    let story_vec = vec![0.85_f32, (1.0 - 0.85 * 0.85_f32).sqrt(), 0.0, 0.0];
    let ledger_vec = vec![0.99_f32, (1.0 - 0.99 * 0.99_f32).sqrt(), 0.0, 0.0];

    let story_text =
        "MEMORY-ARCHIVE quarantine-shift-canary story prose about quarantine-shift-canary";
    let ledger_text =
        "MEMORY-ARCHIVE quarantine-shift-canary [shift: model-derived] LEAKED_SHIFT_CLAIM";

    storage
        .insert_reflection(
            "story-clean",
            story_text,
            &[
                "session_story".to_string(),
                "project_csr".to_string(),
                "conv_q1".to_string(),
                "session_q1".to_string(),
            ],
            &story_vec,
        )
        .unwrap();
    engine.insert_reflection("story-clean".to_string(), story_vec);

    storage
        .insert_reflection(
            "ledger-secret",
            ledger_text,
            &[
                "narrative_shift".to_string(),
                "shift_project:csr".to_string(),
                "shift_conv:q1".to_string(),
                "shift_rev_1".to_string(),
                "shift_kind_risk".to_string(),
                "shift_sig_0123456789abcdef".to_string(),
                "shift_grounded".to_string(),
                "project_csr".to_string(),
                "conv_q1".to_string(),
                "session_q1".to_string(),
            ],
            &ledger_vec,
        )
        .unwrap();
    engine.insert_reflection("ledger-secret".to_string(), ledger_vec);

    let search = Arc::new(RwLock::new(engine));

    for mode in [SearchMode::Vector, SearchMode::Fts, SearchMode::Hybrid] {
        let out = reflect_on_past_with_vec_mode(
            &storage,
            &search,
            &query_vec,
            "MEMORY-ARCHIVE quarantine-shift-canary",
            5,
            0.0,
            Some("csr"),
            0,
            false,
            ConsumptionMode::Off,
            false,
            "other",
            mode,
            RecallRerankMode::Baseline,
            false,
        )
        .await
        .unwrap();
        assert!(
            out.contains("story prose"),
            "expected story-clean in {mode:?} output:\n{out}"
        );
        assert!(
            !out.contains("LEAKED_SHIFT_CLAIM"),
            "ledger row leaked in {mode:?} output:\n{out}"
        );
    }

    let kw_only = reflect_keyword_only(
        &storage,
        &search,
        "MEMORY-ARCHIVE quarantine-shift-canary",
        5,
        Some("csr"),
    )
    .await
    .unwrap();
    assert!(kw_only.contains("story prose"), "{kw_only}");
    assert!(
        !kw_only.contains("LEAKED_SHIFT_CLAIM"),
        "ledger row leaked in reflect_keyword_only:\n{kw_only}"
    );

    let by_conv = lookup_by_conv_tag(
        &storage,
        "q1",
        "quarantine-shift-canary",
        10,
        ConsumptionMode::Off,
        false,
    )
    .unwrap()
    .expect("conv_q1 has story");
    assert!(by_conv.contains("story prose"), "{by_conv}");
    assert!(
        !by_conv.contains("LEAKED_SHIFT_CLAIM"),
        "ledger row leaked in lookup_by_conv_tag:\n{by_conv}"
    );

    let learnings = get_session_learnings(&storage, "q1", 10).unwrap();
    assert!(learnings.contains("story prose"), "{learnings}");
    assert!(
        !learnings.contains("LEAKED_SHIFT_CLAIM"),
        "ledger row leaked in get_session_learnings:\n{learnings}"
    );

    // Hook / anti-pattern / csr_why HNSW reflection lookup gate:
    let raw_hits = search.read().await.search_reflections(&query_vec, 10, 0.0);
    assert!(raw_hits.iter().any(|r| r.id == "ledger-secret"));
    let hydrated: Vec<String> = raw_hits
        .iter()
        .filter_map(|r| storage.get_reflection_by_id(&r.id).ok().flatten())
        .map(|(content, _, _)| content)
        .collect();
    assert_eq!(hydrated, vec![story_text.to_string()]);
}

// Vector mode already multiplies an out-of-family score by 0.3, so the
// story must beat the chunk after that multiplier for the sink to decide.
#[tokio::test]
async fn explicit_scope_sinks_weak_out_of_family_story_below_in_family_hits() {
    // story 0.40 * 0.3 = 0.12 beats chunk 0.10 without the sink.
    assert_eq!(
        first_cid_for_scope("sinkscope-home", 0.10, 0.40).await,
        "conv-in-family"
    );
}

#[tokio::test]
async fn explicit_scope_keeps_a_strong_out_of_family_story_in_place() {
    // story 0.95 clears QUICK_CHECK_FLOOR; 0.95 * 0.3 = 0.285 beats 0.20.
    assert_ne!(
        first_cid_for_scope("sinkscope-home", 0.20, 0.95).await,
        "conv-in-family"
    );
}

#[tokio::test]
async fn bare_family_tag_keeps_a_memory_with_a_topical_project_tag_in_family() {
    // Live shape of e97fa3b4: a bare project tag plus a topical project_
    // tag that resolves the row's project elsewhere. It must not sink.
    assert_ne!(
        first_cid_for_scope_tagged(
            "sinkscope-home",
            0.10,
            0.40,
            &["sinkscope-home", "project_sinkscope-topic"],
        )
        .await,
        "conv-in-family"
    );
}

#[tokio::test]
async fn explicit_scope_keeps_a_keyword_matched_out_of_family_story_in_place() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    let query = [1.0_f32, 0.0, 0.0, 0.0];
    let chunk_vec = vec![0.10_f32, (1.0_f32 - 0.01).sqrt(), 0.0, 0.0];
    let chunk = crate::import::ConversationChunk {
        id: "in-family-chunk".into(),
        conversation_id: "conv-in-family".into(),
        project_name: "sinkscope-home".into(),
        timestamp: "2026-09-20T00:00:00Z".into(),
        content: "unrelated deploy notes".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::Assistant,
        seq: 0,
        is_sidechain: false,
    };
    storage.insert_chunk(&chunk, &chunk_vec).unwrap();
    engine.insert_chunk(chunk.id.clone(), chunk_vec);
    // Weak semantically (0.40, under the floor) but its text carries the
    // query's own terms, so the keyword channel finds it.
    let story_vec = vec![0.40_f32, (1.0_f32 - 0.16).sqrt(), 0.0, 0.0];
    storage
        .insert_reflection(
            "story_sibling",
            "morning mantras pronunciation comment on the reel",
            &[
                "session_story".to_string(),
                "project_sinkscope-away".to_string(),
            ],
            &story_vec,
        )
        .unwrap();
    engine.insert_reflection("story_sibling".to_string(), story_vec);
    let search = Arc::new(RwLock::new(engine));
    let out = reflect_on_past_with_vec_mode(
        &storage,
        &search,
        &query,
        "morning mantras pronunciation",
        2,
        0.0,
        Some("sinkscope-home"),
        0,
        false,
        ConsumptionMode::Off,
        false,
        "other",
        SearchMode::Hybrid,
        RecallRerankMode::Baseline,
        false,
    )
    .await
    .unwrap();
    let start = out.find("<cid>").expect("at least one result") + "<cid>".len();
    let end = out[start..].find("</cid>").unwrap() + start;
    assert_ne!(
        &out[start..end],
        "conv-in-family",
        "a keyword-matched sibling-project story must keep its fused rank:\n{out}"
    );
}

#[tokio::test]
async fn unscoped_search_keeps_the_better_out_of_family_story_first() {
    assert_ne!(
        first_cid_for_scope("all", 0.10, 0.40).await,
        "conv-in-family"
    );
}

#[tokio::test]
async fn exact_explicit_memory_outranks_boosted_user_chunks() {
    assert_eq!(
        memory_vs_user_chunks_first_hit(&["anukriti", "memory-archive"]).await,
        "5eb5aebb-65ab-4fba-9917-86cb83845154"
    );
    assert_eq!(
        memory_vs_user_chunks_first_hit(&["free-form", EXPLICIT_MEMORY_TAG]).await,
        "5eb5aebb-65ab-4fba-9917-86cb83845154"
    );
}

#[tokio::test]
async fn derived_story_keeps_no_standing_against_user_chunks() {
    // Policy boundary: a pipeline story at cosine 1.0 still yields to
    // user-authored history, exactly as before this fix.
    // `<cid>` renders the conversation id: `conv-N` for the user chunks,
    // the reflection's own id for the memory.
    let first = memory_vs_user_chunks_first_hit(&["session_story", "conv_x"]).await;
    assert!(first.starts_with("conv-"), "{first}");
}

/// Live 2026-09-22 defect (project EnhanceMe, query "codex blocked
/// revival..."): a legacy uuid-shaped reflection at raw cosine ~0.12 to
/// the query was still granted UserHistory standing, so `+0.50` lifted it
/// (adj 0.616) above the actually relevant chunk (cosine 0.549, no
/// boost). `EXPLICIT_MEMORY_STANDING_FLOOR` must gate the boost off for a
/// memory this weakly related, so the relevant chunk wins the page.
#[tokio::test]
async fn weak_similarity_memory_does_not_outrank_relevant_chunk() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    let query = [1.0_f32, 0.0, 0.0, 0.0];

    // Relevant chunk: cosine 0.55 to the query, no standing of its own.
    let relevant_cosine = 0.55_f32;
    let relevant_vector = vec![
        relevant_cosine,
        (1.0 - relevant_cosine * relevant_cosine).sqrt(),
        0.0,
        0.0,
    ];
    let chunk = crate::import::ConversationChunk {
        id: "relevant-chunk".into(),
        conversation_id: "conv-relevant".into(),
        project_name: "test".into(),
        timestamp: "2026-09-20T00:00:00Z".into(),
        content: "the codex revival is blocked on the iOS deploy step".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::Assistant,
        seq: 0,
        is_sidechain: false,
    };
    storage.insert_chunk(&chunk, &relevant_vector).unwrap();
    engine.insert_chunk(chunk.id.clone(), relevant_vector);

    // Legacy uuid-shaped memory: cosine 0.12, well under the floor.
    let weak_cosine = 0.12_f32;
    let weak_vector = vec![
        weak_cosine,
        (1.0 - weak_cosine * weak_cosine).sqrt(),
        0.0,
        0.0,
    ];
    let memory_id = "ca488584-0000-4000-8000-000000000001";
    storage
        .insert_reflection(
            memory_id,
            "an unrelated anukriti campaign note",
            &["anukriti".to_string()],
            &weak_vector,
        )
        .unwrap();
    engine.insert_reflection(memory_id.to_string(), weak_vector);

    let search = Arc::new(RwLock::new(engine));
    let out = reflect_on_past_with_vec_mode(
        &storage,
        &search,
        &query,
        "codex blocked revival 2026 iOS deploy blocker where stuck",
        2,
        0.0,
        Some("all"),
        0,
        false,
        ConsumptionMode::Off,
        false,
        "other",
        SearchMode::Vector,
        RecallRerankMode::Baseline,
        false,
    )
    .await
    .unwrap();
    let start = out.find("<cid>").expect("at least one result") + "<cid>".len();
    let end = out[start..].find("</cid>").unwrap() + start;
    assert_eq!(
        &out[start..end],
        "conv-relevant",
        "a cosine-0.12 memory must not outrank a cosine-0.55 relevant chunk:\n{out}"
    );
}

/// Opus review finding (medium, W3-K): `EXPLICIT_MEMORY_STANDING_FLOOR`
/// gated the boost only on the explicit-memory branch of
/// `reflection_recall_standing`. A derived reflection (no
/// `csr:memory` stamp, not uuid-shaped) whose CACHED provenance floor is
/// already `UserHistory` skipped that gate entirely and still got the
/// flat `W_USER` boost at any cosine — live receipt: `extracted_v3_*`
/// rows scored `cos=0.535 user=+0.50 total=1.035`, the same shape as the
/// `ca488584` bug this item was opened for. Same fixture as
/// `weak_similarity_memory_does_not_outrank_relevant_chunk`, but the
/// weak memory is a derived row with a pre-cached `UserHistory` floor
/// instead of an explicit `store_reflection` write.
#[tokio::test]
async fn weak_similarity_cached_user_history_reflection_does_not_outrank_relevant_chunk() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    let query = [1.0_f32, 0.0, 0.0, 0.0];

    let relevant_cosine = 0.55_f32;
    let relevant_vector = vec![
        relevant_cosine,
        (1.0 - relevant_cosine * relevant_cosine).sqrt(),
        0.0,
        0.0,
    ];
    let chunk = crate::import::ConversationChunk {
        id: "relevant-chunk".into(),
        conversation_id: "conv-relevant".into(),
        project_name: "test".into(),
        timestamp: "2026-09-20T00:00:00Z".into(),
        content: "the codex revival is blocked on the iOS deploy step".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::Assistant,
        seq: 0,
        is_sidechain: false,
    };
    storage.insert_chunk(&chunk, &relevant_vector).unwrap();
    engine.insert_chunk(chunk.id.clone(), relevant_vector);

    // Derived reflection: no EXPLICIT_MEMORY_TAG, non-uuid id (so
    // `is_explicit_memory` is false), cosine 0.12 (well under the
    // floor), with a cached provenance floor already UserHistory —
    // the shape `insert_derived_reflection` produces for a row whose
    // support set includes a user message.
    let weak_cosine = 0.12_f32;
    let weak_vector = vec![
        weak_cosine,
        (1.0 - weak_cosine * weak_cosine).sqrt(),
        0.0,
        0.0,
    ];
    let derived_id = "extracted_v3_unrelated-conv";
    storage
        .insert_reflection(
            derived_id,
            "an unrelated extracted narrative",
            &["narrative_extracted_v3".to_string()],
            &weak_vector,
        )
        .unwrap();
    storage
        .with_connection(|conn| {
            conn.execute(
                "UPDATE reflections SET min_trust = ?1 WHERE id = ?2",
                rusqlite::params![
                    crate::provenance::TrustTier::UserHistory.as_i64(),
                    derived_id
                ],
            )?;
            Ok(())
        })
        .unwrap();
    engine.insert_reflection(derived_id.to_string(), weak_vector);

    let search = Arc::new(RwLock::new(engine));
    let out = reflect_on_past_with_vec_mode(
        &storage,
        &search,
        &query,
        "codex blocked revival 2026 iOS deploy blocker where stuck",
        2,
        0.0,
        Some("all"),
        0,
        false,
        ConsumptionMode::Off,
        false,
        "other",
        SearchMode::Vector,
        RecallRerankMode::Baseline,
        false,
    )
    .await
    .unwrap();
    let start = out.find("<cid>").expect("at least one result") + "<cid>".len();
    let end = out[start..].find("</cid>").unwrap() + start;
    assert_eq!(
        &out[start..end],
        "conv-relevant",
        "a cosine-0.12 derived reflection with a cached UserHistory floor must not outrank \
             a cosine-0.55 relevant chunk:\n{out}"
    );
}

#[tokio::test]
async fn bare_project_tag_keeps_memory_in_family_under_a_scope() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    let query = [1.0_f32, 0.0, 0.0, 0.0];
    let chunk = crate::import::ConversationChunk {
        id: "anchor".into(),
        conversation_id: "conv-anchor".into(),
        project_name: "meta-campaigns".into(),
        timestamp: "2026-09-20T00:00:00Z".into(),
        content: "campaign roster baseline".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::Assistant,
        seq: 0,
        is_sidechain: false,
    };
    storage.insert_chunk(&chunk, &[0.6, 0.8, 0.0, 0.0]).unwrap();
    engine.insert_chunk(chunk.id.clone(), vec![0.6, 0.8, 0.0, 0.0]);
    storage
        .insert_reflection(
            "memory-in-family",
            "MEMORY-ARCHIVE campaign roster",
            &["meta-campaigns".into(), EXPLICIT_MEMORY_TAG.into()],
            &query,
        )
        .unwrap();
    engine.insert_reflection("memory-in-family".into(), query.to_vec());
    let search = Arc::new(RwLock::new(engine));
    let out = reflect_on_past_with_vec_mode(
        &storage,
        &search,
        &query,
        "campaign roster memory",
        2,
        0.0,
        Some("meta-campaigns"),
        0,
        false,
        ConsumptionMode::Off,
        false,
        "other",
        SearchMode::Vector,
        RecallRerankMode::Baseline,
        false,
    )
    .await
    .unwrap();
    let start = out
        .find("<cid>memory-in-family</cid>")
        .expect("memory on the page");
    let block_start = out[..start].rfind("<r rank=").expect("result block");
    let block = &out[block_start..start];
    let score: f32 = block
        .split("<s")
        .nth(1)
        .and_then(|s| s.split('>').nth(1))
        .and_then(|s| s.split("</s").next())
        .and_then(|s| s.trim().parse().ok())
        .expect("score");
    assert!(
        block.contains("<p>meta-campaigns</p>"),
        "bare project tag must resolve to the scoped project:\n{out}"
    );
    assert!(
        score > 0.9,
        "no cross-project penalty expected, got {score}:\n{out}"
    );
}

/// Spec (b): under an explicit project filter, reflections and stories
/// outside the family — including ones that resolve to "unknown" because
/// no tag names a project — are NOT dropped (6cf8a2b: a hard drop lost a
/// real sibling-project answer); they must rank after every in-family row
/// (`sink_out_of_family_under_explicit_scope` + the 0.3x cross-project
/// multiplier). Live 2026-09-22 defect (project EnhanceMe): an
/// unknown-project memory and an anukriti story both out-ranked in-family
/// results. A bare project tag that matches the scope still keeps the row
/// in the family.
#[tokio::test]
async fn explicit_project_scope_sinks_out_of_family_and_unknown_reflections() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    let query = [1.0_f32, 0.0, 0.0, 0.0];

    // Out-of-family story: tagged to a different project, cosine 1.0.
    storage
        .insert_reflection(
            "story-out-of-family",
            "MEMORY-ARCHIVE anukriti campaign story",
            &["session_story".into(), "project_anukriti".into()],
            &query,
        )
        .unwrap();
    engine.insert_reflection("story-out-of-family".into(), query.to_vec());

    // Unknown-project explicit memory: no project tag at all, cosine 1.0.
    storage
        .insert_reflection(
            "memory-unknown-project",
            "MEMORY-ARCHIVE some note with no project tag",
            &["free-form".into(), EXPLICIT_MEMORY_TAG.into()],
            &query,
        )
        .unwrap();
    engine.insert_reflection("memory-unknown-project".into(), query.to_vec());

    // In-family explicit memory via a bare project tag, cosine 1.0.
    storage
        .insert_reflection(
            "memory-in-family",
            "MEMORY-ARCHIVE EnhanceMe roster note",
            &["EnhanceMe".into(), EXPLICIT_MEMORY_TAG.into()],
            &query,
        )
        .unwrap();
    engine.insert_reflection("memory-in-family".into(), query.to_vec());

    let search = Arc::new(RwLock::new(engine));
    let out = reflect_on_past_with_vec_mode(
        &storage,
        &search,
        &query,
        "roster memory",
        10,
        0.0,
        Some("EnhanceMe"),
        0,
        false,
        ConsumptionMode::Off,
        false,
        "other",
        SearchMode::Vector,
        RecallRerankMode::Baseline,
        false,
    )
    .await
    .unwrap();

    assert!(
        out.contains("<cid>memory-in-family</cid>"),
        "an in-family bare-tag memory must stay on the page:\n{out}"
    );
    let pos = |cid: &str| {
        out.find(&format!("<cid>{cid}</cid>"))
            .unwrap_or_else(|| panic!("{cid} must be present (sunk, not dropped):\n{out}"))
    };
    let in_family = pos("memory-in-family");
    assert!(
        pos("story-out-of-family") > in_family,
        "an out-of-family story must rank after every in-family row:\n{out}"
    );
    assert!(
        pos("memory-unknown-project") > in_family,
        "an unknown-project memory must rank after every in-family row:\n{out}"
    );
}

/// Opus review finding 1 (W3-K, high): a memory-file reflection carries a
/// `project_*` tag that does NOT itself resolve to the scope's family
/// (`project_meta_campaigns_setup` vs scope `anukriti-meta-campaigns`)
/// alongside the bare family tag every `MEMORY-ARCHIVE` writer also
/// stamps (`anukriti-meta-campaigns`). `reflection_in_family` must
/// accept the row on ANY tag clearing `scope.is_family`, not just the
/// first `project_*` tag found — before this fix it read only that first
/// tag, resolved to `meta_campaigns_setup`, failed `is_family`, and
/// dropped the row outright even though it plainly belongs to the
/// family. Live receipt: `e97fa3b4`/`5eb5aebb`/`22ffc13b` all carry this
/// exact tag shape and vanished from `project=anukriti-meta-campaigns`
/// scoped search.
#[tokio::test]
async fn family_bare_tag_keeps_memory_file_reflection_in_scope() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(4);
    let query = [1.0_f32, 0.0, 0.0, 0.0];
    let id = "e97fa3b4-0000-4000-8000-000000000000";
    let tags = vec![
        "project_meta_campaigns_setup".to_string(),
        "anukriti-meta-campaigns".to_string(),
        EXPLICIT_MEMORY_TAG.to_string(),
    ];
    storage
        .insert_reflection(
            id,
            "MEMORY-ARCHIVE anukriti-meta-campaigns paying customers may22",
            &tags,
            &query,
        )
        .unwrap();
    engine.insert_reflection(id.to_string(), query.to_vec());
    let search = Arc::new(RwLock::new(engine));
    let out = reflect_on_past_with_vec_mode(
        &storage,
        &search,
        &query,
        "paying customers memory",
        5,
        0.0,
        Some("anukriti-meta-campaigns"),
        0,
        false,
        ConsumptionMode::Off,
        false,
        "other",
        SearchMode::Vector,
        RecallRerankMode::Baseline,
        false,
    )
    .await
    .unwrap();
    assert!(
        out.contains(&format!("<cid>{id}</cid>")),
        "a memory-file reflection with a family bare tag must stay in scope even when its \
             project_* tag alone would not resolve to the family:\n{out}"
    );
}

#[tokio::test]
async fn keyword_channel_reaches_reflections_in_hybrid_mode() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    let query = [1.0_f32, 0.0, 0.0, 0.0];
    let chunk = crate::import::ConversationChunk {
        id: "semantic-only".into(),
        conversation_id: "conv-s".into(),
        project_name: "test".into(),
        timestamp: "2026-09-20T00:00:00Z".into(),
        content: "unrelated words entirely".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::Assistant,
        seq: 0,
        is_sidechain: false,
    };
    storage.insert_chunk(&chunk, &query).unwrap();
    engine.insert_chunk(chunk.id.clone(), query.to_vec());
    // Memory whose vector is orthogonal to the query: only the keyword
    // channel can reach it.
    let memory_id = "memory-keyword-only";
    let orthogonal = vec![0.0_f32, 0.0, 0.0, 1.0];
    storage
        .insert_reflection(
            memory_id,
            "MEMORY-ARCHIVE follower growth archived",
            &[EXPLICIT_MEMORY_TAG.to_string()],
            &orthogonal,
        )
        .unwrap();
    engine.insert_reflection(memory_id.to_string(), orthogonal);
    let search = Arc::new(RwLock::new(engine));
    let run = |mode: SearchMode| {
        let storage = Arc::clone(&storage);
        let search = Arc::clone(&search);
        async move {
            reflect_on_past_with_vec_mode(
                &storage,
                &search,
                &query,
                "MEMORY-ARCHIVE follower growth",
                5,
                0.1,
                Some("all"),
                0,
                false,
                ConsumptionMode::Off,
                false,
                "other",
                mode,
                RecallRerankMode::Baseline,
                false,
            )
            .await
            .unwrap()
        }
    };
    let hybrid = run(SearchMode::Hybrid).await;
    assert!(
        hybrid.contains("<cid>memory-keyword-only</cid>"),
        "{hybrid}"
    );
    assert!(
        hybrid.contains("[keyword] [reflection] MEMORY-ARCHIVE"),
        "keyword-only reflections must carry the keyword marker:\n{hybrid}"
    );
    let vector = run(SearchMode::Vector).await;
    assert!(
        !vector.contains("<cid>memory-keyword-only</cid>"),
        "{vector}"
    );
}

/// Spec (e): an exact-prefix reflection must reach page 1 of the keyword
/// channel even when a common term in the query also appears in many
/// chunks. Live 2026-09-22: `reflections_fts` itself ranked 3
/// `MEMORY-ARCHIVE anukriti-meta-campaigns` memories at bm25 8/9/10 of
/// 267 matches (the OR-of-terms query rewards any row mentioning
/// "anukriti"), and the combined chunk+reflection bm25 merge buried them
/// under chunk hits entirely — none of the 3 made the final page.
#[tokio::test]
async fn exact_prefix_reflection_reaches_page_one_over_many_keyword_matching_chunks() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(256);
    let query_vec = [1.0_f32, 0.0, 0.0, 0.0];
    // Chunks sit at cosine 0.4 — weakly relevant, but above min_score, so
    // they count on both the semantic and keyword channels (the real
    // shape: generic background chatter that happens to mention the
    // query terms). The memories are cosine 0.0 (excluded from the
    // semantic channel entirely by `min_score`) — only the keyword
    // channel, via the exact-prefix match, can reach them.
    let chunk_vector = vec![0.4_f32, (1.0_f32 - 0.4 * 0.4).sqrt(), 0.0, 0.0];
    let orthogonal = vec![0.0_f32, 1.0, 0.0, 0.0];
    let min_score = 0.3_f32;

    // 200 chunks that all match the OR-of-terms query (mention "x-y-z").
    for i in 0..200 {
        let chunk = crate::import::ConversationChunk {
            id: format!("chunk-{i}"),
            conversation_id: format!("conv-chunk-{i}"),
            project_name: "test".into(),
            timestamp: "2026-09-20T00:00:00Z".into(),
            content: format!("background note about project-x-y-z number {i}"),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::Assistant,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &chunk_vector).unwrap();
        engine.insert_chunk(chunk.id, chunk_vector.clone());
    }

    // 3 memories whose content starts with the query text verbatim.
    let mut memory_ids = Vec::new();
    for i in 0..3 {
        let id = format!("memory-prefix-{i}");
        storage
            .insert_reflection(
                &id,
                &format!("MEMORY-ARCHIVE project-x-y-z memory number {i}"),
                &[EXPLICIT_MEMORY_TAG.to_string()],
                &orthogonal,
            )
            .unwrap();
        engine.insert_reflection(id.clone(), orthogonal.clone());
        memory_ids.push(id);
    }

    let search = Arc::new(RwLock::new(engine));
    let out = reflect_on_past_with_vec_mode(
        &storage,
        &search,
        &query_vec,
        "MEMORY-ARCHIVE project-x-y-z",
        5,
        min_score,
        Some("all"),
        0,
        false,
        ConsumptionMode::Off,
        false,
        "other",
        SearchMode::Hybrid,
        RecallRerankMode::Baseline,
        false,
    )
    .await
    .unwrap();

    for id in &memory_ids {
        assert!(
            out.contains(&format!("<cid>{id}</cid>")),
            "exact-prefix memory {id} must reach page 1 over 200 OR-matching chunks:\n{out}"
        );
    }
}
