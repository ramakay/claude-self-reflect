use super::*;

#[test]
fn at_path_mention_yields_the_bare_path() {
    let files = extract_file_paths_from_prompt("why does @src/auth.rs reject tokens?");
    assert_eq!(files, vec!["src/auth.rs".to_string()]);
}

#[test]
fn this_sessions_own_subagents_are_never_handed_back_to_it() {
    let chunk = |conversation_id: &str| crate::import::ConversationChunk {
        id: "c".into(),
        conversation_id: conversation_id.into(),
        project_name: "csr-engine".into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        content: "the pane reads the dream log".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::Assistant,
        seq: 0,
        is_sidechain: true,
    };
    let parent = |id: &str| {
        let id = id.to_owned();
        move || Some(id)
    };
    let ours = chunk("agent-a3dd7b830b8b7fa52");
    assert!(is_own_subagent(&ours, Some("6ca2a4d5"), parent("6ca2a4d5")));
    // another session's subagent is memory, and so is a chunk with no parent on record
    assert!(!is_own_subagent(
        &ours,
        Some("6ca2a4d5"),
        parent("f98b6500")
    ));
    assert!(!is_own_subagent(&ours, Some("6ca2a4d5"), || None));
    // not a subagent: its parent is never looked up
    let plain = chunk("f98b6500-1111-4222-8333-444455556666");
    assert!(!is_own_subagent(&plain, Some("6ca2a4d5"), || panic!(
        "looked up"
    )));
    assert!(!is_own_subagent(&ours, None, || panic!("looked up")));
}

#[tokio::test]
async fn reflections_derived_from_this_sessions_own_subagents_are_never_handed_back() {
    let root = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let mut search = crate::search::SearchEngine::new(8);
    // A subagent's chunk records the session that ran it as its parent.
    for (conv, parent) in [("agent-ours", "sess-1"), ("agent-theirs", "sess-2")] {
        let chunk = crate::import::ConversationChunk {
            id: format!("chunk-{conv}"),
            conversation_id: conv.into(),
            project_name: "project".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: "subagent words".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::Assistant,
            seq: 0,
            is_sidechain: true,
        };
        storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
        storage
            .insert_chunk_provenance(
                &chunk.id,
                &crate::provenance::ChunkProvenance {
                    author: crate::provenance::Speaker::Assistant,
                    source_conv_id: parent.into(),
                    supersedes: None,
                },
            )
            .unwrap();
    }
    for (id, conv) in [
        ("refl-own-child", "agent-ours"),
        ("refl-own-session", "sess-1"),
        ("refl-other-child", "agent-theirs"),
        ("refl-other-session", "sess-9"),
    ] {
        storage
            .insert_reflection(
                id,
                &format!("derived from {conv}"),
                &[
                    "narrative_extracted_v3".into(),
                    format!("conv_{conv}"),
                    "project_project".into(),
                ],
                &[1.0, 0.0],
            )
            .unwrap();
        search.insert_reflection(id.into(), vec![1.0, 0.0]);
    }
    let engine = crate::engine::Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );

    let ids = |results: Vec<RawResult>| {
        let mut ids: Vec<String> = results.into_iter().filter_map(|r| r.memory_id).collect();
        ids.sort();
        ids
    };
    let mine = ids(search_reflections_with_vec(
        &engine,
        &[1.0, 0.0],
        10,
        0.0,
        "project",
        Some("sess-1"),
        &mut SearchEvidence::default(),
    )
    .await);
    assert_eq!(mine, ["refl-other-child", "refl-other-session"]);
    // another session sees its own subagent's words as memory, and ours too
    let other = ids(search_reflections_with_vec(
        &engine,
        &[1.0, 0.0],
        10,
        0.0,
        "project",
        Some("sess-2"),
        &mut SearchEvidence::default(),
    )
    .await);
    assert_eq!(
        other,
        ["refl-other-session", "refl-own-child", "refl-own-session"]
    );
    // no session known: nothing is excluded
    let anon = ids(search_reflections_with_vec(
        &engine,
        &[1.0, 0.0],
        10,
        0.0,
        "project",
        None,
        &mut SearchEvidence::default(),
    )
    .await);
    assert_eq!(anon.len(), 4);
}

