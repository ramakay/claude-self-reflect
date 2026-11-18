//! Contract tests for explicit search: an exact identifier is found only where
//! it is written, every positive answer carries its handles, and identical text
//! is shown once. Fixtures are synthetic.

use super::*;

const Q: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

async fn quick(rows: &[(&str, &str, &str, [f32; 4])], query: &str, project: &str) -> String {
    let (storage, search) = quick_check_fixture(rows);
    quick_check_with_vec(&storage, &search, &Q, query, 0.3, Some(project))
        .await
        .unwrap()
}

/// The nearest chunk by similarity holds a different number; the chunk that
/// holds the asked number is far away in vector space. The old answer was
/// `found=true` with the near chunk (score 1.0, the wrong number).
#[tokio::test]
async fn d1_a_long_number_is_answered_from_the_chunk_that_holds_it() {
    let xml = quick(
        &[
            (
                "near",
                "alpha",
                "order 88824670039 shipped",
                [1.0, 0.0, 0.0, 0.0],
            ),
            (
                "exact",
                "alpha",
                "tracking 3256813322194603 delivered",
                [0.0, 1.0, 0.0, 0.0],
            ),
        ],
        "3256813322194603",
        "all",
    )
    .await;
    assert!(xml.contains("<found>true</found>"), "{xml}");
    assert!(xml.contains("<exact>true</exact>"), "{xml}");
    assert!(xml.contains("<chunk_id>exact</chunk_id>"), "{xml}");
    assert!(
        xml.contains("<conversation_id>conv-exact</conversation_id>"),
        "{xml}"
    );
    assert!(xml.contains("3256813322194603"), "{xml}");
    assert!(!xml.contains("88824670039"), "{xml}");
}

#[tokio::test]
async fn d1_a_near_number_or_a_longer_number_is_not_the_number() {
    let xml = quick(
        &[
            (
                "near",
                "alpha",
                "order 88824670039 shipped",
                [1.0, 0.0, 0.0, 0.0],
            ),
            (
                "longer",
                "alpha",
                "reference 32568133221946031 here",
                [0.9, 0.4, 0.0, 0.0],
            ),
            (
                "off-by-one",
                "alpha",
                "tracking 3256813322194604 here",
                [0.8, 0.5, 0.0, 0.0],
            ),
        ],
        "3256813322194603",
        "all",
    )
    .await;
    assert!(xml.contains("<found>false</found>"), "{xml}");
    assert!(xml.contains("<exact>false</exact>"), "{xml}");
    assert!(xml.contains("<count>0</count>"), "{xml}");
    // The similar chunk is offered, labelled, never as the record.
    assert!(xml.contains("<nearest_related>"), "{xml}");
    assert!(xml.contains("does not contain the identifier"), "{xml}");
    assert!(!xml.contains("<found>true</found>"), "{xml}");
}

#[tokio::test]
async fn d1_a_same_prefix_id_is_not_the_id_but_the_id_itself_is_found() {
    let rows = [
        (
            "longer",
            "alpha",
            "case Bond-ZZHE877512 closed",
            [1.0, 0.0, 0.0, 0.0],
        ),
        (
            "suffixed",
            "alpha",
            "case Bond-ZZHE87751-B open",
            [0.9, 0.4, 0.0, 0.0],
        ),
        (
            "spaced",
            "alpha",
            "case Bond ZZHE87751 open",
            [0.8, 0.5, 0.0, 0.0],
        ),
    ];
    let miss = quick(&rows, "what is Bond-ZZHE87751", "all").await;
    assert!(miss.contains("<found>false</found>"), "{miss}");
    assert!(miss.contains("<exact>false</exact>"), "{miss}");
    let mut with_it = rows.to_vec();
    with_it.push((
        "it",
        "alpha",
        "case Bond-ZZHE87751 closed on Friday",
        [0.0, 0.0, 1.0, 0.0],
    ));
    let hit = quick(&with_it, "what is Bond-ZZHE87751", "all").await;
    assert!(hit.contains("<found>true</found>"), "{hit}");
    assert!(hit.contains("<chunk_id>it</chunk_id>"), "{hit}");
    assert!(
        hit.contains("<matched_identifiers>bond-zzhe87751</matched_identifiers>"),
        "{hit}"
    );
}

