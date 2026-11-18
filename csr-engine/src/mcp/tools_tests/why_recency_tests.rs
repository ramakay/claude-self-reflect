use super::*;

#[tokio::test]
async fn why_rendered_receipt_abstains_from_weak_episode_target() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
    let query = "why weak episode evidence must abstain";
    let query_vec = embeddings.embed_single(query).unwrap();
    let mut orthogonal = vec![0.0; query_vec.len()];
    orthogonal[0] = -query_vec[1];
    orthogonal[1] = query_vec[0];
    let orthogonal_norm = orthogonal.iter().map(|v| v * v).sum::<f32>().sqrt();
    for value in &mut orthogonal {
        *value /= orthogonal_norm;
    }
    let weak_vec: Vec<f32> = query_vec
        .iter()
        .zip(orthogonal)
        .map(|(query_value, orthogonal_value)| {
            0.10 * query_value + (1.0_f32 - 0.10_f32.powi(2)).sqrt() * orthogonal_value
        })
        .collect();
    let chunks = [
        (
            "episode-weak-seed",
            "episode-weak-seed-conv",
            query_vec.clone(),
        ),
        ("episode-weak-target", "episode-weak-target-conv", weak_vec),
    ];
    let mut engine = SearchEngine::new(query_vec.len());
    for (seq, (id, conv, vector)) in chunks.into_iter().enumerate() {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conv.into(),
            project_name: "claude-self-reflect".into(),
            timestamp: format!("2026-08-0{}T00:00:00Z", seq + 1),
            content: format!("episode evidence from {conv}"),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &vector).unwrap();
        engine.insert_chunk(chunk.id, vector);
    }
    storage
        .insert_reflection(
            "episode-weak-reflection",
            &serde_json::json!({ "prev_episode_id": "episode-weak-target-conv" }).to_string(),
            &[
                "session_episode".to_string(),
                "conv_episode-weak-seed-conv".to_string(),
            ],
            &[],
        )
        .unwrap();

    let rendered = why(
        &storage,
        &embeddings,
        &Arc::new(RwLock::new(engine)),
        query,
        Some("claude-self-reflect"),
        &crate::search::reinstatement::ReinstateConfig::default(),
    )
    .await
    .unwrap();

    assert!(
        !rendered.contains("conv_episode-weak-target-conv:"),
        "a below-floor episode must abstain rather than surface:\n{rendered}"
    );
    assert!(
        rendered.contains("episodes links=1 resolved=1 accepted=0 surfaced=0"),
        "resolution and acceptance must be separate stages:\n{rendered}"
    );
    assert!(rendered.contains("below-threshold=1"), "{rendered}");
}

#[tokio::test]
async fn why_rendered_receipt_appends_distilled_into_memory_line_on_origin_session_match() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
    let query = "why memory provenance hop surfaces distilled memory";
    let query_vec = embeddings.embed_single(query).unwrap();
    let conversation_id = "why-mem-conv";
    let chunk = crate::import::ConversationChunk {
        id: "why-mem-chunk".into(),
        conversation_id: conversation_id.into(),
        project_name: "claude-self-reflect".into(),
        timestamp: "2026-08-19T00:00:00Z".into(),
        content: "memory provenance hop surfaces distilled memory".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::User,
        seq: 0,
        is_sidechain: false,
    };
    let mut engine = SearchEngine::new(query_vec.len());
    storage.insert_chunk(&chunk, &query_vec).unwrap();
    engine.insert_chunk(chunk.id.clone(), query_vec.clone());

    storage
        .with_transaction(|tx| {
            crate::storage::queries::upsert_memory_registry_batch(
                tx,
                &[crate::storage::queries::MemoryRegistryRow {
                    file_path: "/mem/why-hop-memory.md".into(),
                    project: "proj-why-mem".into(),
                    slug: "why-hop-memory".into(),
                    description: Some("a description with\na newline and stuff".into()),
                    mem_type: Some("reference".into()),
                    origin_session_id: Some(conversation_id.into()),
                    modified_ts: Some("2026-08-19T00:00:00Z".into()),
                    file_mtime: 1_700_000_000,
                    content_hash: "hash-why-hop".into(),
                    links_json: "[]".into(),
                    last_seen_scan: 1,
                }],
            )?;
            Ok(())
        })
        .unwrap();

    let rendered = why(
        &storage,
        &embeddings,
        &Arc::new(RwLock::new(engine)),
        query,
        Some("claude-self-reflect"),
        &crate::search::reinstatement::ReinstateConfig::default(),
    )
    .await
    .unwrap();

    let expected = "distilled into memory: why-hop-memory (reference, proj-why-mem) — a description with a newline and stuff";
    assert!(
        rendered.contains(expected),
        "matching origin_session_id must append a sanitized memory line:\n{rendered}"
    );
    assert!(
        rendered.find("distilled into memory").unwrap() > rendered.find("WHY:").unwrap(),
        "memory line must appear after the WHY header:\n{rendered}"
    );
}

