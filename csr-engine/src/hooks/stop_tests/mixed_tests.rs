use super::*;

#[test]
fn a_thinking_only_end_turn_row_is_not_the_reply() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("t.jsonl");
    let thinking = r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"thinking","thinking":""}]}}"#;
    let text = r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Done."}]}}"#;
    std::fs::write(&path, format!("{thinking}\n")).unwrap();
    assert!(!tail_closes_turn(&path));
    std::fs::write(&path, format!("{thinking}\n{text}\n")).unwrap();
    assert!(tail_closes_turn(&path));
}

#[test]
fn a_tail_read_from_mid_file_skips_the_row_it_starts_inside() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("t.jsonl");
    let filler = format!(
        r#"{{"type":"attachment","pad":"{}"}}"#,
        "x".repeat(TURN_END_TAIL_BYTES as usize)
    );
    let closing = r#"{"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Done."}]}}"#;
    std::fs::write(&path, format!("{filler}\n{closing}\n")).unwrap();
    assert!(tail_closes_turn(&path));
}

#[tokio::test]
async fn the_worker_waits_for_the_closing_row_and_then_reads_a_finished_turn() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("b046750d.jsonl");
    std::fs::write(&path, stop_race_without_closing_row()).unwrap();
    let closing = STOP_RACE.lines().last().unwrap().to_string();
    let writer = {
        let path = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(250));
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            writeln!(file, "{closing}").unwrap();
        })
    };
    let closed = await_turn_end(
        &path,
        std::time::Duration::from_secs(5),
        std::time::Duration::from_millis(20),
    )
    .await;
    writer.join().unwrap();
    assert!(closed);
    let len = std::fs::metadata(&path).unwrap().len();
    let ep = extract_episode_from_file(&path, len, "b046750d", "proj").unwrap();
    assert!(ep.closing_ended);
    assert!(ep.completed.starts_with("**Done: af7e9645"));
}

#[tokio::test]
async fn the_wait_is_bounded_and_fails_open() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("t.jsonl");
    std::fs::write(&path, stop_race_without_closing_row()).unwrap();
    let started = std::time::Instant::now();
    let closed = await_turn_end(
        &path,
        std::time::Duration::from_millis(150),
        std::time::Duration::from_millis(20),
    )
    .await;
    assert!(!closed);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[tokio::test]
async fn episode_carries_instrumentation_when_transcript_is_readable() {
    let (engine, storage) = instrumentation_test_engine();
    let tmp = tempfile::TempDir::new().unwrap();
    let transcript = tmp.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","message":{"content":"make the podcast episode"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"ffmpeg"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","is_error":true,"content":"ffmpeg exit 1"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"no — hindi, not english"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Retried with the hindi voice id."}]}}"#,
            "\n",
        ),
    )
    .unwrap();

    let session_id = "aaaaaaaa-1111-2222-3333-444444444444";
    let input = HookInput {
        transcript_path: Some(transcript.to_string_lossy().to_string()),
        session_id: Some(session_id.to_string()),
        cwd: Some(tmp.path().to_string_lossy().to_string()),
        ..Default::default()
    };

    extract_and_store_episode(&input, &engine, tmp.path())
        .await
        .expect("readable transcript must store an episode");

    let ep = stored_episode(&storage, session_id);
    assert_eq!(ep.error_count, Some(1));
    assert_eq!(ep.top_errors.len(), 1);
    assert_eq!(ep.top_errors[0].tool, "Bash");
    assert_eq!(ep.steer_count, Some(1));
    assert_eq!(ep.steers.len(), 1);
    assert!(ep.steers[0].text.contains("hindi"));
}

#[tokio::test]
async fn stop_capture_persists_a_correction_with_an_exact_byte_receipt() {
    let (engine, storage) = instrumentation_test_engine();
    let tmp = tempfile::TempDir::new().unwrap();
    let transcript = tmp.path().join("correction.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","timestamp":"2026-09-01T10:00:00Z","message":{"content":"Implement the parser"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-09-01T10:01:00Z","message":{"content":"I changed `old_parser`."}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-09-01T10:02:00Z","message":{"content":"no, that is not what I asked for"}}"#,
            "\n",
        ),
    )
    .unwrap();
    let session_id = "cccccccc-1111-2222-3333-444444444444";
    let input = HookInput {
        transcript_path: Some(transcript.to_string_lossy().to_string()),
        session_id: Some(session_id.to_string()),
        cwd: Some(tmp.path().to_string_lossy().to_string()),
        ..Default::default()
    };

    extract_and_store_episode(&input, &engine, tmp.path())
        .await
        .unwrap();

    let episode = stored_episode(&storage, session_id);
    assert_eq!(episode.correction_count, Some(1));
    let mut events = storage.list_intent_events(None, None).unwrap();
    assert_eq!(events.len(), 1);
    let bytes = std::fs::read(&transcript).unwrap();
    assert_eq!(
        &bytes[events[0].byte_start..events[0].byte_end],
        events[0].quote.as_bytes()
    );

    let first_high_water: crate::transcript::intent_events::IntentHighWater = serde_json::from_str(
        &storage
            .get_meta(&format!("intent_high_water:{session_id}"))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first_high_water.last_turn, 3);
    assert_eq!(
        first_high_water.byte_offset,
        std::fs::metadata(&transcript).unwrap().len() as usize
    );
    extract_and_store_episode(&input, &engine, tmp.path())
        .await
        .unwrap();
    assert_eq!(storage.list_intent_events(None, None).unwrap().len(), 1);

    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(
        file,
        r#"{{"type":"assistant","timestamp":"2026-09-01T10:03:00Z","message":{{"content":"I used the root config."}}}}"#
    )
    .unwrap();
    writeln!(
        file,
        r#"{{"type":"user","timestamp":"2026-09-01T10:04:00Z","message":{{"content":"wrong file. use `config/app.toml`."}}}}"#
    )
    .unwrap();
    extract_and_store_episode(&input, &engine, tmp.path())
        .await
        .unwrap();
    events = storage.list_intent_events(None, None).unwrap();
    assert_eq!(events.len(), 2);
    let second_high_water: crate::transcript::intent_events::IntentHighWater =
        serde_json::from_str(
            &storage
                .get_meta(&format!("intent_high_water:{session_id}"))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
    assert_eq!(second_high_water.last_turn, 5);
    assert!(second_high_water.byte_offset > first_high_water.byte_offset);
}