#[test]
fn a_handed_line_names_the_session_claude_can_open() {
    let label = handoff_label(&handed(Some("a3e857eb-1111-4222-8333-444455556666")));
    assert!(label.starts_with("conversation|"), "{label}");
    assert!(label.ends_with("|session a3e857eb"), "{label}");
    // a subagent's chunk, or none at all: no id to open
    assert!(!handoff_label(&handed(Some("agent-a1b2c3d4e5"))).contains("session"));
    assert!(!handoff_label(&handed(None)).contains("session"));
}

#[tokio::test]
async fn chunk_search_reads_cached_floor_without_provenance_event_tables() {
    let root = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let chunk = crate::import::ConversationChunk {
        id: "cached-search-floor".into(),
        conversation_id: "search-conv".into(),
        project_name: "project".into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        content: "cached structural trust".into(),
        message_count: 1,
        summary: None,
        author: crate::provenance::Speaker::User,
        seq: 0,
        is_sidechain: false,
    };
    storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
    storage
        .with_connection(|conn| {
            conn.execute(
                "UPDATE chunks SET min_trust=3 WHERE id='cached-search-floor'",
                [],
            )?;
            conn.execute("DROP TABLE chunk_spans", [])?;
            conn.execute("DROP TABLE provenance_events", [])?;
            Ok(())
        })
        .unwrap();
    let mut search = crate::search::SearchEngine::new(8);
    search.insert_chunk(chunk.id.clone(), vec![1.0, 0.0]);
    let engine = crate::engine::Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );

    let results = search_chunks_with_vec_core(
        &engine,
        "",
        crate::search::hybrid::SearchMode::Vector,
        &[1.0, 0.0],
        1,
        0.0,
        "project",
        None,
        |_, _| Default::default(),
    )
    .await;

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].min_trust,
        crate::provenance::TrustTier::UserHistory
    );
}

#[test]
fn a_tool_dump_is_told_from_a_report_and_from_a_decision() {
    let listing = "34:**LUT tournament prototype** renders 28 LUTs\n35:  let grid = []\n36:  for lut in luts { run(lut) }";
    let decision = "Decision: keep the floor on the label; the share decides what is handed back.";
    assert!(is_tool_dump(Some(1.0), listing));
    assert!(is_tool_dump(Some(0.5), listing));
    assert!(!is_tool_dump(Some(0.49), listing));
    assert!(!is_tool_dump(Some(0.0), listing));
    assert!(!is_tool_dump(None, listing));
    assert!(!is_tool_dump(Some(1.0), decision));
    assert!(!is_tool_dump(
        Some(1.0),
        "Both fixes are merged in PR #62 and the backend tests are green."
    ));
    assert!(!is_tool_dump(
        Some(1.0),
        "The scorer fix landed at commit 3f9a2c1d after the review found the timing bug."
    ));
    // A bare test log is what the run printed, not a statement about it.
    assert!(is_tool_dump(
        Some(1.0),
        "test result: ok. 1153 passed; 0 failed; 0 ignored"
    ));
    assert!(is_tool_dump(Some(1.0), "1153 passed; 0 failed"));
    // A bare word is not a receipt: only the labelled form is.
    assert!(is_tool_dump(
        Some(1.0),
        "the decision was documented upstream"
    ));
}

