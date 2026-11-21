use super::*;
use crate::provenance::Speaker;

fn make_chunk(id: &str, conversation_id: &str, content: &str) -> ConversationChunk {
    ConversationChunk {
        id: id.into(),
        conversation_id: conversation_id.into(),
        project_name: "test-project".into(),
        timestamp: "2026-01-15T10:00:00Z".into(),
        content: content.into(),
        message_count: 1,
        summary: None,
        author: Speaker::ToolResult,
        seq: 0,
        is_sidechain: false,
    }
}

#[test]
fn dedupe_plan_origins_drops_plan_when_origin_present() {
    use std::collections::HashMap;
    let mk = |id: &str, conv: &str| EnrichedResult {
        score: 0.9,
        chunk: make_chunk(id, conv, "shared decision text"),
        resolution: None,
        trust: crate::provenance::TrustTier::Unknown,
        validity_demoted: false,
    };
    // Correlated plan + its origin conversation both matched: plan drops.
    let mut results = vec![mk("p1", "plan:witty"), mk("c1", "conv-a")];
    let origin_of: HashMap<String, String> = [("p1".to_string(), "conv-a".to_string())].into();
    dedupe_plan_origins(&mut results, &origin_of);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.id, "c1");

    // Origin NOT in the result set: correlated plan survives.
    let mut results = vec![mk("p1", "plan:witty"), mk("c2", "conv-b")];
    dedupe_plan_origins(&mut results, &origin_of);
    assert_eq!(results.len(), 2);

    // Uncorrelated plan (no provenance edge) never drops.
    let mut results = vec![mk("p2", "plan:crispy"), mk("c1", "conv-a")];
    dedupe_plan_origins(&mut results, &HashMap::new());
    assert_eq!(results.len(), 2);
}

#[test]
fn dedupe_sidechain_origins_prefers_parent_conversation() {
    use std::collections::HashMap;
    let mk = |id: &str, conv: &str, sidechain: bool| {
        let mut chunk = make_chunk(id, conv, "shared evidence text");
        chunk.is_sidechain = sidechain;
        EnrichedResult {
            score: 0.9,
            chunk,
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        }
    };
    let parent_of: HashMap<String, String> =
        [("side-1".to_string(), "parent-1".to_string())].into();

    let mut tied = vec![
        mk("side-1", "agent-child", true),
        mk("origin-1", "parent-1", false),
    ];
    dedupe_sidechain_origins(&mut tied, &parent_of);
    assert_eq!(tied.len(), 1);
    assert_eq!(tied[0].chunk.id, "origin-1");

    let mut parent_absent = vec![mk("side-1", "agent-child", true)];
    dedupe_sidechain_origins(&mut parent_absent, &parent_of);
    assert_eq!(parent_absent.len(), 1);
}

#[test]
fn truncate_chars_multi_byte_boundary() {
    // Chinese chars are 3 bytes each; truncating by byte count would panic.
    let s = "你好世界你好世界"; // 8 chars
    let result = truncate_chars(s, 4);
    assert_eq!(result.chars().count(), 4);
    assert_eq!(result, "你好世界");
}

#[test]
fn truncate_chars_max_larger_than_string() {
    let s = "café";
    assert_eq!(truncate_chars(s, 100), s);
    assert_eq!(truncate_chars(s, 4), s);
}

#[test]
fn age_stamp_valid_iso() {
    let stamp = age_stamp("2026-01-01T00:00:00Z");
    assert!(stamp.starts_with("as of 2026-01-01 ("), "got: {stamp}");
    assert!(stamp.contains(')'), "got: {stamp}");
}

#[test]
fn age_stamp_unparseable_unchanged() {
    assert_eq!(age_stamp("not-a-timestamp"), "not-a-timestamp");
}

#[test]
fn dedupe_drops_duplicate_chunk_id() {
    let mut results = vec![
        EnrichedResult {
            score: 0.9,
            chunk: make_chunk("same-id", "conv-a", "content A"),
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        },
        EnrichedResult {
            score: 0.5,
            chunk: make_chunk("same-id", "conv-b", "content B"),
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        },
    ];
    dedupe_results(&mut results);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].score, 0.9);
    assert_eq!(results[0].chunk.content, "content A");
}

#[test]
fn dedupe_drops_same_conversation_same_prefix_different_ids() {
    let content = "shared prefix content that is long enough for testing";
    let mut results = vec![
        EnrichedResult {
            score: 0.9,
            chunk: make_chunk("id-1", "conv-1", content),
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        },
        EnrichedResult {
            score: 0.7,
            chunk: make_chunk("id-2", "conv-1", content),
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        },
    ];
    dedupe_results(&mut results);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.id, "id-1");
}

#[test]
fn dedupe_keeps_different_conversations_same_content() {
    let content = "identical content across conversations";
    let mut results = vec![
        EnrichedResult {
            score: 0.9,
            chunk: make_chunk("id-1", "conv-1", content),
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        },
        EnrichedResult {
            score: 0.8,
            chunk: make_chunk("id-2", "conv-2", content),
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        },
    ];
    dedupe_results(&mut results);
    assert_eq!(results.len(), 2);
}

#[test]
fn dedupe_normalizes_whitespace_and_case_within_conversation() {
    let mut results = vec![
        EnrichedResult {
            score: 0.9,
            chunk: make_chunk("id-1", "conv-1", "Hello   World"),
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        },
        EnrichedResult {
            score: 0.7,
            chunk: make_chunk("id-2", "conv-1", "hello world"),
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        },
    ];
    dedupe_results(&mut results);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk.id, "id-1");
}

#[test]
fn resolution_note_formats_resolved() {
    let note = resolution_note(
        "resolved",
        "shipped commit abc123",
        "2026-07-20T10:00:00Z",
        "user_confirmed",
    )
    .unwrap();
    assert!(note.starts_with("resolved —"), "got: {note}");
    assert!(note.contains("shipped commit abc123"));
    assert!(note.contains("2026-07-20"));
}

#[test]
fn resolution_note_formats_still_open() {
    let note = resolution_note(
        "still_open",
        "unused evidence",
        "2026-07-20T10:00:00Z",
        "user_confirmed",
    )
    .unwrap();
    assert!(note.starts_with("still open —"), "got: {note}");
    assert!(note.contains("2026-07-20"));
}

#[test]
fn resolution_note_formats_regressed() {
    let note = resolution_note(
        "regressed",
        "broke again in v9.4",
        "2026-07-20T10:00:00Z",
        "user_confirmed",
    )
    .unwrap();
    assert!(note.starts_with("regressed —"), "got: {note}");
    assert!(note.contains("broke again in v9.4"));
    assert!(note.contains("2026-07-20"));
}