#[test]
fn active_stop_hook_is_skipped() {
    let input = HookInput {
        stop_hook_active: Some(true),
        ..Default::default()
    };
    assert!(stop_hook_is_active(&input));
}

#[tokio::test]
async fn oversized_transcript_is_skipped_without_error() {
    let (engine, storage) = instrumentation_test_engine();
    let tmp = tempfile::TempDir::new().unwrap();
    let transcript = tmp.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        r#"{"type":"user","message":{"content":"make the podcast episode"}}"#,
    )
    .unwrap();
    // Stub an over-cap size without writing 64 MiB of real bytes: a
    // sparse `set_len` bump makes `metadata().len()` read as huge while
    // the file's real content — and disk usage — stay tiny.
    {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&transcript)
            .unwrap();
        f.set_len(crate::transcript::instrumentation::MAX_TRANSCRIPT_SCAN_BYTES + 1)
            .unwrap();
    }
    assert!(
        std::fs::metadata(&transcript).unwrap().len()
            > crate::transcript::instrumentation::MAX_TRANSCRIPT_SCAN_BYTES,
        "fixture must actually exceed the cap for this test to mean anything"
    );

    let session_id = "bbbbbbbb-1111-2222-3333-444444444444";
    let input = HookInput {
        transcript_path: Some(transcript.to_string_lossy().to_string()),
        session_id: Some(session_id.to_string()),
        cwd: Some(tmp.path().to_string_lossy().to_string()),
        ..Default::default()
    };

    extract_and_store_episode(&input, &engine, tmp.path())
        .await
        .expect("an oversized transcript must not fail the hook");

    let ep = stored_episode(&storage, session_id);
    assert!(
        ep.error_count.is_none(),
        "oversized scan must leave error_count None"
    );
    assert!(ep.top_errors.is_empty());
    assert!(
        ep.steer_count.is_none(),
        "oversized scan must leave steer_count None"
    );
    assert_eq!(
        storage
            .get_meta(&format!("intent_high_water:{session_id}"))
            .unwrap(),
        None,
        "oversized Stop transcript must not enter intent extraction"
    );
    assert!(ep.steers.is_empty());
}

#[test]
fn missing_transcript_never_fails_the_hook() {
    let tmp = tempfile::TempDir::new().unwrap();
    let transcript = tmp.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        r#"{"type":"user","message":{"content":"do the work"}}"#,
    )
    .unwrap();

    // File present at the point the caller would normally scan it.
    assert!(scan_instrumentation(&transcript, 1 << 20).is_some());

    // The transcript vanishes between the hook's initial existence
    // check/read and a later instrumentation scan attempt (real-world
    // race: session rotation, watcher move, Ctrl+C mid-write).
    // `scan_instrumentation` must degrade to `None` — never panic, never
    // propagate an I/O error — which is exactly what lets its caller
    // (`extract_and_store_episode`) leave the instrumentation fields at
    // `None` while still storing the rest of the episode. Hooks must
    // never block Claude Code (repo CLAUDE.md).
    std::fs::remove_file(&transcript).unwrap();
    assert!(scan_instrumentation(&transcript, 1 << 20).is_none());
}

#[test]
fn episode_v2_extracts_taskcreate_taskupdate() {
    let lines_owned = [
        user_line("do the work"),
        assistant_named_tool(
            "TaskCreate",
            "toolu_1",
            serde_json::json!({"subject": "first task"}),
        ),
        tool_result_bound("toolu_1", "Task #1 created successfully."),
        assistant_named_tool(
            "TaskCreate",
            "toolu_2",
            serde_json::json!({"subject": "second task"}),
        ),
        tool_result_bound("toolu_2", "Task #2 created successfully."),
        assistant_named_tool(
            "TaskUpdate",
            "toolu_3",
            serde_json::json!({"taskId": "2", "status": "completed"}),
        ),
        // Task state changes only on a matching successful result.
        tool_result_bound("toolu_3", "Updated task #2 status"),
    ];
    let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
    let ep = extract_episode(&lines, "sess-tc", "proj");

    assert_eq!(ep.todos.len(), 2);
    assert_eq!(ep.todos[0].content, "first task");
    assert_eq!(ep.todos[0].status, "pending");
    assert_eq!(ep.todos[1].content, "second task");
    assert_eq!(ep.todos[1].status, "completed");
    // next_steps is the first non-completed item (id 1 / first task)
    assert_eq!(ep.next_steps.as_deref(), Some("first task"));
}

#[test]
fn taskupdate_unknown_id_ignored() {
    let lines_owned = [
        user_line("update a phantom task"),
        assistant_named_tool(
            "TaskUpdate",
            "toolu_x",
            serde_json::json!({"taskId": "9", "status": "completed"}),
        ),
    ];
    let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
    let ep = extract_episode(&lines, "sess-unk", "proj");
    assert!(ep.todos.is_empty());
    assert_eq!(ep.next_steps, None);
}

