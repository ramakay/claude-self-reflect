//! Contract tests for the explicit-search answers: the exact-identifier quick
//! check, the handles every positive answer carries, and the weak-match and
//! order labels of a result page.

use super::*;
use crate::provenance::Speaker;

fn row(id: &str, conversation: &str, score: f32, content: &str) -> EnrichedResult {
    EnrichedResult {
        score,
        chunk: ConversationChunk {
            id: id.into(),
            conversation_id: conversation.into(),
            project_name: "proj-a".into(),
            timestamp: "2026-01-15T10:00:00Z".into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: Speaker::User,
            seq: 0,
            is_sidechain: false,
        },
        resolution: None,
        trust: crate::provenance::TrustTier::Unknown,
        validity_demoted: false,
    }
}

fn hit(content: &str, id: &str) -> ExactHit {
    ExactHit {
        row: row("chunk-x", "conv-x", 0.0, content),
        matched: vec![id.to_string()],
        missing: Vec::new(),
        literal_rows: 1,
    }
}

#[test]
fn d2_a_similarity_answer_carries_conversation_chunk_and_project() {
    let results = vec![row("c-1", "conv-1", 0.80, "the decision about retries")];
    let xml = format_quick_check(&results, "retries");
    assert!(xml.contains("<found>true</found>"), "{xml}");
    assert!(
        xml.contains("<conversation_id>conv-1</conversation_id>"),
        "{xml}"
    );
    assert!(xml.contains("<chunk_id>c-1</chunk_id>"), "{xml}");
    assert!(xml.contains("<project>proj-a</project>"), "{xml}");
}

#[test]
fn d1_an_exact_answer_says_exact_and_holds_the_identifier_in_its_preview() {
    let xml = format_quick_check_exact(&hit("order 3256813322194603 shipped", "3256813322194603"));
    assert!(xml.contains("<found>true</found>"), "{xml}");
    assert!(xml.contains("<exact>true</exact>"), "{xml}");
    assert!(
        xml.contains("<matched_identifiers>3256813322194603</matched_identifiers>"),
        "{xml}"
    );
    assert!(xml.contains("<score>n/a</score>"), "{xml}");
    assert!(xml.contains("3256813322194603 shipped"), "{xml}");
    assert!(
        xml.contains("<conversation_id>conv-x</conversation_id>"),
        "{xml}"
    );
    assert!(xml.contains("<chunk_id>chunk-x</chunk_id>"), "{xml}");
}

#[test]
fn d1_the_preview_is_a_window_around_an_identifier_past_the_first_500_chars() {
    let filler = "word ".repeat(300);
    let content = format!("{filler}the id is Bond-ZZHE87751 here {filler}");
    let xml = format_quick_check_exact(&hit(&content, "bond-zzhe87751"));
    assert!(
        xml.contains("Bond-ZZHE87751"),
        "the id must be in the preview: {xml}"
    );
    let preview = xml
        .split("<preview>")
        .nth(1)
        .and_then(|rest| rest.split("</preview>").next())
        .unwrap();
    assert!(
        preview.chars().count() < PREVIEW_CHARS + 10,
        "{}",
        preview.len()
    );
}

#[test]
fn d1_a_miss_is_found_false_and_the_related_chunk_is_labelled_not_confirming() {
    let near = vec![row(
        "c-9",
        "conv-9",
        0.74,
        "an order with number 88824670039",
    )];
    let ids = vec!["3256813322194603".to_string()];
    let xml = format_quick_check_exact_miss(&near, &ids, false);
    assert!(xml.contains("<found>false</found>"), "{xml}");
    assert!(xml.contains("<exact>false</exact>"), "{xml}");
    assert!(xml.contains("<count>0</count>"), "{xml}");
    assert!(xml.contains("does not contain the identifier"), "{xml}");
    assert!(xml.contains("<nearest_related>"), "{xml}");
    assert!(
        xml.contains("<conversation_id>conv-9</conversation_id>"),
        "{xml}"
    );
    // A related chunk under the floor is not shown at all.
    let weak = vec![row("c-8", "conv-8", 0.30, "unrelated text")];
    let xml = format_quick_check_exact_miss(&weak, &ids, false);
    assert!(!xml.contains("<nearest_related>"), "{xml}");
    assert!(!xml.contains("unrelated text"), "{xml}");
}