#[test]
fn agent_resolution_never_receives_verified_annotation() {
    assert_eq!(
        resolution_note(
            "resolved",
            "agent assertion",
            "2026-07-20T10:00:00Z",
            "agent",
        ),
        None
    );
}

#[test]
fn search_results_render_the_cached_floor_for_every_result() {
    use crate::provenance::TrustTier;
    let results = vec![
        EnrichedResult {
            score: 0.9,
            chunk: make_chunk("id-user", "conv-1", "a user line"),
            resolution: None,
            trust: TrustTier::UserHistory,
            validity_demoted: false,
        },
        EnrichedResult {
            score: 0.8,
            chunk: make_chunk("id-unknown", "conv-2", "a legacy line"),
            resolution: None,
            trust: TrustTier::Unknown,
            validity_demoted: false,
        },
    ];
    let rendered = format_search_results(&results, "query", "all", 1, 1);
    assert!(
        rendered.contains("<trust>user_history</trust>"),
        "{rendered}"
    );
    assert!(
        rendered.contains("<trust>unknown</trust>"),
        "Unknown is rendered, never omitted:\n{rendered}"
    );
    let more = format_more_results(&results, "query", 0, 2);
    assert_eq!(more.matches("<trust>").count(), 2, "{more}");
}

#[test]
fn format_search_results_renders_resolution_and_footer_count() {
    let results = vec![
        EnrichedResult {
            score: 0.9,
            chunk: make_chunk("id-open", "conv-1", "open item"),
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        },
        EnrichedResult {
            score: 0.8,
            chunk: make_chunk("id-resolved", "conv-1", "resolved item"),
            resolution: resolution_note(
                "resolved",
                "verified in prod",
                "2026-07-20T10:00:00Z",
                "user_confirmed",
            ),
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        },
    ];
    let xml = format_search_results(&results, "q", "all", 1, 1);
    assert!(xml.contains("<resolution>resolved"), "got: {xml}");
    assert!(
        xml.contains("<note>1 resolved item(s) demoted"),
        "got: {xml}"
    );
    // The unresolved result must have no <resolution> tag rendered for it —
    // check there is exactly one <resolution> element in total.
    assert_eq!(xml.matches("<resolution>").count(), 1);
}

#[test]
fn format_search_results_no_resolution_tag_when_none() {
    let results = vec![EnrichedResult {
        score: 0.9,
        chunk: make_chunk("id-1", "conv-1", "plain item"),
        resolution: None,
        trust: crate::provenance::TrustTier::Unknown,
        validity_demoted: false,
    }];
    let xml = format_search_results(&results, "q", "all", 1, 1);
    assert!(!xml.contains("<resolution>"), "got: {xml}");
    assert!(!xml.contains("<note>"), "got: {xml}");
}

#[test]
fn format_search_results_explains_reranked_order_with_raw_extrema() {
    let results = vec![
        EnrichedResult {
            score: 0.474,
            chunk: make_chunk("boosted", "conv-1", "boosted result"),
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        },
        EnrichedResult {
            score: 0.765,
            chunk: make_chunk("demoted", "conv-2", "demoted result"),
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: false,
        },
        EnrichedResult {
            score: 0.200,
            chunk: make_chunk("validity-tail", "conv-3", "validity-demoted tail"),
            resolution: None,
            trust: crate::provenance::TrustTier::Unknown,
            validity_demoted: true,
        },
    ];

    let display_rank_scores = vec![
        DisplayRankScore {
            chunk_id: "boosted".to_string(),
            adjusted_score: 1.124,
        },
        DisplayRankScore {
            chunk_id: "demoted".to_string(),
            adjusted_score: 0.265,
        },
    ];
    let xml = format_search_results_with_rank_scores(
        &results,
        "rerank",
        "all",
        5,
        3,
        &display_rank_scores,
    );

    assert!(
        xml.contains("top score: 0.765"),
        "upfront summary must use the maximum raw score: {xml}"
    );
    assert!(
        xml.contains("top-score=\"0.765\""),
        "XML summary must use the maximum raw score: {xml}"
    );
    assert!(
        xml.contains("<range>0.200-0.765</range>"),
        "range must use raw min..max even after partitioning: {xml}"
    );
    let boosted = xml.find("<id>boosted</id>").unwrap();
    let demoted = xml.find("<id>demoted</id>").unwrap();
    let validity_tail = xml.find("<id>validity-tail</id>").unwrap();
    assert!(boosted < demoted && demoted < validity_tail, "{xml}");
    assert!(xml.contains("<s adj=\"1.124\">0.474</s>"), "got: {xml}");
    assert!(xml.contains("<s adj=\"0.265\">0.765</s>"), "got: {xml}");
    assert!(
        xml.contains("<s>0.200</s>"),
        "validity-only tail movement must not gain an adjusted score: {xml}"
    );
}

#[test]
fn kill_switch_output_is_byte_identical_to_pre_partition_behavior() {
    // With CSR_NO_VALIDITY_PARTITION=1 the partition never sets
    // `validity_demoted` and never appends a note — so the formatter must
    // produce the exact pre-lane output, byte for byte, even when a
    // resolution-ledger evidence string happens to CONTAIN the literal
    // "[stale anchor]". The old substring sniff fired the dream-verdict
    // footer on exactly this fixture; the flag must not.
    let chunk = ConversationChunk {
        id: "c-1".into(),
        conversation_id: "conv-1".into(),
        project_name: "test-project".into(),
        // Deliberately unparseable so age_stamp echoes it verbatim —
        // keeps the expected string deterministic across runs.
        timestamp: "ts-fixed".into(),
        content: "hello world".into(),
        message_count: 1,
        summary: None,
        author: Speaker::ToolResult,
        seq: 0,
        is_sidechain: false,
    };
    let results = vec![EnrichedResult {
        score: 0.9,
        chunk,
        resolution: Some(
            "resolved — evidence cites [stale anchor] old_fn wording (verified 2026-01-01)"
                .to_string(),
        ),
        trust: crate::provenance::TrustTier::Unknown,
        validity_demoted: false, // kill switch on: the partition never set it
    }];
    let xml = format_search_results(&results, "q", "all", 5, 3);
    let expected = "\u{1f3af} RESULTS: 1 matches (high relevance, top score: 0.900)\n\
\u{26a1} PERFORMANCE: 8ms (1 collection searched)\n\
\n\
<search>\n\
\x20 <summary count=\"1\" relevance=\"high\" top-score=\"0.900\" rank1-score=\"0.900\" margin=\"n/a\">\n\
\x20   <preview>hello world</preview>\n\
\x20 </summary>\n\
\x20 <meta>\n\
\x20   <q>q</q>\n\
\x20   <scope>all</scope>\n\
\x20   <count>1</count>\n\
\x20   <range>0.900-0.900</range>\n\
\x20   <perf>\n\
\x20     <ttl>8</ttl>\n\
\x20     <emb>3</emb>\n\
\x20     <srch>5</srch>\n\
\x20     <cols>1</cols>\n\
\x20   </perf>\n\
\x20 </meta>\n\
\x20 <results>\n\
\x20   <r rank=\"1\">\n\
\x20     <s>0.900</s>\n\
\x20     <p>test-project</p>\n\
\x20     <t>ts-fixed</t>\n\
\x20     <excerpt><![CDATA[hello world]]></excerpt>\n\
\x20     <cid>conv-1</cid>\n\
\x20     <id>c-1</id>\n\
\x20     <resolution>resolved \u{2014} evidence cites [stale anchor] old_fn wording (verified 2026-01-01)</resolution>\n\
\x20     <trust>unknown</trust>\n\
\x20   </r>\n\
\x20 </results>\n\
\x20 <note>1 resolved item(s) demoted within page \u{2014} matched but verified addressed</note>\n\
</search>\n";
    assert_eq!(xml, expected, "kill-switch output must be byte-identical");
    assert!(
        !xml.contains("dream verdict"),
        "'[stale anchor]' inside ledger evidence must not fire the dream-verdict footer"
    );
}