#[test]
fn taskcreate_result_id_binding_nonsequential() {
    // Non-sequential ids prove we bind from tool_result text, not ordinal index.
    let lines_owned = [
        user_line("resume prior tasks"),
        assistant_named_tool(
            "TaskCreate",
            "toolu_a",
            serde_json::json!({"subject": "item three"}),
        ),
        tool_result_bound("toolu_a", "Task #3 created successfully."),
        assistant_named_tool(
            "TaskCreate",
            "toolu_b",
            serde_json::json!({"subject": "item seven"}),
        ),
        tool_result_bound("toolu_b", "Task #7 created successfully."),
        assistant_named_tool(
            "TaskUpdate",
            "toolu_c",
            serde_json::json!({"taskId": "7", "status": "completed"}),
        ),
        tool_result_bound("toolu_c", "Updated task #7 status"),
    ];
    let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
    let ep = extract_episode(&lines, "sess-ns", "proj");

    assert_eq!(ep.todos.len(), 2);
    assert_eq!(ep.todos[0].content, "item three");
    assert_eq!(ep.todos[0].status, "pending");
    assert_eq!(ep.todos[1].content, "item seven");
    assert_eq!(ep.todos[1].status, "completed");
}

#[test]
fn todowrite_after_taskcreate_replaces() {
    let lines_owned = [
        user_line("mix task systems"),
        assistant_named_tool(
            "TaskCreate",
            "toolu_1",
            serde_json::json!({"subject": "from taskcreate"}),
        ),
        tool_result_bound("toolu_1", "Task #1 created successfully."),
        assistant_named_tool(
            "TodoWrite",
            "toolu_2",
            serde_json::json!({
                "todos": [
                    {"content": "from todowrite a", "status": "pending"},
                    {"content": "from todowrite b", "status": "in_progress"}
                ]
            }),
        ),
        tool_result_bound("toolu_2", "Todos have been modified successfully"),
        // Stale id from the cleared TaskCreate map must not corrupt the list.
        assistant_named_tool(
            "TaskUpdate",
            "toolu_3",
            serde_json::json!({"taskId": "1", "status": "completed"}),
        ),
        tool_result_bound("toolu_3", "Updated task #1 status"),
    ];
    let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
    let ep = extract_episode(&lines, "sess-prec", "proj");

    assert_eq!(ep.todos.len(), 2);
    assert_eq!(ep.todos[0].content, "from todowrite a");
    assert_eq!(ep.todos[0].status, "pending");
    assert_eq!(ep.todos[1].content, "from todowrite b");
    assert_eq!(ep.todos[1].status, "in_progress");
}

#[test]
fn load_task_dir_state_reads_numeric_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("1.json"),
        r#"{"subject":"first","status":"pending"}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("2.json"),
        r#"{"subject":"second","status":"completed"}"#,
    )
    .unwrap();
    // Non-json / non-numeric names must be skipped without error.
    std::fs::write(dir.path().join(".lock"), "busy").unwrap();
    std::fs::write(dir.path().join("notes.txt"), "ignore me").unwrap();

    let state = load_task_state_from_dir(dir.path()).expect("dir readable");
    assert_eq!(state.files_seen, 2);
    assert_eq!(state.parse_failures, 0);
    assert_eq!(
        state.todos,
        vec![
            TodoItem {
                content: "first".to_string(),
                status: "pending".to_string(),
            },
            TodoItem {
                content: "second".to_string(),
                status: "completed".to_string(),
            },
        ]
    );
}

#[test]
fn task_dir_all_deleted_is_authoritative_empty_and_failures_counted() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("1.json"),
        r#"{"subject":"gone","status":"deleted"}"#,
    )
    .unwrap();
    std::fs::write(dir.path().join("2.json"), "not json at all").unwrap();
    let state = load_task_state_from_dir(dir.path()).expect("dir readable");
    // Deleted task counts as a seen file (authority signal) but not a todo;
    // the garbage file counts as a parse failure (schema-drift signal).
    assert_eq!(state.files_seen, 2);
    assert_eq!(state.parse_failures, 1);
    assert!(state.todos.is_empty());
}

#[test]
fn outcome_pending_task_caps_at_partial() {
    let todos = [TodoItem {
        content: "x".into(),
        status: "pending".into(),
    }];
    let signals = OutcomeSignals {
        final_failed: false,
        proven: true,
        ended: true,
    };
    let outcome = compute_outcome(5, &signals, &todos);
    assert_eq!(outcome, "partial");
}

#[test]
fn r8_interruption_closes_main_turn_without_becoming_anchor() {
    for notice in [
        "[Request interrupted by user]",
        "[Request interrupted by user for tool use]",
        "[Request interrupted]",
    ] {
        assert!(ends_turn(
            &serde_json::json!({"message":{"content":notice}}),
            "user"
        ));
        assert!(!ends_turn(
            &serde_json::json!({"message":{"content":[{"type":"tool_result","content":notice}]}}),
            "user"
        ));
        for sidechain in [false, true] {
            let rows = [
                serde_json::json!({"type":"user", "timestamp":"2026-10-13T00:00:00Z", "message":{"content":"Design the discussion side pane for users."}}),
                serde_json::json!({"type":"assistant", "message":{"content":[], "stop_reason":"tool_use"}}),
                serde_json::json!({"type":"user", "isSidechain":sidechain, "message":{"content":[{"type":"text","text":notice}]}}),
                serde_json::json!({"type":"user", "timestamp":"2026-10-13T01:00:00Z", "message":{"content":"Fix the database durability regression before release."}}),
                serde_json::json!({"type":"assistant", "message":{"content":"Done.","stop_reason":"end_turn"}}),
            ];
            let ep = extract(&rows.iter().map(ToString::to_string).collect::<Vec<_>>());
            if sidechain {
                assert!(ep.compact_anchor.is_none());
            } else {
                assert_eq!(
                    ep.compact_anchor.as_ref().map(|a| a.request.as_str()),
                    Some("Fix the database durability regression before release.")
                );
            }
            assert!(!ep
                .anchor_request
                .unwrap_or_default()
                .contains("interrupted"));
        }
    }
}

