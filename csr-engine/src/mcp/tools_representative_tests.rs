use super::*;

fn chunk(id: &str, score: f64, boilerplate: bool) -> PoolChunk {
    PoolChunk {
        id: id.to_string(),
        score,
        boilerplate,
    }
}

#[test]
fn boilerplate_top_with_content_second_picks_content() {
    let pool = vec![chunk("tool", 0.9, true), chunk("talk", 0.5, false)];
    assert_eq!(session_representative("tool", &pool), "talk");
}

#[test]
fn highest_scored_content_wins_over_lower_content() {
    let pool = vec![
        chunk("tool", 0.9, true),
        chunk("low", 0.2, false),
        chunk("mid", 0.5, false),
    ];
    assert_eq!(session_representative("tool", &pool), "mid");
}

#[test]
fn boilerplate_only_keeps_the_pick() {
    let pool = vec![chunk("tool", 0.9, true), chunk("tool2", 0.4, true)];
    assert_eq!(session_representative("tool", &pool), "tool");
}

#[test]
fn content_top_is_unchanged() {
    let pool = vec![chunk("talk", 0.9, false), chunk("tool", 0.5, true)];
    assert_eq!(session_representative("talk", &pool), "talk");
}

#[test]
fn unknown_pick_abstains() {
    let pool = vec![chunk("talk", 0.9, false)];
    assert_eq!(session_representative("missing", &pool), "missing");
}

#[test]
fn apply_swaps_only_sessions_with_a_boilerplate_pick() {
    let mut collapsed = vec!["t1".to_string(), "c2".to_string(), "e3".to_string()];
    let keys: HashMap<String, String> = [("t1", "s1"), ("c2", "s2"), ("e3", "s3")]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    let mut pool: HashMap<String, Vec<PoolChunk>> = HashMap::new();
    pool.insert(
        "s1".into(),
        vec![chunk("t1", 0.9, true), chunk("a1", 0.3, false)],
    );
    pool.insert(
        "s2".into(),
        vec![chunk("c2", 0.9, false), chunk("t2", 0.3, true)],
    );
    apply_content_representatives(&mut collapsed, &keys, &pool);
    assert_eq!(collapsed, vec!["a1", "c2", "e3"]);
}

type RepFixture = (
    Vec<String>,
    HashMap<String, String>,
    HashMap<String, Vec<PoolChunk>>,
);

fn f1a_rep_fixture() -> RepFixture {
    let collapsed = vec!["a1".to_string(), "c2".to_string()];
    let keys: HashMap<String, String> = [("t1", "s1"), ("a1", "s1"), ("c2", "s2")]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
    let mut pool: HashMap<String, Vec<PoolChunk>> = HashMap::new();
    pool.insert(
        "s1".into(),
        vec![chunk("t1", 0.9, true), chunk("a1", 0.3, false)],
    );
    pool.insert("s2".into(), vec![chunk("c2", 0.9, false)]);
    (collapsed, keys, pool)
}

#[test]
fn s9r_ce_keeps_content_representative_over_tool_row() {
    // The cross-encoder scores the tool row of s1 highest by a hair and
    // picks it; the content chunk scores within the tie margin, so the
    // content chunk is shown.
    let (collapsed, keys, pool) = f1a_rep_fixture();
    let scored = vec![
        ("t1".to_string(), "s1".to_string(), 0u8, 5.0f32),
        ("a1".to_string(), "s1".to_string(), 0u8, 4.9f32),
        ("c2".to_string(), "s2".to_string(), 0u8, 3.0f32),
    ];
    let plain = ce_reorder(&collapsed, &keys, &scored);
    assert_eq!(plain.order, vec!["t1", "c2"], "CE alone picks the tool row");
    let ordered = ce_reorder_with_representatives(&collapsed, &keys, &scored, &pool);
    assert_eq!(ordered.order, vec!["a1", "c2"]);
    assert_eq!(ordered.session_scores, plain.session_scores);
    // A session without a boilerplate pick is untouched.
    let only_content = vec![scored[1].clone(), scored[2].clone()];
    let before = ce_reorder(&collapsed, &keys, &only_content);
    let after = ce_reorder_with_representatives(&collapsed, &keys, &only_content, &pool);
    assert_eq!(before, after);
}