#[test]
fn format_code_graph_marks_name_only_vs_definition_with_lines() {
    use crate::storage::codegraph::NodeRow;
    let name_only_node = NodeRow {
        name: "resolve_edges".into(),
        kind: "function".into(),
        file: "src/other_project/resolver.rs".into(),
        last_conv_id: "conv_1".into(),
        name_only: true,
        ..NodeRow::default()
    };
    let def_node = NodeRow {
        name: "resolve_edges".into(),
        kind: "function".into(),
        file: "src/extraction/resolver.rs".into(),
        last_conv_id: "conv_2".into(),
        span_start: 9,
        span_end: 20,
        name_only: false,
        ..NodeRow::default()
    };
    let xml = format_code_graph("callers", "resolve_edges", &[name_only_node, def_node], &[]);
    assert!(xml.contains("match='name-only'"), "got: {xml}");
    assert!(xml.contains("match='definition'"), "got: {xml}");
    // span_start/span_end are 0-based; rendered as 1-based lines.
    assert!(xml.contains("lines='10-21'"), "got: {xml}");
}

#[test]
fn format_code_graph_omits_lines_for_zero_span() {
    use crate::storage::codegraph::NodeRow;
    let module_node = NodeRow {
        name: "src/demo.rs".into(),
        kind: "module".into(),
        file: "src/demo.rs".into(),
        last_conv_id: "conv_1".into(),
        span_start: 0,
        span_end: 0,
        name_only: false,
        ..NodeRow::default()
    };
    let xml = format_code_graph("callers", "demo", &[module_node], &[]);
    assert!(!xml.contains("lines="), "got: {xml}");
}

#[test]
fn format_code_graph_renders_attribution_never_first_conv_id() {
    // WP2 Stage 2: consumer surfaces must render the two-channel
    // `attribution` field and must never present `first_conv_id` as
    // introduction evidence.
    use crate::storage::codegraph::NodeRow;
    let attributed = NodeRow {
        name: "foo".into(),
        kind: "function".into(),
        file: "a.rs".into(),
        first_conv_id: "conv_should_not_appear".into(),
        attribution: "transcript:70690eeb".into(),
        ..NodeRow::default()
    };
    let unattributed = NodeRow {
        name: "bar".into(),
        kind: "function".into(),
        file: "a.rs".into(),
        first_conv_id: "conv_should_not_appear_either".into(),
        attribution: String::new(),
        ..NodeRow::default()
    };
    let xml = format_code_graph("callers", "foo", &[attributed, unattributed], &[]);
    assert!(
        xml.contains("attribution='transcript:70690eeb'"),
        "got: {xml}"
    );
    assert!(
        xml.contains("attribution='unattributed'"),
        "empty attribution must render as the literal 'unattributed' state: {xml}"
    );
    assert!(
        !xml.contains("conv_should_not_appear"),
        "first_conv_id must never be presented as introduction evidence: {xml}"
    );
}

#[test]
fn format_file_ledger_renders_attribution_never_first_conv() {
    use crate::storage::codegraph::{FileLedger, NodeRow};
    let ledger = FileLedger {
        file: "a.rs".into(),
        symbols: vec![NodeRow {
            name: "foo".into(),
            kind: "function".into(),
            file: "a.rs".into(),
            first_conv_id: "conv_should_not_appear".into(),
            attribution: "git:624e7229".into(),
            ..NodeRow::default()
        }],
        timeline: vec![],
        callers: vec![],
        indexed: true,
    };
    let xml = format_file_ledger(&ledger);
    assert!(xml.contains("attribution='git:624e7229'"), "got: {xml}");
    assert!(
        !xml.contains("first_conv="),
        "first_conv must no longer be rendered as introduction evidence: {xml}"
    );
    assert!(!xml.contains("conv_should_not_appear"), "got: {xml}");
}

// ─── quick_check abstention (fabrication defect, measured 2026-08-03) ───

fn quick_result(score: f32, content: &str) -> EnrichedResult {
    EnrichedResult {
        score,
        chunk: make_chunk("qc1", "conv-qc", content),
        resolution: None,
        trust: crate::provenance::TrustTier::Unknown,
        validity_demoted: false,
    }
}

#[test]
fn quick_check_empty_reports_not_found() {
    let xml = format_quick_check(&[], "never discussed anything");
    assert!(xml.contains("<found>false</found>"), "got: {xml}");
    assert!(xml.contains("<count>0</count>"), "got: {xml}");
    assert!(
        xml.contains("topic likely not discussed before"),
        "got: {xml}"
    );
    assert!(!xml.contains("<preview>"), "got: {xml}");
    // No candidate at all: nothing to disclose as rejected.
    assert!(!xml.contains("<best_rejected_score>"), "got: {xml}");
}

#[test]
fn quick_check_below_floor_suppresses_preview_and_count() {
    // Literal score of the weakest measured fabricated probe:
    // "onboarding call with the new backend contractor about payroll access"
    // matched Clerk SMS OTP cost discussion at 0.456 — pre-fix this rendered
    // count=1 with a confident preview.
    let results = vec![quick_result(
        0.44,
        "Clerk SMS OTP costs per verification in India",
    )];
    let xml = format_quick_check(&results, "onboarding call about payroll access");
    assert!(xml.contains("<found>false</found>"), "got: {xml}");
    assert!(xml.contains("<count>0</count>"), "got: {xml}");
    assert!(!xml.contains("<count>1</count>"), "got: {xml}");
    assert!(!xml.contains("<preview>"), "got: {xml}");
    assert!(!xml.contains("Clerk SMS OTP"), "got: {xml}");
    assert!(
        xml.contains("<best_rejected_score>0.440</best_rejected_score>"),
        "got: {xml}"
    );
}