#[test]
fn compact_anchor_allows_clause_cut_and_tracks_receipt_origins() {
    let first = "Design the discussion side pane for users.";
    let latest = "Drive release readiness and verify all candidate checks";
    let long = format!(
        "{latest}, {}",
        "with extensive detailed verification ".repeat(20)
    );
    assert!(crate::hooks::recap::clean_intent(&long).is_none());
    for old_receipt in [false, true] {
        let mut lines = segment(first, "first", 1);
        if old_receipt {
            lines.extend(committed("old", "abcdef1", "feat(pane): add side pane"));
            lines.push(turn_end_line());
        }
        lines.extend(segment(&long, "latest", 1));
        lines.extend(committed(
            "new",
            "abcdef2",
            "fix(release): verify candidate",
        ));
        let ep = extract(&lines);
        let anchor = ep.compact_anchor.as_ref().unwrap();
        assert_eq!(anchor.request, latest);
        assert_eq!(anchor.completions_untied, old_receipt);
        assert_eq!(ep.anchor_request.as_deref(), Some(first));
        assert!(ep.completions_untied);
    }
}

#[test]
fn anchor_is_the_first_task_whole_never_cut() {
    let ep = extract(&multi_topic_session());
    assert!(ep.request.starts_with("anthropics codemod"));
    assert_eq!(ep.recap_evidence_version, Some(RECAP_EVIDENCE_VERSION));
    assert_eq!(
        ep.anchor_request.as_deref(),
        Some(
            "anthropics codemod, when github links are bought up or disccussed, can \
                 present a side pane (enhanced for herdr users) - thoughts?"
        )
    );
}

#[test]
fn receipts_produced_under_the_anchor_stay_tied_to_it() {
    // The only commit came under the first topic, which is the anchor:
    // it is the anchor's own result even though a later task followed.
    let ep = extract(&multi_topic_session());
    assert_eq!(
        ep.completions,
        vec![CompletionReceipt {
            claim: "feat(pane): first topic commit".into(),
            receipt: "0aa11bb".into(),
        }]
    );
    assert!(!ep.completions_untied);
}

#[test]
fn single_prompt_intent_keeps_whole_sentence_across_soft_wrap() {
    let ep = extract(&[typed_line(FIRST_PROMPT, "typed")]);
    assert_eq!(
        ep.anchor_request.as_deref(),
        Some(
            "anthropics codemod, when github links are bought up or disccussed, can \
                 present a side pane (enhanced for herdr users) - thoughts?"
        )
    );
}

#[test]
fn dreamer_what_if_is_a_task_rendered_as_a_note() {
    let ep = extract(&[
        typed_line(
            "What if Claude opening a memory note also pulled in its session?\n\n\
                 CSR's dreamer asked this while you worked. It is a question, not a task.\n\n\
                 > Notes are where Claude looks first.",
            "typed",
        ),
        // Sent mid-turn, yet nothing joins onto a note.
        typed_line("and check the pane width while you are at it", "queued"),
        typed_line("ok, do it please", "typed"),
    ]);
    assert_eq!(
        ep.anchor_request.as_deref(),
        Some("note: What if Claude opening a memory note also pulled in its session?")
    );
}

#[test]
fn a_v2_dreamer_what_if_is_a_note_without_its_quote_envelope() {
    let ep = extract(&[typed_line(
        "\u{201c}What if Claude opening a memory note also pulled in its session?\u{201d}\n\n\
             CSR's dreamer asked this while you worked, quoted. Its reasoning:\n\n\
             > Notes are where Claude looks first.",
        "typed",
    )]);
    assert_eq!(
        ep.anchor_request.as_deref(),
        Some("note: What if Claude opening a memory note also pulled in its session?")
    );
}

#[test]
fn note_opener_shares_the_import_prefix() {
    assert!(
        SIDE_AGENT_NOTE_OPENER.starts_with(crate::extraction::provenance::SIDE_AGENT_NOTE_PREFIX)
    );
}

#[test]
fn forwarded_note_renders_its_bold_title() {
    let ep = extract(&[typed_line(
        "Here is a note offered by a side agent:\n\n\
             > Heads up · Logic's bar lines in this project are a 120 BPM placeholder, not the song's real beat. A region you drag can snap off the music.\n\
             >\n\
             > **Logic's bar lines don't match the song's beat**\n\
             > - The top bar shows **120 KEEP**. That means the project runs at a fixed 120 BPM.",
        "typed",
    )]);
    assert_eq!(
        ep.anchor_request.as_deref(),
        Some("note: Logic's bar lines don't match the song's beat")
    );
}

#[test]
fn forwarded_note_without_a_title_uses_its_first_heads_up_sentence() {
    let ep = extract(&[typed_line(
        "Here is a note offered by a side agent:\n\n\
             > Heads up · The recap came out garbled twice today. Both runs cut the prompt.",
        "typed",
    )]);
    assert_eq!(
        ep.anchor_request.as_deref(),
        Some("note: The recap came out garbled twice today.")
    );
}

