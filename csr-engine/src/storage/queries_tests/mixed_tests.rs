use super::*;

#[test]
fn list_project_names_empty_prefix_matches_the_distinct_scan() {
    let conn = mem();
    assert!(list_project_names(&conn, "", i64::MAX as usize)
        .unwrap()
        .is_empty());
    let names = [
        "",
        "a",
        "A",
        "a_b",
        "a%b",
        "a-b",
        "ab",
        "claude-self-reflect",
        "Zed",
        "zed",
        "é",
        "日本",
        "with space",
    ];
    for (n, name) in names.iter().cycle().take(40).enumerate() {
        conn.execute(
            "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, message_count)
                 VALUES (?1, 'c', ?2, '2026-01-01T00:00:00Z', 'x', 1)",
            params![format!("id-{n}"), name],
        )
        .unwrap();
    }
    for limit in [0_i64, 1, 3, 7, 13, 100, i64::MAX] {
        assert_eq!(
            list_project_names(&conn, "", limit as usize).unwrap(),
            project_names_oracle(&conn, limit),
            "limit {limit}"
        );
    }
}

#[test]
fn recent_sessions_match_the_whole_project_scan_they_replace() {
    let conn = mem();
    // Seven conversations over two projects, interleaved in time, two chunks
    // each at distinct timestamps (no ties to order arbitrarily); one
    // headless; one conversation with chunks in both projects.
    for conv in 0..7 {
        for n in 0..2 {
            let mut chunk = test_chunk(&format!("r{conv}-{n}"), "content");
            chunk.conversation_id = format!("conv-{conv}");
            chunk.project_name = if conv % 3 == 0 { "proj-b" } else { "proj-a" }.into();
            chunk.summary = Some(format!("summary {conv}-{n}"));
            chunk.message_count = 1 + n;
            let day = 1 + (conv * 5 + n * 11) % 27;
            chunk.timestamp = format!("2026-08-{day:02}T00:00:00Z");
            insert_chunk(&conn, &chunk, &[0.1; 4]).unwrap();
        }
    }
    let mut straddler = test_chunk("r-straddle", "content");
    straddler.conversation_id = "conv-1".into();
    straddler.project_name = "proj-b".into();
    straddler.timestamp = "2026-08-30T00:00:00Z".into();
    insert_chunk(&conn, &straddler, &[0.1; 4]).unwrap();
    conn.execute(
        "INSERT INTO conversation_entrypoint (conversation_id, entrypoint)
             VALUES ('conv-2', 'sdk-cli')",
        [],
    )
    .unwrap();
    // Four conversations whose newest chunk has the same timestamp, newer
    // than all the rest, inserted in reverse id order: which one a LIMIT 1
    // returns must not depend on insertion or index order.
    for (n, id) in ["tie-d", "tie-c", "tie-b", "tie-a"].into_iter().enumerate() {
        for half in 0..2 {
            let mut chunk = test_chunk(&format!("{id}-{half}"), "content");
            chunk.conversation_id = id.into();
            chunk.project_name = "proj-a".into();
            chunk.timestamp = if half == 0 {
                "2026-09-15T00:00:00Z".into()
            } else {
                format!("2026-09-0{}T00:00:00Z", 1 + n)
            };
            insert_chunk(&conn, &chunk, &[0.1; 4]).unwrap();
        }
    }

    for project in [Some("proj-a"), Some("proj-b"), Some("absent"), None] {
        for exclude_headless in [false, true] {
            for limit in [1, 2, 3, 10] {
                let got: Vec<_> = recent_sessions(&conn, limit, project, exclude_headless)
                    .unwrap()
                    .into_iter()
                    .map(|s| {
                        (
                            s.conversation_id,
                            s.timestamp,
                            s.total_messages as i64,
                            s.chunk_count as i64,
                            s.summary,
                        )
                    })
                    .collect();
                assert_eq!(
                    got,
                    recent_sessions_oracle(&conn, limit, project, exclude_headless),
                    "project {project:?} headless-excluded {exclude_headless} limit {limit}"
                );
            }
        }
    }
    // A tie is decided by conversation id, whatever the limit.
    for limit in 1..=4 {
        let ids: Vec<String> = recent_sessions(&conn, limit, Some("proj-a"), false)
            .unwrap()
            .into_iter()
            .map(|s| s.conversation_id)
            .collect();
        assert_eq!(
            ids,
            ["tie-a", "tie-b", "tie-c", "tie-d"][..limit],
            "limit {limit}"
        );
    }
    // The headless run is skipped only when asked.
    let all = recent_sessions(&conn, 10, Some("proj-a"), false).unwrap();
    assert!(all.iter().any(|s| s.conversation_id == "conv-2"));
    let interactive = recent_sessions(&conn, 10, Some("proj-a"), true).unwrap();
    assert!(interactive.iter().all(|s| s.conversation_id != "conv-2"));
}