#[tokio::test]
async fn why_rendered_receipt_omits_memory_line_when_no_origin_session_matches() {
    let storage_empty = Arc::new(Storage::open_memory().unwrap());
    let storage_unrelated = Arc::new(Storage::open_memory().unwrap());
    let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
    let query = "why memory provenance hop stays silent without a match";
    let query_vec = embeddings.embed_single(query).unwrap();
    let conversation_id = "why-mem-nomatch-conv";
    let chunk = crate::import::ConversationChunk {
        id: "why-mem-nomatch-chunk".into(),
        conversation_id: conversation_id.into(),
        project_name: "claude-self-reflect".into(),
        timestamp: "2026-08-19T00:00:00Z".into(),
        content: "memory provenance hop stays silent without a match".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::User,
        seq: 0,
        is_sidechain: false,
    };

    let mut engine_empty = SearchEngine::new(query_vec.len());
    storage_empty.insert_chunk(&chunk, &query_vec).unwrap();
    engine_empty.insert_chunk(chunk.id.clone(), query_vec.clone());

    let mut engine_unrelated = SearchEngine::new(query_vec.len());
    storage_unrelated.insert_chunk(&chunk, &query_vec).unwrap();
    engine_unrelated.insert_chunk(chunk.id.clone(), query_vec.clone());
    storage_unrelated
        .with_transaction(|tx| {
            crate::storage::queries::upsert_memory_registry_batch(
                tx,
                &[crate::storage::queries::MemoryRegistryRow {
                    file_path: "/mem/unrelated.md".into(),
                    project: "proj-why-mem".into(),
                    slug: "unrelated-memory".into(),
                    description: Some("should not appear".into()),
                    mem_type: Some("reference".into()),
                    origin_session_id: Some("unrelated-session-id".into()),
                    modified_ts: Some("2026-08-19T00:00:00Z".into()),
                    file_mtime: 1_700_000_000,
                    content_hash: "hash-unrelated".into(),
                    links_json: "[]".into(),
                    last_seen_scan: 1,
                }],
            )?;
            Ok(())
        })
        .unwrap();

    let cfg = crate::search::reinstatement::ReinstateConfig::default();
    let rendered_empty = why(
        &storage_empty,
        &embeddings,
        &Arc::new(RwLock::new(engine_empty)),
        query,
        Some("claude-self-reflect"),
        &cfg,
    )
    .await
    .unwrap();
    let rendered_unrelated = why(
        &storage_unrelated,
        &embeddings,
        &Arc::new(RwLock::new(engine_unrelated)),
        query,
        Some("claude-self-reflect"),
        &cfg,
    )
    .await
    .unwrap();

    assert!(
        !rendered_empty.contains("distilled into memory"),
        "zero memory_registry rows must leave csr_why output without a memory line:\n{rendered_empty}"
    );
    assert!(
        !rendered_unrelated.contains("distilled into memory"),
        "non-matching origin_session_id must leave csr_why output without a memory line:\n{rendered_unrelated}"
    );
    assert_eq!(
        rendered_empty, rendered_unrelated,
        "non-matching registry rows must keep output byte-identical to the empty-registry case"
    );
}