/// Source code and raw logs carry the receipt words as identifiers and
/// command output; only a label that opens a statement counts.
#[test]
fn source_and_logs_do_not_pass_as_a_decision_or_a_receipt() {
    for dump in [
        "struct Policy { decision: bool, retries: usize }",
        "pub struct Policy {\n    decision: bool,\n    retries: usize,\n}",
        "let verdict: Verdict = judge(&chunk);\nlet receipts: Vec<Receipt> = Vec::new();",
        "fn decide(x: &Input) -> Decision {\n    Decision { decision: x.ok }\n}",
        "commit 3f9a2c1d4e5f60718293a4b5c6d7e8f901234567\nAuthor: t <t@t>\nDate:   Wed Oct 21\n",
        "commit 3f9a2c1d4e5f60718293a4b5c6d7e8f901234567\n\n    Merge PR #62\n",
    ] {
        assert!(is_tool_dump(Some(1.0), dump), "{dump}");
    }
    for kept in [
        "Decision: we keep the 800ms budget because the p95 never crossed it.",
        "**Decision:** keep the label on the floor and filter on the share instead.",
        "- Root cause: the scorer returned a hardcoded timing value before the match ran.",
        "Checked the build. Verdict: the second option is faster and no less safe.",
        "**Receipts:** 1440x2560, 60 fps, 5400 frames, 90.000 s, 261 MB.",
    ] {
        assert!(!is_tool_dump(Some(1.0), kept), "{kept}");
    }
}

/// Prose quotes code all the time; only a declaration or source shape is
/// vetoed, not a call or a path inside a sentence.
#[test]
fn prose_that_quotes_code_is_still_a_statement() {
    for kept in [
        "Decision: call seal() after the watcher finishes reading",
        "Decision: route it through Storage::chunk_trust_and_tool_share for the whole batch.",
        "Root cause: the watcher returned early from import_session() before the flush.",
    ] {
        assert!(!is_tool_dump(Some(1.0), kept), "{kept}");
    }
    for dump in [
        "decision: Option<Decision>,",
        "Decision: handle_event(&mut self) {",
        "verdict: Verdict::Ok;",
    ] {
        assert!(is_tool_dump(Some(1.0), dump), "{dump}");
    }
}

/// A receipts label counts when the whole line is a statement and the tail
/// carries a figure; a struct field named `receipts` carries neither.
#[test]
fn a_receipts_field_in_source_is_not_a_receipt() {
    for dump in [
        "struct Policy {\n    receipts: Vec<Receipt>,\n}",
        "pub struct Log {\n    pub receipts: [u8; 32],\n}",
        "Receipts: none of it matters",
        "receipts: Vec<Receipt>",
    ] {
        assert!(is_tool_dump(Some(1.0), dump), "{dump}");
    }
    for kept in [
        "**Receipts:** 1440x2560, 60 fps, 5400 frames, 261 MB.",
        "Receipts: built at 3f9a2c1d4e5f, audio md5 identical.",
    ] {
        assert!(!is_tool_dump(Some(1.0), kept), "{kept}");
    }
}

/// A short sentence naming the change is a receipt.
#[test]
fn a_short_sentence_naming_a_pr_is_a_receipt() {
    assert!(!is_tool_dump(Some(1.0), "merged in PR #62, tests green"));
    assert!(!is_tool_dump(
        Some(1.0),
        "fixed in commit 3f9a2c1d, tests green"
    ));
    assert!(is_tool_dump(
        Some(1.0),
        "commit 3f9a2c1d4e5f60718293a4b5c6d7e8f901234567"
    ));
    assert!(is_tool_dump(Some(1.0), "    Merge PR #62"));
}

/// A prose line may end in a comma; only a field (`name: Type,` or
/// `name = value,`) is source.
#[test]
fn prose_ending_in_a_comma_is_still_a_statement() {
    for kept in [
        "Decision: keep the watcher running,\nthen seal the final chunk.",
        "Decision: keep the watcher running,",
        "- Root cause: the scorer returned early,\nso the match never ran.",
    ] {
        assert!(!is_tool_dump(Some(1.0), kept), "{kept}");
    }
    for dump in [
        "decision: Option<Decision>,",
        "    decision: bool,\n    retries: usize,",
        "pub struct Log {\n    receipts: [u8; 32],\n}",
        "Decision = Verdict::Ok,",
        "verdict: &'static str,",
    ] {
        assert!(is_tool_dump(Some(1.0), dump), "{dump}");
    }
}