#[test]
fn unenriched_conversations_match_the_grouped_join_they_replace() {
    let conn = mem();
    // Nine conversations, one with two transcript files, one imported
    // but chunkless, and every enrichment status.
    for conv in 0..9 {
        // Newest chunk day 20 + 7c mod 9: distinct per conversation
        // (no ties to order arbitrarily), not insertion order.
        for (n, day) in [(0, conv), (1, 20 + conv * 7 % 9)] {
            let mut chunk = test_chunk(&format!("c{conv}-{n}"), "content");
            chunk.conversation_id = format!("conv-{conv}");
            chunk.timestamp = format!("2026-08-{:02}T00:00:00Z", 1 + day);
            insert_chunk(&conn, &chunk, &[0.1; 4]).unwrap();
        }
        upsert_import_state_explicit(
            &conn,
            &format!("/p/conv-{conv}.jsonl"),
            &format!("conv-{conv}"),
            2,
            "2026-09-01T00:00:00Z",
        )
        .unwrap();
    }
    upsert_import_state_explicit(&conn, "/p/sub/conv-4.jsonl", "conv-4", 1, "x").unwrap();
    upsert_import_state_explicit(&conn, "/p/empty.jsonl", "empty", 0, "x").unwrap();
    mark_enrichment_completed(&conn, "conv-1", "extracted_v3", "r1").unwrap();
    mark_enrichment_failed(&conn, "conv-2", "extracted_v3", "boom").unwrap();
    mark_enrichment_completed(&conn, "conv-5", "ratification", "r5").unwrap();
    conn.execute(
        "INSERT INTO enrichment_state (conversation_id, enrichment_type, status)
             VALUES ('conv-7', 'extracted_v3', 'unavailable')",
        [],
    )
    .unwrap();

    for kind in ["extracted_v3", "ratification", "ai_narrative"] {
        for limit in [1, 3, 5, 50] {
            let oracle: Vec<(String, String)> = conn
                .prepare(UNENRICHED_ORACLE)
                .unwrap()
                .query_map(params![kind, limit as i64], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            let rewritten = get_unenriched_conversations(&conn, kind, limit).unwrap();
            // Dedupe before LIMIT so duplicate paths cannot displace a
            // different conversation from the requested batch size.
            let order = |rows: &[(String, String)]| -> Vec<String> {
                rows.iter().map(|(conv, _)| conv.clone()).collect()
            };
            assert_eq!(order(&rewritten), order(&oracle), "{kind} limit {limit}");
            let mut rows = rewritten.clone();
            let mut expected = oracle.clone();
            rows.sort();
            expected.sort();
            assert_eq!(rows, expected, "{kind} limit {limit}");
        }
    }
    let all = get_unenriched_conversations(&conn, "extracted_v3", 50).unwrap();
    assert!(all
        .iter()
        .all(|(conv, _)| conv != "conv-1" && conv != "conv-7"));
    assert!(
        all.iter().any(|(conv, _)| conv == "conv-2"),
        "failed retries"
    );
    assert!(all.iter().all(|(conv, _)| conv != "empty"), "needs chunks");
    assert_eq!(all.iter().filter(|(conv, _)| conv == "conv-4").count(), 1);
}

#[test]
fn fts5_search_reflections_returns_bm25_order_with_zero_based_ordinals() {
    let conn = mem();
    insert_reflection(&conn, "dense", "needle needle needle", &[], &[0.1; 4]).unwrap();
    insert_reflection(
        &conn,
        "sparse",
        "needle with several unrelated filler words",
        &[],
        &[0.2; 4],
    )
    .unwrap();
    insert_reflection(&conn, "miss", "nothing relevant here", &[], &[0.3; 4]).unwrap();

    let hits = fts5_search_reflections(&conn, "needle", 10).unwrap();

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].0, "dense");
    assert_eq!(hits[0].1, 0);
    assert_eq!(hits[1].0, "sparse");
    assert_eq!(hits[1].1, 1);
    assert!(
        hits[0].2 <= hits[1].2,
        "bm25 must be ascending (better first)"
    );
    assert!(fts5_search_reflections(&conn, "!!", 10).unwrap().is_empty());
}

/// Opus review finding (high, W3-K spec (e)): a short or single-token
/// prefix matches hundreds of unrelated rows and must not be promoted —
/// live receipts: `Session` alone matched 129 reflections, `we` matched
/// 5 unrelated stories opening with "We ..."/"we ...". Below the
/// specificity floor (min tokens, min chars) the matcher must return
/// nothing rather than let `promote_exact_prefix_reflections` shove
/// every hit to the front of the page.
#[test]
fn short_or_single_token_prefix_is_not_promoted() {
    let conn = mem();
    for i in 0..5 {
        insert_reflection(
            &conn,
            &format!("session-{i}"),
            &format!("Session note number {i} about something"),
            &[],
            &[0.1; 4],
        )
        .unwrap();
    }
    assert!(
        fts5_reflection_prefix_matches(&conn, "Session", 10)
            .unwrap()
            .is_empty(),
        "a single-token 7-char prefix must not promote 5 unrelated rows"
    );
    for (i, opener) in ["We integrated manychat", "we are seeing wild swings"]
        .iter()
        .enumerate()
    {
        insert_reflection(
            &conn,
            &format!("we-{i}"),
            &format!("{opener} across the campaign"),
            &[],
            &[0.1; 4],
        )
        .unwrap();
    }
    assert!(
        fts5_reflection_prefix_matches(&conn, "we", 10)
            .unwrap()
            .is_empty(),
        "a 2-char prefix must not promote"
    );
}

/// Same finding: a prefix that clears the specificity floor but still
/// matches more than the small promotion cap must not be promoted either
/// — let bm25/RRF fusion decide instead of forcing every match to the
/// front of the page.
#[test]
fn specific_prefix_over_the_promotion_cap_is_not_promoted() {
    let conn = mem();
    for i in 0..6 {
        insert_reflection(
            &conn,
            &format!("archive-{i}"),
            &format!("MEMORY-ARCHIVE common project note number {i}"),
            &[],
            &[0.1; 4],
        )
        .unwrap();
    }
    assert!(
        fts5_reflection_prefix_matches(&conn, "MEMORY-ARCHIVE common project", 10)
            .unwrap()
            .is_empty(),
        "6 matches is over the small promotion cap"
    );
}