/// The chain that a pairwise near-tie comparator gets wrong. Scores 0.90 /
/// 0.86 / 0.82 on one anchor: each adjacent pair is within epsilon but the
/// ends are 0.08 apart, so a pairwise predicate says A>B, B>C and C>A — a
/// cycle, which is not a valid total order and can make sort_by panic.
/// Every input permutation must produce the SAME output and must not panic.
#[test]
fn why_recency_tiebreak_is_a_total_order_on_an_epsilon_chain() {
    let a = why_item("a", "conv-a", 0.90, "2026-01-01T00:00:00Z");
    let b = why_item("b", "conv-b", 0.86, "2026-01-02T00:00:00Z");
    let c = why_item("c", "conv-c", 0.82, "2026-01-03T00:00:00Z");
    let mut anchors = HashMap::new();
    for id in ["a", "b", "c"] {
        anchors.insert(id.to_string(), ("proj".to_string(), "F.md".to_string()));
    }

    let perms = [
        vec![a.clone(), b.clone(), c.clone()],
        vec![a.clone(), c.clone(), b.clone()],
        vec![b.clone(), a.clone(), c.clone()],
        vec![b.clone(), c.clone(), a.clone()],
        vec![c.clone(), a.clone(), b.clone()],
        vec![c.clone(), b.clone(), a.clone()],
    ];

    let mut orders = perms
        .into_iter()
        .map(|p| {
            apply_why_recency_tiebreak(p, &anchors)
                .items
                .iter()
                .map(|i| i.chunk_id.clone())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    orders.dedup();

    assert_eq!(
        orders.len(),
        1,
        "every permutation must yield one stable order, got {orders:?}"
    );
}

/// The tie-break is worthless if the renderer re-sorts it away. It used to:
/// each conversation group was re-sorted by ASCENDING timestamp, so the
/// oldest — i.e. the superseded claim — rendered above its own correction,
/// and every unit test still passed because they only exercised the pure
/// ranking function. Assert on the rendered string instead.
#[test]
fn format_why_renders_in_ranked_order_not_chronological() {
    let older_wrong = why_item("wrong", "conv-x", 0.840, "2026-01-01T00:00:00Z");
    let newer_fixed = why_item("fixed", "conv-x", 0.810, "2026-01-02T00:00:00Z");
    let mut anchors = HashMap::new();
    for id in ["wrong", "fixed"] {
        anchors.insert(
            id.to_string(),
            ("proj".to_string(), "CLAUDE.md".to_string()),
        );
    }
    let ranking = apply_why_recency_tiebreak(vec![older_wrong, newer_fixed], &anchors);
    assert_eq!(
        ranking.items[0].chunk_id, "fixed",
        "precondition: ranking puts the correction first"
    );

    let rendered = format_why(
        "why",
        &ranking.items,
        &ranking.hoisted_chunk_ids,
        &crate::search::reinstatement::ReinstateTrace::default(),
    );
    let newer_at = rendered.find("0.810").expect("newer item must render");
    let older_at = rendered.find("0.840").expect("older item must render");
    assert!(
        newer_at < older_at,
        "renderer must preserve ranked order; got:\n{rendered}"
    );
}

#[test]
fn why_recency_tiebreak_prefers_newer_within_epsilon_same_anchor() {
    let older_wrong = why_item("wrong", "conv-x", 0.715, "2026-01-01T00:00:00Z");
    let newer_fixed = why_item("fixed", "conv-x", 0.696, "2026-01-02T00:00:00Z"); // 0.019 gap
    let mut anchors = HashMap::new();
    anchors.insert(
        "wrong".to_string(),
        ("proj".to_string(), "CLAUDE.md".to_string()),
    );
    anchors.insert(
        "fixed".to_string(),
        ("proj".to_string(), "CLAUDE.md".to_string()),
    );

    let ranking = apply_why_recency_tiebreak(vec![older_wrong, newer_fixed], &anchors);

    assert_eq!(
        ranking.items[0].chunk_id, "fixed",
        "newer correction must rank first within epsilon on the same anchor"
    );
    let rendered = format_why(
        "why",
        &ranking.items,
        &ranking.hoisted_chunk_ids,
        &crate::search::reinstatement::ReinstateTrace::default(),
    );
    assert!(
        rendered.contains("score=0.696 [recent↑]"),
        "the lower-scored item hoisted by recency must carry a marker:\n{rendered}"
    );
    assert!(
        rendered.find("score=0.696 [recent↑]").unwrap()
            < rendered.find("score=0.715 trust=").unwrap(),
        "the newer item must render before its higher-scored sibling:\n{rendered}"
    );
    assert!(
        rendered.contains("score=0.715 trust="),
        "the higher-scored sibling must not carry a marker:\n{rendered}"
    );
}

#[test]
fn why_recency_tiebreak_leaves_order_when_scores_outside_epsilon() {
    let higher_older = why_item("higher", "conv-old", 0.90, "2026-01-01T00:00:00Z");
    let lower_newer = why_item("lower", "conv-new", 0.50, "2026-01-02T00:00:00Z"); // 0.40 gap, well outside 0.05
    let mut anchors = HashMap::new();
    anchors.insert(
        "higher".to_string(),
        ("proj".to_string(), "CLAUDE.md".to_string()),
    );
    anchors.insert(
        "lower".to_string(),
        ("proj".to_string(), "CLAUDE.md".to_string()),
    );

    let ranking = apply_why_recency_tiebreak(vec![higher_older, lower_newer], &anchors);

    assert_eq!(
        ranking.items[0].chunk_id, "higher",
        "outside epsilon must not be reordered by recency"
    );
    let rendered = format_why(
        "why",
        &ranking.items,
        &ranking.hoisted_chunk_ids,
        &crate::search::reinstatement::ReinstateTrace::default(),
    );
    assert!(
        !rendered.contains('↑'),
        "strict score ordering outside epsilon must not carry a marker:\n{rendered}"
    );
}

#[test]
fn why_recency_tiebreak_leaves_order_across_different_anchors() {
    let older_a = why_item("a-old", "conv-a-old", 0.840, "2026-01-01T00:00:00Z");
    let newer_b = why_item("b-new", "conv-b-new", 0.820, "2026-01-02T00:00:00Z"); // within 0.05 but different file
    let mut anchors = HashMap::new();
    anchors.insert(
        "a-old".to_string(),
        ("proj".to_string(), "file_a.rs".to_string()),
    );
    anchors.insert(
        "b-new".to_string(),
        ("proj".to_string(), "file_b.rs".to_string()),
    );

    let ranking = apply_why_recency_tiebreak(vec![older_a, newer_b], &anchors);

    assert_eq!(
        ranking.items[0].chunk_id, "a-old",
        "different anchors must never be reordered by recency, even within epsilon"
    );
    let rendered = format_why(
        "why",
        &ranking.items,
        &ranking.hoisted_chunk_ids,
        &crate::search::reinstatement::ReinstateTrace::default(),
    );
    assert!(
        !rendered.contains('↑'),
        "items on different anchors must not carry a marker:\n{rendered}"
    );
}

#[test]
fn exact_stem_match_wins_over_a_substring_match_in_another_project() {
    let dir = tempfile::tempdir().unwrap();
    // proj-a's session id is an EXACT match for the query; proj-b's
    // session id merely CONTAINS the query as a substring. The exact
    // match must win regardless of filesystem enumeration order.
    write_session_file(dir.path(), "proj-a", "abc123");
    write_session_file(dir.path(), "proj-b", "abc123-extra-suffix");

    match find_conversation_file(dir.path(), "abc123", None) {
        ConversationLookup::Found(path, project) => {
            assert_eq!(project, "proj-a");
            assert!(path.to_string_lossy().contains("proj-a"));
        }
        ConversationLookup::NotFound => panic!("expected a match"),
        ConversationLookup::Ambiguous(candidates) => {
            panic!("exact match must win outright, not report ambiguous: {candidates:?}")
        }
    }
}

#[test]
fn ambiguous_substring_match_reports_every_candidate_instead_of_picking_one() {
    let dir = tempfile::tempdir().unwrap();
    // Neither file is an exact match for "abc123" — both merely
    // contain it. With no exact match to break the tie, this must be
    // reported ambiguous rather than resolved to whichever the
    // filesystem happened to enumerate first.
    write_session_file(dir.path(), "proj-a", "abc123-one");
    write_session_file(dir.path(), "proj-b", "abc123-two");

    match find_conversation_file(dir.path(), "abc123", None) {
        ConversationLookup::Ambiguous(mut candidates) => {
            candidates.sort();
            assert_eq!(candidates.len(), 2);
            assert_eq!(
                candidates[0],
                ("abc123-one".to_string(), "proj-a".to_string())
            );
            assert_eq!(
                candidates[1],
                ("abc123-two".to_string(), "proj-b".to_string())
            );
        }
        other => panic!(
            "expected Ambiguous with 2 candidates, got a resolved/not-found result instead: {}",
            matches!(other, ConversationLookup::Found(..))
        ),
    }
}

#[test]
fn get_full_conversation_surfaces_ambiguous_candidates_instead_of_a_wrong_file() {
    let dir = tempfile::tempdir().unwrap();
    write_session_file(dir.path(), "proj-a", "abc123-one");
    write_session_file(dir.path(), "proj-b", "abc123-two");

    let out = get_full_conversation(dir.path(), None, "abc123", None);
    assert!(out.contains("is ambiguous"));
    assert!(out.contains("abc123-one"));
    assert!(out.contains("abc123-two"));
    assert!(out.contains("proj-a"));
    assert!(out.contains("proj-b"));
}

#[test]
fn transcript_run_surfaces_ambiguous_session_id_honestly() {
    let dir = tempfile::tempdir().unwrap();
    write_session_file(dir.path(), "proj-a", "abc123-one");
    write_session_file(dir.path(), "proj-b", "abc123-two");

    let req = crate::transcript::TranscriptRequest {
        session: "abc123".to_string(),
        view: crate::transcript::ViewKind::Stats,
        ..Default::default()
    };
    let out = crate::transcript::run(dir.path(), &req);
    assert!(out.contains("ambiguous"));
    assert!(out.contains("abc123-one"));
    assert!(out.contains("abc123-two"));
}
#[tokio::test]
async fn a_stored_reflection_deleted_before_its_index_insert_stays_out_of_the_index() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let search = Arc::new(RwLock::new(SearchEngine::new(8)));
    let _race =
        crate::storage::lock_probe::delete_reflection_then_sync(storage.clone(), search.clone());
    store_embedded_reflection(
        &storage,
        &search,
        "gone".into(),
        "quartzgone",
        &[],
        vec![0.1; 384],
    )
    .await
    .unwrap();
    assert!(!search.read().await.has_reflection("gone"));
}

#[tokio::test]
async fn a_failed_fence_read_stops_a_reflection_before_it_commits() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let search = Arc::new(RwLock::new(SearchEngine::new(8)));
    let _fail = crate::storage::lock_probe::fail_fence_reads();
    assert!(store_embedded_reflection(
        &storage,
        &search,
        "x".into(),
        "quartzx",
        &[],
        vec![0.1; 384]
    )
    .await
    .is_err());
    assert_eq!(storage.count_reflection_embeddings().unwrap(), 0);
}

