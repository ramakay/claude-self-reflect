use super::*;

#[tokio::test]
async fn an_import_leaves_a_large_mirror_backlog_to_the_daemon_pass() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-big.jsonl");
    write_session(&src, &fake_github());
    let mut body = fs::read_to_string(&src).unwrap();
    for _ in 0..9 {
        body.push_str(&ignorable_line(1 << 20));
    }
    fs::write(&src, body).unwrap();

    assert!(engine.import_file(&src, "proj").await.unwrap() > 0);
    let rel = "-tmp-proj/sess-big.jsonl";
    assert!(engine.storage().mirror_row(rel).unwrap().is_none());

    let stats = engine.mirror().sync_pass(None);
    assert_eq!((stats.synced, stats.errors), (1, 0));
    assert_eq!(
        engine
            .storage()
            .mirror_row(rel)
            .unwrap()
            .unwrap()
            .source_bytes,
        fs::metadata(&src).unwrap().len()
    );
}

#[tokio::test]
async fn a_busy_transcript_lock_does_not_stall_the_import() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-busy.jsonl");
    write_session(&src, &fake_stripe());
    engine.import_file(&src, "proj").await.unwrap();

    append(&src, line(9, "a later message").as_bytes());
    let lock = File::create(append_name(&rig.zst("sess-busy.jsonl"), ".lock")).unwrap();
    lock.lock_exclusive().unwrap();
    let started = std::time::Instant::now();
    engine.import_file(&src, "proj").await.unwrap();
    let took = started.elapsed();
    FileExt::unlock(&lock).unwrap();
    assert!(took < Duration::from_millis(1500), "import waited {took:?}");
    // The next pass catches the mirror up.
    let stats = engine.mirror().sync_pass(None);
    assert_eq!((stats.synced, stats.errors), (1, 0));
}

#[tokio::test]
async fn legacy_rows_stay_byte_identical_and_new_messages_land_scrubbed() {
    legacy_rows_survive_and_new_messages_land(Via::Engine).await;
}

#[tokio::test]
async fn legacy_rows_stay_byte_identical_through_the_watcher_path() {
    legacy_rows_survive_and_new_messages_land(Via::Watcher).await;
}

#[tokio::test]
async fn a_legacy_conversation_without_receipts_is_placed_by_its_text() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-bare.jsonl");
    let conv = legacy_conversation(&engine, &src).await;
    engine
        .storage()
        .with_connection(|conn| {
            conn.execute("DELETE FROM chunk_spans", [])?;
            Ok(())
        })
        .unwrap();
    let before = stored_rows(&engine, &conv);
    let old_tail = chunks_like(&engine, "end 3");

    append(
        &src,
        session_message(10, "freshly appended alpha").as_bytes(),
    );
    filetime_touch(&src);
    import_via(&engine, &rig, &src, Via::Engine).await;

    let after = stored_rows(&engine, &conv);
    for row in &before {
        assert!(after.contains(row), "a stored row changed or went: {row}");
    }
    assert_eq!(chunks_like(&engine, "freshly appended alpha"), 1);
    // The messages the stored rows hold are not chunked a second time.
    assert_eq!(chunks_like(&engine, "end 3"), old_tail);
}

#[tokio::test]
async fn a_legacy_conversation_that_cannot_be_placed_is_left_alone_until_it_can() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-lost.jsonl");
    let conv = legacy_conversation(&engine, &src).await;
    let original: (String, String) = engine
        .storage()
        .with_connection(|conn| {
            conn.execute("DELETE FROM chunk_spans", [])?;
            Ok(conn.query_row(
                "SELECT id, content FROM chunks WHERE conversation_id = ?1
                     ORDER BY seq DESC, rowid DESC LIMIT 1",
                [&conv],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    let set_tail = |content: &str| {
        engine
            .storage()
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET content = ?2 WHERE id = ?1",
                    [&original.0, content],
                )?;
                Ok(())
            })
            .unwrap();
    };
    set_tail("text that is nowhere in the transcript");
    let before = stored_rows(&engine, &conv);

    append(
        &src,
        session_message(10, "freshly appended alpha").as_bytes(),
    );
    filetime_touch(&src);
    import_via(&engine, &rig, &src, Via::Engine).await;
    import_via(&engine, &rig, &src, Via::Watcher).await;

    assert_eq!(stored_rows(&engine, &conv), before);
    assert_eq!(chunks_like(&engine, "freshly appended alpha"), 0);
    assert!(engine.storage().legacy_frontier(&conv).unwrap().is_none());

    // Once the place can be found, the pending message lands.
    set_tail(&original.1);
    append(
        &src,
        session_message(11, "freshly appended bravo").as_bytes(),
    );
    filetime_touch(&src);
    import_via(&engine, &rig, &src, Via::Engine).await;
    assert_eq!(chunks_like(&engine, "freshly appended alpha"), 1);
    assert_eq!(chunks_like(&engine, "freshly appended bravo"), 1);
}