/// The identifier exists, but only in a conversation outside the asked project
/// family: inside the scope the answer is `found=false`.
#[tokio::test]
async fn d1_an_identifier_that_only_another_project_holds_is_not_found_in_scope() {
    let rows = [
        (
            "theirs",
            "elsewhere-project",
            "invoice INV_2024_0007 paid",
            [0.0, 1.0, 0.0, 0.0],
        ),
        (
            "mine",
            "scope-project",
            "unrelated notes about retries",
            [1.0, 0.0, 0.0, 0.0],
        ),
    ];
    let scoped = quick(&rows, "INV_2024_0007", "scope-project").await;
    assert!(scoped.contains("<found>false</found>"), "{scoped}");
    let everywhere = quick(&rows, "INV_2024_0007", "all").await;
    assert!(everywhere.contains("<found>true</found>"), "{everywhere}");
    assert!(
        everywhere.contains("<conversation_id>conv-theirs</conversation_id>"),
        "{everywhere}"
    );
}

/// The id is in the chat because the asker just typed it: the question itself,
/// and CSR's own logged call, are not evidence.
#[tokio::test]
async fn d1_a_restatement_of_the_question_is_not_evidence() {
    let echoes = [
        (
            "asked",
            "alpha",
            "did we ever see 3256813322194603?",
            [1.0, 0.0, 0.0, 0.0],
        ),
        (
            "call",
            "alpha",
            "[mcp__claude-self-reflect__csr_quick_check: 3256813322194603]",
            [0.9, 0.4, 0.0, 0.0],
        ),
    ];
    let miss = quick(&echoes, "3256813322194603", "all").await;
    assert!(miss.contains("<found>false</found>"), "{miss}");
    let mut with_answer = echoes.to_vec();
    with_answer.push((
        "answer",
        "alpha",
        "tracking 3256813322194603 delivered on the 4th",
        [0.0, 1.0, 0.0, 0.0],
    ));
    let hit = quick(&with_answer, "3256813322194603", "all").await;
    assert!(hit.contains("<found>true</found>"), "{hit}");
    assert!(hit.contains("<chunk_id>answer</chunk_id>"), "{hit}");
}

#[tokio::test]
async fn d1_a_query_without_an_identifier_answers_as_before_and_carries_handles() {
    let xml = quick(
        &[("top", "project-a", "top candidate", [1.0, 0.0, 0.0, 0.0])],
        "probe",
        "all",
    )
    .await;
    assert!(xml.contains("<found>true</found>"), "{xml}");
    assert!(!xml.contains("<exact>"), "{xml}");
    assert!(
        xml.contains("<conversation_id>conv-top</conversation_id>"),
        "{xml}"
    );
    assert!(xml.contains("<chunk_id>top</chunk_id>"), "{xml}");
    assert!(xml.contains("<project>project-a</project>"), "{xml}");
}

#[tokio::test]
async fn d1_the_keyword_fallback_holds_the_same_contract() {
    let (storage, search) = quick_check_fixture(&[
        (
            "near",
            "alpha",
            "order 88824670039 shipped for 3256813322194604",
            [1.0, 0.0, 0.0, 0.0],
        ),
        (
            "exact",
            "alpha",
            "tracking 3256813322194603 delivered",
            [0.0, 1.0, 0.0, 0.0],
        ),
    ]);
    let hit = keyword_fallback(
        &storage,
        &search,
        "3256813322194603",
        5,
        Some("alpha"),
        KeywordTool::Quick,
    )
    .await
    .unwrap();
    assert!(hit.contains("<found>true</found>"), "{hit}");
    assert!(hit.contains("<chunk_id>exact</chunk_id>"), "{hit}");
    let miss = keyword_fallback(
        &storage,
        &search,
        "3256813322194605",
        5,
        Some("alpha"),
        KeywordTool::Quick,
    )
    .await
    .unwrap();
    assert!(miss.contains("<found>false</found>"), "{miss}");
    assert!(miss.contains("<exact>false</exact>"), "{miss}");
}

/// "last month" is the previous calendar month. A chunk written now is outside
/// it, so the window is empty: the answer prints the exact bounds it searched
/// and names the next wider window instead of returning a bare "no results".
#[tokio::test]
async fn d6_an_empty_last_month_window_prints_its_bounds_and_the_wider_window() {
    let (storage, search) = quick_check_fixture(&[(
        "now",
        "alpha",
        "quartzwindow written this month",
        [1.0, 0.0, 0.0, 0.0],
    )]);
    storage
        .with_connection(|conn| {
            conn.execute(
                "UPDATE chunks SET timestamp = ?1",
                [chrono::Utc::now().to_rfc3339()],
            )?;
            Ok(())
        })
        .unwrap();
    let reply = keyword_fallback(
        &storage,
        &search,
        "quartzwindow",
        5,
        Some("alpha"),
        KeywordTool::Recency {
            time_range: Some("last month"),
            since: None,
            until: None,
        },
    )
    .await
    .unwrap();
    assert!(reply.starts_with("<no_results>"), "{reply}");
    assert!(reply.contains("previous calendar month"), "{reply}");
    assert!(reply.contains("T00:00:00Z to "), "{reply}");
    assert!(reply.contains("last 2 months"), "{reply}");
    assert!(!reply.contains("quartzwindow written"), "{reply}");
}

