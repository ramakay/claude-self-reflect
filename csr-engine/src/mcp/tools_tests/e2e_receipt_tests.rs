use super::*;

#[test]
fn validity_note_exact_wording_demote() {
    let hit = ChunkWitnessVerdict {
        chunk_id: "chunk-demote".into(),
        file: "/repo/src/lib.rs".into(),
        symbol: Some("old_fn".into()),
        channel: VerdictChannel::Demote,
        verdict: "anchor_obsolete",
        receipt_oid: Some("abcdef1234567890".into()),
    };
    assert_eq!(
        validity_note(&hit),
        "[stale anchor] old_fn no longer in current code (receipt abcdef1)"
    );
}

#[test]
fn validity_note_exact_wording_annotate() {
    let hit = ChunkWitnessVerdict {
        chunk_id: "chunk-annotate".into(),
        file: "/repo/src/lib.rs".into(),
        symbol: Some("foo".into()),
        channel: VerdictChannel::Annotate,
        verdict: "superseded_by",
        receipt_oid: Some("abcdef1234567890".into()),
    };
    assert_eq!(
        validity_note(&hit),
        "[evolved] foo changed since this conversation (as of abcdef1)"
    );
}

#[test]
fn validity_prefers_demote_over_annotate_for_same_chunk() {
    // One chunk touching two symbols — one demoted, one merely evolved —
    // takes its own strongest channel without affecting sibling chunks.
    let demote_hit = ChunkWitnessVerdict {
        chunk_id: "chunk-both".into(),
        file: "/repo/src/a.rs".into(),
        symbol: Some("gone_fn".into()),
        channel: VerdictChannel::Demote,
        verdict: "anchor_obsolete",
        receipt_oid: Some("head1".into()),
    };
    let annotate_hit = ChunkWitnessVerdict {
        chunk_id: "chunk-both".into(),
        file: "/repo/src/b.rs".into(),
        symbol: Some("evolved_fn".into()),
        channel: VerdictChannel::Annotate,
        verdict: "superseded_by",
        receipt_oid: Some("head2".into()),
    };
    let mut chunk = enriched_result_conv("chunk-both", "conv-both", 0.9, 0).chunk;
    chunk.content = "gone_fn and evolved_fn".into();
    let validity = reduce_validity_hits(
        BTreeMap::from([("chunk-both".into(), vec![annotate_hit, demote_hit])]),
        &[chunk],
    );
    assert!(is_demote_channel(&validity, "chunk-both"));
}

#[tokio::test]
async fn e2e_fts_validity_failure_keeps_unconditional_keyword_hit() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let now = chrono::Utc::now();
    let semantic = crate::import::ConversationChunk {
        id: "chunk-semantic".into(),
        conversation_id: "conv-semantic".into(),
        project_name: "testproj".into(),
        timestamp: (now - chrono::Duration::days(300)).to_rfc3339(),
        content: "semantic candidate above the fallback threshold".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::User,
        seq: 0,
        is_sidechain: false,
    };
    let keyword = crate::import::ConversationChunk {
        id: "chunk-fts-failure".into(),
        conversation_id: "conv-fts-failure".into(),
        project_name: "testproj".into(),
        timestamp: now.to_rfc3339(),
        content: "zebraquark fallback candidate".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::User,
        seq: 1,
        is_sidechain: false,
    };
    let semantic_vec = vec![0.7, 0.714_142_86, 0.0, 0.0];
    let keyword_vec = vec![0.0, 0.0, 1.0, 0.0];
    storage.insert_chunk(&semantic, &semantic_vec).unwrap();
    storage.insert_chunk(&keyword, &keyword_vec).unwrap();
    storage
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO conversation_ancestry_cache
                     (conversation_id, state, release_tag, releases_behind, repository, refreshed_at)
                     VALUES ('conv-semantic', 'shipped', 'v1.0.0', 100, '/repo', ?1)",
                [now.to_rfc3339()],
            )?;
            // Only the FTS conversation selects this row. SQLite accepts
            // the malformed integer dynamically; NodeRow decoding then
            // supplies a real read error for the second validity batch.
            conn.execute(
                "INSERT INTO code_nodes
                     (id, file, kind, name, span_start, span_end,
                      first_conv_id, last_conv_id)
                     VALUES ('bad-fts-node', '/repo/src/bad.rs', 'function',
                             'bad', 'not-an-integer', 1,
                             'conv-fts-failure', 'conv-fts-failure')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let mut engine = SearchEngine::new(16);
    engine.insert_chunk(semantic.id.clone(), semantic_vec);
    engine.insert_chunk(keyword.id.clone(), keyword_vec);
    let search = Arc::new(RwLock::new(engine));

    let out = reflect_on_past_with_vec(
        &storage,
        &search,
        &[1.0, 0.0, 0.0, 0.0],
        "zebraquark",
        3,
        0.1,
        Some("all"),
        0,
        true,
        ConsumptionMode::Full,
        false,
    )
    .await
    .unwrap();

    assert!(
        out.contains("<id>chunk-semantic</id>"),
        "the semantic hit must survive the fail-open validity batch:\n{out}"
    );
    assert!(
        out.contains("<id>chunk-fts-failure</id>"),
        "always-on hybrid search must keep the FTS hit even when its validity batch fails open:\n{out}"
    );
}

