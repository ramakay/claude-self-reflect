use super::*;

fn evidence(
    chunk_id: &str,
    score: f32,
    timestamp: &str,
) -> crate::search::reinstatement::EvidenceItem {
    crate::search::reinstatement::EvidenceItem {
        chunk_id: chunk_id.to_string(),
        conversation_id: "conv-x".to_string(),
        score,
        best_route: crate::search::reinstatement::Via::Seed,
        routes: std::collections::BTreeSet::from([crate::search::reinstatement::Via::Seed]),
        route_scores: std::collections::BTreeMap::from([(
            crate::search::reinstatement::Via::Seed,
            score,
        )]),
        timestamp: timestamp.to_string(),
        excerpt: chunk_id.to_string(),
        ratification: None,
        trust: crate::provenance::TrustTier::Unknown,
    }
}

#[test]
fn same_anchor_within_epsilon() {
    let higher = evidence("higher", 0.715, "2026-01-01T00:00:00Z");
    let newer = evidence("newer", 0.696, "2026-01-02T00:00:00Z");
    let anchors = HashMap::from([
        (
            "higher".to_string(),
            ("project".to_string(), "file.rs".to_string()),
        ),
        (
            "newer".to_string(),
            ("project".to_string(), "file.rs".to_string()),
        ),
    ]);

    let ranking = apply_why_recency_tiebreak(vec![higher, newer], &anchors);
    let rendered = format_why(
        "why",
        &ranking.items,
        &ranking.hoisted_chunk_ids,
        &crate::search::reinstatement::ReinstateTrace::default(),
    );

    assert_eq!(ranking.items[0].chunk_id, "newer");
    assert!(ranking.hoisted_chunk_ids.contains("newer"));
    assert!(rendered.contains("score=0.696 [recent↑]"));
    assert!(!ranking.hoisted_chunk_ids.contains("higher"));
}

#[test]
fn items_outside_epsilon() {
    let higher = evidence("higher", 0.90, "2026-01-01T00:00:00Z");
    let newer = evidence("newer", 0.50, "2026-01-02T00:00:00Z");
    let anchors = HashMap::from([
        (
            "higher".to_string(),
            ("project".to_string(), "file.rs".to_string()),
        ),
        (
            "newer".to_string(),
            ("project".to_string(), "file.rs".to_string()),
        ),
    ]);

    let ranking = apply_why_recency_tiebreak(vec![newer, higher], &anchors);
    let rendered = format_why(
        "why",
        &ranking.items,
        &ranking.hoisted_chunk_ids,
        &crate::search::reinstatement::ReinstateTrace::default(),
    );

    assert_eq!(ranking.items[0].chunk_id, "higher");
    assert!(ranking.hoisted_chunk_ids.is_empty());
    assert!(!rendered.contains(WHY_RECENCY_HOIST_MARKER));
}

#[test]
fn different_anchors() {
    let higher = evidence("higher", 0.715, "2026-01-01T00:00:00Z");
    let newer = evidence("newer", 0.696, "2026-01-02T00:00:00Z");
    let anchors = HashMap::from([
        (
            "higher".to_string(),
            ("project".to_string(), "file-a.rs".to_string()),
        ),
        (
            "newer".to_string(),
            ("project".to_string(), "file-b.rs".to_string()),
        ),
    ]);

    let ranking = apply_why_recency_tiebreak(vec![newer, higher], &anchors);
    let rendered = format_why(
        "why",
        &ranking.items,
        &ranking.hoisted_chunk_ids,
        &crate::search::reinstatement::ReinstateTrace::default(),
    );

    assert_eq!(ranking.items[0].chunk_id, "higher");
    assert!(ranking.hoisted_chunk_ids.is_empty());
    assert!(!rendered.contains(WHY_RECENCY_HOIST_MARKER));
}