/// Three conversations hold the same brief; a fourth holds other text. Returns
/// the page's conversation ids and the page, with `live` as the live chat.
async fn brief_page(live: Option<&str>) -> (Vec<String>, String) {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    let brief = "Standing brief: keep receipts for every claim you make in a report";
    let rows = [
        ("c1", "conv-1", brief, 0.95_f32),
        ("c2", "conv-2", brief, 0.90),
        ("c3", "conv-3", brief, 0.85),
        (
            "c4",
            "conv-4",
            "a different decision about the retry budget",
            0.80,
        ),
    ];
    for (sequence, (id, conversation, content, cosine)) in rows.into_iter().enumerate() {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conversation.into(),
            project_name: "test".into(),
            timestamp: "2026-09-20T00:00:00Z".into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: sequence,
            is_sidechain: false,
        };
        let vector = vec![cosine, (1.0 - cosine * cosine).sqrt(), 0.0, 0.0];
        storage.insert_chunk(&chunk, &vector).unwrap();
        engine.insert_chunk(chunk.id.clone(), vector);
    }
    let search = Arc::new(RwLock::new(engine));
    LIVE_SESSION_OVERRIDE.with(|cell| *cell.borrow_mut() = live.map(str::to_string));
    let out = reflect_on_past_with_vec_mode(
        &storage,
        &search,
        &Q,
        "standing brief receipts",
        4,
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
    LIVE_SESSION_OVERRIDE.with(|cell| *cell.borrow_mut() = None);
    let cids = out
        .split("<cid>")
        .skip(1)
        .filter_map(|rest| rest.split("</cid>").next())
        .map(str::to_string)
        .collect();
    (cids, out)
}

/// The page shows the best-ranked copy once, then the other text; the first row
/// and the order of the rest are what the ranking produced.
#[tokio::test]
async fn d3_identical_text_in_several_conversations_is_shown_once() {
    let (cids, out) = brief_page(None).await;
    assert_eq!(cids, ["conv-1", "conv-4"], "{out}");
    assert!(
        out.contains("<same-text-collapsed>2</same-text-collapsed>"),
        "{out}"
    );
}

/// The best copy is in the live chat, which the page sinks. The copies are
/// collapsed on that final order, so the brief still leads the page: the next
/// best copy keeps rank 1 and the live copy is the one left out. Collapsing
/// before the sink would have kept the live copy, then sunk it, and rank 1
/// would have been the unrelated text.
#[tokio::test]
async fn d3_the_collapse_keeps_the_first_text_when_its_best_copy_is_the_live_chat() {
    let (cids, out) = brief_page(Some("conv-1")).await;
    assert_eq!(cids, ["conv-2", "conv-4"], "{out}");
    assert!(
        out.contains("<same-text-collapsed>2</same-text-collapsed>"),
        "{out}"
    );
}

/// An exact-id answer from a chunk that holds CSR's own output keeps the
/// `[CSR text]` label the similarity path gives such a row; a plain chunk
/// beside it carries none.
#[tokio::test]
async fn d1_an_exact_answer_from_csr_text_keeps_the_csr_text_label() {
    let csr = quick(
        &[(
            "recap",
            "alpha",
            "recap [2m ago]: case Bond-ZZHE87751 closed on Friday. [[CSR:RECAP]]",
            [0.0, 1.0, 0.0, 0.0],
        )],
        "what is Bond-ZZHE87751",
        "all",
    )
    .await;
    assert!(csr.contains("<found>true</found>"), "{csr}");
    assert!(csr.contains("<chunk_id>recap</chunk_id>"), "{csr}");
    assert!(
        csr.contains("<resolution>[CSR text]"),
        "the exact row is labelled as CSR text:\n{csr}"
    );
    let plain = quick(
        &[(
            "plain",
            "alpha",
            "case Bond-ZZHE87751 closed on Friday",
            [0.0, 1.0, 0.0, 0.0],
        )],
        "what is Bond-ZZHE87751",
        "all",
    )
    .await;
    assert!(plain.contains("<found>true</found>"), "{plain}");
    assert!(!plain.contains("[CSR text]"), "{plain}");
}