#[test]
fn rendered_notes_pass_the_guard_but_the_raw_dreamer_prompt_never_does() {
    use crate::extraction::provenance::{extractable, is_csr_emission};
    let raw = "What if Claude opening a memory note also pulled in its session?\n\n\
                   CSR's dreamer asked this while you worked. It is a question, not a task.\n\n\
                   > Notes are where Claude looks first.";
    assert!(is_csr_emission(raw));
    assert!(extractable(raw).is_none());
    let rendered = "note: What if Claude opening a memory note also pulled in its session?";
    assert!(!is_csr_emission(rendered));
    assert_eq!(extractable(rendered).as_deref(), Some(rendered));
}

#[test]
fn future_tense_plan_is_never_a_completion() {
    let ep = extract(&[
        typed_line(
            "We need a screen recording of the warp terminal even if recreated as part of this",
            "queued",
        ),
        assistant_text_line(
            "I'll recreate it headless: a real claude session runs in a private tmux, and \
                 its real frames replay inside a browser.",
        ),
    ]);
    assert!(ep.completions.is_empty());
    let got = crate::hooks::recap::compose_recap(
        &ep,
        &crate::hooks::recap::RecapFeeds {
            settled: vec![],
            still_open: vec![],
            retired_while_away: vec![],
            corrections: vec![],
            open_proposals: 0,
            native_warn: None,
            top_dream: None,
        },
        "2m ago",
    )
    .unwrap();
    assert_eq!(
        got,
        "recap [2m ago]: We need a screen recording of the warp terminal even if recreated \
             as part of this. [[CSR:RECAP]]"
    );
}

#[test]
fn commit_after_latest_task_is_a_receipted_completion() {
    let ep = extract(&[
        typed_line("fix the garbled recap", "typed"),
        git_commit("toolu_c1"),
        tool_result_bound(
            "toolu_c1",
            "[fix/recap-garble 1a2b3c4d] fix(recap): intent from the latest task\n 2 files changed, 9 insertions(+)",
        ),
        // A failed commit and a non-git command printing the same shape
        // are not completions.
        git_commit("toolu_c2"),
        serde_json::json!({"type": "user", "message": {"content": [{
            "type": "tool_result", "tool_use_id": "toolu_c2", "is_error": true,
            "content": "[main 9f9f9f9] would-be subject"
        }]}})
        .to_string(),
        assistant_named_tool("Bash", "toolu_cat", serde_json::json!({"command": "cat log.txt"})),
        tool_result_bound("toolu_cat", "[main 7e7e7e7] quoted from a log"),
    ]);
    assert_eq!(
        ep.completions,
        vec![CompletionReceipt {
            claim: "fix(recap): intent from the latest task".into(),
            receipt: "1a2b3c4".into(),
        }]
    );
}

#[test]
fn task_completions_after_latest_task_carry_their_receipt() {
    let ep = extract(&[
        typed_line("ship the recap fix", "typed"),
        assistant_named_tool(
            "TaskCreate",
            "toolu_t1",
            serde_json::json!({"subject": "Write regression tests"}),
        ),
        tool_result_bound("toolu_t1", "Task #3 created successfully"),
        assistant_named_tool(
            "TaskUpdate",
            "toolu_t2",
            serde_json::json!({"taskId": "3", "status": "completed"}),
        ),
        tool_result_bound("toolu_t2", "Updated task #3 status"),
        assistant_named_tool(
            "TodoWrite",
            "toolu_t3",
            serde_json::json!({"todos": [
                {"content": "Run the gates", "status": "completed"},
                {"content": "Commit", "status": "pending"}
            ]}),
        ),
        tool_result_bound("toolu_t3", "Todos have been modified successfully"),
    ]);
    assert_eq!(
        ep.completions,
        vec![
            CompletionReceipt {
                claim: "Write regression tests".into(),
                receipt: "task #3 completed".into(),
            },
            CompletionReceipt {
                claim: "Run the gates".into(),
                receipt: "todo completed".into(),
            },
        ]
    );
}

#[test]
fn recap_for_the_live_garble_shape_names_the_whole_first_task_and_no_prose() {
    let ep = extract(&multi_topic_session());
    let got = crate::hooks::recap::compose_recap(
        &ep,
        &crate::hooks::recap::RecapFeeds {
            settled: vec![],
            still_open: vec![],
            retired_while_away: vec![],
            corrections: vec![],
            open_proposals: 0,
            native_warn: None,
            top_dream: None,
        },
        "3h ago",
    )
    .unwrap();
    assert_eq!(
        got,
        "recap [3h ago]: anthropics codemod, when github links are bought up or disccussed, \
             can present a side pane (enhanced for herdr users) - thoughts? \
             Latest: feat(pane): first topic commit (0aa11bb). [[CSR:RECAP]]"
    );
    assert!(!got.contains("How the share works"));
}

#[test]
fn mid_turn_continuation_joins_the_prompt_it_continues() {
    // Live shape (lines 28444/28462): the second message arrived while the
    // first turn was cancelled ("remote-cancel"), never finished.
    let ep = extract(&[
        typed_line("How do we show cost as % of mac", "queued"),
        assistant_stop_line("remote-cancel", "stop_sequence"),
        typed_line("% of max plan if they have it or cents", "queued"),
    ]);
    assert_eq!(
        ep.anchor_request.as_deref(),
        Some("How do we show cost as % of mac % of max plan if they have it or cents")
    );
}

