use super::*;

/// Test 1 (replaces `parse_headline_batch_tolerates_fences_prose_and_clamps_length`).
#[test]
fn parse_sentence_batch_accepts_bare_strings_and_objects_and_clamps() {
    // Bare string shape — the new prompt's actual contract.
    let bare = "{\"abc12345\": \"Fixed the relevance gate in guardrails\"}";
    assert_eq!(
        parse_sentence_batch(bare)
            .get("abc12345")
            .map(String::as_str),
        Some("Fixed the relevance gate in guardrails")
    );

    // Defensive object shapes: {"sentence": ...} and {"headline": ...}.
    let sentence_obj = "```json\n{\"id1\": {\"sentence\": \"Shipped the queue retry path\"}}\n```";
    assert_eq!(
        parse_sentence_batch(sentence_obj)
            .get("id1")
            .map(String::as_str),
        Some("Shipped the queue retry path")
    );
    let headline_obj = "{\"id2\": {\"headline\": \"Failed at the render step\"}}";
    assert_eq!(
        parse_sentence_batch(headline_obj)
            .get("id2")
            .map(String::as_str),
        Some("Failed at the render step")
    );

    // Prose wrapper + code fence + whitespace collapse.
    let prose =
        "Sure, here you go:\n```json\n{\"id3\": \"line one\\n   line two\"}\n```\nHope that helps!";
    assert_eq!(
        parse_sentence_batch(prose).get("id3").map(String::as_str),
        Some("line one line two")
    );

    // A 400-char reply clamps to SENTENCE_MAX_CHARS.
    let long = format!("{{\"id4\": \"{}\"}}", "x".repeat(400));
    let clamped = parse_sentence_batch(&long);
    assert!(clamped.get("id4").unwrap().chars().count() <= SENTENCE_MAX_CHARS);

    assert!(parse_sentence_batch("no json here").is_empty());
    assert!(parse_sentence_batch("{\"id\": \"   \"}").is_empty());
}

/// Test 2. F5: a verb-disagreeing reply's own LINE must never be
/// cached ("Shipped the podcast" over a `failed` session), but the
/// session must not stay uncached either — `narrative_usage` was
/// already spent for this batch, so the card's deterministic fallback
/// gets cached in its place (convergence by construction). Renamed from
/// its pre-F5 name (`..._is_rejected`) — rejection of the model's line
/// still happens, but the card no longer stays a permanent miss.
#[test]
fn verb_disagreeing_reply_rejected_but_fallback_cached_in_its_place() {
    let storage = Storage::open_memory().unwrap();
    let session = StorySession {
        session_id: "failed-session".into(),
        project: "proj".into(),
        timestamp: "2026-08-10T00:00:00Z".into(),
        request: Some("create the Ekadashi podcast episode".into()),
        outcome: Some("failed: render aborted".into()),
        ..Default::default()
    };
    let mut cards = vec![build_story_card(&session)];
    assert_eq!(cards[0].outcome_slug, "failed");
    let hash = sentence_hash(&cards[0]);
    let hashes = vec![hash.clone()];

    let mut replies = std::collections::BTreeMap::new();
    replies.insert(
        cards[0].session_short.clone(),
        "Shipped the podcast".to_string(),
    );
    apply_sentence_replies(&storage, &mut cards, &[0], &hashes, &replies, "test-model");

    assert!(
        cards[0].summary.starts_with("Failed:"),
        "got: {}",
        cards[0].summary
    );
    let cached = cached_sentence(&storage, &cards[0].session_short, &hash);
    assert!(
        cached.is_some(),
        "F5: the session must converge to a cache hit even though the model's line was \
             rejected — narrative_usage was already spent for this batch"
    );
    let (cached_headline, _) = cached.unwrap();
    assert!(
        cached_headline.starts_with("Failed:") && cached_headline != "Shipped the podcast",
        "the model's disagreeing LINE must never be cached, only the deterministic \
             fallback — got: {cached_headline}"
    );
    assert_eq!(
        cached_headline, cards[0].summary,
        "the cached headline must match the fallback actually displayed"
    );
    assert!(
        curation_misses(&storage, &cards).is_empty(),
        "the card must not remain a miss on the next run"
    );
}

/// Test 3.
#[test]
fn deterministic_fallback_sentence_carries_the_outcome_verb() {
    assert!(fallback_sentence("success", Some("ship the mailbox layout")).starts_with("Shipped:"));
    assert!(fallback_sentence("partial", Some("fix the cache miss")).starts_with("Partly done:"));
    assert!(fallback_sentence("failed", Some("debug the flaky test")).starts_with("Failed:"));
    assert!(fallback_sentence("noted", Some("look at the alert")).starts_with("Noted:"));
    // No source text at all still produces a non-empty, correctly
    // prefixed sentence rather than panicking or returning empty.
    assert!(fallback_sentence("failed", None).starts_with("Failed:"));
}