/// `status` reports how many legacy conversations import placed and how many
/// it left alone because it could not: one of each here, and the waiting one
/// moves to placed once its place can be found.
#[tokio::test]
async fn status_counts_one_placed_and_one_waiting_legacy_conversation() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let counts = || crate::status::LegacyFrontierStatus::read(engine.storage());
    assert_eq!(counts(), Default::default());

    // One that can be placed: its stored rows carry transcript receipts.
    let placed = rig.source("sess-placed.jsonl");
    legacy_conversation(&engine, &placed).await;
    // One that cannot: no receipts, and its newest chunk is text the transcript lacks.
    let lost = rig.source("sess-lost-count.jsonl");
    let lost_conv = legacy_conversation(&engine, &lost).await;
    let (tail_id, tail_text): (String, String) = engine
        .storage()
        .with_connection(|conn| {
            conn.execute(
                "DELETE FROM chunk_spans WHERE chunk_id IN
                     (SELECT id FROM chunks WHERE conversation_id = ?1)",
                [&lost_conv],
            )?;
            Ok(conn.query_row(
                "SELECT id, content FROM chunks WHERE conversation_id = ?1
                     ORDER BY seq DESC, rowid DESC LIMIT 1",
                [&lost_conv],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .unwrap();
    let set_tail = |content: &str| {
        engine
            .storage()
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunks SET content = ?2 WHERE id = ?1",
                    [&tail_id, content],
                )?;
                Ok(())
            })
            .unwrap();
    };
    set_tail("text that is nowhere in the transcript");

    // Nothing has tried to place either yet: nothing is waiting.
    assert_eq!(counts(), Default::default());
    for src in [&placed, &lost] {
        append(
            src,
            session_message(10, "freshly appended alpha").as_bytes(),
        );
        filetime_touch(src);
        import_via(&engine, &rig, src, Via::Engine).await;
    }
    assert_eq!(
        (counts().placed, counts().waiting),
        (1, 1),
        "one placed, one left alone"
    );

    // The watcher retrying the same lost conversation is still one waiting.
    import_via(&engine, &rig, &lost, Via::Watcher).await;
    assert_eq!((counts().placed, counts().waiting), (1, 1));

    // Its place can be found again: it is placed, and no longer waiting.
    set_tail(&tail_text);
    append(
        &lost,
        session_message(11, "freshly appended bravo").as_bytes(),
    );
    filetime_touch(&lost);
    import_via(&engine, &rig, &lost, Via::Engine).await;
    assert_eq!((counts().placed, counts().waiting), (2, 0));
}

/// The watcher looks, finds no frontier; the engine then places the
/// conversation, writes its frontier and stamps it, all on another handle
/// to the same database; the watcher then reads the stored rows. It must get
/// the frontier, not "fresh": a fresh import re-chunks from message zero over
/// rows that must never be rewritten.
#[tokio::test]
async fn a_frontier_recorded_between_the_look_and_the_read_is_not_fresh() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-race.jsonl");
    let conv = legacy_conversation(&engine, &src).await;
    let before = stored_rows(&engine, &conv);
    let other = other_importer(&rig);

    let mut engine_saw = None;
    let watcher = resolve_with(engine.storage(), &src, &conv, &mut |gap| {
        if gap == Gap::AfterNoFrontier {
            engine_saw = Some(resolve(&other, &src, &conv).unwrap());
            finish_import(&other, &src);
        }
    })
    .unwrap();

    let Some(Resume::At(point)) = engine_saw else {
        panic!("the engine should have placed it: {engine_saw:?}");
    };
    assert_eq!(point.after_seq, 3);
    assert_eq!(
        watcher,
        Resume::At(point),
        "the watcher must not read fresh"
    );
    assert_eq!(frontier_rows(&engine), 1);
    assert_eq!(stored_rows(&engine, &conv), before);
}