#[test]
fn quick_check_weak_band_labels_and_warns() {
    // The four fabricated probes measured 2026-08-03 all land in the weak
    // band: no floor separates them from genuine topics (genuine minimum was
    // 0.468), so they are labelled and warned about rather than suppressed.
    //
    // Contract change (WP-B, 2026-09-22): each of these four is a lone
    // candidate (no runner-up, so no margin signal), so
    // relevance_assessment stays in the non-decisive branch —
    // the caveat text changed from "may be spurious" to the honest
    // calibration wording, but a caveat still fires on all four (that is
    // the part this test still guards).
    for score in [0.456_f32, 0.461, 0.583, 0.588] {
        let results = vec![quick_result(
            score,
            "unrelated content that merely looks close",
        )];
        let xml = format_quick_check(&results, "fabricated topic");
        assert!(
            xml.contains("<relevance>weak</relevance>"),
            "{score}: {xml}"
        );
        assert!(xml.contains("<warning>"), "{score}: {xml}");
        assert!(xml.contains("weakly calibrated"), "{score}: {xml}");
        assert!(
            !xml.contains("may be spurious"),
            "{score}: caveat wording changed under the contract: {xml}"
        );
        assert!(xml.contains("<found>true</found>"), "{score}: {xml}");
        assert!((QUICK_CHECK_FLOOR..WEAK_BAND_TOP).contains(&score));
    }
}

#[test]
fn weak_band_rank1_high_score_with_no_runner_up_gets_honest_caveat_not_a_ceiling_claim() {
    // Review finding [high] #1: a prior version of this contract let a
    // sufficiently high absolute weak-band score alone ("clears
    // FABRICATED_PROBE_CEILING") claim it was decisive even with no
    // runner-up to compare against. That ceiling was falsified by
    // corpus growth (fabricated probes now measure up to 0.629 — see
    // `WEAK_BAND_DECISIVE_MARGIN` docs) and the branch was removed
    // entirely. With no runner-up, there is nothing to compute a margin
    // against, so even a top score close to WEAK_BAND_TOP must fall
    // through to the honest low-confidence caveat — never "leads" and
    // never a "clears the highest ..." claim, because that text no
    // longer exists anywhere in this module.
    let results = vec![quick_result(0.61, "high weak-band score, no runner-up")];
    let xml = format_quick_check(&results, "probe");
    assert!(xml.contains("<relevance>weak</relevance>"), "got: {xml}");
    assert!(xml.contains("<warning>"), "got: {xml}");
    assert!(!xml.contains("spurious"), "got: {xml}");
    assert!(!xml.contains("indistinguishable"), "got: {xml}");
    assert!(
        !xml.contains("clears the highest"),
        "the ceiling-decisive branch was removed: got: {xml}"
    );
    assert!(
        !xml.contains("leads"),
        "no runner-up means there is nothing to lead over: got: {xml}"
    );
    assert!(xml.contains("weakly calibrated"), "got: {xml}");
}

#[test]
fn weak_band_rank1_wide_margin_over_a_comparable_runner_up_is_decisive() {
    // Contract (WP-B): a weak-band top score with a margin >=
    // WEAK_BAND_DECISIVE_MARGIN over a comparable (cosine) runner-up is
    // decisive — it clearly beat the rest of the list, even though the
    // absolute score alone is weakly calibrated here. No absolute-score
    // ceiling claim exists any more (review finding [high] #1) — this is
    // margin-only decisiveness.
    let results = vec![
        quick_result(0.58, "clear top match"),
        quick_result(0.30, "distant runner-up"),
    ];
    let xml = format_quick_check(&results, "probe");
    assert!(xml.contains("<relevance>weak</relevance>"), "got: {xml}");
    assert!(!xml.contains("spurious"), "got: {xml}");
    assert!(!xml.contains("indistinguishable"), "got: {xml}");
    assert!(xml.contains("leads the field"), "got: {xml}");
    assert!(
        !xml.contains("clears the highest"),
        "the ceiling-decisive branch was removed: got: {xml}"
    );
}

#[test]
fn relevance_assessment_synthetic_top_suppresses_decisive_wording() {
    // WP-B finding [low]: a rank-1 score that came from the keyword/FTS
    // fallback channel (`score_fts_candidate` / `FTS_REFLECTION_SCORE`,
    // fixed undated floors of 0.40/0.45) is not a cosine measurement, so
    // it must never trigger the "leads by margin" claim even when the
    // raw numbers alone would — `WEAK_BAND_DECISIVE_MARGIN` was derived
    // from cosine scores.
    let non_synthetic = relevance_assessment(0.55, Some(0.40), 1, false, true);
    assert!(
        non_synthetic
            .warning
            .as_ref()
            .unwrap()
            .contains("leads the field"),
        "control case should be decisive: {:?}",
        non_synthetic.warning
    );

    let synthetic = relevance_assessment(0.55, Some(0.40), 1, true, true);
    let warning = synthetic
        .warning
        .expect("weak band always carries a warning");
    assert!(
        !warning.contains("leads"),
        "synthetic top score's margin must not be read as decisive: {warning}"
    );
    assert!(
        warning.contains("weakly calibrated"),
        "synthetic score still needs an honest caveat: {warning}"
    );
}

#[test]
fn relevance_assessment_negative_margin_never_claims_leads() {
    // Contract (WP-B) part 3: rerank can put a rank-1 result whose RAW
    // score is below the comparable runner-up's (adjusted score won the
    // tiebreak). A negative margin can never clear
    // WEAK_BAND_DECISIVE_MARGIN (a positive threshold), so it always
    // falls through to the honest low-confidence caveat — the wording
    // must never say "leads the field by a -0.050 margin".
    let assessment = relevance_assessment(0.61, Some(0.66), 1, false, true);
    let warning = assessment
        .warning
        .expect("weak band always carries a warning");
    assert!(
        !warning.contains("leads"),
        "negative margin must not be worded as a lead: got: {warning}"
    );
    assert!(warning.contains("weakly calibrated"), "got: {warning}");
}