/// Test 4.
#[test]
fn prompt_version_bump_invalidates_every_cached_row() {
    let storage = Storage::open_memory().unwrap();
    let session = StorySession {
        session_id: "version-bump-session".into(),
        project: "proj".into(),
        timestamp: "2026-08-10T00:00:00Z".into(),
        request: Some("wire the sort controls".into()),
        outcome: Some("shipped".into()),
        ..Default::default()
    };
    let card = build_story_card(&session);

    // A row cached under a hash that is not today's `sentence_hash`
    // formula — stands in for "an older prompt version's hash", however
    // it was actually produced.
    let stale_hash = format!(
        "{:016x}",
        crate::narrative::fnv1a_64(
            format!("1\u{1}{}\u{1}{}", card.summary, card.episode_json).as_bytes()
        )
    );
    store_sentence(
        &storage,
        &card.session_short,
        &stale_hash,
        "Stale sentence from an older prompt",
        "Stale description.",
        "test-model",
    );
    assert!(
        cached_sentence(&storage, &card.session_short, &sentence_hash(&card)).is_none(),
        "a stale-prompt-version hash must miss"
    );

    // A row cached under today's `sentence_hash` formula hits.
    let current_hash = sentence_hash(&card);
    store_sentence(
        &storage,
        &card.session_short,
        &current_hash,
        "Shipped the sort controls",
        "Wired recency, group, and type sort.",
        "test-model",
    );
    assert_eq!(
        cached_sentence(&storage, &card.session_short, &current_hash),
        Some((
            "Shipped the sort controls".to_string(),
            "Wired recency, group, and type sort.".to_string()
        ))
    );
}

/// Test 5 (adapts `cached_headlines_apply_without_any_ai_invocation`).
#[test]
fn cached_sentences_apply_without_any_ai_invocation() {
    // Invocation disabled: curate must never spawn claude, yet a
    // previously cached sentence (already paid for) still applies, and
    // an uncached card keeps its deterministic fallback summary.
    let storage = Storage::open_memory().unwrap();
    let session = StorySession {
        session_id: "cached-sentence-session".into(),
        project: "proj".into(),
        timestamp: "2026-08-10T00:00:00Z".into(),
        request: Some("pull the latest data.".into()),
        outcome: Some("done".into()),
        ..Default::default()
    };
    let raw_card = build_story_card(&session);
    let hash = sentence_hash(&raw_card);
    store_sentence(
        &storage,
        &raw_card.session_short,
        &hash,
        "Noted the overnight metrics pull",
        "Pulled Meta spend and account balances; MRR snapshot confirmed at $25k.",
        "test-model",
    );

    let uncached = StorySession {
        session_id: "never-curated-session".into(),
        project: "proj".into(),
        timestamp: "2026-08-10T00:00:00Z".into(),
        request: Some("do the other thing".into()),
        outcome: Some("partial".into()),
        ..Default::default()
    };
    let mut cards = vec![raw_card, build_story_card(&uncached)];
    let raw_summary = cards[1].summary.clone();

    curate_sentences_with(&storage, &mut cards, false);

    assert_eq!(cards[0].summary, "Noted the overnight metrics pull");
    assert_eq!(
        cards[0].description,
        "Pulled Meta spend and account balances; MRR snapshot confirmed at $25k."
    );
    assert_eq!(cards[1].summary, raw_summary);
}

/// Test 6.
#[test]
fn curation_converges_second_pass_has_zero_misses() {
    let storage = Storage::open_memory().unwrap();
    let mut cards: Vec<StoryCardView> = (0..6)
        .map(|index| {
            // `session_short` is the first 8 chars of `session_id` (no
            // hashing) — the index must land inside that prefix or every
            // card collides onto the same `journal_headlines` row.
            build_story_card(&StorySession {
                session_id: format!("cvg{index}-converge-session"),
                project: "proj".into(),
                timestamp: "2026-08-10T00:00:00Z".into(),
                request: Some(format!("do task number {index}")),
                outcome: Some("shipped".into()),
                ..Default::default()
            })
        })
        .collect();
    assert_eq!(curation_misses(&storage, &cards).len(), cards.len());

    // Simulate one curation pass: the model agrees with every outcome
    // slug, so `apply_sentence_replies` caches every card (the same
    // store path `curate_sentences_with` uses on a real reply).
    let hashes: Vec<String> = cards.iter().map(sentence_hash).collect();
    let mut replies = std::collections::BTreeMap::new();
    for card in &cards {
        replies.insert(
            card.session_short.clone(),
            format!("Shipped task {}", card.session_short),
        );
    }
    let all_indices: Vec<usize> = (0..cards.len()).collect();
    apply_sentence_replies(
        &storage,
        &mut cards,
        &all_indices,
        &hashes,
        &replies,
        "test-model",
    );
    assert!(curation_misses(&storage, &cards).is_empty());

    // Second pass, allow_invoke=false: everything already hits, so this
    // must spawn nothing and leave zero misses (the unit-level form of
    // "rerun costs zero").
    curate_sentences_with(&storage, &mut cards, false);
    assert!(curation_misses(&storage, &cards).is_empty());
}

