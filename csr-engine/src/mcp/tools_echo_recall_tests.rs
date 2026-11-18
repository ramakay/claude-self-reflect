use super::*;
use crate::import::ConversationChunk;
use crate::provenance::Speaker;
use crate::storage::recap_feeds::ConsumptionMode;

const QUERY: &str = "why did we drop Docker and Qdrant";
const SIG: &str = r#"Signature: {"completion_status":"partial","confidence":"MEDIUM","frameworks":["rust"],"pattern_reusability":"medium","error_recovery":false,"total_edits":0,"iteration_count":2,"tools_used":["ToolSearch"],"files_modified":[],"concepts":[]}"#;
const WHY_ROW: &str = "WHY: why did we drop Qdrant\n\nconv_3ebfc6f9-d73f-49b3-b29b-202ed5f35255:\n  via=reflection score=0.794 [2026-07-15T19:39:01.638330+00:00] conv_3ebfc6f9-d73f-49b3-b29b-202ed5f35255: ## User Request why did we drop Docker and Qdrant";
const SEARCH_ROW: &str = "🎯 RESULTS: 5 matches (partial relevance, top score: 0.450)\n⚡ PERFORMANCE: 6ms (1 collection searched)\n\n<search>\n  <summary count=\"5\" relevance=\"partial\" top-score=\"0.450\">\n    <preview>[keyword] why did we drop Docker and Qdrant</preview>\n  </summary>\n  <meta>\n    <q>why</q>\n  </meta>\n  <results>\n  </results>\n</search>";

fn vec_at(cos: f32) -> Vec<f32> {
    vec![cos, (1.0 - cos * cos).sqrt(), 0.0, 0.0]
}

fn chunk(id: &str, conv: &str, cos: f32, content: &str) -> (ConversationChunk, Vec<f32>) {
    (
        ConversationChunk {
            id: id.into(),
            conversation_id: conv.into(),
            project_name: "echoproj".into(),
            timestamp: "2026-10-12T00:00:00Z".into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: Speaker::Assistant,
            seq: 0,
            is_sidechain: false,
        },
        vec_at(cos),
    )
}

fn ids(out: &str) -> Vec<String> {
    out.split("<id>")
        .skip(1)
        .filter_map(|part| part.split("</id>").next())
        .map(str::to_string)
        .collect()
}