#[test]
fn d1_a_related_row_that_holds_the_identifier_is_never_listed_as_related() {
    // The nearest row is a restatement of the question: it holds the id, which
    // the answer does not count, and must not be offered as related text.
    let echo = vec![row("c-5", "conv-5", 0.90, "did we see 3256813322194603?")];
    let ids = vec!["3256813322194603".to_string()];
    let xml = format_quick_check_exact_miss(&echo, &ids, false);
    assert!(xml.contains("<found>false</found>"), "{xml}");
    assert!(!xml.contains("<nearest_related>"), "{xml}");
    assert!(!xml.contains("conv-5"), "{xml}");
}

#[test]
fn d1_a_full_lookup_window_does_not_claim_the_identifier_was_never_written() {
    let ids = vec!["3256813322194603".to_string()];
    let full = format_quick_check_exact_miss(&[], &ids, true);
    assert!(
        full.contains("not proof that it was never written"),
        "{full}"
    );
    assert!(full.contains("<found>false</found>"), "{full}");
    let not_full = format_quick_check_exact_miss(&[], &ids, false);
    assert!(!not_full.contains("not proof"), "{not_full}");
}

#[test]
fn d1_the_keyword_fallback_needs_the_identifier_in_the_row() {
    let rows = vec![
        row("c-1", "conv-1", 0.0, "[keyword] order 88824670039 shipped"),
        row(
            "c-2",
            "conv-2",
            0.0,
            "[keyword] order 32568133221946031 shipped",
        ),
    ];
    let miss = format_keyword_quick_check(&rows, "3256813322194603");
    assert!(miss.contains("<found>false</found>"), "{miss}");
    assert!(miss.contains("<exact>false</exact>"), "{miss}");
    let mut rows = rows;
    rows.push(row(
        "c-3",
        "conv-3",
        0.0,
        "[keyword] id 3256813322194603 here",
    ));
    let found = format_keyword_quick_check(&rows, "3256813322194603");
    assert!(found.contains("<found>true</found>"), "{found}");
    assert!(
        found.contains("<conversation_id>conv-3</conversation_id>"),
        "{found}"
    );
    assert!(!found.contains("[keyword]"), "{found}");
}

#[test]
fn d2_the_keyword_fallback_carries_handles_without_an_identifier() {
    let rows = vec![row("c-1", "conv-1", 0.0, "[keyword] retries and backoff")];
    let xml = format_keyword_quick_check(&rows, "retries");
    assert!(xml.contains("<found>true</found>"), "{xml}");
    assert!(
        xml.contains("<conversation_id>conv-1</conversation_id>"),
        "{xml}"
    );
    assert!(xml.contains("<chunk_id>c-1</chunk_id>"), "{xml}");
}

fn page(rows: &[EnrichedResult], query: &str) -> String {
    format_search_results(rows, query, "all", 5, 3)
}

#[test]
fn d4_keyword_rows_under_a_good_first_row_are_marked_weak_and_keep_their_place() {
    let rows = vec![
        row("c-1", "conv-1", 0.82, "the answer about token budgets"),
        row(
            "c-2",
            "conv-2",
            0.41,
            "[keyword] budgets mentioned in passing",
        ),
        row("c-3", "conv-3", 0.40, "[keyword] another stray word match"),
        row("c-4", "conv-4", 0.66, "a real second answer on budgets"),
    ];
    let xml = page(&rows, "token budgets");
    assert_eq!(xml.matches("<tier>weak</tier>").count(), 2, "{xml}");
    assert!(xml.contains("<weak-matches>2 of 4 rows"), "{xml}");
    // Rank numbers and order are untouched.
    let ids: Vec<&str> = xml
        .match_indices("<id>")
        .map(|(at, _)| &xml[at..at + 9])
        .collect();
    assert_eq!(ids, ["<id>c-1</", "<id>c-2</", "<id>c-3</", "<id>c-4</"]);
    assert_eq!(xml.matches("<r rank=").count(), 4);
    // Only rows 2 and 3 carry the tier.
    let weak_at: Vec<usize> = xml
        .match_indices("<tier>weak</tier>")
        .map(|(at, _)| at)
        .collect();
    assert!(weak_at[0] > xml.find("<id>c-2</id>").unwrap());
    assert!(weak_at[1] < xml.find("<id>c-4</id>").unwrap());
}

#[test]
fn d4_a_keyword_row_that_holds_the_query_identifier_is_not_weak() {
    let rows = vec![
        row("c-1", "conv-1", 0.82, "the answer"),
        row(
            "c-2",
            "conv-2",
            0.40,
            "[keyword] case INV_2024_0007 was closed",
        ),
        row(
            "c-3",
            "conv-3",
            0.40,
            "[keyword] case INV_2024_00071 is unrelated",
        ),
    ];
    let xml = page(&rows, "what happened to INV_2024_0007");
    assert_eq!(xml.matches("<tier>weak</tier>").count(), 1, "{xml}");
    assert!(xml.find("<tier>weak</tier>").unwrap() > xml.find("<id>c-3</id>").unwrap());
}