/// F5 regression: a fully malformed/unparseable batch reply
/// (`parse_sentence_batch` returns an empty map, exactly as it does for
/// `"not json at all"`) must still leave every card in the batch cached
/// — with its deterministic fallback, since `apply_sentence_replies` is
/// what `curate_sentences_with` now calls unconditionally after
/// `narrative_usage` is recorded, regardless of reply quality.
#[test]
fn malformed_batch_reply_caches_fallbacks_for_every_card_converging_next_pass() {
    let storage = Storage::open_memory().unwrap();
    let mut cards: Vec<StoryCardView> = (0..3)
        .map(|index| {
            build_story_card(&StorySession {
                session_id: format!("mal{index}-malformed-session"),
                project: "proj".into(),
                timestamp: "2026-08-10T00:00:00Z".into(),
                request: Some(format!("do malformed task {index}")),
                outcome: Some("shipped".into()),
                ..Default::default()
            })
        })
        .collect();
    assert_eq!(curation_misses(&storage, &cards).len(), cards.len());

    let hashes: Vec<String> = cards.iter().map(sentence_hash).collect();
    // The exact shape `curate_sentences_with` produces from an
    // unparseable model reply: `parse_sentence_batch` on garbage text.
    let replies = parse_sentence_batch("this is not json at all");
    assert!(replies.is_empty(), "fixture must actually be unparseable");

    let all_indices: Vec<usize> = (0..cards.len()).collect();
    apply_sentence_replies(
        &storage,
        &mut cards,
        &all_indices,
        &hashes,
        &replies,
        "test-model",
    );

    assert!(
        curation_misses(&storage, &cards).is_empty(),
        "a malformed reply must not leave any batch card as a permanent miss"
    );
    for card in &cards {
        let expected_fallback = fallback_sentence("success", card.request_text.as_deref());
        assert_eq!(
            card.summary, expected_fallback,
            "displayed summary must be the deterministic fallback"
        );
        assert_eq!(
            cached_sentence(&storage, &card.session_short, &sentence_hash(card))
                .map(|(headline, _)| headline),
            Some(expected_fallback),
            "cached headline must be the fallback, not any model text"
        );
        assert_eq!(
            cached_model(&storage, &card.session_short).as_deref(),
            Some(FALLBACK_MODEL_TAG),
            "a fallback-cached row must be tagged as such, not attributed to the model"
        );
    }
}

/// F5 regression: a partial reply — some ids present and acceptable,
/// the rest missing from the model's JSON entirely — must cache BOTH
/// kinds: the present, verb-agreeing sessions get the model's sentence;
/// every missing session gets its deterministic fallback. No card is
/// left as a miss.
#[test]
fn partial_batch_reply_caches_model_sentences_and_fallbacks_together() {
    let storage = Storage::open_memory().unwrap();
    let mut cards: Vec<StoryCardView> = (0..10)
        .map(|index| {
            build_story_card(&StorySession {
                session_id: format!("prt{index}-partial-session"),
                project: "proj".into(),
                timestamp: "2026-08-10T00:00:00Z".into(),
                request: Some(format!("do partial task {index}")),
                outcome: Some("shipped".into()),
                ..Default::default()
            })
        })
        .collect();
    let hashes: Vec<String> = cards.iter().map(sentence_hash).collect();

    // Only the first 3 of 10 ids come back from the model.
    let mut replies = std::collections::BTreeMap::new();
    for card in cards.iter().take(3) {
        replies.insert(
            card.session_short.clone(),
            format!("Shipped task {}", card.session_short),
        );
    }
    assert_eq!(replies.len(), 3);

    let all_indices: Vec<usize> = (0..cards.len()).collect();
    apply_sentence_replies(
        &storage,
        &mut cards,
        &all_indices,
        &hashes,
        &replies,
        "test-model",
    );

    assert!(
        curation_misses(&storage, &cards).is_empty(),
        "every one of the 10 cards must be cached after this batch, present or not"
    );

    let mut model_cached = 0;
    let mut fallback_cached = 0;
    for card in &cards {
        match cached_model(&storage, &card.session_short).as_deref() {
            Some("test-model") => model_cached += 1,
            Some(FALLBACK_MODEL_TAG) => fallback_cached += 1,
            other => panic!("unexpected model tag: {other:?}"),
        }
    }
    assert_eq!(
        model_cached, 3,
        "the 3 present ids get the model's sentence cached"
    );
    assert_eq!(
        fallback_cached, 7,
        "the 7 missing ids get their deterministic fallback cached"
    );
}

/// Test 7.
#[test]
fn curation_chunk_plan_is_ceil_over_ten_and_capped() {
    assert_eq!(curation_chunk_plan(50), 5);
    assert_eq!(curation_chunk_plan(1), 1);
    assert_eq!(curation_chunk_plan(0), 0);
    assert!(curation_chunk_plan(MAX_STORY_CARDS) <= 5);
}

/// Test 8.
#[test]
fn thin_rollup_rows_are_never_sent_to_curation() {
    let storage = Storage::open_memory().unwrap();
    seed_rich_sessions(&storage, 5);
    // Artifact-only sessions classify as THIN (Q2) — never curated.
    for index in 0..40 {
        let session_id = format!("thin-session-{index:03}");
        let timestamp = format!("2026-05-{:02}T0{}:00:00Z", (index % 27) + 1, index % 9);
        seed_anchor(
            &storage,
            &session_id,
            &timestamp,
            &format!("thin_fn_{index}"),
        );
    }
    let empty_dir = tempfile::tempdir().unwrap();
    let raw = gather_report_data_with(
        &storage,
        empty_dir.path(),
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    )
    .unwrap();
    assert_eq!(raw.rich_cards.len(), 5);
    assert!(raw.thin_entries.len() >= 40);
    assert!(curation_misses(&storage, &raw.rich_cards).len() <= 5);
}