/// A page of 5 over `copies` copies of one pasted brief, each in its own
/// conversation and ranked first, then `distinct` distinct rows. Returns the
/// page's conversation ids and the page.
async fn copies_page(copies: usize, distinct: usize) -> (Vec<String>, String) {
    let storage = Arc::new(Storage::open_memory().unwrap());
    let mut engine = SearchEngine::new(8);
    let brief = "Standing brief: keep receipts for every claim you make in a report";
    let mut rows: Vec<(String, String, f32)> = (0..copies)
        .map(|n| {
            (
                format!("copy-{n:03}"),
                brief.to_string(),
                0.99 - n as f32 * 1e-4,
            )
        })
        .collect();
    for n in 0..distinct {
        let text = format!("distinct decision number {n} about the retry budget and its owner");
        rows.push((format!("distinct-{n}"), text, 0.90 - n as f32 * 0.01));
    }
    for (sequence, (id, content, cosine)) in rows.into_iter().enumerate() {
        let chunk = crate::import::ConversationChunk {
            id: id.clone(),
            conversation_id: format!("conv-{id}"),
            project_name: "test".into(),
            timestamp: "2026-09-20T00:00:00Z".into(),
            content,
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: sequence,
            is_sidechain: false,
        };
        let vector = vec![cosine, (1.0 - cosine * cosine).sqrt(), 0.0, 0.0];
        storage.insert_chunk(&chunk, &vector).unwrap();
        engine.insert_chunk(chunk.id.clone(), vector);
    }
    let search = Arc::new(RwLock::new(engine));
    let out = reflect_on_past_with_vec_mode(
        &storage,
        &search,
        &Q,
        "standing brief receipts",
        5,
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
    let cids = out
        .split("<cid>")
        .skip(1)
        .filter_map(|rest| rest.split("</cid>").next())
        .map(str::to_string)
        .collect();
    (cids, out)
}

/// The whole first vector window (120 rows for a page of 5) is copies, and
/// the six distinct rows sit just beyond it. The refetch decision used to
/// count rows, so a window of copies looked like a full page, no wider window
/// was read, and the collapse left a page of one row. It now counts distinct
/// texts, reads one window three times the first, and the page fills.
#[tokio::test]
async fn d3_a_window_full_of_one_text_still_fills_the_page_from_the_distinct_rows_beyond_it() {
    let copies = vector_pool(5) + 10;
    let (cids, out) = copies_page(copies, 6).await;
    assert_eq!(
        cids,
        [
            "conv-copy-000",
            "conv-distinct-0",
            "conv-distinct-1",
            "conv-distinct-2",
            "conv-distinct-3"
        ],
        "{out}"
    );
    let collapsed = format!("<same-text-collapsed>{}</same-text-collapsed>", copies - 1);
    assert!(out.contains(&collapsed), "{out}");
    assert!(!out.contains("<short-page>"), "{out}");
}

/// Copies fill even the wider window (180 rows): the one refetch cannot reach
/// the distinct rows, so the page is short. It says so, with the count of
/// copies the collapse left out, instead of reading as "nothing more".
#[tokio::test]
async fn d3_copies_past_the_wider_window_leave_a_short_page_that_says_so() {
    let wider = 3 * recall_pool(5);
    let (cids, out) = copies_page(wider + 20, 6).await;
    assert_eq!(cids, ["conv-copy-000"], "{out}");
    let collapsed = format!("<same-text-collapsed>{}</same-text-collapsed>", wider - 1);
    assert!(out.contains(&collapsed), "{out}");
    assert!(
        out.contains("<short-page>1 of 5 rows: copies of one text filled the candidate window"),
        "{out}"
    );
}

/// Three copies and two distinct rows: the page is short because nothing more
/// exists, so it carries the collapse count and no short-page line.
#[tokio::test]
async fn d3_a_page_short_because_the_pool_ran_out_has_no_short_page_line() {
    let (cids, out) = copies_page(3, 2).await;
    assert_eq!(
        cids,
        ["conv-copy-000", "conv-distinct-0", "conv-distinct-1"],
        "{out}"
    );
    assert!(
        out.contains("<same-text-collapsed>2</same-text-collapsed>"),
        "{out}"
    );
    assert!(!out.contains("<short-page>"), "{out}");
}