async fn recall(
    rows: &[(ConversationChunk, Vec<f32>)],
    reflections: &[(&str, &[&str], String, f32)],
) -> Vec<String> {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    for (row, vector) in rows {
        storage.insert_chunk(row, vector).unwrap();
        engine.insert_chunk(row.id.clone(), vector.clone());
    }
    for (id, tags, content, cos) in reflections {
        let tags: Vec<String> = tags.iter().map(|tag| tag.to_string()).collect();
        storage
            .insert_reflection(id, content, &tags, &vec_at(*cos))
            .unwrap();
        engine.insert_reflection(id.to_string(), vec_at(*cos));
    }
    let search = Arc::new(RwLock::new(engine));
    let out = reflect_on_past_with_vec_mode(
        &storage,
        &search,
        &[1.0, 0.0, 0.0, 0.0],
        QUERY,
        30,
        0.0,
        Some("echoproj"),
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
    ids(&out)
}

fn position(order: &[String], id: &str) -> usize {
    order
        .iter()
        .position(|candidate| candidate == id)
        .unwrap_or_else(|| panic!("{id} missing from {order:?}"))
}

type ProbeChunks = Vec<(ConversationChunk, Vec<f32>)>;
type ProbeReflections = Vec<(&'static str, &'static [&'static str], String, f32)>;

fn probe_rows() -> (ProbeChunks, ProbeReflections) {
    let rows = vec![
        chunk(
            "echo-chunk",
            "conv1",
            0.75,
            "why did we drop Docker and Qdrant for a single Rust binary?\n\nChecking past sessions for the actual reasoning.",
        ),
        chunk("why-row", "conv9", 0.80, WHY_ROW),
        chunk("search-row", "conv9", 0.78, SEARCH_ROW),
        chunk(
            "answer-chunk",
            "conv1",
            0.50,
            "Installing it was too painful, and the scale never needed a database server. A Phase 0 spike then showed an embedded index was faster anyway.",
        ),
        chunk(
            "answer-chunk-2",
            "conv2",
            0.52,
            "Memory has answer. Qdrant died with whole v7 Python/Docker stack in v8.0 rewrite. Why drop: install friction, Docker Desktop needed.",
        ),
    ];
    let reflections = vec![
        (
            "story_conv1",
            &["session_story", "project_echoproj", "conv_conv1"][..],
            format!("why did we drop Docker and Qdrant for a single Rust binary? --- {SIG}. Partially completed"),
            0.68,
        ),
        (
            "extracted_v3_conv1",
            &["narrative_extracted_v3", "project_echoproj", "conv_conv1"][..],
            format!("## User Request\nwhy did we drop Docker and Qdrant for a single Rust binary?\n\n\n---\n{SIG}\nContext:\n## Validation"),
            0.72,
        ),
    ];
    (rows, reflections)
}

#[tokio::test]
async fn pagination_keeps_the_sink_order_of_page_one() {
    let echo = "why did we drop Docker and Qdrant for a single Rust binary?\n\nChecking past sessions for the actual reasoning.";
    let answer = "Installing it was too painful, and the scale never needed a database server.";
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    for (row, vector) in [
        chunk("echo-chunk", "conv1", 0.90, echo),
        chunk("answer-chunk", "conv2", 0.80, answer),
    ] {
        storage.insert_chunk(&row, &vector).unwrap();
        engine.insert_chunk(row.id.clone(), vector);
    }
    let search = Arc::new(RwLock::new(engine));
    let scope =
        SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
            .unwrap();
    let q = [1.0f32, 0.0, 0.0, 0.0];
    let page = |offset: usize| {
        get_more_results_in_scope(
            &storage,
            &search,
            &q,
            QUERY,
            offset,
            1,
            0.0,
            &scope,
            ConsumptionMode::Off,
            false,
        )
    };
    let first = page(0).await.unwrap();
    let second = page(1).await.unwrap();
    assert!(
        first.contains("too painful") && !first.contains("Checking past"),
        "{first}"
    );
    assert!(
        second.contains("Checking past") && !second.contains("too painful"),
        "{second}"
    );
}

/// Page one and the pages after it order the window the same way: rows that
/// carry no answer sink, then a confirmed-resolved row sinks below the
/// unresolved ones. Walking get_more one row at a time must visit every
/// row page one returned exactly once, in page one's order.
#[tokio::test]
async fn page_one_and_get_more_cover_every_result_once_in_the_same_order() {
    let phrases = [
        "Checking past sessions",
        "too painful",
        "Resolved note",
        "Phase 0 spike",
    ];
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    for (row, vector) in [
        chunk(
            "echo-chunk",
            "conv1",
            0.95,
            "why did we drop Docker and Qdrant for a single Rust binary?\n\nChecking past sessions for the actual reasoning.",
        ),
        chunk(
            "answer-chunk",
            "conv2",
            0.90,
            "Installing it was too painful, and the scale never needed a database server.",
        ),
        chunk(
            "resolved-chunk",
            "conv3",
            0.92,
            "Resolved note: the Qdrant container was removed in the v8 rewrite and verified.",
        ),
        chunk(
            "plain-chunk",
            "conv4",
            0.85,
            "A Phase 0 spike showed an embedded index was faster anyway.",
        ),
    ] {
        storage.insert_chunk(&row, &vector).unwrap();
        engine.insert_chunk(row.id.clone(), vector);
    }
    storage
        .insert_resolutions(
            &["resolved-chunk".to_string()],
            "resolved",
            "shipped and verified",
            None,
            "user_confirmed",
        )
        .unwrap();
    let search = Arc::new(RwLock::new(engine));
    let scope =
        SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
            .unwrap();
    let q = [1.0f32, 0.0, 0.0, 0.0];
    let order_of = |text: &str| -> Vec<usize> {
        let mut found: Vec<(usize, usize)> = phrases
            .iter()
            .enumerate()
            .filter_map(|(at, phrase)| text.find(phrase).map(|pos| (pos, at)))
            .collect();
        found.sort();
        found.into_iter().map(|(_, at)| at).collect()
    };

    let page_one = reflect_on_past_with_vec_in_scope(
        &storage,
        &search,
        &q,
        QUERY,
        10,
        0.0,
        &scope,
        0,
        true,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();
    let first_page = order_of(&page_one);

    let mut paged = Vec::new();
    for offset in 0..4 {
        let page = get_more_results_in_scope(
            &storage,
            &search,
            &q,
            QUERY,
            offset,
            1,
            0.0,
            &scope,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        let seen = order_of(&page);
        assert_eq!(seen.len(), 1, "offset {offset}: {page}");
        paged.push(seen[0]);
    }
    let beyond = get_more_results_in_scope(
        &storage,
        &search,
        &q,
        QUERY,
        4,
        1,
        0.0,
        &scope,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();
    assert!(beyond.contains("No results"), "{beyond}");

    let mut every_row = paged.clone();
    every_row.sort();
    assert_eq!(every_row, vec![0, 1, 2, 3], "each row once: {paged:?}");
    assert_eq!(paged, first_page, "same order as page one");
    // Echo (0) above the resolved row (2), both below the answers.
    assert_eq!(&paged[2..], &[0, 2], "{paged:?}");
}

/// Fifty earlier askings of the question sit in the vector window ahead of
/// the three answers. A page of five used to be cut from a fifteen-row
/// window the echoes filled; the pool is now wide enough to reach past them,
/// the sink moves them down, and the page is full with the answers on it.
/// Each asking is worded a little differently: byte-identical text in several
/// conversations is shown once, so fifty identical echoes would be one row.
#[tokio::test]
async fn a_five_row_page_is_full_and_holds_the_answers_behind_fifty_echoes() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    let mut rows = Vec::new();
    for n in 0..50 {
        rows.push(chunk(
            &format!("echo-{n:02}"),
            &format!("echoconv{n:02}"),
            0.95 - n as f32 * 0.002,
            &format!("why did we drop Docker and Qdrant for a single Rust binary, asking {n}?\n\nChecking past sessions for the actual reasoning."),
        ));
    }
    for (n, text) in [
        "Installing it was too painful, and the scale never needed a database server.",
        "A Phase 0 spike showed an embedded index was faster anyway.",
        "Docker Desktop was required for every install, and users gave up there.",
    ]
    .iter()
    .enumerate()
    {
        rows.push(chunk(
            &format!("answer-{n}"),
            &format!("answerconv{n}"),
            0.60 - n as f32 * 0.01,
            text,
        ));
    }
    for (row, vector) in &rows {
        storage.insert_chunk(row, vector).unwrap();
        engine.insert_chunk(row.id.clone(), vector.clone());
    }
    let search = Arc::new(RwLock::new(engine));
    let scope =
        SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
            .unwrap();
    let page = reflect_on_past_with_vec_in_scope(
        &storage,
        &search,
        &[1.0, 0.0, 0.0, 0.0],
        QUERY,
        5,
        0.0,
        &scope,
        0,
        true,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();
    let shown = ids(&page);
    assert_eq!(shown.len(), 5, "a full page: {shown:?}");
    for answer in ["answer-0", "answer-1", "answer-2"] {
        assert!(
            shown.iter().any(|id| id == answer),
            "{answer} must be on the five-row page: {shown:?}"
        );
    }
}

/// A pool mixing vector chunks, a vector reflection, a keyword-only chunk
/// and a keyword-only reflection, walked three rows at a time. Every row
/// appears once and the pages concatenate to the single large page, in the
/// order page one fixed. Page one's own order is the one fusion, sink and
/// settling produced, so the later pages cannot rebuild a vector-only one.
#[tokio::test]
async fn three_pages_of_a_mixed_pool_are_slices_of_one_order() {
    let phrases = [
        "ALPHA-vector",
        "BRAVO-vector",
        "CHARLIE-vector",
        "DELTA-vector",
        "ECHO-reflection-vector",
        "FOXTROT-needle-only-chunk",
        "GOLF-needle-only-reflection",
        "HOTEL-vector",
        "INDIA-echo-row",
    ];
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    for (n, (text, cos)) in [
        ("ALPHA-vector Qdrant was removed from the install path.", 0.90),
        ("BRAVO-vector the embedded index replaced the server.", 0.84),
        ("CHARLIE-vector Docker is no longer needed to run it.", 0.80),
        ("DELTA-vector the scale never needed a database server.", 0.74),
        ("HOTEL-vector a Phase 0 spike showed it was faster.", 0.70),
        (
            "INDIA-echo-row why did we drop Docker and Qdrant for a single Rust binary?\n\nChecking past sessions for the actual reasoning.",
            0.88,
        ),
    ]
    .iter()
    .enumerate()
    {
        let (row, vector) = chunk(&format!("row-{n}"), &format!("conv{n}"), *cos, text);
        storage.insert_chunk(&row, &vector).unwrap();
        engine.insert_chunk(row.id.clone(), vector);
    }
    // Keyword-only chunk: in the store and its keyword index, not in the
    // vector index.
    let (needle, needle_vec) = chunk(
        "needle-chunk",
        "convneedle",
        0.10,
        "FOXTROT-needle-only-chunk the Qdrant Docker drop is recorded here.",
    );
    storage.insert_chunk(&needle, &needle_vec).unwrap();
    // A vector reflection, and a keyword-only one.
    let tags = vec![
        "narrative_extracted_v3".to_string(),
        "project_echoproj".to_string(),
    ];
    storage
        .insert_reflection(
            "refl-vector",
            "ECHO-reflection-vector Qdrant was dropped because install was painful.",
            &tags,
            &vec_at(0.78),
        )
        .unwrap();
    engine.insert_reflection("refl-vector".to_string(), vec_at(0.78));
    storage
        .insert_reflection(
            "refl-needle",
            "GOLF-needle-only-reflection we drop Docker and Qdrant to stay a single binary.",
            &tags,
            &vec_at(0.05),
        )
        .unwrap();
    let search = Arc::new(RwLock::new(engine));
    let scope =
        SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
            .unwrap();
    let q = [1.0f32, 0.0, 0.0, 0.0];
    let order_of = |text: &str| -> Vec<usize> {
        let mut found: Vec<(usize, usize)> = phrases
            .iter()
            .enumerate()
            .filter_map(|(at, phrase)| text.find(phrase).map(|pos| (pos, at)))
            .collect();
        found.sort();
        found.into_iter().map(|(_, at)| at).collect()
    };

    let whole = reflect_on_past_with_vec_in_scope(
        &storage,
        &search,
        &q,
        QUERY,
        9,
        0.0,
        &scope,
        0,
        true,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();
    let whole_order = order_of(&whole);
    assert_eq!(whole_order.len(), 9, "all nine rows come back: {whole}");

    let first = reflect_on_past_with_vec_in_scope(
        &storage,
        &search,
        &q,
        QUERY,
        3,
        0.0,
        &scope,
        0,
        true,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();
    let mut paged = order_of(&first);
    for offset in [3, 6] {
        let page = get_more_results_in_scope(
            &storage,
            &search,
            &q,
            QUERY,
            offset,
            3,
            0.0,
            &scope,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        let seen = order_of(&page);
        assert_eq!(seen.len(), 3, "offset {offset}: {page}");
        paged.extend(seen);
    }
    let mut every_row = paged.clone();
    every_row.sort();
    assert_eq!(
        every_row,
        (0..9).collect::<Vec<_>>(),
        "each row once: {paged:?}"
    );
    assert_eq!(paged, whole_order, "the pages are slices of one order");
}

/// Pages after the first are cut from the pool page one settled, not from a
/// fresh search: a row that appears in the index between the two calls does
/// not shuffle the order the first page promised.
#[tokio::test]
async fn later_pages_come_from_the_pool_page_one_built() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    for n in 0..6 {
        let (row, vector) = chunk(
            &format!("row-{n}"),
            &format!("conv{n}"),
            0.90 - n as f32 * 0.05,
            &format!("ROW{n} the install was painful for reason number {n}."),
        );
        storage.insert_chunk(&row, &vector).unwrap();
        engine.insert_chunk(row.id.clone(), vector);
    }
    let search = Arc::new(RwLock::new(engine));
    let scope =
        SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
            .unwrap();
    let q = [1.0f32, 0.0, 0.0, 0.0];
    let _ = reflect_on_past_with_vec_in_scope(
        &storage,
        &search,
        &q,
        QUERY,
        2,
        0.0,
        &scope,
        0,
        true,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();
    // A better row arrives after page one.
    let (late, late_vec) = chunk("row-late", "convlate", 0.99, "LATE the best row of all.");
    storage.insert_chunk(&late, &late_vec).unwrap();
    search.write().await.insert_chunk(late.id.clone(), late_vec);
    let page = get_more_results_in_scope(
        &storage,
        &search,
        &q,
        QUERY,
        2,
        2,
        0.0,
        &scope,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();
    assert!(page.contains("ROW2") && page.contains("ROW3"), "{page}");
    assert!(!page.contains("LATE"), "{page}");
}

/// Walk past the first fifteen rows at the default page size: page one at
/// limit 5 holds a sixty-row pool (seventy rows exist, so the window is
/// full), and offsets 5 to 25 are all slices of it.
/// Each row appears once, in page one's order, and a row that arrives after
/// page one never shows up.
#[tokio::test]
async fn a_walk_in_pages_of_five_stays_on_the_pool_page_one_built() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    for n in 0..70 {
        let (row, vector) = chunk(
            &format!("row-{n:02}"),
            &format!("conv{n:02}"),
            0.95 - n as f32 * 0.005,
            &format!("ROW{n:02} the install was painful for reason number {n}."),
        );
        storage.insert_chunk(&row, &vector).unwrap();
        engine.insert_chunk(row.id.clone(), vector);
    }
    let search = Arc::new(RwLock::new(engine));
    let scope =
        SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
            .unwrap();
    let q = [1.0f32, 0.0, 0.0, 0.0];
    let rows_in = |text: &str| -> Vec<usize> {
        let mut found: Vec<(usize, usize)> = (0..70)
            .filter_map(|n| text.find(&format!("ROW{n:02}")).map(|pos| (pos, n)))
            .collect();
        found.sort();
        found.into_iter().map(|(_, n)| n).collect()
    };
    let first = reflect_on_past_with_vec_in_scope(
        &storage,
        &search,
        &q,
        QUERY,
        5,
        0.0,
        &scope,
        0,
        true,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();
    let mut walked = rows_in(&first);
    assert_eq!(walked.len(), 5, "{first}");
    let (late, late_vec) = chunk("row-late", "convlate", 0.99, "LATE the best row of all.");
    storage.insert_chunk(&late, &late_vec).unwrap();
    search.write().await.insert_chunk(late.id.clone(), late_vec);
    for offset in (5..30).step_by(5) {
        let page = get_more_results_in_scope(
            &storage,
            &search,
            &q,
            QUERY,
            offset,
            5,
            0.0,
            &scope,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        assert!(!page.contains("LATE"), "offset {offset}: {page}");
        let seen = rows_in(&page);
        assert_eq!(seen.len(), 5, "offset {offset}: {page}");
        walked.extend(seen);
    }
    assert_eq!(walked, (0..30).collect::<Vec<_>>(), "once each, in order");
}

/// The row markers (`ECHO007`, `ANSWER2`, `ROW5`) of a page, in order. Page
/// one and the later pages name rows differently, so tests read the text; the
/// summary's preview repeats the first row, so a marker counts once a page.
fn marks(out: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    out.split_whitespace()
        .flat_map(|word| word.split(|c: char| !c.is_ascii_alphanumeric()))
        .filter(|word| {
            ["ECHO", "ANSWER", "ROW"].iter().any(|tag| {
                word.strip_prefix(tag).is_some_and(|rest| {
                    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
                })
            })
        })
        .filter(|word| seen.insert(*word))
        .map(str::to_string)
        .collect()
}

/// Rows the vector window of page one cannot see, an echo-filled first
/// 120, then five answers. Rebuilding the pool wider for the offset past
/// the cached one sinks the echoes behind the answers; the page must not
/// repeat the echoes it showed or skip the answers. What a page published
/// stays where it was shown.
#[tokio::test]
async fn a_page_past_the_cached_pool_repeats_no_row_and_shows_the_answers() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    for n in 0..120 {
        let (row, vector) = chunk(
            &format!("echo-{n:03}"),
            &format!("convecho{n:03}"),
            0.99 - n as f32 * 0.002,
            &format!("ECHO{n:03} why did we drop Docker and Qdrant?"),
        );
        storage.insert_chunk(&row, &vector).unwrap();
        engine.insert_chunk(row.id.clone(), vector);
    }
    for n in 0..5 {
        let (row, vector) = chunk(
            &format!("answer-{n}"),
            &format!("convanswer{n}"),
            0.60 - n as f32 * 0.01,
            &format!("ANSWER{n} Qdrant went because installing a server was painful, reason {n}."),
        );
        storage.insert_chunk(&row, &vector).unwrap();
        engine.insert_chunk(row.id.clone(), vector);
    }
    let search = Arc::new(RwLock::new(engine));
    let scope =
        SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
            .unwrap();
    let q = [1.0f32, 0.0, 0.0, 0.0];
    let first = reflect_on_past_with_vec_in_scope(
        &storage,
        &search,
        &q,
        QUERY,
        5,
        0.0,
        &scope,
        0,
        true,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();
    let mut seen: Vec<String> = marks(&first);
    assert_eq!(seen.len(), 5, "{first}");
    for offset in (5..125).step_by(5) {
        let page = get_more_results_in_scope(
            &storage,
            &search,
            &q,
            QUERY,
            offset,
            5,
            0.0,
            &scope,
            ConsumptionMode::Off,
            false,
        )
        .await
        .unwrap();
        let got = marks(&page);
        assert_eq!(got.len(), 5, "offset {offset}: {page}");
        if offset == 120 {
            assert!(
                got.iter().all(|id| id.starts_with("ANSWER")),
                "the five answers follow the 120 echoes already shown: {got:?}"
            );
        }
        seen.extend(got);
    }
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        seen.len(),
        "no row repeats across pages: {seen:?}"
    );
    assert_eq!(
        seen.iter().filter(|id| id.starts_with("ANSWER")).count(),
        5,
        "every answer appears once: {seen:?}"
    );
}

fn six_rows() -> (Arc<Storage>, Arc<RwLock<SearchEngine>>) {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    for n in 0..8 {
        let (row, vector) = chunk(
            &format!("row-{n}"),
            &format!("conv{n}"),
            0.90 - n as f32 * 0.04,
            &format!("ROW{n} the install was painful for reason number {n}."),
        );
        storage.insert_chunk(&row, &vector).unwrap();
        engine.insert_chunk(row.id.clone(), vector);
    }
    (storage, Arc::new(RwLock::new(engine)))
}

type Rig = (Arc<Storage>, Arc<RwLock<SearchEngine>>, SearchProjectScope);

const Q4: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

async fn page_one() -> Rig {
    let (storage, search) = six_rows();
    let scope =
        SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
            .unwrap();
    let first = reflect_on_past_with_vec_in_scope(
        &storage,
        &search,
        &Q4,
        QUERY,
        5,
        0.0,
        &scope,
        0,
        true,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();
    assert!(first.contains("ROW0") && !first.contains("ROW5"), "{first}");
    (storage, search, scope)
}

async fn page_two(rig: &Rig) -> String {
    let (storage, search, scope) = rig;
    get_more_results_in_scope(
        storage,
        search,
        &Q4,
        QUERY,
        5,
        3,
        0.0,
        scope,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap()
}

/// A verdict another process wrote after page one (a direct ledger insert
/// here: the in-process cache is not told) still reaches the next page.
#[tokio::test]
async fn a_verdict_recorded_after_page_one_shows_on_the_next_page() {
    let rig = page_one().await;
    let before = rig.0.verdict_generation();
    rig.0
        .insert_resolutions(
            &["row-5".to_string()],
            "resolved",
            "fixed in 9.5.8",
            None,
            crate::storage::queries::RESOLUTION_SOURCE_USER_CONFIRMED,
        )
        .unwrap();
    assert_ne!(before, rig.0.verdict_generation());
    let page = page_two(&rig).await;
    assert!(page.contains("<resolution>"), "{page}");
    let order = marks(&page);
    assert_eq!(
        order.last().map(String::as_str),
        Some("ROW5"),
        "a confirmed-resolved row sinks below the unresolved ones: {order:?}"
    );
}

/// Recording a verdict through `csr_resolve` drops the pools of the store.
#[tokio::test]
async fn resolving_a_chunk_drops_the_cached_pools() {
    let rig = page_one().await;
    let held = |storage: &Arc<Storage>| {
        POOL_CACHE.lock().unwrap().iter().any(|entry| {
            entry
                .storage
                .upgrade()
                .is_some_and(|held| Arc::ptr_eq(&held, storage))
        })
    };
    assert!(held(&rig.0), "page one cached its pool");
    resolve_chunks(
        &rig.0,
        vec!["row-6".to_string()],
        "regressed".to_string(),
        "broke again".to_string(),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(!held(&rig.0), "the store's pools are gone");
    let page = page_two(&rig).await;
    assert_eq!(marks(&page), ["ROW5", "ROW6", "ROW7"], "{page}");
}

/// `project="all"` with a current project boosts that family; the same
/// `all` search without one does not (the health probe). Their pools are
/// not interchangeable.
#[tokio::test]
async fn a_pool_built_for_another_current_project_is_not_reused() {
    let (storage, search) = six_rows();
    let boosted =
        SearchProjectScope::resolve_with(&storage, Some("all"), Some("echoproj"), None, None)
            .unwrap();
    let unboosted =
        SearchProjectScope::resolve_with(&storage, Some("all"), None, None, None).unwrap();
    let other =
        SearchProjectScope::resolve_with(&storage, Some("all"), Some("elsewhere"), None, None)
            .unwrap();
    let q = [1.0f32, 0.0, 0.0, 0.0];
    reflect_on_past_with_vec_in_scope(
        &storage,
        &search,
        &q,
        QUERY,
        5,
        0.0,
        &boosted,
        0,
        true,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap();
    let key = |scope: &SearchProjectScope| {
        PoolKey::new(
            QUERY,
            scope,
            0.0,
            SearchMode::Hybrid,
            true,
            ConsumptionMode::Off,
            false,
            storage.verdict_generation(),
        )
    };
    assert!(cached_pool(&storage, &key(&boosted), 5).is_some());
    assert!(cached_pool(&storage, &key(&unboosted), 5).is_none());
    assert!(cached_pool(&storage, &key(&other), 5).is_none());
}

/// Codex's inputs, whole: none of these rows is a question echo or a CSR
/// payload, so none of them sinks.
#[test]
fn the_rows_codex_found_sinking_stay_where_their_text_puts_them() {
    let cases = [
        (
            format!("[reflection] ## User Request\nwhy did the import time out?\n\n## Active Issues\nFix: raise the timeout to 120s.\n\n---\n{SIG}"),
            "why did the import time out",
        ),
        (
            "WHY: storage\n\nNo evidence chain found.\n\nRoot cause: migration 42 wrote the lock.".to_string(),
            "storage",
        ),
        (
            "[mcp__claude_self_reflect__csr_why]\nFix: use SQLite.".to_string(),
            "why sqlite",
        ),
        (
            "How long we retain caches is 300 seconds".to_string(),
            "how long do we retain caches",
        ),
        (
            "Recall uses SQLite and HNSW".to_string(),
            "what does recall use",
        ),
    ];
    for (row, query) in cases {
        assert_eq!(non_answer_class(&row, query), 0, "{row}");
    }
}

/// A memory whose text opens with the query is promoted ahead of fusion,
/// whatever its similarity.
#[tokio::test]
async fn an_exact_opener_memory_is_first_whatever_its_similarity() {
    let rows = vec![chunk(
        "strong-chunk",
        "conv1",
        0.90,
        "The embedded index was faster, so the server went away.",
    )];
    let reflections = vec![(
        "opener-memory",
        &["memory-decay", "echoproj"][..],
        format!("{QUERY}: because installing it was painful."),
        0.20,
    )];
    let order = recall(&rows, &reflections).await;
    assert_eq!(order[0], "opener-memory", "{order:?}");
}

#[tokio::test]
async fn the_answer_outranks_every_echo_of_its_own_question() {
    let (rows, reflections) = probe_rows();
    let order = recall(&rows, &reflections).await;
    // `echo-chunk` shares conv1 with `answer-chunk`: one row per session,
    // and the answer is the row that stands for conv1.
    assert!(!order.contains(&"echo-chunk".to_string()), "{order:?}");
    let first_echo = ["story_conv1", "extracted_v3_conv1"]
        .iter()
        .map(|id| position(&order, id))
        .min()
        .unwrap();
    for answer in ["answer-chunk", "answer-chunk-2"] {
        assert!(
            position(&order, answer) < first_echo,
            "{answer} must lead the question echoes: {order:?}"
        );
    }
    assert!(
        ["answer-chunk", "answer-chunk-2"].contains(&order[0].as_str()),
        "rank 1 must carry an answer: {order:?}"
    );
}

/// A forked worker's tmux pane recording is indexed as text. Its frames
/// repeat the typed question and a spinner, so it matches the question
/// best and says nothing. Live excerpt (agent-aa8201b27121421bd).
const KEYSTROKE_FRAMES: &str = r#"y did we drop Docker and Qdrant
24 20.706 9 ❯ why did we drop Docker and Qdrant f
25 20.87 9 ❯ why did we drop Docker and Qdrant for
26 21.034 9 ❯ why did we drop Docker and Qdrant for a
27 21.2 9 ❯ why did we drop Docker and Qdrant for a s
28 21.367 9 ❯ why did we drop Docker and Qdrant for a sin
29 21.532 9 ❯ why did we drop Docker and Qdrant for a singl
30 21.697 9 ❯ why did we drop Docker and Qdrant for a single
31 21.863 9 ❯ why did we drop Docker and Qdrant for a single Ru
32 22.028 9 ❯ why did we drop Docker and Qdrant for a single Rust
33 22.197 9 ❯ why did we drop Docker and Qdrant for a single Rust b
34 22.363 9 ❯ why did we drop Docker and Qdrant for a single Rust bin
35 22.528 9 ❯ why did we drop Docker and Qdrant for a single Rust binar
36 22.693 9 ❯ why did we drop Docker and Qdrant for a single Rust binary
37 23.53 11 ❯ why did we d"#;
const SPINNER_FRAMES: &str = r#"154 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | · Actioning… (3s · thinking with medium effort) | ❯
55 27.318 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✢ Actioning… (3s · thinking with medium effort) | ❯
56 27.485 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✶ Actioning… (4s · thinking with medium effort) |"#;

#[tokio::test]
async fn terminal_replay_frames_do_not_outrank_answers() {
    let (mut rows, reflections) = probe_rows();
    rows.push(chunk("keystroke-frames", "conv7", 0.90, KEYSTROKE_FRAMES));
    rows.push(chunk("spinner-frames", "conv7", 0.88, SPINNER_FRAMES));
    let order = recall(&rows, &reflections).await;
    // Both recordings are conv7: one row stands for the session.
    assert!(!order.contains(&"spinner-frames".to_string()), "{order:?}");
    for frames in ["keystroke-frames"] {
        for answer in ["answer-chunk", "answer-chunk-2"] {
            assert!(
                position(&order, answer) < position(&order, frames),
                "{frames} must sit below {answer}: {order:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_chunk_with_one_prompt_line_among_real_content_keeps_its_rank() {
    let (mut rows, reflections) = probe_rows();
    rows.push(chunk(
        "screen-with-answer",
        "conv8",
        0.95,
        "\u{276f} why did we drop Docker and Qdrant for a single Rust binary?\n\u{23fa} Installing it was too painful, and the scale never needed a database server. A Phase 0 spike then showed an embedded index was faster anyway.",
    ));
    let order = recall(&rows, &reflections).await;
    assert_eq!(order[0], "screen-with-answer", "{order:?}");
}

/// CSR's own `csr_why` and search output is left out of recall before any
/// ranking (it used to sink to the bottom): the answers are all that is left.
#[tokio::test]
async fn csr_why_and_search_output_are_left_out_of_recall() {
    let (rows, reflections) = probe_rows();
    let order = recall(&rows, &reflections).await;
    for noise in ["why-row", "search-row"] {
        assert!(
            !order.contains(&noise.to_string()),
            "{noise} is CSR's own output and must not be recalled: {order:?}"
        );
    }
    for answer in ["answer-chunk", "answer-chunk-2"] {
        assert!(order.contains(&answer.to_string()), "{order:?}");
    }
}

#[tokio::test]
async fn a_repeated_question_that_also_carries_its_answer_is_not_demoted() {
    let (mut rows, reflections) = probe_rows();
    rows.push(chunk(
        "repeat-with-answer",
        "conv3",
        0.60,
        "why did we drop Docker and Qdrant? Because installing Docker was too painful and an embedded index was faster.",
    ));
    let order = recall(&rows, &reflections).await;
    // `echo-chunk` shares conv1 with `answer-chunk`: one row per session.
    assert!(!order.contains(&"echo-chunk".to_string()), "{order:?}");
    for echo in ["story_conv1", "extracted_v3_conv1"] {
        assert!(
            position(&order, "repeat-with-answer") < position(&order, echo),
            "a question that carries its answer must lead the bare echoes: {order:?}"
        );
    }
    assert!(!order.contains(&"why-row".to_string()), "{order:?}");
}

#[tokio::test]
async fn a_derived_story_that_opens_with_the_query_is_not_promoted_to_first() {
    let (rows, reflections) = probe_rows();
    let order = recall(&rows, &reflections).await;
    assert_ne!(order[0], "story_conv1", "{order:?}");
}

#[tokio::test]
async fn an_explicit_memory_that_opens_with_the_query_is_still_promoted() {
    let (rows, mut reflections) = probe_rows();
    reflections.push((
        "5eb5aebb-65ab-4fba-9917-86cb83845154",
        &["csr:memory", "echoproj"][..],
        "why did we drop Docker and Qdrant: the install burden and the scale never needed a server.".to_string(),
        0.30,
    ));
    let order = recall(&rows, &reflections).await;
    assert_eq!(
        order[0], "5eb5aebb-65ab-4fba-9917-86cb83845154",
        "{order:?}"
    );
}

// ---- echo filter: CSR's own output and logged copies of the query ----

fn set_author(storage: &Storage, id: &str, conv: &str, author: Speaker) {
    storage
        .insert_chunk_provenance(
            id,
            &crate::provenance::ChunkProvenance {
                author,
                source_conv_id: conv.into(),
                supersedes: None,
            },
        )
        .unwrap();
}

/// `echoes` rows that log the query being asked (a tool result, a call
/// rendering, a typed prompt) in the best vector places, then `answers`
/// rows that answer it.
fn echo_filled(echoes: usize, answers: usize) -> (Arc<Storage>, Arc<RwLock<SearchEngine>>) {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    for n in 0..echoes {
        let (text, author) = match n % 3 {
            0 => (
                format!("ECHO{n:03} {{\"query\": \"{QUERY}\", \"limit\": 5}}"),
                Speaker::ToolResult,
            ),
            1 => (
                format!("ECHO{n:03} Pulling CSR context.\n\n[mcp__claude-self-reflect__csr_reflect_on_past: {QUERY}]"),
                Speaker::Assistant,
            ),
            _ => (
                format!("{QUERY} and then fix it, please ECHO{n:03}"),
                Speaker::User,
            ),
        };
        let (row, vector) = chunk(
            &format!("echo-{n:03}"),
            &format!("convecho{n:03}"),
            0.99 - n as f32 * 0.002,
            &text,
        );
        storage.insert_chunk(&row, &vector).unwrap();
        set_author(&storage, &row.id, &row.conversation_id, author);
        engine.insert_chunk(row.id.clone(), vector);
    }
    for n in 0..answers {
        let (row, vector) = chunk(
            &format!("answer-{n}"),
            &format!("convanswer{n}"),
            0.50 - n as f32 * 0.01,
            // No word of the query, so only the vector lane can reach it.
            &format!("ANSWER{n} installing a server was painful for reason {n}."),
        );
        storage.insert_chunk(&row, &vector).unwrap();
        set_author(&storage, &row.id, &row.conversation_id, Speaker::Assistant);
        engine.insert_chunk(row.id.clone(), vector);
    }
    (storage, Arc::new(RwLock::new(engine)))
}

async fn limit_five(storage: &Arc<Storage>, search: &Arc<RwLock<SearchEngine>>) -> String {
    let scope =
        SearchProjectScope::resolve_with(storage, Some("all"), Some("echoproj"), None, None)
            .unwrap();
    reflect_on_past_with_vec_in_scope(
        storage,
        search,
        &Q4,
        QUERY,
        5,
        0.0,
        &scope,
        0,
        true,
        ConsumptionMode::Off,
        false,
    )
    .await
    .unwrap()
}

/// The trap from the first attempt: a filter that drops the echoes from a
/// page fetched at exactly `limit` and never refills it returned three
/// weak rows at limit 5. Eight logged copies outrank six answers; the
/// page is still five answers.
#[tokio::test]
async fn a_limit_five_page_is_refilled_with_answers_when_logged_copies_outrank_them() {
    let (storage, search) = echo_filled(8, 6);
    let page = limit_five(&storage, &search).await;
    let seen = marks(&page);
    assert_eq!(seen.len(), 5, "five rows when six answers exist: {page}");
    assert!(
        seen.iter().all(|mark| mark.starts_with("ANSWER")),
        "no logged copy of the query on the page: {seen:?}"
    );
}

/// More logged copies than the first window holds: the window is widened
/// once and the page is still five answers.
#[tokio::test]
async fn a_limit_five_page_widens_the_window_when_it_held_only_logged_copies() {
    let (storage, search) = echo_filled(130, 6);
    let page = limit_five(&storage, &search).await;
    let seen = marks(&page);
    assert_eq!(seen.len(), 5, "five rows when six answers exist: {page}");
    assert!(
        seen.iter().all(|mark| mark.starts_with("ANSWER")),
        "{seen:?}"
    );
}

/// Fewer answers than the page: every answer shows, no logged copy fills
/// the rest.
#[tokio::test]
async fn a_page_with_fewer_answers_than_places_shows_no_logged_copy() {
    let (storage, search) = echo_filled(8, 3);
    let page = limit_five(&storage, &search).await;
    let seen = marks(&page);
    assert_eq!(seen, ["ANSWER0", "ANSWER1", "ANSWER2"], "{page}");
}

/// Rows that repeat the query's words but are not records of it stay.
#[tokio::test]
async fn prose_that_repeats_the_query_is_not_dropped() {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    let rows = [
        // An answer that restates the question inside its own sentence.
        (
            "keep-prose",
            Speaker::Assistant,
            "ROW1 You asked why did we drop Docker and Qdrant, and the answer is install burden."
                .to_string(),
        ),
        // A code read that mentions the markers is a mention, not CSR's output.
        (
            "keep-code",
            Speaker::ToolResult,
            "ROW2 const NEXT: &str = \"NEXT:\"; const LAST: &str = \"LAST:\"; // TODOS: ANCHORS:"
                .to_string(),
        ),
    ];
    for (n, (id, author, text)) in rows.iter().enumerate() {
        let (row, vector) = chunk(id, &format!("conv{n}"), 0.90 - n as f32 * 0.05, text);
        storage.insert_chunk(&row, &vector).unwrap();
        set_author(&storage, id, &row.conversation_id, *author);
        engine.insert_chunk(row.id.clone(), vector);
    }
    let search = Arc::new(RwLock::new(engine));
    let page = limit_five(&storage, &search).await;
    assert_eq!(marks(&page), ["ROW1", "ROW2"], "{page}");
}