/// Test 9.
#[test]
fn description_still_written_so_cache_hit_predicate_holds() {
    let storage = Storage::open_memory().unwrap();
    let session = StorySession {
        session_id: "sentence-only-reply-session".into(),
        project: "proj".into(),
        timestamp: "2026-08-10T00:00:00Z".into(),
        request: Some("ship the mailbox layout".into()),
        outcome: Some("shipped and merged".into()),
        ..Default::default()
    };
    let mut cards = vec![build_story_card(&session)];
    let hash = sentence_hash(&cards[0]);
    let hashes = vec![hash.clone()];

    // A reply carrying only a bare sentence — the new prompt's actual
    // contract has no description field at all.
    let mut replies = std::collections::BTreeMap::new();
    replies.insert(
        cards[0].session_short.clone(),
        "Shipped the mailbox layout".to_string(),
    );
    apply_sentence_replies(&storage, &mut cards, &[0], &hashes, &replies, "test-model");

    let stored: Option<(String, String)> = storage
        .with_connection(|conn| {
            Ok(conn
                .query_row(
                    "SELECT headline, description FROM journal_headlines WHERE session_id = ?1",
                    rusqlite::params![cards[0].session_short],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .ok())
        })
        .unwrap();
    let (stored_headline, stored_description) = stored.expect("row must exist after apply");
    assert_eq!(stored_headline, "Shipped the mailbox layout");
    assert!(
        !stored_description.is_empty(),
        "description must be non-empty so the row hits next run"
    );

    // Next run, same hash: must be a cache hit, not a miss — the exact
    // regression the description fallback (`else { reply.clone() }`)
    // guards against.
    assert!(cached_sentence(&storage, &cards[0].session_short, &hash).is_some());
}

#[test]
fn outcome_badges_classify_conservatively() {
    assert_eq!(outcome_badge(None), ("noted", None));
    assert_eq!(
        outcome_badge(Some("Shipped to production")),
        ("success", Some("success".into()))
    );
    assert_eq!(
        outcome_badge(Some("partial — two items left")),
        ("partial", Some("partial".into()))
    );
    assert_eq!(
        outcome_badge(Some("failed at the gate")),
        ("failed", Some("failed".into()))
    );
    let (slug, badge) = outcome_badge(Some("parked for review"));
    assert_eq!(slug, "noted");
    assert_eq!(badge.as_deref(), Some("parked for review"));
}

#[test]
fn stale_cache_entry_is_ignored_when_content_hash_changes() {
    let storage = Storage::open_memory().unwrap();
    let session = StorySession {
        session_id: "stale-hash-session".into(),
        project: "proj".into(),
        timestamp: "2026-08-10T00:00:00Z".into(),
        request: Some("original request".into()),
        outcome: Some("done".into()),
        ..Default::default()
    };
    let card = build_story_card(&session);
    store_sentence(
        &storage,
        &card.session_short,
        "0000000000000000",
        "Sentence for content that no longer exists",
        "Description for content that no longer exists.",
        "test-model",
    );
    let mut cards = vec![card];
    let raw_summary = cards[0].summary.clone();
    curate_sentences_with(&storage, &mut cards, false);
    assert_eq!(
        cards[0].summary, raw_summary,
        "a hash-mismatched cache row must not be applied"
    );
}

#[test]
fn first_sentence_never_cuts_inside_decimals_urls_or_filenames() {
    assert_eq!(
        first_sentence("5-day v10.1 release gate hold. More detail follows."),
        "5-day v10.1 release gate hold."
    );
    assert_eq!(
        first_sentence("review https://github.com/ramakay/csr then merge"),
        "review https://github.com/ramakay/csr then merge"
    );
    assert_eq!(
        first_sentence("upscale saadhana-v6-480p.mp4 to 4k. Done overnight."),
        "upscale saadhana-v6-480p.mp4 to 4k."
    );
    assert_eq!(first_sentence("ends exactly here."), "ends exactly here.");
}

#[test]
fn signature_scaffold_never_reaches_summary_or_request_text() {
    let session = StorySession {
        session_id: "sig-scrub-session".into(),
        project: "proj".into(),
        timestamp: "2026-08-10T00:00:00Z".into(),
        request: Some(
            "generate a cover photo --- Signature: {\"completion_status\":\"partial\"}".into(),
        ),
        outcome: Some("partial".into()),
        ..Default::default()
    };
    let card = build_story_card(&session);
    assert!(
        !card.summary.contains("Signature"),
        "summary leaked scaffold: {}",
        card.summary
    );
    assert!(
        !card
            .request_text
            .as_deref()
            .unwrap_or_default()
            .contains("Signature"),
        "request text leaked scaffold"
    );
    assert!(!card.episode_json.contains("completion_status"));
    // Phase 3's fused sentence prefixes the scrubbed text with the
    // outcome verb ("Partly done: ...") rather than starting with it
    // verbatim.
    assert!(card.summary.contains("generate a cover photo"));
}

#[test]
fn empty_db_renders_no_session_story_signal() {
    let storage = Storage::open_memory().unwrap();
    let html = render(&storage, crate::storage::recap_feeds::ConsumptionMode::Off);
    assert!(html.contains("No session has enough story signal"));
    assert!(html.contains("<!doctype html>"));
    assert!(html.contains("CSR Dream Journal"));
    assert_no_external_resources(&html);
    assert!(
        !html.contains("class=\"index-row"),
        "an empty corpus must not render any index rows"
    );
}

#[test]
fn consumption_mode_does_not_hide_story_data() {
    let storage = Storage::open_memory().unwrap();
    seed_episode(
        &storage,
        "visible-story",
        "2026-01-01T01:00:00Z",
        "Show the session story",
        "done",
        &[],
        &[],
    );
    let html = render(&storage, crate::storage::recap_feeds::ConsumptionMode::Off);
    assert!(html.contains("Show the session story"));
    assert_eq!(html.matches("class=\"index-row").count(), 1);
    assert_eq!(html.matches("class=\"detail-pane\"").count(), 1);
}

#[test]
fn synthetic_event_keeps_totals_and_renders_as_artifact_receipt() {
    let storage = Storage::open_memory().unwrap();
    storage
        .insert_witness(&ledger_row(
            "proj",
            "/repo/src/lib.rs",
            Some("foo"),
            "aaa",
            "b3:1",
        ))
        .unwrap();
    let witness = storage
        .witnesses_for_file("proj", "/repo/src/lib.rs")
        .unwrap()
        .pop()
        .unwrap();
    storage
        .insert_witness_verdict(&WitnessVerdictRow {
            witness_id: witness.id,
            verdict: VerdictKind::SupersededBy,
            successor_witness_id: None,
            receipt_oid: Some("bbb".into()),
            observed_head_oid: "bbb".into(),
        })
        .unwrap();
    // An anchor alone (no request/narrative) is THIN under Q2 — pair it
    // with a request+outcome so this session is RICH and its artifact
    // bento is reachable in the detail pane.
    seed_episode(
        &storage,
        "event-session",
        "2026-01-02T01:00:00Z",
        "touch the foo symbol",
        "done",
        &[],
        &[],
    );
    seed_anchor(&storage, "event-session", "2026-01-02T01:00:00Z", "foo");

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    assert!(html.contains("foo"));
    assert!(html.contains("⌗ superseded bbb"));
    assert!(html.contains("lib.rs"));
    assert!(html.contains("no receipt") || html.matches("⌗ superseded").count() >= 1);
}

/// Symbol names are user code identifiers interpolated into HTML; minijinja
/// auto-escaping depends on TEMPLATE_NAME ending in `.html`. This regression
/// test fails if that wiring is ever broken (e.g. the template gets renamed).
/// Extended (finding: server-rendering user text is new attack surface once
/// the mailbox stopped going through mermaid's own escaping-free SVG path):
/// the ASK stage's request quote and the thin-rollup's prompt excerpt are
/// new sinks and must escape identically.
#[test]
fn hostile_symbol_names_are_escaped_in_html_and_episode_json() {
    let storage = Storage::open_memory().unwrap();
    let payload = "<script>alert(1)</script>";
    storage
        .insert_witness(&ledger_row(
            "proj",
            "/repo/src/lib.rs",
            Some(payload),
            "aaa",
            "b3:1",
        ))
        .unwrap();
    let w = storage
        .witnesses_for_file("proj", "/repo/src/lib.rs")
        .unwrap()
        .pop()
        .unwrap();
    storage
        .insert_witness_verdict(&WitnessVerdictRow {
            witness_id: w.id,
            verdict: VerdictKind::AnchorObsolete,
            successor_witness_id: None,
            receipt_oid: Some("bbb".into()),
            observed_head_oid: "bbb".into(),
        })
        .unwrap();
    seed_episode(
        &storage,
        "hostile-session",
        "2026-01-03T01:00:00Z",
        payload,
        "done",
        &[],
        &[],
    );
    seed_anchor(&storage, "hostile-session", "2026-01-03T01:00:00Z", payload);
    // New sink: a thin (registry-only) session whose prompt is hostile.
    seed_registry(&storage, "hostile-thin", "2026-01-03T02:00:00Z", payload);

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    assert!(!html.contains(payload), "raw script tag leaked into HTML");
    // minijinja HTML-escapes `/` as well: </script> -> &lt;&#x2f;script&gt;
    assert!(html.contains("&lt;script&gt;alert(1)&lt;&#x2f;script&gt;"));
    assert!(
        html.contains("\\u003cscript\\u003ealert(1)\\u003c/script\\u003e"),
        "JSON script context must neutralize HTML delimiters"
    );
    let episode_data = episode_data_blocks(&html);
    assert_eq!(episode_data.len(), 1);
    assert_eq!(episode_data[0]["artifacts"][0]["symbol"], payload);
    // The thin-rollup excerpt must escape too.
    assert!(html.contains("class=\"thin-excerpt\""));
}

// ---- SINCE THEN payback panel (journal v2 Phase 5) ---------------------

/// All four `witness_verdicts` states plus the no-verdict-at-all case
/// render their exact, receipt-grounded chip text.
#[test]
fn since_then_rows_carry_receipts_for_all_verdict_states() {
    let storage = Storage::open_memory().unwrap();
    seed_episode(
        &storage,
        "s1",
        "2026-02-01T09:00:00Z",
        "Audit the retry symbols",
        "done",
        &[],
        &[],
    );
    for symbol in [
        "sym_superseded",
        "sym_obsolete",
        "sym_reinstated",
        "sym_witnessed",
        "sym_unverified",
    ] {
        seed_attribution(
            &storage,
            "s1",
            "/repo/src/lib.rs",
            symbol,
            "2026-02-01T09:00:00Z",
        );
    }

    let w_superseded = seed_witness(
        &storage,
        "proj",
        "/repo/src/lib.rs",
        "sym_superseded",
        "aoid1",
    );
    seed_verdict_event(
        &storage,
        w_superseded,
        "superseded_by",
        Some("1234567890abcdef"),
        "2026-02-01 10:00:00",
    );
    let w_obsolete = seed_witness(
        &storage,
        "proj",
        "/repo/src/lib.rs",
        "sym_obsolete",
        "aoid2",
    );
    seed_verdict_event(
        &storage,
        w_obsolete,
        "anchor_obsolete",
        None,
        "2026-02-01 11:00:00",
    );
    let w_reinstated = seed_witness(
        &storage,
        "proj",
        "/repo/src/lib.rs",
        "sym_reinstated",
        "aoid3",
    );
    seed_verdict_event(
        &storage,
        w_reinstated,
        "anchor_reinstated",
        None,
        "2026-02-01 12:00:00",
    );
    // "sym_witnessed": witnessed, no verdict event ever recorded — F1:
    // this must NOT render as "still live", only as "witnessed <date> ·
    // unverified since" (no dream verdict re-checked it at HEAD).
    seed_witness(
        &storage,
        "proj",
        "/repo/src/lib.rs",
        "sym_witnessed",
        "aoid4",
    );
    // "sym_unverified": attribution only, no witness_ledger row at all.

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    assert!(html.contains("class=\"since-then\""));
    assert!(html.contains(r#"class="conclusion superseded">superseded ⌗12345678 · 2026-02-01</p>"#));
    assert!(html.contains(r#"class="conclusion obsolete">obsolete, anchor gone · 2026-02-01</p>"#));
    assert!(html.contains(r#"class="conclusion reinstated">reinstated · 2026-02-01</p>"#));
    assert!(
        !html.contains("still live"),
        "F1: witnessed-with-no-verdict must never be rendered as HEAD-verified 'still live'"
    );
    assert!(
        html.contains(r#"class="conclusion witnessed_unverified">witnessed "#)
            && html.contains(" · unverified since</p>"),
        "witnessed-with-no-verdict renders the honest 'witnessed <date> · unverified since' \
             chip, not a liveness claim"
    );
    assert!(html.contains(r#"class="conclusion unverified">unverified</p>"#));
    assert!(html.contains(r#"5 symbols touched"#));
}

/// A rich session with no `code_node_attribution` rows and no
/// `episode_anchors` rows renders no `.since-then` element at all —
/// panel absent beats panel empty.
#[test]
fn since_then_absent_without_attribution_or_anchors() {
    let storage = Storage::open_memory().unwrap();
    seed_episode(
        &storage,
        "s2",
        "2026-02-02T09:00:00Z",
        "Read the logs, nothing to change",
        "done",
        &[],
        &[],
    );

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    assert!(!html.contains("class=\"since-then\""));
}

/// A symbol named by an open todo gets a "so" clause; a symbol with no
/// evidence anywhere in the chain ends at the conclusion with no advice.
#[test]
fn so_clause_renders_only_with_evidence() {
    let storage = Storage::open_memory().unwrap();
    seed_episode(
        &storage,
        "s3",
        "2026-02-03T09:00:00Z",
        "Look at the queue helpers",
        "partial",
        &[],
        &[("check flush_queue reset path", "pending")],
    );
    seed_attribution(
        &storage,
        "s3",
        "/repo/src/lib.rs",
        "flush_queue",
        "2026-02-03T09:00:00Z",
    );
    seed_attribution(
        &storage,
        "s3",
        "/repo/src/lib.rs",
        "quiet_helper",
        "2026-02-03T09:00:01Z",
    );
    // Neither symbol is ever witnessed -> both are UNVERIFIED, isolating
    // the so-clause evidence chain from the deterministic superseded case.

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    assert_eq!(
        html.matches("class=\"so-clause\"").count(),
        1,
        "exactly one row has evidence for a so-clause: {html}"
    );
    // minijinja HTML-escapes the quotes around the todo text.
    assert!(html.contains("open todo: &quot;check flush_queue reset path&quot;"));
}

/// A superseded symbol's "so" clause always names the receipt, even with
/// no todo/proposal/blocker evidence at all.
#[test]
fn superseded_symbol_so_clause_names_the_receipt() {
    let storage = Storage::open_memory().unwrap();
    seed_episode(
        &storage,
        "s4",
        "2026-02-04T09:00:00Z",
        "Touch the core function",
        "done",
        &[],
        &[],
    );
    seed_attribution(
        &storage,
        "s4",
        "/repo/src/lib.rs",
        "core_fn",
        "2026-02-04T09:00:00Z",
    );
    let witness_id = seed_witness(&storage, "proj", "/repo/src/lib.rs", "core_fn", "aoid5");
    seed_verdict_event(
        &storage,
        witness_id,
        "superseded_by",
        Some("abcdef0123456789"),
        "2026-02-04 10:00:00",
    );

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    assert!(html.contains("read the superseding change before touching this again (⌗abcdef01)"));
}

/// 25 symbols (5 superseded, 20 unverified): the cap renders 20 rows with
/// an honest "+5 more", and every superseded row sorts before every
/// rendered unverified row (adverse-verdict-first).
#[test]
fn symbol_cap_and_adverse_first_ordering() {
    let storage = Storage::open_memory().unwrap();
    seed_episode(
        &storage,
        "s5",
        "2026-02-05T09:00:00Z",
        "Sweep the legacy module",
        "done",
        &[],
        &[],
    );
    for index in 0..5 {
        let symbol = format!("sup{index:02}");
        seed_attribution(
            &storage,
            "s5",
            "/repo/src/lib.rs",
            &symbol,
            &format!("2026-02-05T00:{index:02}:00Z"),
        );
        let witness_id = seed_witness(
            &storage,
            "proj",
            "/repo/src/lib.rs",
            &symbol,
            &format!("aoid-sup-{index}"),
        );
        seed_verdict_event(
            &storage,
            witness_id,
            "superseded_by",
            Some(&format!("receipt{index:02}abcdef")),
            "2026-02-05 12:00:00",
        );
    }
    for index in 0..20 {
        let symbol = format!("unv{index:02}");
        seed_attribution(
            &storage,
            "s5",
            "/repo/src/lib.rs",
            &symbol,
            &format!("2026-02-05T01:{index:02}:00Z"),
        );
    }

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    assert_eq!(html.matches("class=\"since-then-row\"").count(), 20);
    assert!(html.contains("+5 more symbols"));
    assert_eq!(
        html.matches("class=\"conclusion superseded\"").count(),
        5,
        "all 5 superseded rows must survive the cap"
    );
    let last_superseded = html.rfind("class=\"conclusion superseded\"").unwrap();
    let first_unverified = html.find("class=\"conclusion unverified\"").unwrap();
    assert!(
        last_superseded < first_unverified,
        "every superseded row must sort before every rendered unverified row"
    );
}

/// A day with a retirement event shows the digest; a day with neither
/// retirements nor open proposals shows no digest line at all.
#[test]
fn day_digest_only_when_nonzero() {
    let storage = Storage::open_memory().unwrap();
    seed_episode(
        &storage,
        "d1",
        "2026-03-05T10:00:00Z",
        "Retire the old validator",
        "done",
        &[],
        &[],
    );
    seed_attribution(
        &storage,
        "d1",
        "/repo/src/lib.rs",
        "old_validator",
        "2026-03-05T10:00:00Z",
    );
    let witness_id = seed_witness(
        &storage,
        "proj",
        "/repo/src/lib.rs",
        "old_validator",
        "aoid6",
    );
    seed_verdict_event(
        &storage,
        witness_id,
        "superseded_by",
        Some("cafebabe01234567"),
        "2026-03-05 12:00:00",
    );

    seed_episode(
        &storage,
        "d2",
        "2026-03-06T10:00:00Z",
        "Look at unrelated code",
        "done",
        &[],
        &[],
    );
    seed_attribution(
        &storage,
        "d2",
        "/repo/src/lib.rs",
        "unrelated_fn",
        "2026-03-06T10:00:00Z",
    );

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    assert_eq!(
        html.matches("class=\"dream-digest\"").count(),
        1,
        "only the day with real dream activity gets a digest line"
    );
    assert!(html.contains("while away: 1 anchor retired"));
}

/// Extends the hostile-payload regression (test above) to the new SINCE
/// THEN sinks: symbol name, `csr_why(...)` hint, and the "so" clause's
/// open-todo evidence.
#[test]
fn hostile_symbol_names_escaped_in_since_then() {
    let storage = Storage::open_memory().unwrap();
    let payload = "<script>alert(2)</script>";
    let todo_content = format!("investigate {payload}");
    seed_episode(
        &storage,
        "hostile-since-then",
        "2026-01-04T01:00:00Z",
        "investigate the hostile symbol",
        "done",
        &[],
        &[(todo_content.as_str(), "pending")],
    );
    seed_attribution(
        &storage,
        "hostile-since-then",
        "/repo/src/lib.rs",
        payload,
        "2026-01-04T01:00:00Z",
    );

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    assert!(html.contains("class=\"since-then\""));
    assert!(
        !html.contains(payload),
        "raw hostile payload leaked into a since-then sink"
    );
    assert!(html.contains("&lt;script&gt;alert(2)&lt;&#x2f;script&gt;"));
}

#[test]
fn report_no_open_writes_file_to_tempdir() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_memory().unwrap();
    let out_path = dir.path().join("dream-test.html");

    let written = run_report(&storage, dir.path(), Some(out_path.clone()), true).unwrap();
    assert_eq!(written, out_path);
    let contents = std::fs::read_to_string(&written).unwrap();
    assert!(contents.contains("CSR Dream Journal"));
    assert!(contents.contains("No session has enough story signal"));
}

#[test]
fn report_creates_parent_directory_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open_memory().unwrap();
    let out_path = dir.path().join("nested").join("deeper").join("dream.html");

    let written = run_report(&storage, dir.path(), Some(out_path.clone()), true).unwrap();
    assert!(written.exists());
}

#[test]
fn short_oid_truncates_to_eight_chars() {
    assert_eq!(short_oid("0123456789abcdef"), "01234567");
    assert_eq!(short_oid("abc"), "abc");
}

/// Regression for finding 9: `replace_exactly_once` must reject a
/// missing legacy fragment loudly rather than let `replacen` silently
/// no-op, which would leave stale legacy markup in the rendered report
/// with no error at all.
#[test]
#[should_panic(expected = "SOME_FRAGMENT")]
fn replace_exactly_once_panics_when_fragment_is_absent() {
    replace_exactly_once("nothing to see here", "NEEDLE", "X", "SOME_FRAGMENT");
}

/// Regression for finding 9: a fragment that drifted into occurring more
/// than once must also be rejected — `replacen(..., 1)` would otherwise
/// silently leave a second stale copy behind after replacing only the
/// first.
#[test]
#[should_panic(expected = "found 2")]
fn replace_exactly_once_panics_when_fragment_repeats() {
    replace_exactly_once("NEEDLE and NEEDLE again", "NEEDLE", "X", "SOME_FRAGMENT");
}

#[test]
fn replace_exactly_once_replaces_the_single_occurrence() {
    assert_eq!(
        replace_exactly_once("a NEEDLE b", "NEEDLE", "X", "SOME_FRAGMENT"),
        "a X b"
    );
}

#[test]
fn witness_ledger_module_smoke() {
    // Sanity: the helper row constructor round-trips through the real
    // storage module used by `gather_report_data` above.
    let conn_row = ledger_row("p", "/f.rs", None, "oid", "stamp");
    assert_eq!(conn_row.symbol, None);
    let _ = witness_ledger::insert_witness;
    let _ = witness_verdicts::VerdictChannel::Demote;
}

#[test]
fn session_with_request_and_outcome_only_renders_ask_and_outcome_stages() {
    let storage = Storage::open_memory().unwrap();
    seed_episode(
        &storage,
        "template-session",
        "2026-02-03T01:00:00Z",
        "Investigate the release gate",
        "partial",
        &[],
        &[],
    );

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    assert!(html.contains(r#"data-stage="S_INPUT""#));
    assert!(html.contains(r#"data-stage="S_OUT""#));
    assert!(
        !html.contains(r#"data-stage="S_DELIB""#),
        "an outcome with no narrative and no investigated files must not draw a DELIBERATION card"
    );
    assert!(
        !html.contains(r#"data-stage="S_STEER""#),
        "a session with no todos must not draw a STEER card"
    );
}

#[test]
fn report_caps_cards_at_fifty_and_counts_older_and_low_signal_omissions() {
    let storage = Storage::open_memory().unwrap();
    for index in 0..52 {
        let session = format!("session-{index:02}");
        let timestamp = format!("2026-03-{:02}T01:00:00Z", index + 1);
        seed_episode(
            &storage,
            &session,
            &timestamp,
            &format!("request {index}"),
            "done",
            &[],
            &[],
        );
    }
    for index in 0..2 {
        seed_registry(
            &storage,
            &format!("no-signal-{index}"),
            &format!("2026-01-0{}T01:00:00Z", index + 1),
            "",
        );
    }

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );

    assert_eq!(html.matches("class=\"index-row").count(), 50);
    assert!(html.contains("2 older sessions omitted"));
    assert!(html.contains("2 sessions omitted — no prompt, no artifact, no episode"));
}

// ---- §7.1 pinned Phase 1 tests ---------------------------------------

/// Pinned test 1: replaces the old "the vendored Mermaid runtime must be
/// embedded exactly once" assertion — the runtime, and everything that
/// rendered it, is gone.
#[test]
fn mermaid_runtime_is_gone_and_report_is_small() {
    let storage = Storage::open_memory().unwrap();
    seed_rich_sessions(&storage, 50);

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    let lower = html.to_ascii_lowercase();
    assert!(!lower.contains("statediagram"));
    assert!(!lower.contains("__esbuild_esm_mermaid_nm"));
    assert!(!lower.contains("mermaid"));
    assert!(
        html.len() < 2_000_000,
        "50-session mailbox report is {} bytes, expected < 2MB",
        html.len()
    );
    assert_no_external_resources(&html);
}

/// Pinned test 2.
#[test]
fn index_rows_and_detail_panes_are_one_to_one() {
    let storage = Storage::open_memory().unwrap();
    seed_rich_sessions(&storage, 6);

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    assert_eq!(html.matches("class=\"index-row").count(), 6);
    assert_eq!(html.matches("class=\"detail-pane\"").count(), 6);

    let (index_html, detail_html) = split_panes(&html);
    let mut index_ids = attr_values(index_html, r#"data-session=""#);
    let mut detail_ids = attr_values(detail_html, r#"data-session=""#);
    index_ids.sort_unstable();
    detail_ids.sort_unstable();
    assert_eq!(index_ids.len(), 6);
    assert_eq!(
        index_ids, detail_ids,
        "every index row must have exactly one matching detail pane"
    );
}

/// Pinned test 3.
#[test]
fn thin_sessions_are_grouped_below_and_never_in_the_main_index() {
    let storage = Storage::open_memory().unwrap();
    // Artifacts only, no request/narrative -> THIN (Q2).
    seed_anchor(
        &storage,
        "artifact-only-session",
        "2026-02-04T01:00:00Z",
        "retired_symbol",
    );
    // Registry-only with a first_prompt -> THIN (Q2 explicitly promotes
    // this out of "omitted" and into the rollup).
    seed_registry(
        &storage,
        "registry-only-session",
        "2026-02-04T02:00:00Z",
        "check the logs",
    );

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    assert!(!html.contains("class=\"index-row"));
    assert!(html.contains("class=\"thin-group\""));
    assert!(html.contains("2 thin sessions"));
}

/// Pinned test 4.
#[test]
fn sessions_with_no_evidence_at_all_are_counted_not_rendered() {
    let storage = Storage::open_memory().unwrap();
    seed_registry(&storage, "no-evidence-session", "2026-02-05T01:00:00Z", "");

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    assert!(!html.contains("class=\"index-row"));
    assert!(!html.contains("class=\"thin-group\""));
    assert!(html.contains("1 sessions omitted"));
}

/// Pinned test 5.
#[test]
fn index_rows_carry_the_four_sort_keys() {
    let storage = Storage::open_memory().unwrap();
    seed_episode(
        &storage,
        "sort-key-success",
        "2026-02-06T01:00:00Z",
        "ship the feature",
        "shipped to production",
        &[],
        &[],
    );
    seed_episode(
        &storage,
        "sort-key-failed",
        "2026-02-07T01:00:00Z",
        "ship the other feature",
        "failed at the gate",
        &[],
        &[],
    );

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    let (index_html, _) = split_panes(&html);
    let ts_values = attr_values(index_html, r#"data-ts=""#);
    let project_values = attr_values(index_html, r#"data-project=""#);
    let outcome_values = attr_values(index_html, r#"data-outcome=""#);
    let day_values = attr_values(index_html, r#"data-day=""#);
    assert_eq!(ts_values.len(), 2);
    assert_eq!(project_values.len(), 2);
    assert_eq!(outcome_values.len(), 2);
    assert_eq!(day_values.len(), 2);
    for value in &ts_values {
        assert!(!value.is_empty());
    }
    for value in &project_values {
        assert!(!value.is_empty());
    }
    for value in &day_values {
        assert!(!value.is_empty());
    }
    for value in &outcome_values {
        assert!(matches!(*value, "success" | "partial" | "failed" | "noted"));
    }
}