#[test]
fn relevance_assessment_ignores_a_synthetic_keyword_runner_up_margin() {
    // Review finding [high] #2, live receipt: a fabricated probe scored
    // 0.601 with a `[keyword] `-prefixed rank-2 at 0.405 (score_fts_
    // candidate's synthetic floor, not a cosine measurement) — margin
    // 0.196, well past WEAK_BAND_DECISIVE_MARGIN. Comparing a real
    // cosine top score's margin against that fixed floor is comparing
    // two different scales, so `format_search_results_with_rank_scores`
    // must skip a `[keyword] ` rank-2 row and use the next comparable
    // result (or `None`) as the runner-up passed to this function —
    // exercised end-to-end below. As a direct check on the function
    // itself: the same (top, runner_up) pair the caller would have
    // passed pre-fix must still render as decisive when the caller
    // asserts (honestly) that the runner-up IS comparable — the
    // contamination is a caller-side filtering bug, not something this
    // function can detect from the numbers alone.
    let contaminated_call = relevance_assessment(0.601, Some(0.405), 2, false, true);
    let contaminated_warning = contaminated_call.warning.as_ref().unwrap();
    assert!(
        contaminated_warning.contains("leads the field")
            || contaminated_warning.contains("leads the list"),
        "the function trusts the caller's runner-up filtering: {contaminated_warning}"
    );

    // The actual fix lives at the call site: end-to-end through
    // `format_search_results`, a `[keyword] ` rank-2 must never produce
    // this decisive wording. See
    // `weak_band_margin_skips_synthetic_keyword_runner_up` in
    // tests/integration.rs for the full-renderer receipt.
    let no_comparable_runner_up = relevance_assessment(0.601, None, 2, false, true);
    let warning = no_comparable_runner_up
        .warning
        .expect("weak band always carries a warning");
    assert!(
        !warning.contains("leads"),
        "no comparable runner-up means there is nothing to lead over: {warning}"
    );
}

#[test]
fn relevance_assessment_margin_validated_for_path_false_always_hedges() {
    // Review finding [high] #1: `WEAK_BAND_DECISIVE_MARGIN` was only ever
    // validated against `quick_check`'s exact-cosine top1-top2 margin.
    // `margin_validated_for_path: false` must force the low-margin hedge
    // regardless of how large or clean the arithmetic margin is — this is
    // the mechanism `format_search_results_with_rank_scores` uses to stay
    // hedged until a same-path threshold exists.
    //
    // Review finding [low] (W3-W): with the hedge forced on regardless of
    // the arithmetic, the wording must not then state a FACT the
    // arithmetic contradicts. A margin of 0.30 (>= WEAK_BAND_DECISIVE_
    // MARGIN) is not "low" — the real reason for hedging here is that the
    // margin is not calibrated on this path, not that it is small. The
    // text must say that, not "low margin over the rest of the list".
    let would_be_decisive = relevance_assessment(0.60, Some(0.30), 1, false, false);
    let warning = would_be_decisive
        .warning
        .expect("weak band always carries a warning");
    assert!(
        !warning.contains("leads"),
        "margin_validated_for_path=false must suppress decisive wording even for a huge \
             margin: {warning}"
    );
    assert!(
        warning.contains("weakly calibrated"),
        "must still fall back to the honest low-margin caveat: {warning}"
    );
    assert!(
        warning.contains("not calibrated on this search path"),
        "a margin that clears WEAK_BAND_DECISIVE_MARGIN but is uncalibrated must say so \
             honestly, not claim it is low: {warning}"
    );
    assert!(
        !warning.contains("low margin"),
        "the arithmetic margin (0.30) is not low — calling it low is false: {warning}"
    );
    assert!(
        warning.contains("Read the preview"),
        "the same 'read the preview' instruction must survive: {warning}"
    );

    // A GENUINELY low margin (below WEAK_BAND_DECISIVE_MARGIN) on the
    // same uncalibrated path must still get the original "low margin"
    // wording — that case really is low, regardless of calibration.
    let genuinely_low = relevance_assessment(0.60, Some(0.55), 1, false, false);
    let low_warning = genuinely_low
        .warning
        .expect("weak band always carries a warning");
    assert!(
        low_warning.contains("low margin"),
        "a margin below WEAK_BAND_DECISIVE_MARGIN must keep the 'low margin' wording: \
             {low_warning}"
    );
    assert!(
        !low_warning.contains("not calibrated on this search path"),
        "a genuinely low margin should not be reworded as merely uncalibrated: {low_warning}"
    );

    // Control: the identical (top, runner_up, synthetic_top) with
    // margin_validated_for_path=true IS decisive — proving the flag,
    // not some other input, gates this.
    let control = relevance_assessment(0.60, Some(0.30), 1, false, true);
    assert!(
        control.warning.as_ref().unwrap().contains("leads"),
        "control case should be decisive when the path IS validated: {:?}",
        control.warning
    );
}

#[test]
fn quick_check_warning_no_longer_claims_indistinguishable_from_never_discussed() {
    // D4: the old wording ("not distinguishable from topics that were never
    // discussed") told users a weak score meant "probably never happened",
    // but real matches land in this exact band too. The honest claim is that
    // absolute cosine is weakly calibrated here — not a verdict on the topic.
    let results = vec![quick_result(0.50, "borderline content")];
    let xml = format_quick_check(&results, "borderline probe");
    assert!(xml.contains("<warning>"), "got: {xml}");
    assert!(
        !xml.contains("never discussed"),
        "warning must not claim indistinguishability from never-discussed topics: {xml}"
    );
    assert!(
        xml.contains("weakly calibrated"),
        "warning should honestly describe weak calibration instead: {xml}"
    );
}

#[test]
fn quick_check_margin_signal_differs_with_top1_top2_gap() {
    // D4 part 2: a cheap discriminating signal (top1-top2 margin) must render
    // differently for a decisive win vs. a near-tie, even when both top scores
    // land in the same relevance band.
    let decisive = vec![
        quick_result(0.70, "clear top match"),
        quick_result(0.20, "distant runner-up"),
    ];
    let near_tie = vec![
        quick_result(0.70, "clear top match"),
        quick_result(0.69, "near-tied runner-up"),
    ];

    let xml_decisive = format_quick_check(&decisive, "probe");
    let xml_tie = format_quick_check(&near_tie, "probe");

    assert!(
        xml_decisive.contains("<margin>0.500</margin>"),
        "got: {xml_decisive}"
    );
    assert!(xml_tie.contains("<margin>0.010</margin>"), "got: {xml_tie}");
    assert!(
        xml_decisive.contains("<count>1</count>"),
        "the runner-up is margin evidence, not a rendered result: {xml_decisive}"
    );
    assert!(
        !xml_decisive.contains("distant runner-up"),
        "the runner-up preview must remain hidden: {xml_decisive}"
    );
}

#[test]
fn quick_check_margin_is_na_with_no_second_candidate() {
    // This slice represents the complete candidate corpus: n/a is valid
    // only because the corpus genuinely contains no runner-up.
    let results = vec![quick_result(0.80, "only candidate")];
    assert_eq!(results.len(), 1);
    let xml = format_quick_check(&results, "probe");
    assert!(xml.contains("<margin>n/a</margin>"), "got: {xml}");
}

#[test]
fn quick_check_mid_band_is_labelled_partial() {
    let results = vec![quick_result(0.673, "npm OIDC trusted publisher workflow")];
    let xml = format_quick_check(&results, "npm OIDC trusted publisher");
    assert!(xml.contains("<found>true</found>"), "got: {xml}");
    assert!(xml.contains("<relevance>partial</relevance>"), "got: {xml}");
    assert!(!xml.contains("may be spurious"), "got: {xml}");
    assert!(
        xml.contains("npm OIDC trusted publisher workflow"),
        "got: {xml}"
    );
}