/// The positive case this guard must not break: a specific, multi-token,
/// long-enough prefix with a small number of matches is still promoted,
/// newest first — the exact shape
/// `exact_prefix_reflection_reaches_page_one_over_many_keyword_matching_chunks`
/// (mcp::tools) depends on.
#[test]
fn specific_prefix_within_the_cap_is_promoted_newest_first() {
    let conn = mem();
    insert_reflection(
        &conn,
        "older",
        "MEMORY-ARCHIVE project-x-y-z memory number 0",
        &[],
        &[0.1; 4],
    )
    .unwrap();
    insert_reflection(
        &conn,
        "newer",
        "MEMORY-ARCHIVE project-x-y-z memory number 1",
        &[],
        &[0.1; 4],
    )
    .unwrap();
    conn.execute(
        "UPDATE reflections SET timestamp = '2020-01-01T00:00:00Z' WHERE id = 'older'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE reflections SET timestamp = '2026-01-01T00:00:00Z' WHERE id = 'newer'",
        [],
    )
    .unwrap();
    let matches =
        fts5_reflection_prefix_matches(&conn, "MEMORY-ARCHIVE project-x-y-z", 10).unwrap();
    assert_eq!(matches, vec!["newer".to_string(), "older".to_string()]);
}

/// A session story opens with the request that started its session, so a
/// prefix hit on one is the question coming back. Only a deliberate memory
/// is promoted, and a story neither takes the promotion nor counts toward
/// the cap that withholds it.
#[test]
fn a_derived_row_is_not_promoted_by_its_prefix() {
    let conn = mem();
    let derived = |id: &str, tag: &str| {
        insert_reflection(
            &conn,
            id,
            "why did we drop Docker and Qdrant for a single Rust binary? --- Signature: {}",
            &[tag.to_string(), "project_p".to_string()],
            &[0.1; 4],
        )
        .unwrap();
    };
    for (i, tag) in [
        "session_story",
        "session_episode",
        "session_briefing",
        "context_cache",
        "session_story",
    ]
    .iter()
    .enumerate()
    {
        derived(&format!("story_{i}"), tag);
    }
    insert_reflection(
        &conn,
        "extracted_v3_x",
        "why did we drop Docker and Qdrant? the extraction restates it",
        &["narrative_extracted_v3".to_string()],
        &[0.1; 4],
    )
    .unwrap();
    insert_reflection(
        &conn,
        "5eb5aebb-65ab-4fba-9917-86cb83845154",
        "why did we drop Docker and Qdrant: install burden",
        &["csr:memory".to_string()],
        &[0.1; 4],
    )
    .unwrap();
    let matches =
        fts5_reflection_prefix_matches(&conn, "why did we drop Docker and Qdrant", 10).unwrap();
    assert_eq!(
        matches,
        vec!["5eb5aebb-65ab-4fba-9917-86cb83845154".to_string()]
    );
}

/// Derived rows are excluded in SQL, before the fetch cap: a run of newer
/// derived prefix matches must not push an older explicit memory out of
/// the rows the filter ever sees.
#[test]
fn newer_derived_prefix_matches_do_not_starve_an_older_explicit_memory() {
    let conn = mem();
    insert_reflection(
        &conn,
        "5eb5aebb-65ab-4fba-9917-86cb83845154",
        "why did we drop Docker and Qdrant: install burden",
        &["csr:memory".to_string()],
        &[0.1; 4],
    )
    .unwrap();
    for i in 0..12 {
        std::thread::sleep(std::time::Duration::from_millis(2));
        insert_reflection(
            &conn,
            &format!("extracted_v3_{i}"),
            "why did we drop Docker and Qdrant? the extraction restates it",
            &["narrative_extracted_v3".to_string()],
            &[0.1; 4],
        )
        .unwrap();
    }
    let matches =
        fts5_reflection_prefix_matches(&conn, "why did we drop Docker and Qdrant", 10).unwrap();
    assert_eq!(
        matches,
        vec!["5eb5aebb-65ab-4fba-9917-86cb83845154".to_string()]
    );
}

/// A prefix containing LIKE metacharacters must not act as a wildcard —
/// `%` alone must never match every reflection.
#[test]
fn like_metacharacters_in_the_prefix_are_escaped() {
    let conn = mem();
    insert_reflection(
        &conn,
        "literal-percent",
        "% is a literal opener here for this specific memory note",
        &[],
        &[0.1; 4],
    )
    .unwrap();
    insert_reflection(
        &conn,
        "unrelated",
        "this row has nothing to do with the prefix at all",
        &[],
        &[0.1; 4],
    )
    .unwrap();
    // Below specificity floor by itself, so wrap it to reach the char
    // minimum while still testing that '%' is escaped rather than
    // acting as a wildcard: a literal-prefix query starting with '%'
    // must not match the unrelated row.
    let matches = fts5_reflection_prefix_matches(&conn, "% is a literal opener", 10).unwrap();
    assert_eq!(matches, vec!["literal-percent".to_string()]);
}

/// Word-boundary guard: a prefix must not match mid-token. The query is
/// a strict string-prefix of `mid-token`'s content ("campaign" is a
/// prefix of "campaigns") but stops inside the word, so it must not
/// count as a match; `exact` ends exactly at a word boundary (a space)
/// and must.
#[test]
fn prefix_match_requires_a_word_boundary() {
    let conn = mem();
    insert_reflection(
        &conn,
        "exact",
        "MEMORY-ARCHIVE anukriti-meta-campaign notes here",
        &[],
        &[0.1; 4],
    )
    .unwrap();
    insert_reflection(
        &conn,
        "mid-token",
        "MEMORY-ARCHIVE anukriti-meta-campaigns notes here",
        &[],
        &[0.1; 4],
    )
    .unwrap();
    let matches =
        fts5_reflection_prefix_matches(&conn, "MEMORY-ARCHIVE anukriti-meta-campaign", 10).unwrap();
    assert_eq!(matches, vec!["exact".to_string()]);
}