#[test]
fn d4_a_page_with_one_good_result_has_no_weak_label_and_the_first_row_is_never_weak() {
    let one = page(&[row("c-1", "conv-1", 0.9, "the single good answer")], "q");
    assert!(!one.contains("weak"), "{one}");
    let kw_first = vec![
        row(
            "c-1",
            "conv-1",
            0.44,
            "[keyword] first and only lexical hit",
        ),
        row("c-2", "conv-2", 0.30, "[keyword] second"),
    ];
    let xml = page(&kw_first, "lexical hit");
    // Both rows are keyword: this is the keyword-only page, nothing is weak.
    assert!(!xml.contains("<tier>weak</tier>"), "{xml}");
    let mixed = vec![
        row("c-1", "conv-1", 0.44, "[keyword] first lexical hit"),
        row("c-2", "conv-2", 0.80, "a similar answer"),
        row("c-3", "conv-3", 0.30, "[keyword] third"),
    ];
    let xml = page(&mixed, "lexical hit");
    assert_eq!(xml.matches("<tier>weak</tier>").count(), 1, "{xml}");
    assert!(xml.find("<tier>weak</tier>").unwrap() > xml.find("<id>c-3</id>").unwrap());
}

#[test]
fn d5_a_page_whose_scores_do_not_descend_says_why_and_a_sorted_page_stays_silent() {
    let unsorted = vec![
        row("c-1", "conv-1", 0.80, "first"),
        row("c-2", "conv-2", 0.566, "fourth in score"),
        row(
            "c-3",
            "conv-3",
            0.665,
            "fifth, scores higher than the row above",
        ),
    ];
    let xml = page(&unsorted, "q");
    assert!(
        xml.contains("<order>Rows are in ranked order, not score order."),
        "{xml}"
    );
    // The scores and the order are exactly what they were.
    assert!(xml.find("<s>0.566</s>").unwrap() < xml.find("<s>0.665</s>").unwrap());
    let sorted = vec![
        row("c-1", "conv-1", 0.80, "first"),
        row("c-2", "conv-2", 0.70, "second"),
    ];
    assert!(!page(&sorted, "q").contains("<order>"));
}

#[test]
fn d4_a_get_more_page_marks_weak_rows_and_says_when_scores_do_not_descend() {
    let rows = vec![
        row("c-5", "conv-5", 0.30, "[keyword] stray word match"),
        row("c-6", "conv-6", 0.62, "a real answer on budgets"),
        row("c-7", "conv-7", 0.20, "far below the relevance floor"),
    ];
    // Page two: its first row is row 6 of the pool, so it is not exempt.
    let more = format_more_results(&rows, "token budgets", 4, 9);
    assert_eq!(more.matches("<tier>weak</tier>").count(), 2, "{more}");
    assert!(more.contains("<weak-matches>2 of 3 rows"), "{more}");
    assert!(more.contains("<order>Rows are in ranked order"), "{more}");
    // The rows and their numbers are untouched.
    assert!(more.contains("<result index=\"5\">"), "{more}");
    assert!(
        more.find("[keyword] stray").unwrap() < more.find("a real answer").unwrap(),
        "{more}"
    );
    // The first page of the pool keeps its first row unmarked.
    let first = format_more_results(&rows, "token budgets", 0, 9);
    assert_eq!(first.matches("<tier>weak</tier>").count(), 1, "{first}");
    // The keyword fallback's rows are all keyword hits: nothing is weak.
    let kw = vec![
        row("k-1", "conv-1", 0.30, "[keyword] one"),
        row("k-2", "conv-2", 0.30, "[keyword] two"),
    ];
    let outage = format_more_keyword(&kw, "one", 2, 5);
    assert!(!outage.contains("weak"), "{outage}");
}

#[test]
fn d4_a_sorted_get_more_page_is_unchanged() {
    let rows = vec![
        row("c-5", "conv-5", 0.80, "the first answer"),
        row("c-6", "conv-6", 0.70, "the second answer"),
    ];
    let more = format_more_results(&rows, "q", 4, 6);
    assert!(!more.contains("<tier>"), "{more}");
    assert!(!more.contains("<order>"), "{more}");
    assert!(!more.contains("<weak-matches>"), "{more}");
}