#[test]
fn quick_check_strong_match_keeps_preview_and_count() {
    let results = vec![quick_result(
        0.816,
        "SessionStart memory manifest injection fix",
    )];
    let xml = format_quick_check(&results, "SessionStart memory manifest injection");
    assert!(xml.contains("<found>true</found>"), "got: {xml}");
    assert!(xml.contains("<count>1</count>"), "got: {xml}");
    assert!(xml.contains("<score>0.816</score>"), "got: {xml}");
    assert!(xml.contains("<relevance>good</relevance>"), "got: {xml}");
    assert!(
        xml.contains("SessionStart memory manifest injection fix"),
        "got: {xml}"
    );
    assert!(!xml.contains("may be spurious"), "got: {xml}");
}

#[test]
fn relevance_label_bands_are_shared_with_search() {
    assert_eq!(relevance_label(0.90), "high");
    assert_eq!(relevance_label(0.80), "good");
    assert_eq!(relevance_label(0.62), "partial");
    assert_eq!(relevance_label(0.619), "weak");
    assert_eq!(relevance_label(0.30), "weak");
    // Same banding drives the search renderer's summary attribute.
    let xml = format_search_results(
        &[quick_result(0.50, "borderline content")],
        "q",
        "proj",
        1,
        1,
    );
    assert!(xml.contains("relevance=\"weak\""), "got: {xml}");
}

#[test]
fn format_recent_work_preserves_caller_order_not_conversation_id_order() {
    // `get_recent_chunks` already orders rows by last-activity descending;
    // the conversation ids here sort the *opposite* way alphabetically, so
    // a regression back to keying/re-sorting by conversation id would flip
    // the output order.
    let mut newest = make_chunk("a-newest", "aaaa-newest", "newest content");
    newest.timestamp = "2026-09-20T10:00:00Z".into();
    let mut oldest = make_chunk("z-oldest", "zzzz-oldest", "oldest content");
    oldest.timestamp = "2026-09-01T10:00:00Z".into();

    let xml = format_recent_work(&[newest, oldest], "session", &Default::default());
    let newest_pos = xml.find("aaaa-newest").unwrap();
    let oldest_pos = xml.find("zzzz-oldest").unwrap();
    assert!(
        newest_pos < oldest_pos,
        "caller order must be preserved, got: {xml}"
    );
}

#[test]
fn ce_relevance_label_bands() {
    // high: gap >= 1.5 and spread >= 3.0
    assert_eq!(ce_relevance_label(6.0, Some(3.0), 1.0), "high");
    // gap too small for high, spread still clears good
    assert_eq!(ce_relevance_label(6.0, Some(5.0), 1.0), "good");
    // spread just under high with a big gap is good
    assert_eq!(ce_relevance_label(3.9, Some(1.0), 1.0), "good");
    // the gap edge of good: 1.0 is good, just under is partial
    assert_eq!(ce_relevance_label(3.0, Some(2.0), 1.0), "good");
    assert_eq!(ce_relevance_label(3.0, Some(2.9), 1.0), "partial");
    // partial between 1.0 and 2.0
    assert_eq!(ce_relevance_label(2.5, Some(2.4), 1.0), "partial");
    assert_eq!(ce_relevance_label(2.0, None, 1.0), "partial");
    // weak needs a spread under 1.0 and a near-tie gap
    assert_eq!(ce_relevance_label(1.9, Some(1.88), 1.0), "weak");
    assert_eq!(ce_relevance_label(1.9, Some(1.8), 1.0), "partial");
    // no runner-up means gap 0: never high, however large the spread
    assert_eq!(ce_relevance_label(9.0, None, 0.0), "good");
    // margins only: shifting every score leaves the label unchanged
    assert_eq!(ce_relevance_label(-4.0, Some(-7.0), -9.0), "high");
    assert_eq!(ce_relevance_label(16.0, Some(13.0), 11.0), "high");
}

#[test]
fn ce_label_thresholds_are_the_s9_judged_pair_calibration() {
    assert_eq!(CE_GAP_HIGH, 1.5);
    assert_eq!(CE_GAP_GOOD, 1.0);
    assert_eq!(CE_GAP_TIE, 0.05);
    assert_eq!(CE_SPREAD_HIGH, 3.0);
    assert_eq!(CE_SPREAD_GOOD, 2.0);
    assert_eq!(CE_SPREAD_PARTIAL, 1.0);
    // Rank-1 rows of the 40 judged pairs, (top, runner-up, median) in
    // logits as the final ranker summary printed them.
    // Pair 40: picked, clear lead.
    assert_eq!(ce_relevance_label(11.71, Some(11.71 - 1.73), 8.69), "high");
    // Pair 21: picked, real lead.
    assert_eq!(ce_relevance_label(11.97, Some(11.97 - 1.11), 8.75), "good");
    // Pair 38: an echo of the query, gap 0.53, no longer good.
    assert_eq!(
        ce_relevance_label(11.04, Some(11.04 - 0.53), 6.60),
        "partial"
    );
    // Pair 10: a near-tie at the top with a wide pool spread, not good.
    assert_eq!(
        ce_relevance_label(10.89, Some(10.89 - 0.04), 7.89),
        "partial"
    );
    // Pair 12: picked, gap 0.01 and no spread, the one picked weak.
    assert_eq!(ce_relevance_label(12.00, Some(12.00 - 0.01), 11.91), "weak");
    // Pair 11: picked, gap 0.06, no longer weak.
    assert_eq!(
        ce_relevance_label(10.56, Some(10.56 - 0.06), 10.19),
        "partial"
    );
    // Pair 4: picked, gap 0.17, no longer weak.
    assert_eq!(
        ce_relevance_label(11.15, Some(11.15 - 0.17), 10.65),
        "partial"
    );
    // A lone candidate has no rival: a spread alone can call it good.
    assert_eq!(ce_relevance_label(9.0, None, 6.0), "good");
    // A lone candidate with no spread is weak.
    assert_eq!(ce_relevance_label(9.0, None, 9.0), "weak");
}

// ---- per-model bands: a stopgap that keeps the label mix, not a fix ----

const TUNED_RUN2_SHA: &str = "73b9d13ac00020e657dc43319a9717fbf0fc10d7243ad5710d48837c6137abab";

/// The label rule exactly as it stood before bands became a value: the
/// literal numbers, not the constants.
fn pre_stopgap_label(top: f32, runner_up: Option<f32>, median: f32) -> &'static str {
    let gap = runner_up.map_or(0.0, |second| top - second);
    let spread = top - median;
    if gap >= 1.5 && spread >= 3.0 {
        "high"
    } else if spread >= 2.0 && runner_up.is_none_or(|_| gap >= 1.0) {
        "good"
    } else if spread < 1.0 && gap < 0.05 {
        "weak"
    } else {
        "partial"
    }
}