#[test]
fn a_finished_turn_separates_tasks() {
    for finished in [
        turn_end_line(),
        assistant_stop_line("Here is the answer.", "end_turn"),
    ] {
        let mut lines = vec![
            typed_line("How do we show cost as a share of the plan", "typed"),
            finished,
            typed_line("now fix the recap garble", "typed"),
        ];
        lines.extend(committed("b", "1a2b3c4", "fix(recap): garble"));
        let ep = extract(&lines);
        assert_eq!(
            ep.anchor_request.as_deref(),
            Some("How do we show cost as a share of the plan")
        );
        // The receipt belongs to a later, separate task.
        assert!(ep.completions_untied);
    }
}

#[test]
fn joined_continuation_that_fails_clean_intent_abstains() {
    let long = format!("{} end", "word ".repeat(30));
    let ep = extract(&[
        turn_end_line(),
        typed_line(&long, "typed"),
        assistant_stop_line("working", "tool_use"),
        typed_line("and also the side pane width", "queued"),
    ]);
    assert_eq!(ep.anchor_request, None);
}

#[test]
fn explicit_null_stop_reason_is_not_a_finished_turn() {
    // Only a MISSING stop_reason is a legacy finish; streamed rows carry
    // an explicit null while the turn is still running.
    let ep = extract(&[
        turn_end_line(),
        typed_line("How do we show cost as % of mac", "typed"),
        assistant_null_stop_line("thinking about it"),
        typed_line("% of max plan if they have it or cents", "queued"),
    ]);
    assert_eq!(
        ep.anchor_request.as_deref(),
        Some("How do we show cost as % of mac % of max plan if they have it or cents")
    );
}

#[test]
fn an_ack_reopens_the_turn_so_a_clarification_joins_the_task() {
    let ep = extract(&[
        typed_line("Fix login validation", "typed"),
        assistant_stop_line("Plan: tighten the email check. Go?", "end_turn"),
        typed_line("go", "typed"),
        assistant_stop_line("editing", "tool_use"),
        typed_line("only for the signup form", "queued"),
    ]);
    assert_eq!(
        ep.anchor_request.as_deref(),
        Some("Fix login validation only for the signup form")
    );
}

#[test]
fn system_reminder_contents_never_become_the_intent() {
    let ep = extract(&[typed_line(
        "<system-reminder>\nBuild failed: missing foo.rs.\n</system-reminder>\n\nFix login validation.",
        "typed",
    )]);
    assert_eq!(ep.anchor_request.as_deref(), Some("Fix login validation."));
}

#[test]
fn task_completion_counts_only_on_a_successful_tool_result() {
    let task = |result: String| {
        extract(&[
            typed_line("ship the deploy", "typed"),
            assistant_named_tool(
                "TaskCreate",
                "toolu_c",
                serde_json::json!({"subject": "Deploy"}),
            ),
            tool_result_bound("toolu_c", "Task #3 created successfully"),
            assistant_named_tool(
                "TaskUpdate",
                "toolu_u",
                serde_json::json!({"taskId": "3", "status": "completed"}),
            ),
            result,
        ])
    };
    assert!(task(error_result("toolu_u", "Task not found"))
        .completions
        .is_empty());
    assert_eq!(
        task(tool_result_bound("toolu_u", "Updated task #3 status")).completions,
        vec![CompletionReceipt {
            claim: "Deploy".into(),
            receipt: "task #3 completed".into(),
        }]
    );
}

#[test]
fn todo_completion_counts_only_on_a_successful_tool_result() {
    let todo = |result: String| {
        extract(&[
            typed_line("ship the deploy", "typed"),
            assistant_named_tool(
                "TodoWrite",
                "toolu_w",
                serde_json::json!({"todos": [{"content": "Deploy", "status": "completed"}]}),
            ),
            result,
        ])
    };
    assert!(todo(error_result("toolu_w", "InputValidationError"))
        .completions
        .is_empty());
    assert_eq!(
        todo(tool_result_bound(
            "toolu_w",
            "Todos have been modified successfully"
        ))
        .completions,
        vec![CompletionReceipt {
            claim: "Deploy".into(),
            receipt: "todo completed".into(),
        }]
    );
}

#[test]
fn only_a_committing_git_subcommand_yields_a_commit_receipt() {
    assert!(commit_receipts_for("git log -1 --format='[main %h] %s' --grep=commit").is_empty());
    let one = vec![CompletionReceipt {
        claim: "fix(recap): subject".into(),
        receipt: "1a2b3c4".into(),
    }];
    assert_eq!(
        commit_receipts_for(
            r#"git -c user.name="Rama Annaswamy" -c user.email=x@y commit -m "fix""#
        ),
        one
    );
    assert_eq!(commit_receipts_for("cd x && git commit -m y"), one);
    assert_eq!(
        commit_receipts_for("git --no-pager -C /repo cherry-pick abc1234"),
        one
    );
    assert!(commit_receipts_for("echo git commit").is_empty());
}

#[test]
fn a_commit_made_over_ssh_yields_a_receipt_and_other_ssh_calls_do_not() {
    let one = vec![CompletionReceipt {
        claim: "fix(recap): subject".into(),
        receipt: "1a2b3c4".into(),
    }];
    assert_eq!(
        commit_receipts_for(
            "ssh studio 'cd ~/build/rc && git add -A && git commit -F /tmp/m 2>&1 | tail -3'"
        ),
        one
    );
    assert_eq!(
        commit_receipts_for("ssh -o BatchMode=yes -p 22 studio git commit -m y"),
        one
    );
    assert_eq!(
        commit_receipts_for("/usr/bin/ssh studio \"git -C ~/build/rc commit -F /tmp/m\""),
        one
    );
    // The remote command is read like any other: a listing is no receipt.
    assert!(commit_receipts_for("ssh studio 'git log --grep=commit'").is_empty());
    assert!(commit_receipts_for("ssh studio 'echo git commit'").is_empty());
    // No remote command, or a different program, is no commit.
    assert!(commit_receipts_for("ssh studio").is_empty());
    assert!(commit_receipts_for("scp m studio:git commit").is_empty());
    // `bash -c` and friends stay unread.
    assert!(commit_receipts_for("bash -c 'git commit -m y'").is_empty());
}

