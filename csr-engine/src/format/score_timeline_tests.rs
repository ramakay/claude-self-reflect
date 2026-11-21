use super::*;
use std::collections::BTreeMap;

fn chunk(id: &str) -> ConversationChunk {
    ConversationChunk {
        id: id.into(),
        conversation_id: "one-conversation".into(),
        project_name: "test".into(),
        timestamp: "2026-11-09T10:00:00Z".into(),
        content: "a useful answer".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::Assistant,
        seq: 0,
        is_sidechain: false,
    }
}

#[test]
fn timeline_labels_chunk_counts_even_when_a_conversation_has_multiple_chunks() {
    let chunks = [chunk("first"), chunk("second")];
    let groups = BTreeMap::from([("2026-11-09".into(), chunks.iter().collect())]);
    let text = format_timeline(&groups, "today");
    assert!(text.contains("<period key='2026-11-09' chunks='2'>"));
    assert!(!text.contains("conversations="));
    assert!(text.contains("<stats messages='2'/>"));
}

#[test]
fn relevance_rises_with_the_printed_score_regardless_of_ce_margins() {
    let high_ce = CeSummary {
        top: 20.0,
        runner_up: Some(1.0),
        median: 0.0,
    };
    let weak_ce = CeSummary {
        top: 1.0,
        runner_up: Some(1.0),
        median: 1.0,
    };
    for (score, label) in [
        (0.5, "weak"),
        (0.7, "partial"),
        (0.8, "good"),
        (0.9, "high"),
    ] {
        let results = [EnrichedResult {
            score,
            chunk: chunk("answer"),
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        }];
        for ce in [None, Some(&high_ce), Some(&weak_ce)] {
            let text = format_search_results_with_ce(&results, "q", "all", 0, 0, &[], ce);
            assert!(
                text.contains(&format!("({label} relevance, top score: {score:.3})")),
                "{text}"
            );
            assert!(
                text.contains(&format!("relevance=\"{label}\" top-score=\"{score:.3}\"")),
                "{text}"
            );
        }
    }
}