#[test]
fn reflections_fts_follows_replace_and_delete() {
    // `INSERT OR REPLACE` never fires the delete trigger for the row it
    // replaces, so the keyword index must be keyed by id, not rowid.
    let conn = mem();
    insert_reflection(&conn, "r1", "alpha beta", &[], &[0.1; 4]).unwrap();
    assert_eq!(
        fts5_search_reflections(&conn, "alpha", 10).unwrap().len(),
        1
    );

    insert_reflection(&conn, "r1", "gamma delta", &[], &[0.1; 4]).unwrap();
    assert!(fts5_search_reflections(&conn, "alpha", 10)
        .unwrap()
        .is_empty());
    let after = fts5_search_reflections(&conn, "gamma", 10).unwrap();
    assert_eq!(
        after.len(),
        1,
        "replacement must not leave a duplicate keyword row"
    );

    conn.execute(
        "DELETE FROM reflection_embeddings WHERE reflection_id = 'r1'",
        [],
    )
    .unwrap();
    conn.execute("DELETE FROM reflections WHERE id = 'r1'", [])
        .unwrap();
    assert!(fts5_search_reflections(&conn, "gamma", 10)
        .unwrap()
        .is_empty());
    let triggers: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'reflections_fts_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(triggers, 3);
}

#[test]
fn first_chunk_prefixes_matches_the_correlated_query() {
    let conn = mem();
    // (id, conversation, seq): out-of-order numbers, a numbered row inserted
    // after unnumbered ones, an all-unnumbered conversation, a single chunk.
    let rows: [(&str, &str, Option<i64>); 9] = [
        ("a3", "conv-a", Some(3)),
        ("a1", "conv-a", Some(1)),
        ("a2", "conv-a", Some(2)),
        ("b-null-1", "conv-b", None),
        ("b-null-2", "conv-b", None),
        ("c-null", "conv-c", None),
        ("c-num", "conv-c", Some(7)),
        ("d-only", "conv-d", Some(0)),
        ("e-tie-1", "conv-e", Some(5)),
    ];
    for (id, conversation, seq) in rows {
        let mut chunk = test_chunk(id, &format!("opening of {id} with a long enough text"));
        chunk.conversation_id = conversation.into();
        insert_chunk(&conn, &chunk, &[0.1; 4]).unwrap();
        conn.execute("UPDATE chunks SET seq = ?1 WHERE id = ?2", params![seq, id])
            .unwrap();
    }
    let mut chunk = test_chunk("e-tie-2", "second at the same seq");
    chunk.conversation_id = "conv-e".into();
    insert_chunk(&conn, &chunk, &[0.1; 4]).unwrap();
    conn.execute("UPDATE chunks SET seq = 5 WHERE id = 'e-tie-2'", [])
        .unwrap();

    let asked: Vec<String> = [
        "conv-a",
        "conv-b",
        "conv-c",
        "conv-d",
        "conv-e",
        "conv-none",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for prefix_chars in [5usize, 512] {
        let mut got = first_chunk_prefixes(&conn, &asked, prefix_chars).unwrap();
        got.sort();
        assert_eq!(
            got,
            first_chunk_prefixes_oracle(&conn, &asked, prefix_chars)
        );
        assert_eq!(got.len(), 5, "an unknown conversation is absent: {got:?}");
    }
    let got = first_chunk_prefixes(&conn, &asked, 512).unwrap();
    let by_conv: std::collections::HashMap<_, _> = got.into_iter().collect();
    assert!(by_conv["conv-a"].starts_with("opening of a1"));
    assert!(by_conv["conv-b"].starts_with("opening of b-null-1"));
    assert!(by_conv["conv-c"].starts_with("opening of c-num"));
    // A conversation named twice is read once.
    let twice = vec!["conv-a".to_string(), "conv-a".to_string()];
    assert_eq!(first_chunk_prefixes(&conn, &twice, 512).unwrap().len(), 1);
}

#[test]
fn fts5_search_returns_bm25_order_with_zero_based_ordinals() {
    let conn = mem();
    insert_chunk(
        &conn,
        &test_chunk("dense", "needle needle needle"),
        &[0.1; 4],
    )
    .unwrap();
    insert_chunk(
        &conn,
        &test_chunk("sparse", "needle with several unrelated filler words"),
        &[0.2; 4],
    )
    .unwrap();

    let hits = fts5_search(&conn, "needle", 10, None).unwrap();

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].0.id, "dense");
    assert_eq!(hits[0].1, 0);
    assert!(hits[0].2.is_finite());
    assert_eq!(hits[1].0.id, "sparse");
    assert_eq!(hits[1].1, 1);
    assert!(hits[1].2.is_finite());
    assert!(
        hits[0].2 <= hits[1].2,
        "FTS5 bm25 order must be ascending: {hits:?}"
    );
}