/// A statement label may follow a markdown bullet or a list number.
#[test]
fn a_bulleted_or_numbered_label_is_a_statement() {
    for kept in [
        "- Fixed: the watcher now seals trailing chunks",
        "* Decision: keep the 800ms budget because p95 never crossed it.",
        "1. Verdict: the second option is faster and no less safe.",
        "2) Root cause: the scorer returned a hardcoded timing value.",
        "  - **Decision:** keep the label on the floor and filter on the share.",
        "+ Receipts: 1440x2560, 60 fps, 5400 frames.",
    ] {
        assert!(!is_tool_dump(Some(1.0), kept), "{kept}");
    }
    for dump in [
        "- decision: bool,",
        "1. verdict: Verdict::Ok;",
        "* fixed: Option<Fixed>,",
    ] {
        assert!(is_tool_dump(Some(1.0), dump), "{dump}");
    }
}

/// The first fifteen candidates are all dumps: the report ranked below
/// them, still over the floor, is what the prompt gets.
#[tokio::test]
async fn a_run_of_top_ranked_dumps_does_not_empty_the_slots() {
    let root = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let mut search = crate::search::SearchEngine::new(64);
    let mut rows: Vec<(String, String, Option<f64>, [f32; 2])> = (0..15)
        .map(|i| {
            (
                format!("dump-{i:02}"),
                format!("{i}: let value_{i} = table[{i}];"),
                Some(1.0),
                [1.0, 0.0],
            )
        })
        .collect();
    rows.push((
        "report".into(),
        "Cleanup is done. The simulator folder dropped from 284 GB to 67 GB.".into(),
        Some(0.0),
        [0.8, 0.6],
    ));
    for (seq, (id, content, share, vec)) in rows.iter().enumerate() {
        let chunk = crate::import::ConversationChunk {
            id: id.clone(),
            conversation_id: format!("conv-{id}"),
            project_name: "project".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: content.clone(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, vec).unwrap();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET min_trust=1, tool_result_share=?2 WHERE id=?1",
                    rusqlite::params![id, share],
                )?;
                Ok(())
            })
            .unwrap();
        search.insert_chunk(id.clone(), vec.to_vec());
    }
    let engine = crate::engine::Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );
    let results = search_chunks_with_vec_core(
        &engine,
        "",
        crate::search::hybrid::SearchMode::Vector,
        &[1.0, 0.0],
        15,
        0.55,
        "project",
        None,
        |_, _| Default::default(),
    )
    .await;
    let ids: Vec<_> = results
        .iter()
        .filter_map(|r| r.memory_id.as_deref())
        .collect();
    assert_eq!(ids, ["report"]);
}

/// The repository check is the slow part of a pass. The refill runs up to
/// three passes, so a conversation is checked once per prompt, not once
/// per pass.
#[tokio::test]
async fn the_refill_checks_a_conversation_once() {
    let (engine, _root) = refill_engine(|_| "conv-shared".into()).await;
    let calls = std::cell::Cell::new(0);
    let results = search_chunks_with_vec_core(
        &engine,
        "",
        crate::search::hybrid::SearchMode::Vector,
        &[1.0, 0.0],
        15,
        0.55,
        "project",
        None,
        |_, _| {
            calls.set(calls.get() + 1);
            Default::default()
        },
    )
    .await;
    assert_eq!(calls.get(), 1, "one conversation, one check");
    assert_eq!(results.len(), 3, "the refill still reaches the reports");
}

/// Every pass meets new conversations, and each check costs its full
/// 250 ms. The refill stops when too little of the budget is left, so
/// three passes cannot spend the 800 ms a prompt has.
#[tokio::test]
async fn the_refill_stops_when_the_checks_have_spent_the_budget() {
    let (engine, _root) = refill_engine(|i| format!("conv-dump-{i}")).await;
    let calls = std::cell::Cell::new(0);
    let began = std::time::Instant::now();
    let _ = search_chunks_with_vec_core(
        &engine,
        "",
        crate::search::hybrid::SearchMode::Vector,
        &[1.0, 0.0],
        15,
        0.55,
        "project",
        None,
        |_, _| {
            calls.set(calls.get() + 1);
            std::thread::sleep(std::time::Duration::from_millis(250));
            Default::default()
        },
    )
    .await;
    assert!(calls.get() <= 2, "{} checks", calls.get());
    assert!(
        began.elapsed() < std::time::Duration::from_millis(650),
        "{:?}",
        began.elapsed()
    );
}