#[tokio::test]
async fn fts_scaffold_suppresses_ancestry_and_neutral_paths_are_bit_identical() {
    let scaffold = "<command-message>zebraquark quoted workflow</command-message>";
    let pre_change = render_fts_candidate(scaffold, None).await;
    let current_release = render_fts_candidate(scaffold, Some(0)).await;
    let shipped = render_fts_candidate(scaffold, Some(5)).await;

    assert_eq!(
        rendered_result(&current_release, "chunk-fts-scaffold"),
        rendered_result(&pre_change, "chunk-fts-scaffold")
    );
    assert_eq!(
        rendered_result(&shipped, "chunk-fts-scaffold"),
        rendered_result(&pre_change, "chunk-fts-scaffold")
    );

    let organic = "zebraquark organic conversation";
    let organic_pre_change = render_fts_candidate(organic, None).await;
    let organic_current = render_fts_candidate(organic, Some(0)).await;
    assert_eq!(
        rendered_result(&organic_current, "chunk-fts-scaffold"),
        rendered_result(&organic_pre_change, "chunk-fts-scaffold")
    );
}

#[tokio::test]
async fn e2e_active_forgetting_reorders_only_demoted_section_and_preserves_fallback_set() {
    let q = [1.0f32, 0.0, 0.0, 0.0];
    let (off_storage, off_search) = active_forgetting_e2e_fixture();
    let off = reflect_on_past_with_vec(
        &off_storage,
        &off_search,
        &q,
        "zebraquark",
        5,
        0.1,
        Some("all"),
        0,
        true,
        ConsumptionMode::Full,
        false,
    )
    .await
    .unwrap();

    let off_ids = [
        "chunk-valid-a",
        "chunk-fts-valid",
        "chunk-valid-b",
        "chunk-demoted-old",
        "chunk-demoted-new",
    ];
    assert!(off_ids.windows(2).all(|pair| {
        pos_of(&off, &format!("<id>{}</id>", pair[0]))
            < pos_of(&off, &format!("<id>{}</id>", pair[1]))
    }));

    let (on_storage, on_search) = active_forgetting_e2e_fixture();
    let on = reflect_on_past_with_vec(
        &on_storage,
        &on_search,
        &q,
        "zebraquark",
        5,
        0.1,
        Some("all"),
        0,
        true,
        ConsumptionMode::Full,
        true,
    )
    .await
    .unwrap();

    assert!(
        pos_of(&on, "<id>chunk-valid-a</id>") < pos_of(&on, "<id>chunk-fts-valid</id>")
            && pos_of(&on, "<id>chunk-fts-valid</id>") < pos_of(&on, "<id>chunk-valid-b</id>")
            && pos_of(&on, "<id>chunk-valid-b</id>") < pos_of(&on, "<id>chunk-demoted-new</id>")
            && pos_of(&on, "<id>chunk-demoted-new</id>")
                < pos_of(&on, "<id>chunk-demoted-old</id>"),
        "active forgetting must reorder only the demoted section by accelerated score:\n{on}"
    );
    for id in ["chunk-fts-valid", "chunk-valid-a", "chunk-valid-b"] {
        assert_eq!(
            rendered_result(&on, id),
            rendered_result(&off, id),
            "valid result {id} must remain byte-identical"
        );
    }
}