/// One ranking pass over several projects must hand each project exactly
/// the window a search of that project alone returns: same rows, order,
/// ordinals and bm25, with the cut at `limit` per project.
#[test]
fn fts5_search_projects_equals_one_search_per_project() {
    let conn = mem();
    for i in 0..14 {
        let project = ["alpha", "beta", "gamma"][i % 3];
        let mut chunk = test_chunk(
            &format!("c{i:02}"),
            &format!("needle {} {}", "needle ".repeat(i % 5), "filler ".repeat(i)),
        );
        chunk.project_name = project.into();
        insert_chunk(&conn, &chunk, &[0.1; 4]).unwrap();
    }
    let mut other = test_chunk("zz", "needle needle needle needle needle");
    other.project_name = "outsider".into();
    insert_chunk(&conn, &other, &[0.1; 4]).unwrap();

    for limit in [1, 3, 5, 50] {
        let projects = ["alpha", "beta", "gamma", "absent"];
        let together = fts5_search_projects(&conn, "needle filler", limit, &projects).unwrap();
        assert_eq!(together.len(), projects.len());
        for (project, window) in projects.iter().zip(&together) {
            let alone = fts5_search(&conn, "needle filler", limit, Some(project)).unwrap();
            let shape = |hits: &[(ConversationChunk, usize, f64)]| {
                hits.iter()
                    .map(|(chunk, ordinal, bm25)| (chunk.id.clone(), *ordinal, *bm25))
                    .collect::<Vec<_>>()
            };
            assert_eq!(shape(window), shape(&alone), "{project} at limit {limit}");
            assert!(window.iter().all(|(c, _, _)| c.project_name == *project));
        }
    }
    assert!(fts5_search_projects(&conn, "needle", 5, &["all"]).is_err());
    assert!(fts5_search_projects(&conn, "x", 5, &["alpha"])
        .unwrap()
        .iter()
        .all(Vec::is_empty));
}

#[test]
fn fts5_doc_count_stops_at_the_cap_and_expr_search_matches_the_text_search() {
    let conn = mem();
    for i in 0..5 {
        insert_chunk(
            &conn,
            &test_chunk(&format!("common-{i}"), "commonword filler"),
            &[0.1; 4],
        )
        .unwrap();
    }
    insert_chunk(
        &conn,
        &test_chunk("rare", "commonword raretoken"),
        &[0.2; 4],
    )
    .unwrap();

    assert_eq!(fts5_chunk_doc_count(&conn, "commonword", 3).unwrap(), 3);
    assert_eq!(fts5_chunk_doc_count(&conn, "commonword", 100).unwrap(), 6);
    assert_eq!(fts5_chunk_doc_count(&conn, "raretoken", 3).unwrap(), 1);
    assert_eq!(fts5_chunk_doc_count(&conn, "absentword", 3).unwrap(), 0);

    let words = fts5_query_words("raretoken a commonword");
    assert_eq!(words, ["raretoken", "commonword"]);
    assert_eq!(
        fts5_match_expr("raretoken a commonword").unwrap(),
        fts5_or_expr(&words)
    );
    let by_text = fts5_search(&conn, "raretoken", 5, None).unwrap();
    let by_expr =
        fts5_search_expr(&conn, &fts5_or_expr(&["raretoken".to_string()]), 5, None).unwrap();
    assert_eq!(by_text.len(), 1);
    assert_eq!(by_text[0].0.id, by_expr[0].0.id);
}

#[test]
fn a_path_is_one_phrase_that_a_chunk_naming_it_matches_and_scattered_words_do_not() {
    let conn = mem();
    let path = "csr-engine/src/storage/mod.rs";
    for (id, text) in [
        (
            "named",
            "I edited csr-engine/src/storage/mod.rs to add a column",
        ),
        (
            "scattered",
            "the storage engine in src of csr, a mod for rs files",
        ),
        ("other", "csr-engine/src/storage/queries.rs changed instead"),
    ] {
        insert_chunk(&conn, &test_chunk(id, text), &[0.1; 4]).unwrap();
    }

    // The old free-text route folds the path into one token no chunk holds.
    assert!(fts5_search(&conn, path, 10, None).unwrap().is_empty());

    let expr = fts5_path_expr(path).unwrap();
    assert_eq!(expr, "\"csr engine src storage mod rs\"");
    let ids: Vec<String> = fts5_search_expr(&conn, &expr, 10, None)
        .unwrap()
        .into_iter()
        .map(|(chunk, _, _)| chunk.id)
        .collect();
    assert_eq!(ids, ["named"]);

    let abs = fts5_path_expr("/Users/me/projects/app/src/a_b.rs").unwrap();
    assert_eq!(abs, "\"Users me projects app src a b rs\"");
    assert_eq!(fts5_path_expr("///"), None);
    assert_eq!(fts5_path_expr(""), None);
}

#[test]
fn free_text_words_still_drop_everything_but_alphanumerics_underscore_and_hyphen() {
    // fts5_query_words is shared with prompts and MCP search; the path fix
    // must not change it.
    assert_eq!(
        fts5_query_words("csr-engine/src/mod.rs a hello_world"),
        ["csr-enginesrcmodrs", "hello_world"]
    );
}