/// Checks of 190, 190 and 250 ms add up to 630 ms: a pass must not start
/// unless the budget left covers the worst check, so the whole search
/// stays inside 600 ms.
#[tokio::test]
async fn checks_of_190_190_and_250_ms_stay_inside_the_budget() {
    let (engine, _root) = refill_engine(|i| format!("conv-dump-{i}")).await;
    let delays = [190u64, 190, 250];
    let calls = std::cell::Cell::new(0usize);
    let began = std::time::Instant::now();
    let _ = search_chunks_with_vec_core(
        &engine,
        "",
        crate::search::hybrid::SearchMode::Vector,
        &[1.0, 0.0],
        15,
        0.55,
        "project",
        None,
        |_, within| {
            let at = calls.get();
            calls.set(at + 1);
            assert!(within <= std::time::Duration::from_millis(250));
            std::thread::sleep(std::time::Duration::from_millis(delays[at.min(2)]));
            Default::default()
        },
    )
    .await;
    assert!(
        began.elapsed() <= std::time::Duration::from_millis(600),
        "{:?} over {} checks",
        began.elapsed(),
        calls.get()
    );
}

/// Four external-floor chunks match the query equally. The one that only
/// prints a tool's output is not handed back; the report written after a
/// tool ran, the dump that states a decision, and the legacy chunk with no
/// recorded share all are.
#[tokio::test]
async fn a_tool_dump_is_not_injected_and_a_decision_or_report_is() {
    let root = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let mut search = crate::search::SearchEngine::new(8);
    let rows: [(&str, &str, Option<f64>); 4] = [
        (
            "dump",
            "12:    dot: $('[data-part=\"dot\"]'),\n13:    test: $('[data-mode=\"test\"]'),\n14:    steps: $$(\"[data-step]\"),",
            Some(1.0),
        ),
        (
            "decision",
            "Decision: the hook keeps the trust label and filters on the share.",
            Some(1.0),
        ),
        (
            "report",
            "Cleanup is done. The simulator folder dropped from 284 GB to 67 GB.",
            Some(0.0),
        ),
        ("legacy", "an older chunk imported before spans existed", None),
    ];
    for (seq, (id, content, share)) in rows.iter().enumerate() {
        let chunk = crate::import::ConversationChunk {
            id: (*id).into(),
            conversation_id: format!("conv-{id}"),
            project_name: "project".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: (*content).into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET min_trust=1, tool_result_share=?2 WHERE id=?1",
                    rusqlite::params![id, share],
                )?;
                Ok(())
            })
            .unwrap();
        search.insert_chunk((*id).to_string(), vec![1.0, 0.0]);
    }
    let engine = crate::engine::Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );
    let results = search_chunks_with_vec_core(
        &engine,
        "",
        crate::search::hybrid::SearchMode::Vector,
        &[1.0, 0.0],
        5,
        0.0,
        "project",
        None,
        |_, _| Default::default(),
    )
    .await;
    let mut ids: Vec<_> = results
        .iter()
        .filter_map(|r| r.memory_id.as_deref())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, ["decision", "legacy", "report"]);
    // The trust label is unchanged: the floor still reads external.
    assert!(results
        .iter()
        .all(|r| r.min_trust == crate::provenance::TrustTier::External));
}