#[tokio::test]
async fn e2e_active_forgetting_pagination_is_disjoint_exhaustive_and_repeatable() {
    let (storage, search) = active_forgetting_e2e_fixture();
    let q = [1.0f32, 0.0, 0.0, 0.0];

    // The initial page ends inside the demoted section. Its accelerated
    // order is [new, old], so get_more must continue with old rather than
    // repeat new from the raw-score order [old, new].
    let page1 = reflect_on_past_with_vec(
        &storage,
        &search,
        &q,
        "topic",
        3,
        0.1,
        Some("all"),
        0,
        true,
        ConsumptionMode::Full,
        true,
    )
    .await
    .unwrap();
    let page2 = get_more_results_with_vec_active(
        &storage,
        &search,
        &q,
        "topic",
        3,
        1,
        0.1,
        Some("all"),
        ConsumptionMode::Full,
        true,
    )
    .await
    .unwrap();

    let expected = [
        ("chunk-valid-a", "valid alpha claim"),
        ("chunk-valid-b", "valid beta claim"),
        ("chunk-demoted-new", "recent stale old_fn claim"),
        ("chunk-demoted-old", "old stale old_fn claim"),
    ];
    assert!(expected[..3].windows(2).all(|pair| {
        pos_of(&page1, &format!("<id>{}</id>", pair[0].0))
            < pos_of(&page1, &format!("<id>{}</id>", pair[1].0))
    }));
    assert!(
        page2.contains("old stale old_fn claim"),
        "get_more must continue the initial active-forgetting order:\n{page2}"
    );

    for (id, marker) in &expected[..3] {
        assert!(
            page1.contains(&format!("<id>{id}</id>")) && !page2.contains(marker),
            "{id} must appear only on page 1"
        );
    }
    assert!(
        !page1.contains("<id>chunk-demoted-old</id>") && page2.contains("old stale old_fn claim"),
        "chunk-demoted-old must appear only on page 2"
    );
    assert_eq!(
        expected
            .iter()
            .filter(|(id, marker)| {
                page1.contains(&format!("<id>{id}</id>")) || page2.contains(marker)
            })
            .count(),
        expected.len(),
        "pages must exhaust the active-forgetting candidate set"
    );

    let demoted_page = get_more_results_with_vec_active(
        &storage,
        &search,
        &q,
        "topic",
        2,
        2,
        0.1,
        Some("all"),
        ConsumptionMode::Full,
        true,
    )
    .await
    .unwrap();
    assert!(
        pos_of(&demoted_page, "recent stale old_fn claim")
            < pos_of(&demoted_page, "old stale old_fn claim"),
        "get_more must preserve accelerated ordering within the demoted section:\n{demoted_page}"
    );
    let demoted_page_again = get_more_results_with_vec_active(
        &storage,
        &search,
        &q,
        "topic",
        2,
        2,
        0.1,
        Some("all"),
        ConsumptionMode::Full,
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        demoted_page, demoted_page_again,
        "repeated get_more requests must return identical ordering"
    );
}