/// The same, one step later: the watcher has already computed its own
/// candidate (a slow file parse) when the other importer writes a frontier.
/// The one stored first stands, and is what the watcher returns.
#[tokio::test]
async fn the_frontier_stored_first_stands_when_a_slower_importer_computed_its_own() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-race-late.jsonl");
    let conv = legacy_conversation(&engine, &src).await;
    let other = other_importer(&rig);
    let first_chunk = match engine.storage().legacy_state(&src, &conv).unwrap() {
        LegacyState::Legacy(rows) => rows.first_chunk,
        state => panic!("expected a legacy conversation, got {state:?}"),
    };

    // The other importer recorded a different (earlier) place first.
    let watcher = resolve_with(engine.storage(), &src, &conv, &mut |gap| {
        if gap == Gap::BeforeCommit {
            other.record_legacy_frontier(&conv, 1, first_chunk).unwrap();
        }
    })
    .unwrap();

    match watcher {
        Resume::At(point) => assert_eq!(
            (point.after_seq, point.first_chunk),
            (1, first_chunk),
            "the watcher used its own candidate over the stored one"
        ),
        other => panic!("expected the stored frontier, got {other:?}"),
    }
    assert_eq!(
        engine.storage().legacy_frontier(&conv).unwrap(),
        Some((1, first_chunk))
    );
    assert_eq!(frontier_rows(&engine), 1);
}

/// While the watcher computed its candidate the conversation was stamped by
/// a scrubbing import: it is not legacy any more, and nothing is recorded.
#[tokio::test]
async fn a_conversation_stamped_while_a_candidate_was_computed_is_not_given_a_frontier() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-race-stamped.jsonl");
    let conv = legacy_conversation(&engine, &src).await;
    let other = other_importer(&rig);
    let watcher = resolve_with(engine.storage(), &src, &conv, &mut |gap| {
        if gap == Gap::BeforeCommit {
            finish_import(&other, &src);
        }
    })
    .unwrap();
    assert_eq!(watcher, Resume::Fresh);
    assert_eq!(frontier_rows(&engine), 0);
}

/// A candidate computed from rows that are not the stored rows now is not
/// written.
#[tokio::test]
async fn a_candidate_computed_from_other_rows_is_not_written() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-race-rows.jsonl");
    let conv = legacy_conversation(&engine, &src).await;
    let LegacyState::Legacy(mut stale) = engine.storage().legacy_state(&src, &conv).unwrap() else {
        panic!("expected a legacy conversation");
    };
    stale.first_chunk += 1;
    assert_eq!(
        engine
            .storage()
            .commit_legacy_frontier(&src, &conv, &stale, 3)
            .unwrap(),
        FrontierCommit::RowsChanged
    );
    assert_eq!(frontier_rows(&engine), 0);
}

/// The newest stored chunk's text also occurs earlier in the transcript.
/// The place is the one the earlier chunks lead up to, not the first match.
#[tokio::test]
async fn repeated_text_is_placed_by_the_chunks_before_it() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-repeat.jsonl");
    // Each message is a chunk of its own: [P] [Q] [P].
    let (p, q) = (repeated("alphaword"), repeated("bravoword"));
    let conv = legacy_conversation_of(&engine, &src, &[p.clone(), q.clone(), p.clone()]).await;
    drop_receipts(&engine);
    let before = stored_rows(&engine, &conv);
    assert_eq!(before.len(), 3);

    append(
        &src,
        session_message(10, "freshly appended alpha").as_bytes(),
    );
    filetime_touch(&src);
    import_via(&engine, &rig, &src, Via::Watcher).await;

    assert_eq!(
        engine.storage().legacy_frontier(&conv).unwrap(),
        Some((2, 3)),
        "placed at the last message, not at the first copy of its text"
    );
    let after = stored_rows(&engine, &conv);
    for row in &before {
        assert!(after.contains(row), "a stored row changed or went: {row}");
    }
    assert_eq!(
        after.len(),
        4,
        "no stored message was chunked a second time"
    );
    assert_eq!(chunks_like(&engine, "freshly appended alpha"), 1);
}