#[test]
fn an_escaped_quote_in_a_remote_command_is_not_a_commit_receipt() {
    // The remote shell prints text, then lists an existing commit: the
    // bracketed line is a log's, and `git commit` sits inside a quoted word.
    assert!(commit_receipts_for(
        r#"ssh host 'printf "%s\n" "x\"; git commit -m x"; git log -1 --format="[main %h] %s"'"#
    )
    .is_empty());
    // Escapes at the outer level: an escaped `;` or quote joins its word.
    assert!(commit_receipts_for(r"echo x\; git commit -m y").is_empty());
    assert!(commit_receipts_for(r#"echo "a\"; git commit -m y""#).is_empty());
    assert!(commit_receipts_for(r#"ssh host "echo \"; git commit -m y""#).is_empty());
    // Single quotes take a backslash literally, so the quote after it closes.
    let one = vec![CompletionReceipt {
        claim: "fix(recap): subject".into(),
        receipt: "1a2b3c4".into(),
    }];
    assert_eq!(commit_receipts_for(r"echo 'a\'; git commit -m y"), one);
    // An open quote or a lone trailing backslash is an uncertain parse over ssh.
    assert!(commit_receipts_for("ssh host 'git commit -m \"x'").is_empty());
    assert!(commit_receipts_for("ssh host 'git commit -m x \\'").is_empty());
}

#[test]
fn ssh_options_group_the_getopt_way() {
    let one = vec![CompletionReceipt {
        claim: "fix(recap): subject".into(),
        receipt: "1a2b3c4".into(),
    }];
    for command in [
        "ssh -tp 22 host 'git commit -m x'",
        "ssh -p22 host 'git commit -m x'",
        "ssh -oFoo=bar host 'git commit -m x'",
        "ssh -tt -o BatchMode=yes -i ~/.ssh/k -l me host 'git commit -m x'",
        "ssh -p 22 -- host 'git commit -m x'",
        r#"ssh host "git commit -m \"a b\"""#,
    ] {
        assert_eq!(commit_receipts_for(command), one, "{command}");
    }
    // Not plainly understood: no receipt.
    for command in [
        "ssh -Z host 'git commit -m x'",
        "ssh -p host",
        "ssh host -t 'git commit -m x'",
        "ssh - host 'git commit -m x'",
    ] {
        assert!(commit_receipts_for(command).is_empty(), "{command}");
    }
}

#[test]
fn a_quiet_commit_is_a_receipt_when_the_log_line_names_its_own_subject() {
    let command = r#"git add -A && git commit -q -m "fix(recap): subject" && git log --oneline -1"#;
    assert_eq!(
        bash_receipts(command, "1a2b3c4 fix(recap): subject", false),
        vec![receipt("fix(recap): subject", "1a2b3c4")]
    );
    // A heredoc commit followed by a separate line proves nothing: the
    // log runs whether or not the commit did.
    let heredoc =
        "git commit -q -F - <<'EOF'\nfix(recap): subject\n\nbody\nEOF\ngit log --oneline -1";
    assert!(bash_receipts(heredoc, "1a2b3c4 fix(recap): subject", false).is_empty());
    // Hook chatter before the line changes nothing.
    assert_eq!(
        bash_receipts(
            command,
            "no leaks found\n1a2b3c4 fix(recap): subject",
            false
        ),
        vec![receipt("fix(recap): subject", "1a2b3c4")]
    );
}

#[test]
fn a_quiet_commit_gives_no_receipt_for_a_commit_it_did_not_make() {
    // The commit failed behind a `;`: the log shows the older HEAD.
    let command = r#"git commit -q -m "feat: new thing"; git log --oneline -1"#;
    assert!(bash_receipts(command, "9d8c7b6 fix: older commit", false).is_empty());
    // A longer log lists older commits too: only `-1` is read.
    let command = r#"git commit -q -m "feat: new thing" && git log --oneline -3"#;
    assert!(bash_receipts(
        command,
        "aaa1111 feat: new thing\nbbb2222 fix: older commit\nccc3333 chore: older still",
        false
    )
    .is_empty());
    // A call that failed is no receipt even when the line is there.
    let command = r#"git commit -q -m "feat: new thing" && git log --oneline -1"#;
    assert!(bash_receipts(command, "aaa1111 feat: new thing", true).is_empty());
    // A log line without a committing command in the call is history.
    assert!(bash_receipts("git log --oneline -1", "aaa1111 feat: new thing", false).is_empty());
}

#[test]
fn a_quiet_commit_that_failed_is_no_receipt_when_an_older_head_has_the_same_subject() {
    // Nothing was staged, so the commit failed; the log that follows shows
    // the older HEAD, whose subject is the same. Only a log that runs
    // because the commit succeeded (`&&`) is proof of a new HEAD.
    for command in [
        "git commit -q -m 'fix: x'; git log --oneline -1",
        "git commit -q -m 'fix: x'\ngit log --oneline -1",
        "git commit -q -m 'fix: x' || true; git log --oneline -1",
        "git log --oneline -1 && git commit -q -m 'fix: x'",
        "git commit -q -m 'fix: x' | cat && git log --oneline -1",
    ] {
        assert!(
            bash_receipts(command, "9d8c7b6 fix: x", false).is_empty(),
            "{command}"
        );
    }
    // A pipe in an earlier, unrelated piece does not hide the commit.
    assert_eq!(
        bash_receipts(
            "gitleaks protect --staged 2>&1 | tail -2 && git commit -q -m 'fix: x' && git log --oneline -1",
            "9d8c7b6 fix: x",
            false
        ),
        vec![receipt("fix: x", "9d8c7b6")]
    );
    // A piped log is more than `git log --oneline -1`: no receipt.
    assert!(bash_receipts(
        "git commit -q -m 'fix: x' && git log --oneline -1 | cat",
        "9d8c7b6 fix: x",
        false
    )
    .is_empty());
}

#[test]
fn a_log_of_another_revision_is_no_receipt_for_the_commit() {
    let out = "9d8c7b6 fix: x";
    for command in [
        "git commit -q -m 'fix: x' && git log --oneline -1 HEAD~1",
        "git commit -q -m 'fix: x' && git log --oneline -1 HEAD^",
        "git commit -q -m 'fix: x' && git log --oneline -1 main",
        "git commit -q -m 'fix: x' && git show --oneline -s abc1234",
        "git commit -q -m 'fix: x' && git log --oneline -1 -- src/a.rs",
        "git commit -q -m 'fix: x' && git log --oneline -1 origin/main..HEAD",
        "git commit -q -m 'fix: x' && git log --oneline -1 HEAD",
        "git commit -q -m 'fix: x' && git log --oneline --skip 1 -1",
        "git commit -q -m 'fix: x' && git log --oneline -1 --skip=1",
        "git commit -q -m 'fix: x' && git show -s --oneline",
        "git commit -q -m 'fix: x' && git log --format='%h %s' -1",
    ] {
        assert!(bash_receipts(command, out, false).is_empty(), "{command}");
    }
    for command in [
        "git commit -q -m 'fix: x' && git log --oneline -1",
        "git commit -q -m 'fix: x' && git log --oneline -n1",
        "git commit -q -m 'fix: x' && git log --oneline -n 1",
        "git commit -q -m 'fix: x' && git log -1 --oneline",
    ] {
        assert_eq!(
            bash_receipts(command, out, false),
            vec![receipt("fix: x", "9d8c7b6")],
            "{command}"
        );
    }
}

#[test]
fn the_same_commit_seen_twice_is_one_receipt() {
    let ep = extract(&[
        typed_line("land the recap fix", "typed"),
        assistant_named_tool(
            "Bash",
            "c1",
            serde_json::json!({"command": "git commit -m x"}),
        ),
        tool_result_bound("c1", "[main 1a2b3c4] fix(recap): subject"),
        assistant_named_tool(
            "Bash",
            "c2",
            serde_json::json!({"command": "git commit --amend --no-edit"}),
        ),
        tool_result_bound("c2", "[main 1a2b3c4] fix(recap): subject"),
    ]);
    assert_eq!(
        ep.completions,
        vec![receipt("fix(recap): subject", "1a2b3c4")]
    );
}

#[test]
fn a_merged_pr_is_a_receipt_only_with_the_merge_commit_in_the_output() {
    let command = "gh pr merge 76 --squash 2>&1 | tail -3; gh pr view 76 --json state,mergeCommit -q '.state+\" \"+.mergeCommit.oid'";
    assert_eq!(
        bash_receipts(
            command,
            "MERGED a8f845882e5068d3eed6d2ca160b182a49c23b06",
            false
        ),
        vec![receipt("merged PR #76", "a8f8458")]
    );
    assert_eq!(
        bash_receipts(command, "MERGED 2026-10-21T00:22:31Z 5b1a3b1", false),
        vec![receipt("merged PR #76", "5b1a3b1")]
    );
    // Still open, or the call failed: nothing was merged.
    assert!(bash_receipts(command, "OPEN ", false).is_empty());
    assert!(bash_receipts(command, "MERGED a8f8458", true).is_empty());
    // Two merges in one call cannot be told apart.
    let two =
        "gh pr merge 1 --squash; gh pr merge 2 --squash; gh pr view 2 --json state,mergeCommit";
    assert!(bash_receipts(two, "MERGED a8f8458", false).is_empty());
    // A view of an already merged PR is not a merge by this session.
    assert!(bash_receipts(
        "gh pr view 76 --json state,mergeCommit",
        "MERGED a8f8458",
        false
    )
    .is_empty());
}

#[test]
fn the_recap_intent_is_the_assistant_s_first_sentence_under_the_anchor_task() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("intent-1.jsonl");
    let rows = [
        r#"{"type":"user","timestamp":"2026-11-12T10:00:00Z","message":{"role":"user","content":"can you look at why the relay socket keeps dropping when the pane wakes up, it started after the last install"}}"#,
        r#"{"type":"assistant","timestamp":"2026-11-12T10:00:05Z","message":{"role":"assistant","content":[{"type":"text","text":"I'll trace where the relay socket is closed during a pane wake. First the companion logs."}]}}"#,
        r#"{"type":"assistant","timestamp":"2026-11-12T10:00:09Z","message":{"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"Found it: the wake path reopened the socket twice."}]}}"#,
    ];
    std::fs::write(&path, rows.join("\n") + "\n").unwrap();
    let len = std::fs::metadata(&path).unwrap().len();
    let ep = extract_episode_from_file(&path, len, "intent-1", "proj").unwrap();
    assert_eq!(
        crate::hooks::recap::composed_intent(&ep).as_deref(),
        Some("I'll trace where the relay socket is closed during a pane wake.")
    );
}