#[tokio::test]
async fn r3_missing_store_ack_reports_pending_id_and_reflection_fts_finds_it() {
    use crate::search::vector_ipc::{ProxyTimings, VectorClient, VectorProxy};
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("csr-engine.db");
    let storage = Arc::new(Storage::open(&db).unwrap());
    let proxy = Arc::new(VectorProxy::new(
        VectorClient::for_database(&db).unwrap(),
        storage.clone(),
    ));
    proxy.set_timings(ProxyTimings {
        backoff_first: std::time::Duration::from_millis(1),
        backoff_total: std::time::Duration::from_millis(5),
        down_window: std::time::Duration::from_secs(30),
    });
    let search = Arc::new(RwLock::new(SearchEngine::proxied(proxy)));
    let reply = store_embedded_reflection(
        &storage,
        &search,
        "pending-id".into(),
        "quartzpending acknowledgment",
        &[],
        vec![0.1; 384],
    )
    .await
    .unwrap();
    assert!(reply.contains("semantic indexing is pending"), "{reply}");
    assert!(reply.contains("ID: pending-id"));
    assert!(!reply.contains("successfully"));
    assert!(storage
        .fts5_search("quartzpending", 10, None)
        .unwrap()
        .is_empty());
    assert_eq!(
        storage
            .fts5_search_reflections("quartzpending", 10)
            .unwrap()[0]
            .0,
        "pending-id"
    );
}

#[tokio::test]
async fn r3_keyword_fallback_preserves_dates_scope_pagination_and_formats() {
    let rows: Vec<_> = (0..8)
        .map(|n| (format!("id{n}"), format!("quartzneedle unique{n}")))
        .collect();
    let input: Vec<_> = rows
        .iter()
        .map(|(id, text)| (id.as_str(), "alpha", text.as_str(), [1.0, 0.0, 0.0, 0.0]))
        .collect();
    let (storage, search) = quick_check_fixture(&input);
    storage
        .with_connection(|conn| {
            conn.execute("UPDATE chunks SET timestamp='2026-10-02T00:00:00Z'", [])?;
            conn.execute(
                "UPDATE chunks SET timestamp='2020-01-01T00:00:00Z' WHERE id='id0'",
                [],
            )?;
            conn.execute("UPDATE chunks SET project_name='beta' WHERE id='id1'", [])?;
            Ok(())
        })
        .unwrap();
    let candidates = keyword_candidates(&storage, "quartzneedle", Some("alpha"), None).unwrap();
    assert_eq!(candidates.len(), 7);
    assert!(candidates.iter().all(|r| r.chunk.project_name == "alpha"));
    let page = keyword_fallback(
        &storage,
        &search,
        "quartzneedle",
        3,
        Some("alpha"),
        KeywordTool::More { offset: 3 },
    )
    .await
    .unwrap();
    assert!(page.contains("<offset>3</offset>"), "{page}");
    assert!(
        page.contains("<results_returned>3</results_returned>"),
        "{page}"
    );
    for row in candidates.iter().skip(3).take(3) {
        assert!(page.contains(&row.chunk.content));
    }
    for row in candidates.iter().take(3) {
        assert!(!page.contains(&row.chunk.content));
    }
    let recency = keyword_fallback(
        &storage,
        &search,
        "quartzneedle",
        10,
        Some("alpha"),
        KeywordTool::Recency {
            time_range: None,
            since: Some("2026-10-01"),
            until: Some("2026-10-03"),
        },
    )
    .await
    .unwrap();
    assert!(!recency.contains("unique0"), "{recency}");
    assert!(!recency.contains("unique1"), "{recency}");
    assert!(recency.contains("unique2"), "{recency}");
    for (tool, marker) in [
        (KeywordTool::Quick, "<quick_search>"),
        (KeywordTool::Insights, "<search_summary>"),
        (KeywordTool::Why, "WHY: quartzneedle"),
    ] {
        let reply = keyword_fallback(&storage, &search, "quartzneedle", 5, Some("alpha"), tool)
            .await
            .unwrap();
        assert!(reply.starts_with(marker), "{reply}");
        assert!(!reply.contains("unique1"));
    }
    assert!(validate_page(usize::MAX, 1).is_err());
    assert!(validate_page(0, usize::MAX).is_err());
}