#[tokio::test]
async fn a_receipt_at_a_later_copy_does_not_move_the_frontier_in_the_engine_path() {
    a_receipt_at_a_later_copy_does_not_move_the_frontier(Via::Engine).await;
}

#[tokio::test]
async fn a_receipt_at_a_later_copy_does_not_move_the_frontier_in_the_watcher_path() {
    a_receipt_at_a_later_copy_does_not_move_the_frontier(Via::Watcher).await;
}

/// A receipt that names one of several equally likely copies does not break
/// the tie: with the stored rows unable to say, the conversation waits.
#[tokio::test]
async fn a_receipt_does_not_break_a_tie_the_stored_rows_cannot() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-tie.jsonl");
    let p = repeated("alphaword");
    let conv = legacy_conversation_of(&engine, &src, &[p.clone(), p.clone()]).await;
    // The head of the conversation is gone; the surviving chunk keeps its
    // receipt, which says message 1.
    engine
        .storage()
        .with_connection(|conn| {
            conn.execute_batch("PRAGMA foreign_keys=OFF")?;
            conn.execute(
                "DELETE FROM chunks WHERE conversation_id = ?1 AND seq = 0",
                [&conv],
            )?;
            conn.execute_batch("PRAGMA foreign_keys=ON")?;
            Ok(())
        })
        .unwrap();
    let before = stored_rows(&engine, &conv);
    assert_eq!(before.len(), 1);

    append(&src, session_message(2, &p).as_bytes());
    append(
        &src,
        session_message(3, "freshly appended alpha").as_bytes(),
    );
    filetime_touch(&src);
    import_via(&engine, &rig, &src, Via::Watcher).await;

    assert_eq!(stored_rows(&engine, &conv), before);
    assert_eq!(engine.storage().legacy_frontier(&conv).unwrap(), None);
    assert_eq!(chunks_like(&engine, "freshly appended alpha"), 0);
    assert_eq!(
        crate::status::LegacyFrontierStatus::read(engine.storage()).waiting,
        1
    );
}

/// A run of identical messages is told apart by walking back through every
/// stored chunk to the head of the transcript.
#[tokio::test]
async fn a_run_of_identical_chunks_is_placed_by_the_whole_stored_chain() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-run.jsonl");
    let p = repeated("alphaword");
    let conv = legacy_conversation_of(&engine, &src, &[p.clone(), p.clone()]).await;
    drop_receipts(&engine);
    let before = stored_rows(&engine, &conv);

    append(&src, session_message(2, &p).as_bytes());
    filetime_touch(&src);
    import_via(&engine, &rig, &src, Via::Engine).await;

    assert_eq!(
        engine.storage().legacy_frontier(&conv).unwrap(),
        Some((1, 2))
    );
    let after = stored_rows(&engine, &conv);
    for row in &before {
        assert!(after.contains(row), "a stored row changed or went: {row}");
    }
    assert_eq!(after.len(), 3, "the new copy lands once");
}

/// When the stored rows cannot tell the places apart, nothing is guessed:
/// the conversation waits, no row is written and no frontier recorded.
#[tokio::test]
async fn text_the_stored_rows_cannot_place_is_left_unplaced() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-ambiguous.jsonl");
    let p = repeated("alphaword");
    let conv = legacy_conversation_of(&engine, &src, &[p.clone(), p.clone()]).await;
    drop_receipts(&engine);
    // The head of the conversation is gone from the stored rows.
    engine
        .storage()
        .with_connection(|conn| {
            conn.execute_batch("PRAGMA foreign_keys=OFF")?;
            conn.execute(
                "DELETE FROM chunks WHERE conversation_id = ?1 AND seq = 0",
                [&conv],
            )?;
            conn.execute_batch("PRAGMA foreign_keys=ON")?;
            Ok(())
        })
        .unwrap();
    let before = stored_rows(&engine, &conv);

    append(&src, session_message(2, &p).as_bytes());
    append(
        &src,
        session_message(3, "freshly appended alpha").as_bytes(),
    );
    filetime_touch(&src);
    import_via(&engine, &rig, &src, Via::Watcher).await;

    assert_eq!(stored_rows(&engine, &conv), before);
    assert_eq!(engine.storage().legacy_frontier(&conv).unwrap(), None);
    assert_eq!(chunks_like(&engine, "freshly appended alpha"), 0);
    assert_eq!(
        crate::status::LegacyFrontierStatus::read(engine.storage()).waiting,
        1
    );
}