#[tokio::test]
async fn get_more_continues_the_family_boosted_order_not_raw_hnsw() {
    // The certified live failure: scope=all with a current project. A
    // family chunk at raw 0.89 boosts to ~1.02 and wins page one over an
    // other-project chunk at raw 0.90. Pagination that reconstructs RAW
    // order sees [other, family], so offset=1 returns the family chunk
    // AGAIN and the other chunk is skipped forever.
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mk = |id: &str, project: &str, content: &str| crate::import::ConversationChunk {
        id: id.into(),
        conversation_id: format!("conv-{id}"),
        project_name: project.into(),
        timestamp: "2099-01-01T00:00:00Z".into(),
        content: content.into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::User,
        seq: 0,
        is_sidechain: false,
    };
    let rows = [
        (
            mk(
                "chunk-family",
                "alpha-proj",
                "family claim about pagination",
            ),
            vec![0.89f32, 0.455_960_5, 0.0, 0.0],
        ),
        (
            mk(
                "chunk-other",
                "beta-proj",
                "other-project claim about pagination",
            ),
            vec![0.90f32, 0.435_889_9, 0.0, 0.0],
        ),
    ];
    let mut engine = SearchEngine::new(16);
    for (chunk, vector) in &rows {
        storage.insert_chunk(chunk, vector).unwrap();
        engine.insert_chunk(chunk.id.clone(), vector.clone());
    }
    let search = Arc::new(RwLock::new(engine));
    let scope =
        SearchProjectScope::resolve_with(&storage, Some("all"), Some("alpha-proj"), None, None)
            .unwrap();
    let q = [1.0f32, 0.0, 0.0, 0.0];

    let page1 = reflect_on_past_with_vec_in_scope(
        &storage,
        &search,
        &q,
        "pagination",
        1,
        0.1,
        &scope,
        0,
        true,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();
    assert!(
        page1.contains("family claim about pagination"),
        "boosted family chunk must win page one:\n{page1}"
    );
    assert!(
        !page1.contains("other-project claim about pagination"),
        "page one has room for exactly one result:\n{page1}"
    );

    let page2 = get_more_results_in_scope(
        &storage,
        &search,
        &q,
        "pagination",
        1,
        1,
        0.1,
        &scope,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();
    assert!(
        page2.contains("other-project claim about pagination"),
        "offset=1 must surface the chunk page one displaced, not skip it:\n{page2}"
    );
    assert!(
        !page2.contains("family claim about pagination"),
        "the boosted family chunk must not be duplicated onto page two:\n{page2}"
    );
}

#[tokio::test]
async fn e2e_partition_through_real_reflect_and_get_more_paths() {
    let (storage, search) = e2e_fixture();
    let q = [1.0f32, 0.0, 0.0, 0.0];

    // 1) Demoted chunk sinks below BOTH valid ones and carries the exact
    //    annotation + footer, on the real reflect path.
    let out = reflect_on_past_with_vec(
        &storage,
        &search,
        &q,
        "topic",
        3,
        0.3,
        Some("all"),
        0,
        true,
        ConsumptionMode::Full,
        false,
    )
    .await
    .unwrap();
    let p_v1 = pos_of(&out, "<id>chunk-valid-1</id>");
    let p_v2 = pos_of(&out, "<id>chunk-valid-2</id>");
    let p_stale = pos_of(&out, "<id>chunk-stale</id>");
    assert!(
        p_v1 < p_v2 && p_v2 < p_stale,
        "demoted chunk must sink below every valid one:\n{out}"
    );
    assert!(
        out.contains("[stale anchor] old_fn no longer in current code (receipt deadbee)"),
        "exact annotation string must be present:\n{out}"
    );
    assert!(
        out.contains("bound code anchor no longer current (dream verdict)"),
        "dream-verdict footer must fire for a flagged demotion:\n{out}"
    );

    // 2) OVERFETCH: at limit=2 the valid N+1 candidate (valid-2, never
    //    fetched under the old exactly-limit fetch) takes the slot the
    //    demoted top hit vacated.
    let out = reflect_on_past_with_vec(
        &storage,
        &search,
        &q,
        "topic",
        2,
        0.3,
        Some("all"),
        0,
        true,
        ConsumptionMode::Full,
        false,
    )
    .await
    .unwrap();
    assert!(
        out.contains("<id>chunk-valid-2</id>"),
        "overfetch must surface the valid N+1 candidate:\n{out}"
    );
    assert!(
        !out.contains("<id>chunk-stale</id>"),
        "demoted chunk must not occupy a page slot valid candidates can fill:\n{out}"
    );

    // 3) KILL SWITCH: in vector ablation, partition off restores pure
    //    semantic score order — the demoted chunk leads again, no
    //    annotation anywhere.
    let out = reflect_on_past_with_vec_mode(
        &storage,
        &search,
        &q,
        "topic",
        2,
        0.3,
        Some("all"),
        0,
        false,
        ConsumptionMode::Off,
        false,
        "other",
        SearchMode::Vector,
        RecallRerankMode::Runtime,
        true,
    )
    .await
    .unwrap();
    let p_stale = pos_of(&out, "<id>chunk-stale</id>");
    let p_v1 = pos_of(&out, "<id>chunk-valid-1</id>");
    assert!(
        p_stale < p_v1,
        "kill switch must restore pre-partition order:\n{out}"
    );
    assert!(
        !out.contains("[stale anchor]"),
        "no annotation with switch off:\n{out}"
    );
    assert!(
        !out.contains("dream verdict"),
        "no footer with switch off:\n{out}"
    );

    // 4) get_more page 2 respects the partition: full-set partition
    //    yields [valid-1, valid-2, stale], so page 2 (offset 2) is the
    //    demoted chunk, annotated — and page 1 is demotion-free.
    let page1 = get_more_results_with_vec(
        &storage,
        &search,
        &q,
        "topic",
        0,
        2,
        0.3,
        Some("all"),
        ConsumptionMode::Full,
    )
    .await
    .unwrap();
    assert!(page1.contains("first current topic notes"), "got: {page1}");
    assert!(page1.contains("second current topic notes"), "got: {page1}");
    assert!(
        !page1.contains("old_fn design discussion"),
        "page 1 must not contain the demoted chunk:\n{page1}"
    );
    let page2 = get_more_results_with_vec(
        &storage,
        &search,
        &q,
        "topic",
        2,
        2,
        0.3,
        Some("all"),
        ConsumptionMode::Full,
    )
    .await
    .unwrap();
    assert!(
        page2.contains("old_fn design discussion"),
        "page 2 must carry the demoted chunk after the partition:\n{page2}"
    );
    assert!(
        page2.contains("[stale anchor] old_fn no longer in current code (receipt deadbee)"),
        "page 2 must carry the annotation:\n{page2}"
    );
    // Pagination stability (offset-independent window): no chunk may be
    // duplicated across pages 1+2 and none skipped — every fixture
    // conversation appears on exactly one page — and re-requesting page
    // 2 must reproduce it byte-for-byte (same query + same data → same
    // global order).
    assert!(
        !page2.contains("first current topic notes")
            && !page2.contains("second current topic notes"),
        "page 2 must not duplicate page 1 results:\n{page2}"
    );
    for marker in [
        "first current topic notes",
        "second current topic notes",
        "old_fn design discussion",
    ] {
        let on_p1 = page1.contains(marker);
        let on_p2 = page2.contains(marker);
        assert!(
            on_p1 ^ on_p2,
            "'{marker}' must appear on exactly one page (p1: {on_p1}, p2: {on_p2})"
        );
    }
    let page2_again = get_more_results_with_vec(
        &storage,
        &search,
        &q,
        "topic",
        2,
        2,
        0.3,
        Some("all"),
        ConsumptionMode::Full,
    )
    .await
    .unwrap();
    assert_eq!(
        page2, page2_again,
        "re-requested page 2 must be identical (offset-independent window)"
    );

    // 5) FTS-append: a query that misses semantically (orthogonal
    //    vector) but hits the FTS row of a demoted conversation must
    //    still annotate — validity is resolved AFTER the candidate set
    //    is final (the appended conversation ids are merged in).
    let q_miss = [0.0f32, 0.0, 0.0, 1.0];
    let out = reflect_on_past_with_vec(
        &storage,
        &search,
        &q_miss,
        "zebraquark",
        3,
        0.3,
        Some("all"),
        0,
        true,
        ConsumptionMode::Full,
        false,
    )
    .await
    .unwrap();
    assert!(
        out.contains("[keyword] zebraquark keyword only claim"),
        "FTS fallback must surface the keyword chunk:\n{out}"
    );
    assert!(
        out.contains("[stale anchor] old_fn no longer in current code (receipt deadbee)"),
        "FTS-appended chunk from a demoted conversation must be annotated:\n{out}"
    );
}

#[tokio::test]
async fn e2e_adaptive_refetch_backfills_valid_chunks_beyond_first_window() {
    // 8 demoted chunks outrank 2 valid ones. At limit=2 the first
    // window is overfetch(2) = 6 — ALL demoted, and full (more
    // candidates exist) — so the single adaptive refetch at 10*limit
    // must surface the valid pair from beyond the first window.
    let storage = Arc::new(Storage::open_memory().unwrap());

    let now = chrono::Utc::now().to_rfc3339();
    let mk = |id: &str, conv: &str, content: &str, seq: usize| crate::import::ConversationChunk {
        id: id.into(),
        conversation_id: conv.into(),
        project_name: "testproj".into(),
        timestamp: now.clone(),
        content: content.into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::User,
        seq,
        is_sidechain: false,
    };
    let mut engine = SearchEngine::new(32);
    for i in 0..8 {
        let symbol = format!("old_fn_{i}");
        let conversation_id = format!("conv-demoted-{i}");
        let file = format!("/repo/src/stale_{i}.rs");
        install_demote_verdict_for(&storage, &file, &symbol, &conversation_id, &conversation_id);
        let chunk = mk(
            &format!("chunk-demoted-{i}"),
            &conversation_id,
            &format!("stale claim number {i}"),
            i,
        );
        // cos ≈ 1.0 against [1,0,0,0] — every demoted chunk outranks
        // both valid ones.
        let vec = vec![1.0, 0.001 * (i + 1) as f32, 0.0, 0.0];
        storage.insert_chunk(&chunk, &vec).unwrap();
        engine.insert_chunk(chunk.id.clone(), vec);
    }
    for (id, conv, content, v, seq) in [
        (
            "chunk-valid-a",
            "conv-live-a",
            "live topic alpha notes",
            vec![0.6, 0.8, 0.0, 0.0], // cos 0.6
            8,
        ),
        (
            "chunk-valid-b",
            "conv-live-b",
            "live topic beta notes",
            vec![0.5, 0.866, 0.0, 0.0], // cos ~0.5
            9,
        ),
    ] {
        let chunk = mk(id, conv, content, seq);
        storage.insert_chunk(&chunk, &v).unwrap();
        engine.insert_chunk(chunk.id.clone(), v);
    }
    let search = Arc::new(RwLock::new(engine));
    let q = [1.0f32, 0.0, 0.0, 0.0];

    let out = reflect_on_past_with_vec(
        &storage,
        &search,
        &q,
        "topic",
        2,
        0.3,
        Some("all"),
        0,
        true,
        ConsumptionMode::Full,
        false,
    )
    .await
    .unwrap();
    assert!(
        out.contains("<id>chunk-valid-a</id>") && out.contains("<id>chunk-valid-b</id>"),
        "adaptive refetch must backfill valid chunks from beyond the all-demoted first window:\n{out}"
    );
    assert!(
        !out.contains("<id>chunk-demoted-"),
        "no demoted chunk may occupy a slot the refetched valid pair can fill:\n{out}"
    );
}

// --- exact conv-tag lookup (episode-index handle fix) ---
// Live failure 2026-07-08: csr_reflect_on_past("conv_0b68eace-…") went
// through semantic embedding and returned unrelated cross-project noise.
// A conv handle must resolve by tag, never by embedding.

#[test]
fn conv_handle_extracts_uuid() {
    assert_eq!(
        extract_conv_id("conv_0b68eace-3e21-4841-9567-58d038cd89d7"),
        Some("0b68eace-3e21-4841-9567-58d038cd89d7")
    );
}

#[test]
fn bare_uuid_extracts() {
    assert_eq!(
        extract_conv_id("0b68eace-3e21-4841-9567-58d038cd89d7"),
        Some("0b68eace-3e21-4841-9567-58d038cd89d7")
    );
}

#[test]
fn conv_handle_inside_sentence_extracts() {
    assert_eq!(
        extract_conv_id("full state: conv_0b68eace-3e21-4841-9567-58d038cd89d7 please"),
        Some("0b68eace-3e21-4841-9567-58d038cd89d7")
    );
}

#[test]
fn plain_text_does_not_extract() {
    assert_eq!(extract_conv_id("that docker issue we fixed"), None);
    assert_eq!(extract_conv_id("convention for naming conversations"), None);
    assert_eq!(extract_conv_id("conv_not-a-real-uuid"), None);
}

#[tokio::test]
async fn why_rendered_receipt_resolves_family_symbol_and_preserves_all_routes() {
    // Only the base project directory exists, so the `-csr-engine` namespace is an
    // alias of it; the real `$HOME/projects` plays no part.
    let projects = tempfile::tempdir().unwrap();
    std::fs::create_dir(projects.path().join("claude-self-reflect")).unwrap();
    let _root = crate::search::cross_project::ProjectsRootGuard::set(projects.path());
    let storage = Arc::new(Storage::open_memory().unwrap());
    let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
    let query = "why does `retire_missing_nodes` preserve attribution";
    let query_vec = embeddings.embed_single(query).unwrap();
    let base_project = "claude-self-reflect";
    let alias_project = "claude-self-reflect-csr-engine";

    let chunks = [
        crate::import::ConversationChunk {
            id: "why-base-chunk".into(),
            conversation_id: "why-base-conv".into(),
            project_name: base_project.into(),
            timestamp: "2026-08-01T00:00:00Z".into(),
            content: "general provenance discussion".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        },
        crate::import::ConversationChunk {
            id: "why-symbol-chunk".into(),
            conversation_id: "why-symbol-conv".into(),
            project_name: alias_project.into(),
            timestamp: "2026-08-02T00:00:00Z".into(),
            content: "retire_missing_nodes preserves attribution provenance".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        },
    ];
    let mut engine = SearchEngine::new(query_vec.len());
    for chunk in &chunks {
        storage.insert_chunk(chunk, &query_vec).unwrap();
        engine.insert_chunk(chunk.id.clone(), query_vec.clone());
    }
    storage
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO code_nodes
                     (id, repo, project, file, lang, kind, name, fqname,
                      first_conv_id, last_conv_id, last_session_id)
                     VALUES (?1, 'repo', ?2, 'src/search.rs', 'rust', 'function',
                             'retire_missing_nodes',
                             'search::retire_missing_nodes',
                             'legacy-wrong-conv', 'legacy-wrong-conv', 'legacy-wrong-conv')",
                rusqlite::params!["why-symbol-node", alias_project],
            )?;
            conn.execute(
                "INSERT INTO code_node_attribution
                     (node_id, channel, source_id, observed_ts, evidence)
                     VALUES (?1, 'transcript', ?2, '2026-08-02T00:00:00Z', 'coedit_event')",
                rusqlite::params!["why-symbol-node", "why-symbol-conv"],
            )?;
            Ok(())
        })
        .unwrap();

    let rendered = why(
        &storage,
        &embeddings,
        &Arc::new(RwLock::new(engine)),
        query,
        Some(base_project),
        &crate::search::reinstatement::ReinstateConfig::default(),
    )
    .await
    .unwrap();

    assert!(
        rendered.contains("scope=claude-self-reflect,claude-self-reflect-csr-engine"),
        "family scope must be visible in the receipt:\n{rendered}"
    );
    assert!(
        rendered.contains("symbols matched=1 [retire_missing_nodes]"),
        "the exact query identifier must resolve across the family:\n{rendered}"
    );
    assert!(rendered.contains("conv_why-symbol-conv:"), "{rendered}");
    assert!(
        rendered.contains("reached_by=seed+blend+graph"),
        "dedup must preserve both routes to the same evidence:\n{rendered}"
    );
    assert!(
        rendered.contains("graph walks=1 accepted=1 surfaced=1"),
        "walked, accepted, and surfaced must be separate rendered counters:\n{rendered}"
    );
}