#[test]
fn chunk_reimport_preserves_rowids_and_replaces_one_fts_document() {
    // Break caught: using INSERT OR REPLACE for either TEXT-keyed table
    // deletes and reinserts its row, changing the implicit rowid. For
    // chunks that also leaves the old rowid indexed in FTS.
    let conn = mem();
    insert_chunk(&conn, &test_chunk("stable", "legacytoken"), &[0.1; 4]).unwrap();
    let chunk_rowid_before: i64 = conn
        .query_row("SELECT rowid FROM chunks WHERE id = 'stable'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let embedding_rowid_before: i64 = conn
        .query_row(
            "SELECT rowid FROM chunk_embeddings WHERE chunk_id = 'stable'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    insert_chunk(&conn, &test_chunk("stable", "replacementtoken"), &[0.2; 4]).unwrap();

    let chunk_rowid_after: i64 = conn
        .query_row("SELECT rowid FROM chunks WHERE id = 'stable'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let embedding_rowid_after: i64 = conn
        .query_row(
            "SELECT rowid FROM chunk_embeddings WHERE chunk_id = 'stable'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(chunk_rowid_after, chunk_rowid_before);
    assert_eq!(embedding_rowid_after, embedding_rowid_before);
    assert!(fts5_search(&conn, "legacytoken", 10, None)
        .unwrap()
        .is_empty());
    assert_eq!(
        fts5_search(&conn, "replacementtoken", 10, None).unwrap()[0]
            .0
            .id,
        "stable"
    );
    let indexed_documents: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunks_fts_docsize", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        indexed_documents, 1,
        "reimport must not append an orphan FTS row"
    );

    delete_chunks_for_conversation(&conn, "conv-fts").unwrap();
    assert!(fts5_search(&conn, "replacementtoken", 10, None)
        .unwrap()
        .is_empty());
    let indexed_documents: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunks_fts_docsize", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        indexed_documents, 0,
        "deleting the chunk must delete its FTS row"
    );
}

#[test]
fn external_fts_tracks_direct_chunk_updates_and_deletes() {
    // Break caught: external-content FTS has no automatic synchronization.
    // A writer that bypasses insert_chunk must still be covered.
    let conn = mem();
    conn.execute(
        "INSERT INTO chunks
                 (id, conversation_id, project_name, timestamp, content, message_count)
             VALUES ('direct', 'conv-direct', 'project-fts',
                     '2026-08-12T00:00:00Z', 'beforetoken', 1)",
        [],
    )
    .unwrap();
    assert_eq!(
        fts5_search(&conn, "beforetoken", 10, None).unwrap()[0].0.id,
        "direct"
    );

    conn.execute(
        "UPDATE chunks SET content = 'aftertoken' WHERE id = 'direct'",
        [],
    )
    .unwrap();
    assert!(fts5_search(&conn, "beforetoken", 10, None)
        .unwrap()
        .is_empty());
    assert_eq!(
        fts5_search(&conn, "aftertoken", 10, None).unwrap()[0].0.id,
        "direct"
    );

    conn.execute("DELETE FROM chunks WHERE id = 'direct'", [])
        .unwrap();
    assert!(fts5_search(&conn, "aftertoken", 10, None)
        .unwrap()
        .is_empty());
}

#[test]
fn test_narrative_usage_record_and_summary() {
    let conn = mem();
    let row = NarrativeUsageRow {
        call_site: "briefing".into(),
        model: "claude-haiku-4-5".into(),
        input_tokens: 1500,
        output_tokens: 300,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        duration_ms: 4200,
        success: true,
    };
    record_narrative_usage(&conn, &row).unwrap();
    record_narrative_usage(&conn, &row).unwrap();

    let s = narrative_usage_summary(&conn).unwrap();
    assert_eq!(s.calls_total, 2);
    assert_eq!(s.tokens_total, 3600);
    assert_eq!(s.calls_today, 2); // rows stamped 'now' are today
    assert_eq!(s.tokens_today, 3600);
    assert_eq!(s.cache_tokens_total, 0);
    assert_eq!(s.cache_tokens_today, 0);
    assert_eq!(s.last_model.as_deref(), Some("claude-haiku-4-5"));
}

#[test]
fn test_narrative_usage_summary_empty() {
    let conn = mem();
    let s = narrative_usage_summary(&conn).unwrap();
    assert_eq!(s.calls_total, 0);
    assert_eq!(s.cache_tokens_total, 0);
    assert_eq!(s.cache_tokens_today, 0);
    assert_eq!(s.last_model, None);
}

#[test]
fn test_get_ratification_scores_empty_input() {
    let conn = mem();
    let map = get_ratification_scores(&conn, &[]).unwrap();
    assert!(map.is_empty());
}

/// Regression: `get_chunks_by_ids` alone always defaults `author` to
/// `ToolResult` (chunks table has no author column), which silently starved
/// ratification's `build_digest` of operator turns in production even
/// though import correctly wrote `Speaker::User` rows into
/// `chunk_provenance`. `get_chunks_by_ids_with_provenance` must recover the
/// real author via the LEFT JOIN.
#[test]
fn get_chunks_by_ids_with_provenance_resolves_true_author() {
    let conn = mem();
    let chunk = ConversationChunk {
        id: "c1".into(),
        conversation_id: "conv-1".into(),
        project_name: "p".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        content: "fix the import bug".into(),
        message_count: 1,
        summary: None,
        author: Speaker::ToolResult, // irrelevant: insert_chunk doesn't persist author
        seq: 0,
        is_sidechain: false,
    };
    insert_chunk(&conn, &chunk, &[0.0; 4]).unwrap();
    insert_chunk_provenance(
        &conn,
        "c1",
        &ChunkProvenance {
            author: Speaker::User,
            source_conv_id: "conv-1".into(),
            supersedes: None,
        },
    )
    .unwrap();

    // Plain path (no join) always degrades to ToolResult.
    let plain = get_chunks_by_ids(&conn, &["c1".into()]).unwrap();
    assert_eq!(plain[0].author, Speaker::ToolResult);

    // Provenance-aware path recovers the true author.
    let with_prov = get_chunks_by_ids_with_provenance(&conn, &["c1".into()]).unwrap();
    assert_eq!(with_prov.len(), 1);
    assert_eq!(with_prov[0].author, Speaker::User);
    assert_eq!(with_prov[0].content, "fix the import bug");
}

/// A chunk with no `chunk_provenance` row at all must still degrade to
/// `ToolResult` (not error), matching `get_chunk_provenance`'s fallback.
#[test]
fn get_chunks_by_ids_with_provenance_defaults_when_no_row() {
    let conn = mem();
    let chunk = ConversationChunk {
        id: "c2".into(),
        conversation_id: "conv-2".into(),
        project_name: "p".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        content: "no provenance row".into(),
        message_count: 1,
        summary: None,
        author: Speaker::ToolResult,
        seq: 0,
        is_sidechain: false,
    };
    insert_chunk(&conn, &chunk, &[0.0; 4]).unwrap();

    let with_prov = get_chunks_by_ids_with_provenance(&conn, &["c2".into()]).unwrap();
    assert_eq!(with_prov.len(), 1);
    assert_eq!(with_prov[0].author, Speaker::ToolResult);
}

#[test]
fn test_upsert_and_get_ratification_score() {
    let conn = mem();
    let row = RatificationScoreRow {
        conversation_id: "c1".into(),
        score: 0.6,
        acts_json: r#"{"acts":[]}"#.into(),
        ledger_refs: None,
        extractor_version: "ratification-v1".into(),
    };
    upsert_ratification_score(&conn, &row).unwrap();
    assert_eq!(get_ratification_score(&conn, "c1").unwrap(), Some(0.6));
    let (count, avg) = ratification_summary(&conn).unwrap();
    assert_eq!(count, 1);
    assert!((avg - 0.6).abs() < 1e-6);
    let map = get_ratification_scores(&conn, &["c1".into(), "missing".into()]).unwrap();
    assert_eq!(map.get("c1"), Some(&0.6));
    assert!(!map.contains_key("missing"));
}

#[test]
fn a_story_remembers_how_much_of_the_session_it_covered() {
    let conn = mem();
    assert_eq!(story_messages_covered(&conn, "conv-s").unwrap(), None);
    // a story written before the count was kept covers an unknown part
    mark_enrichment_completed(&conn, "conv-s", "session_story", "story_conv-s").unwrap();
    assert_eq!(story_messages_covered(&conn, "conv-s").unwrap(), None);
    assert_eq!(story_told_through(&conn, "conv-s").unwrap(), None);
    mark_story_written(&conn, "conv-s", "story_conv-s", 300, None).unwrap();
    assert_eq!(story_messages_covered(&conn, "conv-s").unwrap(), Some(300));
    assert_eq!(story_told_through(&conn, "conv-s").unwrap(), None);
    let cutoff = chrono::DateTime::parse_from_rfc3339("2026-10-14T20:59:56Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    mark_story_written(&conn, "conv-s", "story_conv-s", 640, Some(cutoff)).unwrap();
    assert_eq!(story_messages_covered(&conn, "conv-s").unwrap(), Some(640));
    assert_eq!(
        story_told_through(&conn, "conv-s").unwrap().as_deref(),
        Some("2026-10-14T20:59:56.000Z")
    );
    assert!(is_conversation_enriched(&conn, "conv-s", "session_story").unwrap());
}

#[test]
fn test_reset_enrichment() {
    let conn = mem();
    mark_enrichment_completed(&conn, "conv-reset", "ratification", "x").unwrap();
    assert!(is_conversation_enriched(&conn, "conv-reset", "ratification").unwrap());

    reset_enrichment(&conn, "conv-reset", "ratification").unwrap();
    assert!(!is_conversation_enriched(&conn, "conv-reset", "ratification").unwrap());

    // Idempotent: deleting an absent row is Ok
    reset_enrichment(&conn, "conv-reset", "ratification").unwrap();
    assert!(!is_conversation_enriched(&conn, "conv-reset", "ratification").unwrap());
}

#[test]
fn test_narrative_usage_summary_cache_tokens() {
    let conn = mem();
    let row = NarrativeUsageRow {
        call_site: "briefing".into(),
        model: "claude-haiku-4-5".into(),
        input_tokens: 100,
        output_tokens: 57,
        cache_read_tokens: 5000,
        cache_creation_tokens: 1200,
        duration_ms: 4200,
        success: true,
    };
    record_narrative_usage(&conn, &row).unwrap();

    let s = narrative_usage_summary(&conn).unwrap();
    // Fresh tokens only (input + output) — cache must not be folded in.
    assert_eq!(s.tokens_total, 157);
    assert_eq!(s.tokens_today, 157);
    // Cache read + creation tracked separately.
    assert_eq!(s.cache_tokens_total, 6200);
    assert_eq!(s.cache_tokens_today, 6200);
    assert_eq!(s.calls_total, 1);
    assert_eq!(s.calls_today, 1);
}

#[test]
fn test_narrative_usage_failed_call_still_counts() {
    let conn = mem();
    let row = NarrativeUsageRow {
        call_site: "story".into(),
        model: "unknown".into(),
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        duration_ms: 150,
        success: false,
    };
    record_narrative_usage(&conn, &row).unwrap();

    let s = narrative_usage_summary(&conn).unwrap();
    // Failed/attempted calls still count as calls — "calls" means attempts, not successes.
    assert_eq!(s.calls_total, 1);
    assert_eq!(s.calls_today, 1);
    assert_eq!(s.tokens_total, 0);
}

#[test]
fn set_chunk_saga_columns_roundtrip() {
    let conn = mem();
    let chunk = ConversationChunk {
        id: "chunk-saga-1".into(),
        conversation_id: "conv-1".into(),
        project_name: "proj".into(),
        timestamp: "2026-06-10T12:00:00Z".into(),
        content: "hello saga".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::User,
        seq: 0,
        is_sidechain: false,
    };
    insert_chunk(&conn, &chunk, &[0.1; 384]).unwrap();
    set_chunk_saga_columns(&conn, "chunk-saga-1", 7, true).unwrap();
    let (seq, is_sidechain): (i64, i64) = conn
        .query_row(
            "SELECT seq, is_sidechain FROM chunks WHERE id = ?1",
            params!["chunk-saga-1"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(seq, 7);
    assert_eq!(is_sidechain, 1);
}

#[test]
fn sessions_for_file_filters_by_project() {
    let conn = mem();
    seed_cross_project_shared_file(&conn);

    // Scoped to proj_a: must not leak sess_b from proj_b (same file path).
    // Only other-project row exists, so filtered result is empty.
    let got = sessions_for_file(&conn, "shared.rs", "sess_a", Some("proj_a"), 10).unwrap();
    assert!(
        !got.contains(&"sess_b".to_string()),
        "project-scoped lookup must not return other-project sessions: {got:?}"
    );
}

#[test]
fn sessions_for_file_none_project_is_unscoped() {
    let conn = mem();
    seed_cross_project_shared_file(&conn);

    // Unscoped (None): back-compat — can see sess_b across projects.
    let got = sessions_for_file(&conn, "shared.rs", "sess_a", None, 10).unwrap();
    assert!(
        got.contains(&"sess_b".to_string()),
        "unscoped lookup must return other-project sessions: {got:?}"
    );
}

#[test]
fn sessions_for_file_accepts_every_namespace_in_resolved_family() {
    let conn = mem();
    seed_cross_project_shared_file(&conn);

    let got = sessions_for_file_in_projects(
        &conn,
        "shared.rs",
        "sess_a",
        &["proj_a".to_string(), "proj_b".to_string()],
        10,
    )
    .unwrap();
    assert_eq!(got, vec!["sess_b"]);
}

#[test]
fn memory_registry_upsert_and_lookup_by_origin_session() {
    let conn = mem();
    upsert_memory_registry_batch(
        &conn,
        &[
            sample_memory_row("/mem/a.md", "proj", "a", Some("sess-1"), 1),
            sample_memory_row("/mem/b.md", "proj", "b", None, 1),
        ],
    )
    .unwrap();

    let hit = get_memory_registry_by_origin_session(&conn, "sess-1").unwrap();
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].file_path, "/mem/a.md");
    assert_eq!(hit[0].origin_session_id.as_deref(), Some("sess-1"));

    let miss = get_memory_registry_by_origin_session(&conn, "sess-missing").unwrap();
    assert!(miss.is_empty());
}

#[test]
fn memory_registry_upsert_on_conflict_overwrites_all_fields() {
    let conn = mem();
    upsert_memory_registry_batch(
        &conn,
        &[sample_memory_row(
            "/mem/a.md",
            "proj",
            "first",
            Some("s1"),
            1,
        )],
    )
    .unwrap();

    let mut second = sample_memory_row("/mem/a.md", "proj", "second", Some("s2"), 9);
    second.description = Some("updated-desc".into());
    second.content_hash = "hash-updated".into();
    upsert_memory_registry_batch(&conn, &[second.clone()]).unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_registry WHERE file_path = '/mem/a.md'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);

    let rows = get_memory_registry_by_origin_session(&conn, "s2").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].slug, "second");
    assert_eq!(rows[0].description.as_deref(), Some("updated-desc"));
    assert_eq!(rows[0].content_hash, "hash-updated");
    assert_eq!(rows[0].last_seen_scan, 9);
    assert!(get_memory_registry_by_origin_session(&conn, "s1")
        .unwrap()
        .is_empty());
}