#[tokio::test]
async fn r3_recency_fts_filters_before_candidate_limit() {
    let mut rows: Vec<_> = (0..500)
        .map(|n| (format!("old{n}"), "quartzdated".to_string()))
        .collect();
    rows.push((
        "recent".into(),
        format!("quartzdated {}", "padding ".repeat(100)),
    ));
    rows.push(("unrelated".into(), "quartzdated".into()));
    let input: Vec<_> = rows
        .iter()
        .map(|(id, text)| (id.as_str(), "alpha", text.as_str(), [1.0, 0.0, 0.0, 0.0]))
        .collect();
    let (storage, search) = quick_check_fixture(&input);
    storage.with_connection(|conn| {
        conn.execute("UPDATE chunks SET timestamp='2020-01-01T00:00:00Z'", [])?;
        conn.execute("UPDATE chunks SET timestamp='2026-10-02T00:00:00Z' WHERE id IN ('recent', 'unrelated')", [])?;
        conn.execute("UPDATE chunks SET project_name='beta' WHERE id='unrelated'", [])?;
        Ok(())
    }).unwrap();
    assert!(storage
        .fts5_search("quartzdated", 500, Some("alpha"))
        .unwrap()
        .iter()
        .all(|(chunk, _, _)| chunk.id != "recent"));
    for project in [None, Some("alpha")] {
        let reply = keyword_fallback(
            &storage,
            &search,
            "quartzdated",
            10,
            project,
            KeywordTool::Recency {
                time_range: None,
                since: Some("2026-10-01"),
                until: Some("2026-10-03"),
            },
        )
        .await
        .unwrap();
        assert!(reply.contains("padding"), "{reply}");
        assert!(!reply.contains("conv-old"), "{reply}");
        if project.is_some() {
            assert!(!reply.contains("conv-unrelated"), "{reply}");
        }
    }
}

#[tokio::test]
async fn r3_keyword_fallback_formatters_omit_semantic_confidence() {
    let (storage, search) =
        quick_check_fixture(&[("lexical", "alpha", "quartzlexical", [1.0, 0.0, 0.0, 0.0])]);
    for tool in [
        KeywordTool::Concept,
        KeywordTool::Insights,
        KeywordTool::Quick,
        KeywordTool::More { offset: 0 },
        KeywordTool::Recency {
            time_range: None,
            since: Some("2000-01-01"),
            until: Some("2100-01-01"),
        },
    ] {
        let reply = keyword_fallback(&storage, &search, "quartzlexical", 5, Some("alpha"), tool)
            .await
            .unwrap();
        assert!(reply.contains("keyword"), "{reply}");
        for forbidden in [
            "0.000",
            "weak",
            "calibrat",
            "<average_score>",
            "<range>",
            "<s>",
        ] {
            assert!(!reply.contains(forbidden), "{forbidden}: {reply}");
        }
    }
}

#[tokio::test]
async fn r3_keyword_pagination_stays_inside_the_500_candidate_window() {
    let rows: Vec<_> = (0..510)
        .map(|n| (format!("id{n:04}"), format!("quartzwindow unique{n:04}")))
        .collect();
    let input: Vec<_> = rows
        .iter()
        .map(|(id, text)| (id.as_str(), "alpha", text.as_str(), [1.0, 0.0, 0.0, 0.0]))
        .collect();
    let (storage, search) = quick_check_fixture(&input);
    let page = keyword_fallback(
        &storage,
        &search,
        "quartzwindow",
        10,
        Some("alpha"),
        KeywordTool::More { offset: 499 },
    )
    .await
    .unwrap();
    assert!(
        page.contains("<total_available>500</total_available>"),
        "{page}"
    );
    assert!(
        page.contains("<results_returned>1</results_returned>"),
        "{page}"
    );
}

#[test]
fn superseded_best_chunk_loses_the_session_slot_and_a_sunk_session_trails() {
    // sA: a1 (best, superseded) and a2. sB: b1. sC: c1, c2 (both superseded).
    let session_keys = keys(&[
        ("a1", "sA"),
        ("a2", "sA"),
        ("b1", "sB"),
        ("c1", "sC"),
        ("c2", "sC"),
    ]);
    let lists: [&[&str]; 2] = [&["c1", "a1", "c2", "b1", "a2"], &["a1", "c1", "b1"]];
    let none = HashSet::new();
    let before = collapse_with_partition(&lists, &none, &session_keys);
    // Unpartitioned, a1 holds sA's slot and sA's other chunk is not shown.
    assert!(before.contains(&"a1".to_string()), "{before:?}");
    assert!(!before.contains(&"a2".to_string()), "{before:?}");
    let sunk: HashSet<String> = ["a1", "c1", "c2"].iter().map(|s| s.to_string()).collect();
    let after = collapse_with_partition(&lists, &sunk, &session_keys);
    // sB (b1) and sA (now its next chunk, a2) lead; sC trails alone.
    assert_eq!(after, ids(&["b1", "a2", "c1"]));
}

#[test]
fn nothing_sunk_leaves_the_collapse_untouched() {
    let session_keys = keys(&[("a1", "sA"), ("a2", "sA"), ("b1", "sB")]);
    let lists: [&[&str]; 2] = [&["a1", "b1", "a2"], &["b1", "a1"]];
    let semantic = ids(lists[0]);
    let fts = ids(lists[1]);
    let direct = collapse_to_sessions_by(
        &[(&semantic, 1.0), (&fts, 1.0)],
        |id| {
            session_keys
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.to_string())
        },
        |_| 0,
        RRF_K,
    );
    let staged = collapse_with_partition(&lists, &HashSet::new(), &session_keys);
    assert_eq!(staged, direct);
}

#[test]
fn assertion_stage_sinks_a_claim_match_and_notes_it_without_claim_text() {
    let storage = Storage::open_memory().unwrap();
    storage.rebuild_assertions(&[assertion_decision()]).unwrap();
    let claims = storage.claim_assertions().unwrap();
    assert_eq!(claims.len(), 1);
    let mut rows = vec![
        assertion_row("old", "convA", "merge needs a carrier branch for admin"),
        assertion_row("new", "convB", "admin merge: --squash works for admin"),
        assertion_row("other", "convC", "unrelated"),
    ];
    rows[2].resolution = Some("existing note".into());
    let session_keys = keys(&[("old", "convA"), ("new", "convB"), ("other", "convC")]);
    let sunk =
        run_assertions_stage_with(&storage, &claims, &mut rows, &session_keys, &HashMap::new());
    assert_eq!(sunk, HashSet::from(["old".to_string()]));
    let note = rows[0].resolution.as_deref().unwrap();
    assert_eq!(note, "[superseded] merge-mode (receipt rcpt-decision)");
    assert!(!note.contains("ANSWER-TEXT"), "{note}");
    assert!(rows[1].resolution.is_none());
    assert_eq!(rows[2].resolution.as_deref(), Some("existing note"));
}