/// W3-S finding 2: `csr_search_insights` used to filter on an exact
/// project-label match (`get_chunk_ids_for_project`), unlike
/// `csr_reflect_on_past`/`csr_quick_check` which widen to the project
/// family. A session under `claude-self-reflect` would silently lose its
/// `claude-self-reflect-csr-engine` chunks here while keeping them in
/// `csr_reflect_on_past`. Tempdir projects root (only the base project
/// dir present) keeps alias resolution off this machine's real `$HOME`.
/// Revert `family_chunk_ids_at_root` back to an exact-match id set and
/// this fails: `total_matches` drops from 2 to 1.
#[tokio::test]
async fn search_insights_widens_to_the_project_family() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
    let query = "codex blocked revival deploy blocker";
    let query_vec = embeddings.embed_single(query).unwrap();

    let projects_root = tempfile::tempdir().unwrap();
    let base_project = "acme-base";
    let alias_project = "acme-base-csr-engine";
    std::fs::create_dir_all(projects_root.path().join(base_project)).unwrap();
    // alias_project's directory is deliberately absent: that's what makes
    // it an alias rather than a genuine sibling repo.

    let rows = [
        ("base-chunk", base_project),
        ("alias-chunk", alias_project),
        ("unrelated-chunk", "unrelated-project"),
    ];
    let mut engine = SearchEngine::new(query_vec.len());
    for (id, project) in rows {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: format!("conv-{id}"),
            project_name: project.into(),
            timestamp: "2026-08-01T00:00:00Z".into(),
            content: "codex blocked revival deploy blocker".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &query_vec).unwrap();
        engine.insert_chunk(chunk.id, query_vec.clone());
    }
    let search = Arc::new(RwLock::new(engine));

    let xml = search_insights_at_root(
        &storage,
        &embeddings,
        &search,
        query,
        Some(base_project),
        Some(projects_root.path()),
    )
    .await
    .unwrap();

    assert!(
        xml.contains("<total_matches>2</total_matches>"),
        "expected the base project plus its alias, not the unrelated project:\n{xml}"
    );
}