#[tokio::test]
async fn the_scrub_switch_does_not_unprotect_legacy_rows_in_the_engine_path() {
    legacy_rows_survive_the_scrub_switch(Via::Engine).await;
}

#[tokio::test]
async fn the_scrub_switch_does_not_unprotect_legacy_rows_in_the_watcher_path() {
    legacy_rows_survive_the_scrub_switch(Via::Watcher).await;
}

/// A malformed line import skipped must not leave the conversation unplaced:
/// replay skips what import skips and numbers messages the same way.
#[tokio::test]
async fn a_malformed_old_line_does_not_leave_a_receiptless_conversation_unplaced() {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-malformed.jsonl");
    let mut body: Vec<u8> = Vec::new();
    let _off = crate::redact::DisabledForTest::set(true);
    for n in 0..4 {
        body.extend(session_message(n, &format!("old message {n} {}", repeated("zulu"))).bytes());
        match n {
            0 => body.extend(b"{this line is not json\n"),
            1 => body.extend(b"\xff\xfe not utf8 \xc3\x28\n"),
            _ => {}
        }
    }
    fs::write(&src, body).unwrap();
    assert!(engine.import_file(&src, "proj").await.unwrap() >= 4);
    let conv = src.file_stem().unwrap().to_string_lossy().to_string();
    drop(_off);
    drop_receipts(&engine);
    let before = stored_rows(&engine, &conv);

    append(
        &src,
        session_message(10, "freshly appended alpha").as_bytes(),
    );
    filetime_touch(&src);
    import_via(&engine, &rig, &src, Via::Watcher).await;

    assert_eq!(
        engine
            .storage()
            .legacy_frontier(&conv)
            .unwrap()
            .map(|f| f.0),
        Some(3),
        "four messages, numbered the way import numbers them"
    );
    let after = stored_rows(&engine, &conv);
    for row in &before {
        assert!(after.contains(row), "a stored row changed or went: {row}");
    }
    assert_eq!(chunks_like(&engine, "freshly appended alpha"), 1);
    assert_eq!(chunks_like(&engine, "old message 3"), 1);
}

#[test]
fn archived_subagent_transcripts_are_found_in_the_mirror_by_id() {
    let rig = Rig::new();
    let nested = transcripts_dir(&rig.data_dir).join("-tmp-proj/sess-1/subagents");
    fs::create_dir_all(&nested).unwrap();
    let mut body = String::new();
    write_session(&rig.source("scratch.jsonl"), "nothing");
    body.push_str(&fs::read_to_string(rig.source("scratch.jsonl")).unwrap());
    fs::write(
        nested.join("agent-a1b2.jsonl.zst"),
        compress_frame(body.as_bytes()).unwrap(),
    )
    .unwrap();

    let pointer = crate::mcp::tools::get_full_conversation(
        &rig.projects,
        Some(&rig.data_dir),
        "agent-a1b2",
        None,
    );
    assert!(pointer.contains("<mirror_path>"), "got: {pointer}");
    assert!(pointer.contains("-tmp-proj"), "got: {pointer}");
    let request = crate::transcript::TranscriptRequest {
        session: "agent-a1b2".to_string(),
        view: crate::transcript::ViewKind::Stats,
        ..Default::default()
    };
    let stats = crate::transcript::run_with_mirror(&rig.projects, Some(&rig.data_dir), &request);
    assert!(stats.contains("<turns total=\"2\""), "got: {stats}");

    // The same id under a second project is still ambiguous.
    let other = transcripts_dir(&rig.data_dir).join("-tmp-other/sess-2/subagents");
    fs::create_dir_all(&other).unwrap();
    fs::write(
        other.join("agent-a1b2.jsonl.zst"),
        compress_frame(body.as_bytes()).unwrap(),
    )
    .unwrap();
    let pointer = crate::mcp::tools::get_full_conversation(
        &rig.projects,
        Some(&rig.data_dir),
        "agent-a1b2",
        None,
    );
    assert!(pointer.contains("ambiguous"), "got: {pointer}");
}