#[test]
fn assertion_notes_join_existing_and_skip_a_repeated_witness_verdict() {
    use crate::storage::assertions::Assertion;
    let witness = Assertion {
        source: "witness".into(),
        target_kind: "chunk".into(),
        target_id: "w".into(),
        state: "superseded".into(),
        claim: None,
        receipt: "abc1234".into(),
    };
    let resolve = Assertion {
        source: "csr_resolve".into(),
        target_id: "r".into(),
        receipt: "fixed".into(),
        ..witness.clone()
    };
    let by_target: HashMap<String, Assertion> =
        [("w".to_string(), witness), ("r".to_string(), resolve)]
            .into_iter()
            .collect();
    let units_rows = || {
        vec![
            assertion_row("w", "cw", "text"),
            assertion_row("r", "cr", "text"),
        ]
    };
    let rows0 = units_rows();
    let units: Vec<assertions_partition::Unit<'_>> = rows0
        .iter()
        .map(|e| assertions_partition::Unit {
            id: &e.chunk.id,
            session: &e.chunk.conversation_id,
            text: &e.chunk.content,
        })
        .collect();
    let outcome = assertions_partition::partition(&units, &by_target, &[]);
    // The chunk "w" already has the dream's stale-anchor note; "r" has an older note.
    let validity: HashMap<String, ConvValidity> = [(
        "w".to_string(),
        demote_validity("[stale anchor] foo no longer in current code (receipt abc1234)"),
    )]
    .into_iter()
    .collect();
    let mut rows = units_rows();
    rows[1].resolution = Some("older".into());
    merge_assertion_notes(&mut rows, &outcome, &by_target, &validity);
    assert!(rows[0].resolution.is_none(), "{:?}", rows[0].resolution);
    assert_eq!(
        rows[1].resolution.as_deref(),
        Some("older; [superseded] csr_resolve (receipt fixed)")
    );
    // Without the validity note the witness row is annotated.
    let mut rows = units_rows();
    merge_assertion_notes(&mut rows, &outcome, &by_target, &HashMap::new());
    assert_eq!(
        rows[0].resolution.as_deref(),
        Some("[superseded] witness (receipt abc1234)")
    );
}

#[test]
fn s9r_p1_stale_agent_superseded_row_ignored_at_read() {
    let storage = s9r_stage_store(
        "INSERT INTO assertions (source,target_kind,target_id,state,pattern_json,receipt)
             VALUES ('csr_resolve','chunk','x','superseded',
                     '{\"status\":\"resolved\",\"ledger_source\":\"agent\"}','agent claimed fixed'),
                    ('csr_resolve','chunk','y','superseded',
                     '{\"status\":\"resolved\",\"ledger_source\":\"user_confirmed\"}','owner said fixed');",
    );
    let (sunk, rows) = s9r_run_stage(&storage, &HashMap::new());
    assert_eq!(sunk, HashSet::from(["y".to_string()]));
    assert!(rows[0].resolution.is_none(), "{:?}", rows[0].resolution);
}

#[test]
fn s9r_witness_reinstated_does_not_sink() {
    // A later anchor_reinstated leaves no live verdict: the validity map
    // holds nothing for the chunk, so the cached assertion must not sink it.
    let storage = s9r_stage_store(S9R_WITNESS_X);
    let (sunk, rows) = s9r_run_stage(&storage, &HashMap::new());
    assert!(sunk.is_empty(), "{sunk:?}");
    assert!(rows[0].resolution.is_none(), "{:?}", rows[0].resolution);
}

#[test]
fn s9r_witness_annotate_only_mode_does_not_sink() {
    // Annotate-only or off consumption: validity carries the note but no demote.
    let storage = s9r_stage_store(S9R_WITNESS_X);
    let validity: HashMap<String, ConvValidity> = [(
        "x".to_string(),
        ConvValidity {
            demote: false,
            note: "[stale anchor] foo gone (receipt abc1234)".into(),
            csr_only_demote: false,
        },
    )]
    .into_iter()
    .collect();
    let (sunk, rows) = s9r_run_stage(&storage, &validity);
    assert!(sunk.is_empty(), "{sunk:?}");
    assert!(rows[0].resolution.is_none(), "{:?}", rows[0].resolution);
}

#[test]
fn s9r_witness_csr_text_demote_does_not_keep_the_assertion() {
    let storage = s9r_stage_store(S9R_WITNESS_X);
    let validity: HashMap<String, ConvValidity> = [(
        "x".to_string(),
        ConvValidity {
            demote: true,
            note: "[CSR text] this chunk holds CSR's own output (f)".into(),
            csr_only_demote: true,
        },
    )]
    .into_iter()
    .collect();
    let (sunk, _) = s9r_run_stage(&storage, &validity);
    assert!(sunk.is_empty(), "{sunk:?}");
}

#[test]
fn s9r_witness_live_demote_still_sinks() {
    let storage = s9r_stage_store(S9R_WITNESS_X);
    let validity: HashMap<String, ConvValidity> = [(
        "x".to_string(),
        demote_validity("[stale anchor] foo gone (receipt abc1234)"),
    )]
    .into_iter()
    .collect();
    let (sunk, rows) = s9r_run_stage(&storage, &validity);
    assert_eq!(sunk, HashSet::from(["x".to_string()]));
    // The dream note already says it; the assertion does not repeat it.
    assert!(rows[0].resolution.is_none(), "{:?}", rows[0].resolution);
}