/// Same finding as `search_insights_widens_to_the_project_family`, for
/// `csr_search_by_concept`.
#[tokio::test]
async fn search_by_concept_widens_to_the_project_family() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
    let query = "codex blocked revival deploy blocker";
    let query_vec = embeddings.embed_single(query).unwrap();

    let projects_root = tempfile::tempdir().unwrap();
    let base_project = "acme-base";
    let alias_project = "acme-base-csr-engine";
    std::fs::create_dir_all(projects_root.path().join(base_project)).unwrap();

    let rows = [
        ("base-chunk", base_project),
        ("alias-chunk", alias_project),
        ("unrelated-chunk", "unrelated-project"),
    ];
    let mut engine = SearchEngine::new(query_vec.len());
    for (id, project) in rows {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: format!("conv-{id}"),
            project_name: project.into(),
            timestamp: "2026-08-01T00:00:00Z".into(),
            content: "codex blocked revival deploy blocker".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &query_vec).unwrap();
        engine.insert_chunk(chunk.id, query_vec.clone());
    }
    let search = Arc::new(RwLock::new(engine));

    let xml = search_by_concept_at_root(
        &storage,
        &embeddings,
        &search,
        query,
        10,
        Some(base_project),
        Some(projects_root.path()),
    )
    .await
    .unwrap();

    assert!(
        xml.contains("alias-chunk"),
        "family alias chunk missing from search_by_concept output:\n{xml}"
    );
    assert!(
        !xml.contains("unrelated-chunk"),
        "unrelated-project chunk leaked into search_by_concept output:\n{xml}"
    );
}