#[test]
fn memory_registry_delete_stale_removes_only_lower_generation_same_project() {
    let conn = mem();
    upsert_memory_registry_batch(
        &conn,
        &[
            sample_memory_row("/mem/gone.md", "proj-a", "gone", None, 1),
            sample_memory_row("/mem/keep1.md", "proj-a", "keep1", None, 2),
            sample_memory_row("/mem/keep2.md", "proj-a", "keep2", None, 2),
        ],
    )
    .unwrap();

    let deleted = delete_memory_registry_stale(&conn, "proj-a", 2).unwrap();
    assert_eq!(deleted, 1);

    let gone: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_registry WHERE file_path = '/mem/gone.md'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(gone, 0);

    let kept: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_registry WHERE project = 'proj-a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, 2);
}

#[test]
fn memory_registry_delete_stale_never_touches_other_projects() {
    let conn = mem();
    upsert_memory_registry_batch(
        &conn,
        &[
            sample_memory_row("/mem/a.md", "proj-a", "a", None, 1),
            sample_memory_row("/mem/b.md", "proj-b", "b", None, 1),
        ],
    )
    .unwrap();

    // Only proj-a was scanned this pass; proj-b was unreadable → never call
    // delete_memory_registry_stale for it.
    let deleted = delete_memory_registry_stale(&conn, "proj-a", 5).unwrap();
    assert_eq!(deleted, 1);

    let a: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_registry WHERE project = 'proj-a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let b: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_registry WHERE project = 'proj-b'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(a, 0);
    assert_eq!(b, 1);
}

/// An id keeps its paths only while it has few of them. The per-id read
/// stops one row past the cap, so a stem with any number of files costs
/// the same as one with nine.
#[test]
fn import_paths_are_returned_only_for_ids_with_few_of_them() {
    let conn = mem();
    let file = |stem: &str, n: usize| {
        for i in 0..n {
            let path = format!("/cc/projects/-f{i}/{stem}.jsonl");
            upsert_import_state_explicit(&conn, &path, stem, 1, "0").unwrap();
        }
    };
    file("one", 1);
    file("eight", 8);
    file("nine", 9);
    file("journal", 500);

    let ids: Vec<String> = [
        "one", "eight", "nine", "journal", "absent", "one", "journal", "absent",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let got = import_paths_for_conversations(&conn, &ids, 8).unwrap();
    let mut keys: Vec<&String> = got.keys().collect();
    keys.sort();
    assert_eq!(keys, ["eight", "one"]);
    assert_eq!(got["one"], ["/cc/projects/-f0/one.jsonl"]);
    assert_eq!(got["eight"].len(), 8);
    assert!(got["eight"].windows(2).all(|w| w[0] < w[1]));
}