#[test]
fn f1a_ce_keeps_tool_row_that_clearly_outscores_the_content_chunk() {
    // Bisected cause of the probe rank-1 drop (58de0ee5): a tool-result row
    // holding the answer (a memory file read) beat the session's user-text
    // chunk by 0.5 to 3.8 logits and was swapped out anyway.
    let (collapsed, keys, pool) = f1a_rep_fixture();
    for content_score in [4.5f32, 2.0, -3.0] {
        let scored = vec![
            ("t1".to_string(), "s1".to_string(), 0u8, 5.0f32),
            ("a1".to_string(), "s1".to_string(), 0u8, content_score),
            ("c2".to_string(), "s2".to_string(), 0u8, 3.0f32),
        ];
        let ordered = ce_reorder_with_representatives(&collapsed, &keys, &scored, &pool);
        assert_eq!(
            ordered.order,
            vec!["t1", "c2"],
            "content at {content_score}"
        );
    }
}

#[test]
fn f1a_ce_keeps_tool_row_when_the_content_chunk_was_not_scored() {
    // No score for the content chunk: nothing says it is as good as the pick.
    let (collapsed, keys, pool) = f1a_rep_fixture();
    let scored = vec![
        ("t1".to_string(), "s1".to_string(), 0u8, 5.0f32),
        ("c2".to_string(), "s2".to_string(), 0u8, 3.0f32),
    ];
    let ordered = ce_reorder_with_representatives(&collapsed, &keys, &scored, &pool);
    assert_eq!(ordered.order, vec!["t1", "c2"]);
}