#[test]
fn kill_switch_matches_a_store_with_no_assertions_table() {
    let with_rules = Storage::open_memory().unwrap();
    with_rules
        .rebuild_assertions(&[assertion_decision()])
        .unwrap();
    let claims = with_rules.claim_assertions().unwrap();
    let without_table = Storage::open_memory().unwrap();
    without_table
        .with_connection(|conn| Ok(conn.execute_batch("DROP TABLE assertions")?))
        .unwrap();
    assert!(without_table.claim_assertions().unwrap().is_empty());
    assert!(without_table
        .assertions_for_targets(&["old"], &["convA"])
        .unwrap()
        .is_empty());

    let make = || {
        vec![assertion_row(
            "old",
            "convA",
            "merge needs a carrier branch for admin",
        )]
    };
    let session_keys = keys(&[("old", "convA")]);

    // Switch off: the stage is skipped, whatever the store holds.
    assert!(!assertions_partition::assertions_enabled_from(Some("1")));
    let mut off = make();
    let sunk_off =
        run_assertions_stage(&with_rules, &mut off, &session_keys, &HashMap::new(), false);
    // No table, switch on: nothing sinks, nothing is noted.
    let mut bare = make();
    let sunk_bare = run_assertions_stage_with(
        &without_table,
        &[],
        &mut bare,
        &session_keys,
        &HashMap::new(),
    );
    assert!(sunk_off.is_empty() && sunk_bare.is_empty());
    assert_eq!(off[0].resolution, bare[0].resolution);
    assert!(off[0].resolution.is_none());
    // The same rows with the switch on and rules present do sink.
    let mut on = make();
    let sunk_on = run_assertions_stage_with(
        &with_rules,
        &claims,
        &mut on,
        &session_keys,
        &HashMap::new(),
    );
    assert_eq!(sunk_on.len(), 1);
    assert!(on[0].resolution.is_some());
    // Switch off leaves the lists as they were.
    let order = ids(&["old", "x"]);
    assert_eq!(
        assertions_partition::sunk_to_tail(order.clone(), &sunk_off),
        order
    );
    assert_eq!(assertions_partition::without_sunk(&order, &sunk_off), order);
}

#[test]
fn ce_reorder_orders_pooled_sessions_by_session_max_and_keeps_the_rest() {
    // S1 order: sA, sB, sC, sD. The pool scored sA once, sB twice (its
    // second chunk is the better one) and sC once; sD was not pooled.
    let collapsed = ids(&["a1", "b1", "c1", "d1"]);
    let session_keys = keys(&[("a1", "sA"), ("b1", "sB"), ("c1", "sC"), ("d1", "sD")]);
    let scored = vec![
        row("a1", "sA", 0, 2.0),
        row("b1", "sB", 0, 1.0),
        row("b2", "sB", 0, 5.0),
        row("c1", "sC", 0, 4.0),
    ];
    let ordered = ce_reorder(&collapsed, &session_keys, &scored);
    assert_eq!(ordered.order, ids(&["b2", "c1", "a1", "d1"]));
    assert_eq!(ordered.session_scores, vec![5.0, 4.0, 2.0]);
}

#[test]
fn ce_reorder_ties_keep_the_s1_order_and_answer_rows_speak_for_a_session() {
    let collapsed = ids(&["a1", "b1", "e1"]);
    let session_keys = keys(&[("a1", "sA"), ("b1", "sB"), ("e1", "sE")]);
    let scored = vec![
        row("b1", "sB", 0, 3.0),
        row("a1", "sA", 0, 3.0),
        // sE has an echo that scores high and an answer that scores low:
        // the answer row represents it.
        row("e1", "sE", 1, 9.0),
        row("e2", "sE", 0, 1.0),
    ];
    let ordered = ce_reorder(&collapsed, &session_keys, &scored);
    assert_eq!(ordered.order, ids(&["a1", "b1", "e2"]));
    assert_eq!(ordered.session_scores, vec![3.0, 3.0, 1.0]);
}

#[test]
fn ce_reorder_a_session_with_only_non_answer_rows_is_still_scored() {
    let collapsed = ids(&["a1", "e1"]);
    let session_keys = keys(&[("a1", "sA"), ("e1", "sE")]);
    let scored = vec![row("a1", "sA", 0, 1.0), row("e1", "sE", 2, 6.0)];
    let ordered = ce_reorder(&collapsed, &session_keys, &scored);
    assert_eq!(ordered.order, ids(&["e1", "a1"]));
}

#[test]
fn ce_reorder_skips_non_finite_scores_and_empty_pools_keep_s1() {
    let collapsed = ids(&["a1", "b1"]);
    let session_keys = keys(&[("a1", "sA"), ("b1", "sB")]);
    let scored = vec![row("a1", "sA", 0, f32::NAN), row("b1", "sB", 0, 0.5)];
    let ordered = ce_reorder(&collapsed, &session_keys, &scored);
    assert_eq!(ordered.order, ids(&["b1", "a1"]));
    assert_eq!(ordered.session_scores, vec![0.5]);
    let none = ce_reorder(&collapsed, &session_keys, &[]);
    assert_eq!(none.order, collapsed);
    assert!(none.session_scores.is_empty());
}

#[test]
fn ce_pool_takes_the_first_24_fused_and_always_holds_the_s1_top_five() {
    let fused: Vec<String> = (0..40).map(|n| format!("f{n}")).collect();
    // Five protected representatives: two inside the first 24, three out.
    let protected = ids(&["f2", "f9", "f30", "f35", "zz"]);
    let pool = ce_pool_ids(&fused, &protected, CE_POOL);
    assert_eq!(pool.len(), CE_POOL);
    for id in &protected {
        assert!(pool.contains(id), "{id} missing from {pool:?}");
    }
    // The three forced in displaced the three last unprotected ids.
    for gone in ["f23", "f22", "f21"] {
        assert!(!pool.contains(&gone.to_string()), "{gone} kept: {pool:?}");
    }
    assert!(pool.contains(&"f20".to_string()));
    let distinct: HashSet<&String> = pool.iter().collect();
    assert_eq!(distinct.len(), pool.len());
}

#[test]
fn ce_pool_with_a_short_fused_list_appends_the_protected() {
    let pool = ce_pool_ids(&ids(&["a", "b", "a"]), &ids(&["b", "c"]), CE_POOL);
    assert_eq!(pool, ids(&["a", "b", "c"]));
}