#[test]
fn ce_bands_shipped_are_the_s9_constants() {
    let b = CeBands::SHIPPED;
    assert_eq!(b.gap_high, 1.5);
    assert_eq!(b.gap_good, 1.0);
    assert_eq!(b.gap_tie, 0.05);
    assert_eq!(b.spread_high, 3.0);
    assert_eq!(b.spread_good, 2.0);
    assert_eq!(b.spread_partial, 1.0);
}

#[test]
fn ce_shipped_labels_are_byte_identical_to_the_pre_stopgap_rule() {
    // Every (gap, spread) cell on a 0.01 / 0.05 grid, with and without a
    // runner-up, through the public entry point and through the explicit
    // SHIPPED bands.
    for g in 0..=800 {
        for sp in -40..=240 {
            let gap = g as f32 * 0.01;
            let spread = sp as f32 * 0.05;
            let median = 1.25f32;
            let top = median + spread;
            let runner_up = Some(top - gap);
            let want = pre_stopgap_label(top, runner_up, median);
            assert_eq!(ce_relevance_label(top, runner_up, median), want);
            assert_eq!(
                ce_relevance_label_with(top, runner_up, median, &CeBands::SHIPPED),
                want
            );
            assert_eq!(
                CeSummary {
                    top,
                    runner_up,
                    median
                }
                .label_with(&CeBands::SHIPPED),
                want
            );
        }
    }
    for sp in -40..=240 {
        let median = -0.5f32;
        let top = median + sp as f32 * 0.05;
        assert_eq!(
            ce_relevance_label(top, None, median),
            pre_stopgap_label(top, None, median)
        );
    }
}

#[test]
fn ce_bands_apply_only_to_the_listed_onnx() {
    let tuned = CeBands {
        gap_high: 9.0,
        ..CeBands::SHIPPED
    };
    let table = [("abc123", tuned)];
    // Not listed, in any case: shipped.
    assert_eq!(CeBands::lookup("def456", &table), CeBands::SHIPPED);
    assert_eq!(CeBands::lookup("", &table), CeBands::SHIPPED);
    assert_eq!(CeBands::lookup("ABC123", &table), tuned);

    // A model directory is keyed on the hash of its ONNX file.
    let dir = tempfile::tempdir().unwrap();
    // No ONNX file at all.
    assert_eq!(
        CeBands::for_model_dir_in(dir.path(), &table),
        CeBands::SHIPPED
    );
    // A missing directory.
    assert_eq!(
        CeBands::for_model_dir_in(&dir.path().join("nope"), &table),
        CeBands::SHIPPED
    );
    // An ONNX file whose hash is not listed.
    std::fs::write(dir.path().join(CE_ONNX_FILE), b"some other reranker").unwrap();
    assert_eq!(
        CeBands::for_model_dir_in(dir.path(), &table),
        CeBands::SHIPPED
    );
    // The listed hash: sha256("tuned bytes").
    std::fs::write(dir.path().join(CE_ONNX_FILE), b"tuned bytes").unwrap();
    let sha = file_sha256(&dir.path().join(CE_ONNX_FILE)).unwrap();
    assert_eq!(sha.len(), 64);
    let listed = [(sha.as_str(), tuned)];
    assert_eq!(CeBands::for_model_dir_in(dir.path(), &listed), tuned);
    // The same bytes under a table that lists some other hash.
    assert_eq!(
        CeBands::for_model_dir_in(dir.path(), &table),
        CeBands::SHIPPED
    );
    // A directory the shipped reranker would be read from is never listed.
    assert!(
        CE_TUNED_BANDS
            .iter()
            .all(|(sha, _)| *sha
                != "92cdd69ba25685f50b13663f83724d6e8cc77344bb7c0441a26a428564b1c2f6")
    );
    // With the default table, an unlisted ONNX stays on SHIPPED.
    assert_eq!(CeBands::for_model_dir(dir.path()), CeBands::SHIPPED);
}