#[tokio::test]
async fn keyword_only_hits_fill_empty_slots_and_never_displace_vector_hits() {
    use crate::search::hybrid::SearchMode;
    // Three vector hits leave two slots: both keyword-only hits are kept.
    let (engine, _root) = fill_engine(3).await;
    let ids = hybrid_ids(&engine, SearchMode::Hybrid).await;
    let kw = |ids: &[(String, f32)]| ids.iter().filter(|(i, _)| i.starts_with("kw-")).count();
    assert_eq!(ids.len(), 5, "{ids:?}");
    assert_eq!(kw(&ids), 2, "{ids:?}");

    // Five vector hits fill every slot: no keyword-only hit is added.
    let (engine, _root) = fill_engine(5).await;
    let ids = hybrid_ids(&engine, SearchMode::Hybrid).await;
    assert_eq!(ids.len(), 5, "{ids:?}");
    assert_eq!(kw(&ids), 0, "{ids:?}");

    // Four vector hits leave one slot: the better keyword hit is kept.
    let (engine, _root) = fill_engine(4).await;
    let ids = hybrid_ids(&engine, SearchMode::Hybrid).await;
    assert_eq!(ids.len(), 5, "{ids:?}");
    assert_eq!(kw(&ids), 1, "{ids:?}");

    // fts mode asked for keyword hits only: all of them stay.
    let ids = hybrid_ids(&engine, SearchMode::Fts).await;
    assert_eq!(kw(&ids), 2, "{ids:?}");
    assert_eq!(ids.len(), 2, "{ids:?}");
}

#[tokio::test]
async fn hybrid_prompt_search_surfaces_a_keyword_hit_below_the_cosine_floor() {
    use crate::search::hybrid::SearchMode;
    let (engine, _root) = hybrid_engine().await;

    let hybrid = hybrid_ids(&engine, SearchMode::Hybrid).await;
    let names: Vec<&str> = hybrid.iter().map(|(id, _)| id.as_str()).collect();
    // The keyword hit reaches the page; every filter still removes its own victim.
    assert!(names.contains(&"sem-high"), "{names:?}");
    assert!(names.contains(&"kw-ok"), "{names:?}");
    for filtered in [
        "kw-other-project",
        "kw-self",
        "kw-old",
        "kw-csr",
        "kw-unrelated",
    ] {
        assert!(!names.contains(&filtered), "{filtered} leaked: {names:?}");
    }
    // The vector hit keeps its cosine; the keyword-only hit carries the
    // MCP path's decayed keyword score.
    let score = |id: &str| hybrid.iter().find(|(i, _)| i == id).unwrap().1;
    assert!((score("sem-high") - 1.0).abs() < 1e-4, "{hybrid:?}");
    let keyword = score("kw-ok");
    assert!(keyword > 0.40 && keyword <= 0.45, "{keyword}");

    // Vector mode is the old behaviour; fts mode is keyword only.
    let vector = hybrid_ids(&engine, SearchMode::Vector).await;
    assert_eq!(vector.len(), 1);
    assert_eq!(vector[0].0, "sem-high");
    let fts = hybrid_ids(&engine, SearchMode::Fts).await;
    assert_eq!(fts.len(), 1);
    assert_eq!(fts[0].0, "kw-ok");
}

/// The detector reads the whole chunk, before the 300-byte preview is cut:
/// a standing rule whose closing quote lies past the cut is still CSR text.
#[tokio::test]
async fn a_long_csr_text_chunk_is_not_injected_though_its_preview_would_not_match() {
    let root = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let mut search = crate::search::SearchEngine::new(8);
    let long_rule = format!("A standing rule from 10-12: “{}”", "x".repeat(400));
    for (id, content) in [
        ("long-rule", long_rule.as_str()),
        ("plain", "we moved the index load out of the hook"),
    ] {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: format!("conv-{id}"),
            project_name: "project".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
        storage
            .insert_chunk_provenance(
                id,
                &crate::provenance::ChunkProvenance {
                    author: crate::provenance::Speaker::User,
                    source_conv_id: chunk.conversation_id.clone(),
                    supersedes: None,
                },
            )
            .unwrap();
        search.insert_chunk(id.to_string(), vec![1.0, 0.0]);
    }
    let engine = crate::engine::Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );
    let results = search_chunks_with_vec_core(
        &engine,
        "",
        crate::search::hybrid::SearchMode::Vector,
        &[1.0, 0.0],
        5,
        0.0,
        "project",
        None,
        |_, _| Default::default(),
    )
    .await;
    let ids: Vec<_> = results
        .iter()
        .filter_map(|r| r.memory_id.as_deref())
        .collect();
    assert_eq!(ids, ["plain"], "the long rule must not be injected");
}