#[test]
fn f1a_final_ce_summary_reads_the_final_order() {
    let scores: HashMap<String, f32> = [("a", 9.0f32), ("b", 7.0), ("c", 1.0)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    // Pool order was a, b, c; a sink moved b to the top.
    let summary = final_ce_summary(&["b", "a", "c"], &scores).unwrap();
    assert_eq!(summary.top, 7.0);
    assert_eq!(summary.runner_up, Some(9.0));
    assert_eq!(summary.median, 7.0);
    // Unpooled rows skip the runner-up and the median.
    let summary = final_ce_summary(&["a", "x", "c", "a"], &scores).unwrap();
    assert_eq!(summary.top, 9.0);
    assert_eq!(summary.runner_up, Some(1.0));
    assert_eq!(summary.median, 5.0);
}

#[test]
fn f1a_final_ce_summary_is_none_without_a_first_score() {
    let scores: HashMap<String, f32> = [("a".to_string(), 9.0f32)].into_iter().collect();
    assert!(final_ce_summary(&["x", "a"], &scores).is_none());
    assert!(final_ce_summary(&[], &scores).is_none());
    let nan: HashMap<String, f32> = [("a".to_string(), f32::NAN)].into_iter().collect();
    assert!(final_ce_summary(&["a"], &nan).is_none());
}

fn rig_row(conversation: &str, seq: usize, text: &str) -> EnrichedResult {
    EnrichedResult {
        score: 1.0,
        chunk: crate::import::ConversationChunk {
            id: format!("{conversation}-{seq}"),
            conversation_id: conversation.into(),
            project_name: "p".into(),
            timestamp: "2026-10-01T00:00:00Z".into(),
            content: text.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::ToolResult,
            seq,
            is_sidechain: false,
        },
        resolution: None,
        trust: crate::provenance::TrustTier::Unknown,
        validity_demoted: false,
    }
}

/// An assistant chunk holding one Bash call.
fn rig_call(seq: usize, text: &str) -> EnrichedResult {
    let mut row = rig_row("rig", seq, text);
    row.chunk.author = crate::provenance::Speaker::Assistant;
    row
}

fn rig_store(rows: &[&EnrichedResult]) -> Storage {
    let storage = Storage::open_memory().unwrap();
    for row in rows {
        storage
            .insert_chunk_with_source(&row.chunk, &vec![0.1; 384], "conversation")
            .unwrap();
        storage
            .insert_chunk_provenance(
                &row.chunk.id,
                &crate::provenance::ChunkProvenance {
                    author: row.chunk.author,
                    source_conv_id: row.chunk.conversation_id.clone(),
                    supersedes: None,
                },
            )
            .unwrap();
    }
    storage
}

fn rig_ids(rows: &[EnrichedResult]) -> Vec<&str> {
    rows.iter().map(|row| row.chunk.id.as_str()).collect()
}

#[test]
fn synth_sink_rig_rows_demotes_in_place_and_leaves_other_queries_alone() {
    let query = "why do hooks use catch-all wrappers";
    let planted = "why do hooks use catch-all wrappers so they never block claude code? \
            [synthetic re-ask cycle 3] Synthetic E3 test answer.";
    // A session that ran the rig twice among forty Bash calls.
    let mut rig: Vec<EnrichedResult> = vec![
        rig_call(0, "[Bash: csr-engine bench --probe --db-path /x/db]"),
        rig_call(1, "[Bash: python3 s3/probe_ab.py compare a b]"),
    ];
    rig.extend((2..42).map(|seq| rig_call(seq, "[Bash: ls]")));
    let candidate = rig_row("rig", 50, planted);
    let real = rig_row("real", 0, planted);
    let answer = rig_row("answer", 0, "hooks wrap their bodies so a bug never blocks");
    let mut all: Vec<&EnrichedResult> = rig.iter().collect();
    all.extend([&candidate, &real, &answer]);
    let storage = rig_store(&all);
    let tags = HashMap::new();

    // Page order from the ranker: the planted row first.
    let mut page = vec![candidate.clone(), real.clone(), answer.clone()];
    sink_rig_rows(&storage, &mut page, &tags, query);
    assert_eq!(rig_ids(&page), ["real-0", "answer-0", "rig-50"]);

    // A query the rows do not hold: the page is untouched.
    let mut page = vec![candidate.clone(), real.clone(), answer.clone()];
    sink_rig_rows(&storage, &mut page, &tags, "how do I sort a vec");
    assert_eq!(rig_ids(&page), ["rig-50", "real-0", "answer-0"]);

    // A stored reflection with the same text is never judged.
    let mut tagged = HashMap::new();
    tagged.insert("rig-50".to_string(), vec!["reflection".to_string()]);
    let mut page = vec![candidate.clone(), real.clone(), answer.clone()];
    sink_rig_rows(&storage, &mut page, &tagged, query);
    assert_eq!(rig_ids(&page), ["rig-50", "real-0", "answer-0"]);
}

#[tokio::test]
async fn the_recall_header_reports_the_whole_call_not_only_the_search() {
    // A formatter header that claims 31 ms for a call that spent 150 ms
    // before the page was built, on top of its own stages.
    let page = "🎯 RESULTS: 1 matches (high relevance, top score: 0.900)\n⚡ PERFORMANCE: 31ms (1 collection searched)\n\n<search>\n  <meta>\n    <perf>\n      <ttl>31</ttl>\n      <emb>3</emb>\n      <srch>28</srch>\n    </perf>\n  </meta>\n</search>\n";
    let started = Instant::now();
    let stamped = stamp_whole_call(started, async {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        Ok(page.to_string())
    })
    .await
    .unwrap();
    let wall = started.elapsed().as_millis() as u64;
    let shown: u64 = stamped
        .split("PERFORMANCE: ")
        .nth(1)
        .and_then(|rest| rest.split("ms").next())
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("no PERFORMANCE header: {stamped}"));
    assert!(
        (150..=wall).contains(&shown),
        "header says {shown} ms, the call took {wall} ms: {stamped}"
    );
    assert!(
        stamped.contains(&format!("<ttl>{shown}</ttl>")),
        "{stamped}"
    );
    assert!(stamped.contains("<emb>3</emb>") && stamped.contains("<srch>28</srch>"));
    // An error passes through untouched.
    let failed = stamp_whole_call(Instant::now(), async { anyhow::bail!("boom") }).await;
    assert_eq!(failed.unwrap_err().to_string(), "boom");
}