/// W3-S finding 2: the code-graph ledger (`code_nodes`/`code_evolution`)
/// filters by an EXACT `project` match with no family-alias widening.
/// Before `project` defaulting existed, an omitted project meant `""`
/// (fully unscoped) — every project's nodes were visible. If an omitted
/// project were instead defaulted to a specific project name the way the
/// chunk-search tools now default (`resolve_search_scope`), this ledger
/// would silently narrow to just that literal name and drop every node
/// filed under a family alias. `search_by_file` deliberately does not
/// default an omitted/`"all"` project at all — it stays `""` — so this
/// pins that invariant: a node filed under an entirely different project
/// than any plausible default is still visible when `project` is
/// omitted.
#[tokio::test]
async fn search_by_file_omitted_project_is_not_narrowed_to_a_default_scope() {
    use crate::storage::codegraph::NodeRow;

    let storage = Arc::new(Storage::open_memory().unwrap());
    // Neither "claude-self-reflect" nor any of its aliases: stands in
    // for "whatever project a CLAUDE_PROJECT_DIR default-scope
    // resolution would have produced, which must not gate this ledger".
    let stored_project = "claude-self-reflect-csr-engine";
    storage
        .upsert_code_node(&NodeRow {
            id: "node-correlate-fts-hit".into(),
            project: stored_project.into(),
            file: "src/import/plans.rs".into(),
            kind: "function".into(),
            name: "correlate_fts_hit".into(),
            ..NodeRow::default()
        })
        .unwrap();

    let xml = search_by_file(&storage, "src/import/plans.rs", 10, None)
        .await
        .unwrap();
    assert!(
        xml.contains("correlate_fts_hit"),
        "omitted project must not be narrowed to a default scope that \
             can never match a family alias's exact project name:\n{xml}"
    );

    // "all" must behave identically to omitted.
    let xml_all = search_by_file(&storage, "src/import/plans.rs", 10, Some("all"))
        .await
        .unwrap();
    assert!(xml_all.contains("correlate_fts_hit"), "{xml_all}");

    // An explicit, unrelated project still narrows exactly, unaffected.
    let xml_explicit = search_by_file(
        &storage,
        "src/import/plans.rs",
        10,
        Some("some-other-project"),
    )
    .await
    .unwrap();
    assert!(
        !xml_explicit.contains("correlate_fts_hit"),
        "an explicit, non-matching project must still narrow the ledger:\n{xml_explicit}"
    );
}