#[test]
fn ce_embedded_model_reads_the_tuned_bands_and_a_dir_reads_its_own_hash() {
    use crate::search::cross_encoder::ModelSource;
    let tuned = CeBands::lookup(TUNED_RUN2_SHA, CE_TUNED_BANDS);
    assert_ne!(tuned, CeBands::SHIPPED);
    assert_eq!(CeBands::for_source(&ModelSource::Embedded), tuned);
    // An override directory is judged by the bytes in it, not by its path.
    let dir = tempfile::tempdir().unwrap();
    let source = ModelSource::Dir(dir.path().to_path_buf());
    assert_eq!(CeBands::for_source(&source), CeBands::SHIPPED);
    std::fs::write(
        dir.path().join(CE_ONNX_FILE),
        crate::search::cross_encoder::embedded_onnx(),
    )
    .unwrap();
    assert_eq!(CeBands::for_source(&source), tuned);
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn ce_file_sha256_is_the_sha256_of_the_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    std::fs::write(&path, b"abc").unwrap();
    assert_eq!(
        file_sha256(&path).unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(file_sha256(&dir.path().join("missing")).is_none());
}

#[test]
fn ce_tuned_run2_entry_holds_the_quantile_matched_bands() {
    // Fit: the 281 recalls with a cross-encoder summary of the 310 audit
    // queries, shipped model vs tuned-run2, both run on this laptop's CPU
    // over the same database clone; each constant moved to the tuned
    // value at the percentile the shipped constant sits at.
    assert_eq!(CE_TUNED_BANDS.len(), 1);
    let (sha, bands) = CE_TUNED_BANDS[0];
    assert_eq!(sha, TUNED_RUN2_SHA);
    assert_eq!(bands.gap_high, 4.21);
    assert_eq!(bands.gap_good, 3.03);
    assert_eq!(bands.gap_tie, 0.215);
    assert_eq!(bands.spread_high, 7.355);
    assert_eq!(bands.spread_good, 6.01);
    assert_eq!(bands.spread_partial, 3.98);
    assert_eq!(CeBands::lookup(TUNED_RUN2_SHA, CE_TUNED_BANDS), bands);
}

#[test]
fn ce_tuned_bands_stop_the_label_inflation_of_the_shipped_bands() {
    // Rank-1 summaries on the tuned model's logit scale (wider gaps and
    // spreads): (top, runner-up, median, shipped label, tuned label).
    let tuned = CeBands::lookup(TUNED_RUN2_SHA, CE_TUNED_BANDS);
    let rows = [
        (9.0, Some(4.0), 0.0, "high", "high"),
        (7.0, Some(3.5), 0.0, "high", "good"),
        (3.0, Some(2.0), 0.0, "good", "partial"),
        (2.0, Some(1.9), 0.0, "partial", "weak"),
        (5.0, None, 0.0, "good", "partial"),
    ];
    for (top, runner_up, median, shipped_label, tuned_label) in rows {
        let summary = CeSummary {
            top,
            runner_up,
            median,
        };
        assert_eq!(
            summary.label_with(&CeBands::SHIPPED),
            shipped_label,
            "{summary:?}"
        );
        assert_eq!(summary.label_with(&tuned), tuned_label, "{summary:?}");
    }
}

#[test]
fn ce_summary_from_scores_takes_top_runner_up_and_median() {
    let summary = CeSummary::from_scores(&[1.0, 5.0, 3.0, 2.0]).unwrap();
    assert_eq!(summary.top, 5.0);
    assert_eq!(summary.runner_up, Some(3.0));
    assert_eq!(summary.median, 2.5);
    let odd = CeSummary::from_scores(&[4.0, 1.0, 2.0]).unwrap();
    assert_eq!(odd.median, 2.0);
    let single = CeSummary::from_scores(&[7.0]).unwrap();
    assert_eq!(single.runner_up, None);
    assert_eq!(single.median, 7.0);
    assert!(CeSummary::from_scores(&[]).is_none());
    assert!(CeSummary::from_scores(&[f32::NAN]).is_none());
}

#[test]
fn formatter_without_ce_matches_the_cosine_path_byte_for_byte() {
    let results = vec![
        quick_result(0.80, "first row"),
        quick_result(0.50, "second row"),
    ];
    let base = format_search_results_with_rank_scores(&results, "q", "all", 5, 3, &[]);
    let via_ce = format_search_results_with_ce(&results, "q", "all", 5, 3, &[], None);
    assert_eq!(base, via_ce);
    assert_eq!(base, format_search_results(&results, "q", "all", 5, 3));
    assert!(!base.contains("ce-top"), "got: {base}");
    assert!(
        base.contains(
            "<summary count=\"2\" relevance=\"good\" top-score=\"0.800\" rank1-score=\"0.800\" margin=\"0.300\">"
        ),
        "got: {base}"
    );
}

#[test]
fn formatter_with_ce_labels_the_printed_score_and_keeps_every_row() {
    // The printed score is weak even when CE margins are high.
    let results = vec![
        quick_result(0.50, "first row"),
        quick_result(0.40, "second row"),
    ];
    let ce = CeSummary {
        top: 20.0,
        runner_up: Some(10.0),
        median: 1.0,
    };
    let xml = format_search_results_with_ce(&results, "q", "all", 5, 3, &[], Some(&ce));
    assert!(
        xml.contains("relevance=\"weak\"") && !xml.contains("relevance=\"high\""),
        "got: {xml}"
    );
    assert!(
        xml.contains("ce-top=\"20.00\" ce-margin=\"10.00\" ce-median=\"1.00\">"),
        "got: {xml}"
    );
    assert!(
        xml.contains("(weak relevance, top score: 0.500)"),
        "got: {xml}"
    );
    assert!(xml.contains("<caveat>"), "got: {xml}");
    assert_eq!(xml.matches("<r rank=").count(), 2, "got: {xml}");

    // A weak CE label with no runner-up reads n/a and keeps the caveat.
    let weak = CeSummary {
        top: 0.5,
        runner_up: None,
        median: 0.5,
    };
    let xml = format_search_results_with_ce(&results, "q", "all", 5, 3, &[], Some(&weak));
    assert!(xml.contains("relevance=\"weak\""), "got: {xml}");
    assert!(xml.contains("ce-margin=\"n/a\""), "got: {xml}");
    assert!(xml.contains("<caveat>"), "got: {xml}");
    assert_eq!(xml.matches("<r rank=").count(), 2, "got: {xml}");
}

#[test]
fn restamp_performance_sets_the_header_and_ttl_and_leaves_the_stages() {
    let results = vec![EnrichedResult {
        score: 0.9,
        chunk: make_chunk("c-1", "conv-1", "hello world"),
        resolution: None,
        trust: crate::provenance::TrustTier::Unknown,
        validity_demoted: false,
    }];
    let page = format_search_results(&results, "q", "all", 5, 3);
    assert!(
        page.contains("PERFORMANCE: 8ms (1 collection searched)\n"),
        "{page}"
    );
    let stamped = restamp_performance(page.clone(), 1840);
    assert!(
        stamped.contains("\u{26a1} PERFORMANCE: 1840ms (1 collection searched)\n"),
        "{stamped}"
    );
    assert!(stamped.contains("<ttl>1840</ttl>"), "{stamped}");
    // Embed and search keep their own stage times; nothing else moves.
    assert!(stamped.contains("<emb>3</emb>") && stamped.contains("<srch>5</srch>"));
    assert_eq!(
        stamped.replace("1840", "8"),
        page,
        "only the two totals differ"
    );
}

#[test]
fn restamp_performance_leaves_pages_without_the_header_alone() {
    for page in [
        "\u{274c} NO RESULTS: none\n\n<search>\n  <ttl>9</ttl>\n</search>".to_string(),
        "no newline at all".to_string(),
        String::new(),
        "\u{1f3af} RESULTS: 1 matches\n\u{26a1} PERFORMANCE: soon (1 collection searched)\n"
            .to_string(),
        "\u{1f3af} RESULTS: 1 matches".to_string(),
    ] {
        assert_eq!(restamp_performance(page.clone(), 1234), page);
    }
}

#[test]
fn restamp_performance_never_touches_result_text() {
    let page = "\u{1f3af} RESULTS: 1 matches (high relevance, top score: 0.900)\n\u{26a1} PERFORMANCE: 8ms (1 collection searched)\n\n<search>\n  <results>\n    <r rank=\"1\"><excerpt><![CDATA[<ttl>8</ttl> and \u{26a1} PERFORMANCE: 8ms]]></excerpt></r>\n  </results>\n</search>\n".to_string();
    let stamped = restamp_performance(page.clone(), 900);
    assert_eq!(
        stamped,
        page.replacen("PERFORMANCE: 8ms (1", "PERFORMANCE: 900ms (1", 1)
    );
}

#[test]
fn restamp_performance_fixes_the_ttl_of_a_keyword_only_page() {
    let page = "\u{1f3af} RESULTS: 2 matches (keyword relevance; semantic confidence unavailable)\n\n<search>\n  <meta>\n    <perf>\n      <ttl>1</ttl>\n    </perf>\n  </meta>\n</search>\n".to_string();
    let stamped = restamp_performance(page.clone(), 640);
    assert_eq!(stamped, page.replace("<ttl>1</ttl>", "<ttl>640</ttl>"));
    assert!(!stamped.contains("PERFORMANCE"));
}