#[test]
fn ce_pair_text_carries_role_and_date_and_drops_the_keyword_marker() {
    let mut chunk = enriched_result_conv("p1", "conv", 0.5, 0).chunk;
    chunk.author = crate::provenance::Speaker::User;
    chunk.timestamp = "2026-09-03T08:15:00Z".into();
    chunk.content = "[keyword] we chose sqlite".into();
    assert_eq!(ce_pair_text(&chunk), "[user, 2026-09-03] we chose sqlite");
    chunk.timestamp = "not a time".into();
    assert_eq!(ce_pair_text(&chunk), "[user] we chose sqlite");
}

#[test]
fn a_down_daemon_keeps_the_pre_rerank_order_and_never_loads_the_model() {
    use crate::search::vector_ipc::{VectorClient, VectorProxy};
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("csr-engine.db");
    let proxy = VectorProxy::new(
        VectorClient::for_database(&db).unwrap(),
        Arc::new(Storage::open_memory().unwrap()),
    );
    let (ids, enriched, session_keys) = rerank_fixture();
    let collapsed = vec![
        ids[3].clone(),
        ids[1].clone(),
        ids[4].clone(),
        ids[0].clone(),
    ];
    let touched = cross_encoder::global_touches();
    let (order, scores) = ce_rerank_stage_inner(
        "which note is about the socket",
        collapsed.clone(),
        ids.clone(),
        &enriched,
        &session_keys,
        &HashMap::new(),
        &HashMap::new(),
        &[],
        &CallMemo::default(),
        Some(&proxy),
    );
    assert_eq!(order, collapsed, "pre-rerank order stands");
    assert!(scores.is_empty(), "no scores without a reranker");
    assert_eq!(
        cross_encoder::global_touches(),
        touched,
        "a forwarding server must not load the cross-encoder"
    );
    // The hook sees a load when there is no daemon to forward to.
    let _ = ce_rerank_stage_inner(
        "which note is about the socket",
        collapsed,
        ids,
        &enriched,
        &session_keys,
        &HashMap::new(),
        &HashMap::new(),
        &[],
        &CallMemo::default(),
        None,
    );
    assert_eq!(cross_encoder::global_touches(), touched + 1);
}

#[test]
fn cross_encoder_policy_skips_decay_and_legacy_keeps_it() {
    let now = chrono::Utc::now();
    let mut chunk = enriched_result_conv("old", "conv-old", 0.9, 0).chunk;
    chunk.timestamp = (now - chrono::Duration::days(400)).to_rfc3339();
    let config = decay::DecayConfig::for_search();
    let score = |policy| {
        score_chunk_candidate(
            0.9,
            &chunk,
            &now,
            &[],
            &config,
            &HashMap::new(),
            None,
            false,
            &unscoped_search_scope(),
            policy,
        )
        .0
    };
    assert_eq!(
        score(RetrievalPolicy::CrossEncoder).to_bits(),
        0.9f32.to_bits()
    );
    assert!(score(RetrievalPolicy::Legacy) < 0.9);

    let fts = |policy| score_fts_candidate(&chunk, &now, &HashMap::new(), None, false, policy).0;
    assert_eq!(
        fts(RetrievalPolicy::CrossEncoder).to_bits(),
        crate::search::hybrid::FTS_ONLY_BASE_SCORE.to_bits()
    );
    assert!(fts(RetrievalPolicy::Legacy) < crate::search::hybrid::FTS_ONLY_BASE_SCORE);
}

#[test]
fn policy_override_is_per_thread_and_tests_default_to_legacy() {
    // No test sets CSR_RETRIEVAL_LEGACY; the override is the test seam.
    assert_eq!(current_retrieval_policy(), RetrievalPolicy::Legacy);
    {
        let _cross_encoder = force_policy(RetrievalPolicy::CrossEncoder);
        assert_eq!(current_retrieval_policy(), RetrievalPolicy::CrossEncoder);
        std::thread::spawn(|| {
            assert_eq!(current_retrieval_policy(), RetrievalPolicy::Legacy);
        })
        .join()
        .unwrap();
    }
    assert_eq!(current_retrieval_policy(), RetrievalPolicy::Legacy);
}

#[tokio::test]
async fn hybrid_recall_under_the_cross_encoder_policy_returns_every_session_and_fails_open() {
    let _policy = force_policy(RetrievalPolicy::CrossEncoder);
    let storage = Arc::new(Storage::open_memory().unwrap());
    let now = chrono::Utc::now().to_rfc3339();
    let mut engine = SearchEngine::new(8);
    let rows = [
        (
            "ce-a",
            "ce-conv-a",
            "we dropped Docker for a single binary",
            [1.0, 0.0, 0.0, 0.0],
        ),
        (
            "ce-b",
            "ce-conv-b",
            "unrelated note about lunch",
            [0.8, 0.6, 0.0, 0.0],
        ),
        (
            "ce-c",
            "ce-conv-c",
            "Qdrant needed a server we never wanted",
            [0.9, 0.0, 0.43, 0.0],
        ),
    ];
    for (id, conversation, content, vector) in rows {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conversation.into(),
            project_name: "test".into(),
            timestamp: now.clone(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &vector).unwrap();
        engine.insert_chunk(id.to_string(), vector.to_vec());
    }
    let search = Arc::new(RwLock::new(engine));
    let output = reflect_on_past_with_vec_mode(
        &storage,
        &search,
        &[1.0, 0.0, 0.0, 0.0],
        "why did we drop Docker",
        3,
        0.0,
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
    for id in ["ce-a", "ce-b", "ce-c"] {
        assert!(output.contains(&format!("<id>{id}</id>")), "{output}");
    }
    // A scorer on the machine adds the CE attributes; without model files
    // the S1 order stands and the cosine label path is used.
    assert_eq!(
        output.contains("ce-top="),
        cross_encoder::global().is_some(),
        "{output}"
    );
    // No flat boost on this path: the user-authored rows carry no `adj`.
    assert!(!output.contains("adj="), "{output}");
}