#[tokio::test]
async fn why_rendered_receipt_reports_dangling_episode_without_a_fake_hop() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
    let query = "why provenance episodes can abstain safely";
    let query_vec = embeddings.embed_single(query).unwrap();
    let chunk = crate::import::ConversationChunk {
        id: "episode-current-chunk".into(),
        conversation_id: "episode-current".into(),
        project_name: "claude-self-reflect".into(),
        timestamp: "2026-08-03T00:00:00Z".into(),
        content: "provenance episodes abstain safely".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::User,
        seq: 0,
        is_sidechain: false,
    };
    storage.insert_chunk(&chunk, &query_vec).unwrap();
    storage
        .insert_reflection(
            "episode-current-reflection",
            &serde_json::json!({ "prev_episode_id": "missing-episode-target" }).to_string(),
            &[
                "session_episode".to_string(),
                "conv_episode-current".to_string(),
            ],
            &[],
        )
        .unwrap();
    let mut engine = SearchEngine::new(query_vec.len());
    engine.insert_chunk(chunk.id.clone(), query_vec);

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
        rendered.contains(
            "episodes links=1 resolved=0 accepted=0 surfaced=0 (below-cut=0, below-threshold=0, dangling=1, out-of-scope=0, unembedded=0)"
        ),
        "dangling targets must remain an explicit abstention:\n{rendered}"
    );
    assert!(
        !rendered.contains("reached_by=episode"),
        "a dangling target must never fabricate episode evidence:\n{rendered}"
    );
}

#[tokio::test]
async fn why_rendered_receipt_selects_three_distinct_semantic_conversations() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
    let query = "why semantic seeds remain distinct conversations";
    let query_vec = embeddings.embed_single(query).unwrap();
    let rows = [
        ("seed-a-1", "seed-conv-a"),
        ("seed-a-2", "seed-conv-a"),
        ("seed-b", "seed-conv-b"),
        ("seed-c", "seed-conv-c"),
    ];
    let mut engine = SearchEngine::new(query_vec.len());
    for (seq, (id, conv)) in rows.into_iter().enumerate() {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conv.into(),
            project_name: "claude-self-reflect".into(),
            timestamp: format!("2026-08-0{}T00:00:00Z", seq + 1),
            content: format!("semantic evidence from {conv}"),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &query_vec).unwrap();
        engine.insert_chunk(chunk.id, query_vec.clone());
    }

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
        rendered.contains("seeds selected=3 conversations=3 surfaced=3"),
        "multiple chunks from one conversation must consume one seed slot:\n{rendered}"
    );
}

#[tokio::test]
async fn why_rendered_receipt_surfaces_resolved_episode_under_conditional_quota() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
    let query = "why episode provenance remains reachable";
    let query_vec = embeddings.embed_single(query).unwrap();
    let mut orthogonal = vec![0.0; query_vec.len()];
    orthogonal[0] = -query_vec[1];
    orthogonal[1] = query_vec[0];
    let orthogonal_norm = orthogonal.iter().map(|v| v * v).sum::<f32>().sqrt();
    for value in &mut orthogonal {
        *value /= orthogonal_norm;
    }
    let episode_vec: Vec<f32> = query_vec
        .iter()
        .zip(orthogonal)
        .map(|(query_value, orthogonal_value)| {
            0.40 * query_value + (1.0_f32 - 0.40_f32.powi(2)).sqrt() * orthogonal_value
        })
        .collect();
    let chunks = [
        ("episode-seed-chunk", "episode-seed-conv", query_vec.clone()),
        ("episode-target-chunk", "episode-target-conv", episode_vec),
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
            "episode-positive-reflection",
            &serde_json::json!({ "prev_episode_id": "episode-target-conv" }).to_string(),
            &[
                "session_episode".to_string(),
                "conv_episode-seed-conv".to_string(),
            ],
            &[],
        )
        .unwrap();
    let cfg = crate::search::reinstatement::ReinstateConfig {
        k: 1,
        ..crate::search::reinstatement::ReinstateConfig::default()
    };

    let rendered = why(
        &storage,
        &embeddings,
        &Arc::new(RwLock::new(engine)),
        query,
        Some("claude-self-reflect"),
        &cfg,
    )
    .await
    .unwrap();

    assert!(rendered.contains("conv_episode-target-conv:"), "{rendered}");
    assert!(
        rendered.contains("reached_by=blend+episode"),
        "the episode route must survive even when blend also reached the chunk:\n{rendered}"
    );
    assert!(
        rendered.contains(
            "episodes links=1 resolved=1 accepted=1 surfaced=1 (below-cut=0, below-threshold=0, dangling=0, out-of-scope=0, unembedded=0)"
        ),
        "a valid episode above the structural floor must receive the conditional slot:\n{rendered}"
    );
}