/// A stored handoff draft is CSR text: predictive context does not hand it back as memory.
#[tokio::test]
async fn a_labeled_handoff_draft_chunk_is_not_injected() {
    let root = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let mut search = crate::search::SearchEngine::new(8);
    let draft = crate::companion::handoff::canary_draft();
    for (id, content) in [
        ("handoff", draft.as_str()),
        ("plain", "we moved the index load out of the hook"),
    ] {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: format!("conv-{id}"),
            project_name: "project".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
        storage
            .insert_chunk_provenance(
                id,
                &crate::provenance::ChunkProvenance {
                    author: crate::provenance::Speaker::User,
                    source_conv_id: chunk.conversation_id.clone(),
                    supersedes: None,
                },
            )
            .unwrap();
        search.insert_chunk(id.to_string(), vec![1.0, 0.0]);
    }
    let engine = crate::engine::Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );
    let results = search_chunks_with_vec_core(
        &engine,
        "",
        crate::search::hybrid::SearchMode::Vector,
        &[1.0, 0.0],
        5,
        0.0,
        "project",
        None,
        |_, _| Default::default(),
    )
    .await;
    let ids: Vec<_> = results
        .iter()
        .filter_map(|r| r.memory_id.as_deref())
        .collect();
    assert_eq!(ids, ["plain"], "the handoff draft must not be injected");
}

/// A legacy chunk with no chunk_provenance row has an unknown author, not
/// a tool author: predictive context does not inject it either.
#[tokio::test]
async fn a_legacy_csr_text_chunk_without_provenance_is_not_injected() {
    let root = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let mut search = crate::search::SearchEngine::new(8);
    for (id, content) in [
        ("legacy-rule", "A standing rule from 10-12: “Keep receipts”"),
        ("plain", "we moved the index load out of the hook"),
    ] {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: format!("conv-{id}"),
            project_name: "project".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
        search.insert_chunk(id.to_string(), vec![1.0, 0.0]);
    }
    let engine = crate::engine::Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );
    let results = search_chunks_with_vec_core(
        &engine,
        "",
        crate::search::hybrid::SearchMode::Vector,
        &[1.0, 0.0],
        5,
        0.0,
        "project",
        None,
        |_, _| Default::default(),
    )
    .await;
    let ids: Vec<_> = results
        .iter()
        .filter_map(|r| r.memory_id.as_deref())
        .collect();
    assert_eq!(ids, ["plain"]);
}