/// Put `text` as the one prompt of a mirror copy at `<mirror>/<rel>.jsonl.zst`.
fn mirror_copy(rig: &Rig, rel: &str, text: &str) {
    let path = transcripts_dir(&rig.data_dir).join(format!("{rel}.jsonl.zst"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, compress_frame(line(1, text).as_bytes()).unwrap()).unwrap();
}

/// Put `text` as the one prompt of a live JSONL at `<projects>/<rel>.jsonl`.
fn live_copy(rig: &Rig, rel: &str, text: &str) {
    let path = rig.projects.join(format!("{rel}.jsonl"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, line(1, text)).unwrap();
}

fn asks(rig: &Rig, session: &str) -> String {
    crate::transcript::run_with_mirror(
        &rig.projects,
        Some(&rig.data_dir),
        &crate::transcript::TranscriptRequest {
            session: session.to_string(),
            view: crate::transcript::ViewKind::YourAsks,
            ..Default::default()
        },
    )
}

#[test]
fn a_live_subagent_file_is_read_before_its_mirror_copy() {
    let rig = Rig::new();
    live_copy(&rig, "-tmp-proj/sess-1/subagents/agent-c3d4", "live words");
    mirror_copy(
        &rig,
        "-tmp-proj/sess-1/subagents/agent-c3d4",
        "mirrored words",
    );

    let live = rig
        .projects
        .join("-tmp-proj/sess-1/subagents/agent-c3d4.jsonl");
    for id in ["agent-c3d4", "c3d4", "agent-c3"] {
        match crate::transcript::resolve_session_path(&rig.projects, Some(&rig.data_dir), id, None)
        {
            crate::mcp::tools::ConversationLookup::Found(path, project) => {
                assert_eq!(path, live, "id {id}");
                assert_eq!(project, "-tmp-proj", "id {id}");
            }
            _ => panic!("id {id} did not resolve to the live file"),
        }
        let out = asks(&rig, id);
        assert!(out.contains("live words"), "id {id}, got: {out}");
        assert!(!out.contains("mirrored words"), "id {id}, got: {out}");
        assert!(!out.contains("is gone"), "id {id}, got: {out}");
        assert!(!out.contains("transcript mirror"), "id {id}, got: {out}");
    }
    let pointer = crate::mcp::tools::get_full_conversation(
        &rig.projects,
        Some(&rig.data_dir),
        "agent-c3d4",
        None,
    );
    assert!(!pointer.contains("no longer exists"), "got: {pointer}");
    assert!(!pointer.contains("<mirror_path>"), "got: {pointer}");

    // The same live id in a second project is still ambiguous.
    live_copy(
        &rig,
        "-tmp-other/sess-2/subagents/agent-c3d4",
        "other words",
    );
    let out = asks(&rig, "agent-c3d4");
    assert!(out.contains("ambiguous"), "got: {out}");
}

#[test]
fn a_subagent_with_no_live_file_is_read_from_the_mirror_with_the_note() {
    let rig = Rig::new();
    mirror_copy(
        &rig,
        "-tmp-proj/sess-1/subagents/agent-e5f6",
        "mirrored words",
    );
    // A live file of another run must not capture the lookup.
    live_copy(
        &rig,
        "-tmp-proj/sess-1/subagents/agent-0a0a",
        "unrelated words",
    );

    let out = asks(&rig, "agent-e5f6");
    assert!(out.contains("mirrored words"), "got: {out}");
    assert!(out.contains("the original JSONL is gone"), "got: {out}");
}

#[test]
fn a_main_thread_session_keeps_its_live_file_and_its_gone_note() {
    let rig = Rig::new();
    live_copy(&rig, "-tmp-proj/sess-live", "live words");
    mirror_copy(&rig, "-tmp-proj/sess-live", "mirrored words");
    mirror_copy(&rig, "-tmp-proj/sess-gone", "mirrored words");

    let live = asks(&rig, "sess-live");
    assert!(live.contains("live words"), "got: {live}");
    assert!(!live.contains("is gone"), "got: {live}");

    let gone = asks(&rig, "sess-gone");
    assert!(gone.contains("mirrored words"), "got: {gone}");
    assert!(gone.contains("the original JSONL is gone"), "got: {gone}");
}

#[test]
fn yourasks_reads_a_zstd_mirror_and_marks_the_source() {
    let rig = Rig::new();
    let dir = transcripts_dir(&rig.data_dir).join("-tmp-proj");
    fs::create_dir_all(&dir).unwrap();
    let handoff = "Lost and found: \u{201c}Retry the upload\u{201d} went quiet after 09:05. Revisit it as a side task: finish or close it in a few steps without derailing the current work, then tell me in one line.";
    let body = [
        line(1, "make the made-up widget faster"),
        line(
            2,
            "<pasted_content id=\"z9\">pasted made-up words</pasted_content>",
        ),
        line(3, handoff),
    ]
    .concat();
    fs::write(
        dir.join("sess-asks.jsonl.zst"),
        compress_frame(body.as_bytes()).unwrap(),
    )
    .unwrap();
    let out = crate::transcript::run_with_mirror(
        &rig.projects,
        Some(&rig.data_dir),
        &crate::transcript::TranscriptRequest {
            session: "sess-asks".to_string(),
            view: crate::transcript::ViewKind::YourAsks,
            ..Default::default()
        },
    );
    assert!(out.contains("<transcript_yourasks>"), "got: {out}");
    assert!(out.contains("source: transcript mirror"), "got: {out}");
    assert!(out.contains("make the made-up widget faster"), "got: {out}");
    assert!(out.contains("pasted made-up words"), "got: {out}");
    assert!(!out.contains("Lost and found"), "got: {out}");
    assert!(out.contains("hidden: csr_sent=1"), "got: {out}");
}

#[test]
fn grep_reads_tool_calls_and_full_tool_results_from_a_zstd_mirror() {
    let rig = Rig::new();
    let dir = transcripts_dir(&rig.data_dir).join("-tmp-proj");
    fs::create_dir_all(&dir).unwrap();
    let pad = "made-up filler words ".repeat(40);
    let body = [
        serde_json::json!({"type":"assistant","uuid":"a-1","message":{"role":"assistant","content":[
            {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"make-up-tool --flag widget"}}]}})
        .to_string()
            + "\n",
        serde_json::json!({"type":"user","uuid":"u-2","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"t1","content": format!("{pad}failure code ZX-4471 in the widget")}]}})
        .to_string()
            + "\n",
    ]
    .concat();
    fs::write(
        dir.join("sess-grep.jsonl.zst"),
        compress_frame(body.as_bytes()).unwrap(),
    )
    .unwrap();
    let ask = |pattern: &str| {
        crate::transcript::run_with_mirror(
            &rig.projects,
            Some(&rig.data_dir),
            &crate::transcript::TranscriptRequest {
                session: "sess-grep".to_string(),
                view: crate::transcript::ViewKind::Grep,
                grep: Some(pattern.to_string()),
                ..Default::default()
            },
        )
    };
    let result = ask("ZX-4471");
    assert!(
        result.contains("source: transcript mirror"),
        "got: {result}"
    );
    assert!(
        result.contains("[turn 2] user tool_result: "),
        "got: {result}"
    );
    assert!(result.contains("failure code ZX-4471"), "got: {result}");
    let call = ask("make-up-tool");
    assert!(
        call.contains("[turn 1] assistant tool_use Bash: "),
        "got: {call}"
    );
}

#[test]
fn a_damaged_mirror_frame_is_reported_as_incomplete_not_as_a_clean_read() {
    let rig = Rig::new();
    let dir = transcripts_dir(&rig.data_dir).join("-tmp-proj");
    fs::create_dir_all(&dir).unwrap();
    let good = [line(1, "first prompt"), line(2, "second prompt")].concat();
    let torn_source = (3..400)
        .map(|n| line(n, "a later prompt in a torn frame"))
        .collect::<String>();
    let torn = compress_frame(torn_source.as_bytes()).unwrap();
    let mut bytes = compress_frame(good.as_bytes()).unwrap();
    bytes.extend_from_slice(&torn[..torn.len() / 2]);
    fs::write(dir.join("sess-torn.jsonl.zst"), bytes).unwrap();

    let ask = |view, json| {
        crate::transcript::run_with_mirror(
            &rig.projects,
            Some(&rig.data_dir),
            &crate::transcript::TranscriptRequest {
                session: "sess-torn".to_string(),
                view,
                json,
                ..Default::default()
            },
        )
    };
    let stats = ask(crate::transcript::ViewKind::Stats, false);
    assert!(stats.contains("INCOMPLETE"), "got: {stats}");
    let prompts = ask(crate::transcript::ViewKind::Prompts, false);
    assert!(prompts.contains("INCOMPLETE"), "got: {prompts}");
    assert!(prompts.contains("first prompt"));
    let json = ask(crate::transcript::ViewKind::Stats, true);
    assert!(json.contains("\"incomplete\":true"), "got: {json}");
}

/// `cargo test --release --lib mirror::tests::scrub_throughput -- --ignored --nocapture --test-threads=1`
///
/// Runs the mirror's line transform (the redact + private-span scrub) over
/// a generated ~100 MB JSONL corpus, then the whole `sync_file` path over
/// the same bytes, so the second number includes compression, hashing and
/// fsync. The third run is pessimistic: 1 line in 10 trips the prefilter
/// without holding a secret.
#[test]
#[ignore = "throughput measurement; run in release mode with --nocapture"]
fn scrub_throughput() {
    let github = fake_github();
    for (label, noisy_every) in [("typical", 0), ("noisy 1-in-10", 10)] {
        let corpus = bench_corpus(100 << 20, noisy_every);
        let mib = corpus.len() as f64 / (1 << 20) as f64;
        let lines = corpus.iter().filter(|byte| **byte == b'\n').count();

        let started = std::time::Instant::now();
        let scrubbed = scrub_lines(&corpus);
        let secs = started.elapsed().as_secs_f64();
        assert!(!String::from_utf8_lossy(&scrubbed).contains(&github));
        println!(
            "[{label}] scrub only: {mib:.1} MiB, {lines} lines in {secs:.2}s = {:.0} MiB/s",
            mib / secs
        );

        let rig = Rig::new();
        let src = rig.source("big.jsonl");
        fs::write(&src, &corpus).unwrap();
        let started = std::time::Instant::now();
        let outcome = rig.mirror().sync_file(&src);
        let secs = started.elapsed().as_secs_f64();
        assert!(matches!(outcome, SyncOutcome::Appended { .. }));
        let row = rig
            .storage
            .mirror_row("-tmp-proj/big.jsonl")
            .unwrap()
            .unwrap();
        println!(
            "[{label}] full sync:  {mib:.1} MiB in {secs:.2}s = {:.0} MiB/s (zstd {:.1} MiB, {} frames)",
            mib / secs,
            row.zst_bytes as f64 / (1 << 20) as f64,
            row.frames
        );
    }
}

/// `cargo test --release --lib mirror::tests::per_file_overhead -- --ignored --nocapture`
///
/// The backfill is also 20,000 small files: each costs a lock file, an
/// fsynced frame, a directory fsync and a state row. Times a first pass
/// over 2,000 files of about 20 KB, then an idle second pass.
#[test]
#[ignore = "throughput measurement; run in release mode with --nocapture"]
fn per_file_overhead() {
    let rig = Rig::new();
    let corpus = bench_corpus(20 << 10, 0);
    for n in 0..2000 {
        let dir = rig.projects.join(format!("-tmp-proj-{}", n % 40));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("sess-{n}.jsonl")), &corpus).unwrap();
    }
    let started = std::time::Instant::now();
    let stats = rig.mirror().sync_pass(None);
    let secs = started.elapsed().as_secs_f64();
    assert_eq!((stats.files, stats.synced, stats.errors), (2000, 2000, 0));
    println!(
        "first pass: {} files, {:.1} MiB in {secs:.2}s = {:.0} files/s",
        stats.files,
        stats.source_bytes as f64 / (1 << 20) as f64,
        stats.files as f64 / secs
    );
    let started = std::time::Instant::now();
    let stats = rig.mirror().sync_pass(None);
    println!(
        "idle pass:  {} files in {:.3}s ({} synced)",
        stats.files,
        started.elapsed().as_secs_f64(),
        stats.synced
    );
}