/// Lineage pickup skips an episode whose text has a detector hit and falls
/// through to the next candidate.
#[tokio::test]
async fn lineage_pickup_skips_an_episode_whose_text_is_csr_text() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    for (session, request, completed, stamp) in [
        (
            "clean",
            "Fix the auth middleware regression",
            "Fixed the token check in the middleware",
            "2026-10-12T12:00:00Z",
        ),
        (
            "csr",
            "A standing rule from 10-12: “Keep receipts”",
            "Fixed the token check in the middleware",
            "2026-10-13T12:00:00Z",
        ),
    ] {
        let ep = crate::hooks::stop::Episode {
            schema: "v2".into(),
            session_id: session.into(),
            project: "proj".into(),
            timestamp: stamp.into(),
            request: request.into(),
            investigated: vec![],
            completed: completed.into(),
            next_steps: None,
            blockers: None,
            outcome: "success".into(),
            error_signatures: vec![],
            tools_used: vec![],
            files_modified: vec![],
            message_count: 10,
            duration_minutes: 5,
            todos: vec![],
            approved_plan: None,
            prev_episode_id: None,
            error_count: None,
            top_errors: vec![],
            steer_count: None,
            steers: vec![],
            instrumentation_version: None,
            correction_count: None,
            recap_evidence_version: None,
            anchor_request: None,
            completions: vec![],
            completions_untied: false,
            anchor_receipts: None,
            compact_anchor: None,
            late_errors: false,
            closing_success: false,
            closing_ended: false,
            final_failed: false,
            closing_receipt: false,
            outcome_version: None,
            latest_ask: None,
            watermark: None,
            recap_extras: Default::default(),
            anchors: vec![],
        };
        storage
            .insert_reflection(
                &format!("ep-{session}"),
                &serde_json::to_string(&ep).unwrap(),
                &crate::hooks::stop::episode_tags(&ep),
                &[0.0; 4],
            )
            .unwrap();
    }
    let engine = crate::engine::Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(crate::search::SearchEngine::new(
            8,
        ))),
        root.path().to_path_buf(),
    );
    let (picked, _) = pick_lineage_episode(&engine, &cwd, "continue please").unwrap();
    assert_eq!(picked.session_id, "clean");
}

#[tokio::test]
async fn chunk_search_never_echoes_the_asking_session_or_a_harness_block() {
    let root = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let mut search = crate::search::SearchEngine::new(8);
    let rows = [
        ("own-turn", "live-session", "grep output from two minutes ago"),
        (
            "harness-block",
            "older-session",
            "<task-notification>\n<task-id>a0b2</task-id>\n<status>completed</status>\n</task-notification>\nRead the output file.",
        ),
        ("past-work", "older-session", "we moved the index load out of the hook"),
    ];
    for (id, conversation_id, content) in rows {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conversation_id.into(),
            project_name: "project".into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
        search.insert_chunk(chunk.id.clone(), vec![1.0, 0.0]);
    }
    let engine = crate::engine::Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        root.path().to_path_buf(),
    );

    let recalled = |session: Option<&'static str>| {
        let engine = &engine;
        async move {
            let mut ids: Vec<String> = search_chunks_with_vec_core(
                engine,
                "",
                crate::search::hybrid::SearchMode::Vector,
                &[1.0, 0.0],
                10,
                0.0,
                "project",
                session,
                |_, _| Default::default(),
            )
            .await
            .into_iter()
            .filter_map(|result| result.memory_id)
            .collect();
            ids.sort();
            ids
        }
    };

    assert_eq!(recalled(Some("live-session")).await, ["past-work"]);
    // Another session asking still sees that turn; the harness block never returns.
    assert_eq!(
        recalled(Some("someone-else")).await,
        ["own-turn", "past-work"]
    );
    assert_eq!(recalled(None).await, ["own-turn", "past-work"]);
}

#[tokio::test]
async fn a_conversation_the_repository_test_accepts_is_recalled_under_another_label() {
    let (ids, seen) =
        recall_across_labels("repo", &IMPORT_ROWS, &[("sub-session", "repo-sub")]).await;
    assert_eq!(ids, ["root-chunk", "sub-chunk"]);
    // Only the candidates whose label differs are put to the test, with
    // every path recorded for them; the exact match never is.
    let seen = seen.expect("same_repo is consulted when a label differs");
    let mut asked: Vec<&String> = seen.keys().collect();
    asked.sort();
    assert_eq!(asked, ["other-session", "sub-session"]);
    assert_eq!(
        seen["sub-session"],
        ["/cc/projects/-u-projects-repo-sub/sub-session.jsonl"]
    );
}

#[tokio::test]
async fn a_widened_conversation_admits_only_the_label_of_its_folder() {
    // import_state is written after the chunks, so a chunk overwritten by a
    // same-stem file from elsewhere can sit under a conversation whose
    // recorded path passes. Its label is that other file's, and it stays out.
    let (ids, _) =
        recall_across_labels("repo", &IMPORT_ROWS, &[("sub-session", "something-else")]).await;
    assert_eq!(ids, ["root-chunk"]);
}
