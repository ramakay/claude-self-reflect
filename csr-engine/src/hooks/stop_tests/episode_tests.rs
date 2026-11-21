use super::*;

#[test]
fn a_long_closing_summary_keeps_whole_sentences_and_no_cut_number() {
    let s = format!(
        "{} with grep. - Test: callers of ui_mark returned 15 name-only hits, only 2 here.",
        "a ".repeat(40)
    );
    let cut = cut_whole(&s, 110);
    assert!(cut.ends_with("with grep."), "{cut}");
    // No sentence end in reach: the cut stops after a whole word.
    let words = "returned 15 name-only hits and 2 more in this tree";
    assert_eq!(cut_whole(words, 10), "returned");
    assert_eq!(cut_whole(words, 8), "returned");
    // A word longer than the budget, and text that already fits.
    assert_eq!(cut_whole("abcdefghij", 4), "abcd");
    assert_eq!(cut_whole("short.", 300), "short.");
    // A multi-byte letter at the cut.
    assert_eq!(cut_whole("caf\u{e9} au lait", 4), "caf");
}

#[test]
fn propose_task_resolutions_only_for_still_open_matches() {
    use crate::import::ConversationChunk;
    use crate::provenance::Speaker;
    let storage = crate::storage::Storage::open_memory().unwrap();
    let mk = |id: &str, content: &str, status: Option<&str>| {
        let chunk = ConversationChunk {
            id: id.into(),
            conversation_id: format!("conv-{id}"),
            project_name: "proj".into(),
            timestamp: "2026-07-27T12:00:00Z".into(),
            content: content.into(),
            message_count: 1,
            summary: None,
            author: Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[0.0; 4]).unwrap();
        if let Some(s) = status {
            storage
                .insert_resolutions(&[id.to_string()], s, "seed", None, "user_confirmed")
                .unwrap();
        }
    };
    mk(
        "open",
        "fix registry offset checkpoint races",
        Some("still_open"),
    );
    mk(
        "done",
        "narrative cache invalidation stale",
        Some("resolved"),
    );
    mk("naked", "unrelated quantum topic", None);

    let todos = vec![
        TodoItem {
            content: "fix registry offset checkpoint races".into(),
            status: "completed".into(),
        },
        TodoItem {
            content: "narrative cache invalidation stale".into(),
            status: "completed".into(),
        },
        TodoItem {
            content: "unrelated quantum topic".into(),
            status: "pending".into(), // not completed — never proposes
        },
    ];
    propose_task_resolutions(&storage, &todos, "proj", "sess-1");
    // Only the still_open match yields a proposal; resolved + pending don't.
    assert_eq!(storage.count_resolution_proposals().unwrap(), 1);
    // Idempotent per (chunk, session).
    propose_task_resolutions(&storage, &todos, "proj", "sess-1");
    assert_eq!(storage.count_resolution_proposals().unwrap(), 1);
}

#[test]
fn propose_task_resolutions_finds_still_open_chunk_ranked_below_the_top_three() {
    let storage = crate::storage::Storage::open_memory().unwrap();
    let subject = "fix registry offset checkpoint races";
    // Four verdict-free chunks that out-rank the open one on bm25: short and
    // exact. FTS top-3 saw only these.
    for i in 0..4 {
        seed_chunk(
            &storage,
            &format!("decoy-{i}"),
            "proj",
            "2026-07-27T12:00:00Z",
            subject,
            None,
        );
    }
    seed_chunk(
        &storage,
        "open",
        "proj",
        "2026-07-27T12:00:00Z",
        "fix registry offset checkpoint races after the watcher restarts, with a long \
             tail of unrelated words about narrative caches, hooks, daemons and sockets",
        Some(("still_open", "user_confirmed")),
    );
    propose_task_resolutions(&storage, &[completed(subject)], "proj", "sess-1");
    assert_eq!(
        storage.resolution_proposal_chunk_ids().unwrap(),
        vec!["open".to_string()]
    );
}

#[test]
fn propose_task_resolutions_ignores_resolved_agent_and_retired_verdicts() {
    let storage = crate::storage::Storage::open_memory().unwrap();
    let ts = "2026-07-27T12:00:00Z";
    let subject = "fix registry offset checkpoint races";
    let confirmed_open = Some(("still_open", "user_confirmed"));
    seed_chunk(
        &storage,
        "resolved",
        "proj",
        ts,
        subject,
        Some(("resolved", "user_confirmed")),
    );
    seed_chunk(
        &storage,
        "agent-open",
        "proj",
        ts,
        subject,
        Some(("still_open", "agent")),
    );
    // Confirmed still_open, then confirmed resolved: the latest row wins.
    seed_chunk(&storage, "retired", "proj", ts, subject, confirmed_open);
    storage
        .insert_resolutions(
            &["retired".to_string()],
            "resolved",
            "seed",
            None,
            "user_confirmed",
        )
        .unwrap();
    // An agent row after a confirmed still_open does not retire it.
    seed_chunk(&storage, "kept", "proj", ts, subject, confirmed_open);
    storage
        .insert_resolutions(&["kept".to_string()], "resolved", "seed", None, "agent")
        .unwrap();
    // Another project's confirmed still_open is out of scope.
    seed_chunk(&storage, "elsewhere", "other", ts, subject, confirmed_open);
    propose_task_resolutions(&storage, &[completed(subject)], "proj", "sess-1");
    assert_eq!(
        storage.resolution_proposal_chunk_ids().unwrap(),
        vec!["kept".to_string()]
    );
}

#[test]
fn propose_task_resolutions_picks_best_overlap_then_newest() {
    let storage = crate::storage::Storage::open_memory().unwrap();
    let open = Some(("still_open", "user_confirmed"));
    let subject = "fix registry offset checkpoint races";
    // 3 of 4 significant tokens: passes 60% but loses to the full match.
    seed_chunk(
        &storage,
        "partial",
        "proj",
        "2026-07-30T00:00:00Z",
        "registry offset checkpoint",
        open,
    );
    seed_chunk(
        &storage,
        "full-old",
        "proj",
        "2026-07-01T00:00:00Z",
        subject,
        open,
    );
    seed_chunk(
        &storage,
        "full-new",
        "proj",
        "2026-07-20T00:00:00Z",
        subject,
        open,
    );
    propose_task_resolutions(&storage, &[completed(subject)], "proj", "sess-1");
    assert_eq!(
        storage.resolution_proposal_chunk_ids().unwrap(),
        vec!["full-new".to_string()]
    );
}

#[test]
fn propose_task_resolutions_has_no_candidates_without_confirmed_still_open() {
    let storage = crate::storage::Storage::open_memory().unwrap();
    let subject = "fix registry offset checkpoint races";
    for i in 0..50 {
        // Agent rows and resolved verdicts only: nothing is a candidate.
        let verdict = if i % 2 == 0 {
            ("still_open", "agent")
        } else {
            ("resolved", "user_confirmed")
        };
        seed_chunk(
            &storage,
            &format!("c{i}"),
            "proj",
            "2026-07-27T12:00:00Z",
            subject,
            Some(verdict),
        );
    }
    // The candidate read is empty, so the function returns before any
    // per-task work.
    assert!(storage
        .get_still_open_chunks("proj", 500)
        .unwrap()
        .is_empty());
    propose_task_resolutions(&storage, &[completed(subject)], "proj", "sess-1");
    assert_eq!(storage.count_resolution_proposals().unwrap(), 0);
}

/// Timing evidence for the Stop hook: `STOPFTS_CHUNKS` chunks (default
/// 20,000) of shared vocabulary and ten completed tasks.
/// `cargo test --release --lib stop_probe_timing -- --nocapture`.
#[test]
fn stop_probe_timing_on_a_large_project() {
    let storage = crate::storage::Storage::open_memory().unwrap();
    let n: usize = std::env::var("STOPFTS_CHUNKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let vocab = [
        "registry",
        "offset",
        "checkpoint",
        "races",
        "narrative",
        "cache",
        "invalidation",
        "stale",
        "daemon",
        "socket",
        "watcher",
        "restart",
        "hook",
        "index",
        "vector",
        "sync",
    ];
    for i in 0..n {
        let content = format!(
            "{} {} {} {} {} {} chunk {i}",
            vocab[i % 16],
            vocab[(i / 3) % 16],
            vocab[(i / 7) % 16],
            vocab[(i / 11) % 16],
            vocab[(i / 13) % 16],
            vocab[(i / 17) % 16],
        );
        let id = format!("c{i}");
        seed_chunk(
            &storage,
            &id,
            "proj",
            "2026-07-27T12:00:00Z",
            &content,
            None,
        );
    }
    let todos: Vec<TodoItem> = (0..10)
        .map(|i| {
            completed(&format!(
                "fix {} {} {} {} for session {i}",
                vocab[i % 16],
                vocab[(i + 3) % 16],
                vocab[(i + 5) % 16],
                vocab[(i + 9) % 16],
            ))
        })
        .collect();
    let started = std::time::Instant::now();
    propose_task_resolutions(&storage, &todos, "proj", "sess-1");
    eprintln!(
        "STOPFTS timing: {n} chunks, 10 tasks, no verdicts: {:?}",
        started.elapsed()
    );
    assert_eq!(storage.count_resolution_proposals().unwrap(), 0);
    // One confirmed still-open chunk that matches the first task.
    seed_chunk(
        &storage,
        "open",
        "proj",
        "2026-07-27T12:00:00Z",
        "fix registry races cache socket for session 0 noted",
        Some(("still_open", "user_confirmed")),
    );
    let started = std::time::Instant::now();
    propose_task_resolutions(&storage, &todos, "proj", "sess-1");
    eprintln!(
        "STOPFTS timing: {n} chunks, 10 tasks, 1 still-open verdict: {:?}",
        started.elapsed()
    );
}

/// Build a JSONL line for a tool_result with error content.
#[test]
fn test_extract_episode_from_transcript() {
    let lines_owned = [
        user_line("Please fix the authentication bug in the login handler"),
        assistant_tool_line(&[("Read", "/src/auth/login.rs")]),
        assistant_tool_line(&[("Grep", "/src/auth/mod.rs")]),
        assistant_tool_line(&[("Edit", "/src/auth/login.rs")]),
        assistant_text_line("I've fixed the authentication bug. The issue was done and complete."),
    ];
    let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();

    let ep = extract_episode(&lines, "sess-123", "my-project");

    assert_eq!(ep.schema, "v2");
    assert_eq!(ep.session_id, "sess-123");
    assert_eq!(ep.project, "my-project");
    assert!(ep.request.contains("fix the authentication bug"));
    assert!(ep.investigated.contains(&"/src/auth/login.rs".to_string()));
    assert!(ep.investigated.contains(&"/src/auth/mod.rs".to_string()));
    assert!(ep
        .files_modified
        .contains(&"/src/auth/login.rs".to_string()));
    assert!(ep.tools_used.contains(&"Read".to_string()));
    assert!(ep.tools_used.contains(&"Grep".to_string()));
    assert!(ep.tools_used.contains(&"Edit".to_string()));
    assert!(ep.completed.contains("fixed the authentication bug"));
    assert_eq!(ep.outcome, "success"); // "fixed" and "complete" in completed text
    assert_eq!(ep.message_count, 5);
    assert!(ep.error_signatures.is_empty());
}

#[test]
fn test_extract_episode_short_session() {
    let lines_owned = [user_line("Hello")];
    let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();

    let ep = extract_episode(&lines, "sess-short", "test-proj");

    assert_eq!(ep.outcome, "interrupted");
    assert_eq!(ep.message_count, 1);
    assert_eq!(ep.request, "Hello");
}

#[test]
fn test_extract_episode_with_errors() {
    let lines_owned = [
        user_line("Build the project"),
        assistant_text_line("Let me try building..."),
        assistant_tool_line(&[("Read", "/Cargo.toml")]),
        dur_result(
            "error[E0308]: mismatched types\n  --> src/main.rs:42",
            true,
            "2026-10-21T10:00:00Z",
        ),
        assistant_text_line("There was a compilation error."),
    ];
    let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();

    let ep = extract_episode(&lines, "sess-err", "my-project");

    assert_eq!(ep.outcome, "failed");
    assert!(!ep.error_signatures.is_empty());
    assert!(ep
        .error_signatures
        .iter()
        .any(|s| s.contains("error[E0308]")));
}

#[test]
fn duration_is_the_span_between_the_first_and_last_row_times() {
    let ep = extract(&[
        dur_typed(
            "Fix the login validation before the release",
            "2026-10-21T10:00:30Z",
        ),
        dur_done("2026-10-21T10:05:00Z"),
        dur_typed(
            "Now update the changelog for that fix",
            "2026-10-21T12:15:00Z",
        ),
        dur_done("2026-10-21T12:20:45Z"),
    ]);
    assert_eq!(ep.duration_minutes, 140);
    // Rows with no time say nothing: 0, never a guess.
    let ep = extract(&[
        typed_line("Fix the login validation before the release", "typed"),
        assistant_text_line("Done."),
    ]);
    assert_eq!(ep.duration_minutes, 0);
}

#[test]
fn a_long_session_is_not_failed_by_errors_it_worked_through() {
    let early_error = || {
        [
            dur_typed(
                "Build the project and fix what breaks",
                "2026-10-21T10:00:00Z",
            ),
            dur_result(
                "error[E0308]: mismatched types",
                true,
                "2026-10-21T10:01:00Z",
            ),
            dur_done("2026-10-21T10:02:00Z"),
            dur_typed(
                "Now update the changelog for that fix",
                "2026-10-21T11:00:00Z",
            ),
        ]
    };
    // Errors only under the first task, a clean close: not "failed".
    let mut rows = early_error().to_vec();
    rows.push(dur_result(
        "changelog written",
        false,
        "2026-10-21T11:01:00Z",
    ));
    rows.push(dur_timed(
        serde_json::json!({"type": "assistant", "message": {
            "content": [{"type": "text", "text": "Updated."}], "stop_reason": "end_turn"
        }}),
        "2026-10-21T11:02:00Z",
    ));
    let ep = extract(&rows);
    assert!(!ep.error_signatures.is_empty(), "the signatures stay");
    assert!(!ep.late_errors);
    // The closing task finished and left nothing failing.
    assert_eq!(ep.outcome, "success");
    // An error under the closing task, no success: still "failed".
    let mut rows = early_error().to_vec();
    rows.push(dur_result(
        "error[E0599]: no method named x",
        true,
        "2026-10-21T11:01:00Z",
    ));
    let ep = extract(&rows);
    assert!(ep.late_errors);
    assert_eq!(ep.outcome, "failed");
    // Output that merely contains the words (a Read of source) is not a failed call.
    let mut rows = early_error().to_vec();
    rows.push(dur_result(
        "// Never panics. FAILED is a test name; Error: is a variant",
        false,
        "2026-10-21T11:01:00Z",
    ));
    assert!(!extract(&rows).late_errors);
    // A typed prompt that merely mentions FAILED is the person's words.
    let ep = extract(&[
        dur_typed(
            "Why does the suite print FAILED on windows",
            "2026-10-21T11:00:00Z",
        ),
        dur_done("2026-10-21T11:01:00Z"),
    ]);
    assert!(!ep.late_errors);
}

#[test]
fn a_failed_call_counts_whatever_its_message_says() {
    // A shell failure prints no error[/Error:/panic pattern at all.
    let ep = extract(&[
        dur_typed("Run the release script and report", "2026-10-21T10:00:00Z"),
        dur_result("Exit code 1", true, "2026-10-21T10:01:00Z"),
        dur_result("File not found", true, "2026-10-21T10:02:00Z"),
        dur_done("2026-10-21T10:03:00Z"),
    ]);
    assert!(ep.late_errors);
    assert_eq!(ep.outcome, "failed");
}

#[test]
fn a_success_said_under_an_earlier_task_does_not_close_the_session() {
    let ep = extract(&[
        dur_typed(
            "Fix the login validation before the release",
            "2026-10-21T10:00:00Z",
        ),
        dur_timed(
            serde_json::json!({"type": "assistant", "message": {
                "content": [{"type": "text", "text": "Fixed. All tests pass."}],
                "stop_reason": "end_turn"
            }}),
            "2026-10-21T10:02:00Z",
        ),
        dur_typed(
            "Now update the changelog for that fix",
            "2026-10-21T11:00:00Z",
        ),
        dur_result("changelog opened", false, "2026-10-21T11:01:00Z"),
    ]);
    assert!(!ep.closing_success, "the latest task has said nothing yet");
    assert_ne!(ep.outcome, "success");
    // The same words said under the closing task do count.
    let ep = extract(&[
        dur_typed(
            "Fix the login validation before the release",
            "2026-10-21T10:00:00Z",
        ),
        dur_done("2026-10-21T10:02:00Z"),
        dur_typed(
            "Now update the changelog for that fix",
            "2026-10-21T11:00:00Z",
        ),
        dur_timed(
            serde_json::json!({"type": "assistant", "message": {
                "content": [{"type": "text", "text": "Done. The changelog is updated."}],
                "stop_reason": "end_turn"
            }}),
            "2026-10-21T11:02:00Z",
        ),
    ]);
    assert!(ep.closing_success);
    assert_eq!(ep.outcome, "success");
}

#[test]
fn the_task_dir_override_keeps_the_closing_tasks_success_not_the_sessions() {
    let mut ep = extract(&[
        dur_typed(
            "Fix the login validation before the release",
            "2026-10-21T10:00:00Z",
        ),
        dur_timed(
            serde_json::json!({"type": "assistant", "message": {
                "content": [{"type": "text", "text": "Fixed. All tests pass."}],
                "stop_reason": "end_turn"
            }}),
            "2026-10-21T10:02:00Z",
        ),
        dur_typed(
            "Now update the changelog for that fix",
            "2026-10-21T11:00:00Z",
        ),
        dur_result("changelog opened", false, "2026-10-21T11:01:00Z"),
    ]);
    // The session-wide `completed` still holds the early success.
    assert!(crate::extraction::has_success_signal(&ep.completed) || ep.completed.is_empty());
    apply_task_dir_todos(&mut ep, Vec::new());
    assert_ne!(ep.outcome, "success");
    apply_task_dir_todos(
        &mut ep,
        vec![TodoItem {
            content: "write the changelog".into(),
            status: "pending".into(),
        }],
    );
    assert_eq!(ep.next_steps.as_deref(), Some("write the changelog"));
    assert_ne!(ep.outcome, "success");
}

#[test]
fn latest_ask_is_the_literal_last_typed_message_and_never_walks_back() {
    let ep = extract(&[
        dur_typed(
            "Fix the login validation before the release",
            "2026-10-21T10:00:00Z",
        ),
        dur_done("2026-10-21T10:02:00Z"),
        dur_typed(
            "Why did the second worker finish so early",
            "2026-10-21T10:30:00Z",
        ),
        dur_done("2026-10-21T10:32:00Z"),
    ]);
    assert_eq!(
        ep.latest_ask.as_deref(),
        Some("Why did the second worker finish so early")
    );
    // An acknowledgement is still the last thing typed: no walking back.
    let ep = extract(&[
        dur_typed(
            "Fix the login validation before the release",
            "2026-10-21T10:00:00Z",
        ),
        dur_done("2026-10-21T10:02:00Z"),
        dur_typed("ok go", "2026-10-21T10:30:00Z"),
    ]);
    assert_eq!(ep.latest_ask.as_deref(), Some("ok go"));
    // A sidechain row and a tool result are not typed messages.
    let mut side = serde_json::json!({"type": "user", "promptSource": "typed",
        "isSidechain": true, "message": {"content": "Summarize the diff for the agent"}});
    side["timestamp"] = serde_json::json!("2026-10-21T10:40:00Z");
    let ep = extract(&[
        dur_typed(
            "Fix the login validation before the release",
            "2026-10-21T10:00:00Z",
        ),
        dur_done("2026-10-21T10:02:00Z"),
        side.to_string(),
        dur_result("done", false, "2026-10-21T10:41:00Z"),
    ]);
    assert_eq!(
        ep.latest_ask.as_deref(),
        Some("Fix the login validation before the release")
    );
    // No typed row at all: nothing.
    assert!(extract(&[dur_done("2026-10-21T10:02:00Z")])
        .latest_ask
        .is_none());
}

#[test]
fn a_last_typed_row_without_prose_clears_the_ask_it_never_walks_back() {
    let ask = "Why did the second worker finish so early?";
    // A command-only wrapper typed after the question.
    let ep = extract(&[
        dur_typed(ask, "2026-10-21T10:00:00Z"),
        dur_done("2026-10-21T10:02:00Z"),
        dur_typed(
            "<command-name>/clear</command-name>\n<command-message>clear</command-message>",
            "2026-10-21T10:30:00Z",
        ),
    ]);
    assert!(ep.latest_ask.is_none(), "{:?}", ep.latest_ask);
    // A recap pasted back.
    let ep = extract(&[
        dur_typed(ask, "2026-10-21T10:00:00Z"),
        dur_done("2026-10-21T10:02:00Z"),
        dur_typed(
            "recap [3m ago]: Fix the composer. [[CSR:RECAP]]",
            "2026-10-21T10:30:00Z",
        ),
    ]);
    assert!(ep.latest_ask.is_none(), "{:?}", ep.latest_ask);
    // A tool result is not a typed row: the question stands.
    let ep = extract(&[
        dur_typed(ask, "2026-10-21T10:00:00Z"),
        dur_result("ok", false, "2026-10-21T10:02:00Z"),
    ]);
    assert_eq!(ep.latest_ask.as_deref(), Some(ask));
}

#[test]
fn a_sanitized_tool_result_carrier_does_not_clear_the_ask() {
    let ask = "Why did the second worker finish so early?";
    let csr_call = serde_json::json!({"type": "assistant", "message": {"content": [
        {"type": "tool_use", "id": "csr-1", "name": "csr_reflect_on_past", "input": {"query": "history"}}
    ], "stop_reason": "end_turn"}});
    // The sanitizer drops this CSR-only result and leaves an empty content array.
    let carrier = serde_json::json!({"type": "user", "message": {"content": [
        {"type": "tool_result", "tool_use_id": "csr-1", "content": "Error: not found in CSR memory"}
    ]}});
    let ep = extract(&[
        dur_typed(ask, "2026-10-21T10:00:00Z"),
        dur_timed(csr_call, "2026-10-21T10:01:00Z"),
        dur_timed(carrier, "2026-10-21T10:02:00Z"),
    ]);
    assert_eq!(ep.latest_ask.as_deref(), Some(ask));
    // A typed row with nothing in it is not a message either.
    let ep = extract(&[
        dur_typed(ask, "2026-10-21T10:00:00Z"),
        dur_typed("   ", "2026-10-21T10:02:00Z"),
    ]);
    assert_eq!(ep.latest_ask.as_deref(), Some(ask));
}

#[test]
fn a_cut_ask_is_never_stored() {
    let long = format!("Can you check {}?", "界".repeat(150));
    assert!(long.len() > LATEST_ASK_MAX_BYTES);
    let ep = extract(&[
        dur_typed(&long, "2026-10-21T10:00:00Z"),
        dur_done("2026-10-21T10:02:00Z"),
    ]);
    assert!(ep.latest_ask.is_none(), "{:?}", ep.latest_ask);
    // A short first paragraph in front of a long message is kept whole.
    let text = format!(
        "Why did the second worker finish so early?\n\n{}",
        "detail ".repeat(100)
    );
    let ep = extract(&[
        dur_typed(&text, "2026-10-21T10:00:00Z"),
        dur_done("2026-10-21T10:02:00Z"),
    ]);
    assert_eq!(
        ep.latest_ask.as_deref(),
        Some("Why did the second worker finish so early?")
    );
}

#[test]
fn only_a_message_the_request_check_accepts_resets_the_outcome() {
    use crate::hooks::recap::is_request;
    let closing = |remark: &str| {
        extract(&[
            dur_typed(
                "Fix the login validation before the release",
                "2026-10-21T10:00:00Z",
            ),
            dur_timed(
                serde_json::json!({"type": "assistant", "message": {
                    "content": [{"type": "text", "text": "Fixed. All tests pass."}],
                    "stop_reason": "end_turn"
                }}),
                "2026-10-21T10:02:00Z",
            ),
            dur_typed(remark, "2026-10-21T10:10:00Z"),
        ])
    };
    // Remarks: the request check says no, so the success stands.
    for remark in [
        "Thanks for fixing the build",
        "Great work helping us fix the build",
    ] {
        assert!(!is_request(remark), "{remark}");
        let ep = closing(remark);
        assert!(ep.closing_success, "{remark}");
        assert_eq!(ep.outcome, "success", "{remark}");
    }
    // An ask the check accepts starts over.
    let ask = "Can you bump the version?";
    assert!(is_request(ask));
    assert!(!closing(ask).closing_success);
    // "Great now fix the tests": whatever the check says decides it, with
    // no second rule to override it.
    let mixed = "Great now fix the tests";
    assert_eq!(!is_request(mixed), closing(mixed).closing_success);
    // A remark after a failure does not forgive it.
    let ep = extract(&[
        dur_typed("Run the release script and report", "2026-10-21T10:00:00Z"),
        dur_result("Exit code 1", true, "2026-10-21T10:01:00Z"),
        dur_done("2026-10-21T10:02:00Z"),
        dur_typed("thanks, that was useful", "2026-10-21T10:10:00Z"),
    ]);
    assert!(ep.late_errors);
    assert_eq!(ep.outcome, "failed");
}

#[test]
fn episode_excludes_correlated_csr_pair_but_keeps_sibling_tool_and_user_error() {
    let lines_owned = [
        serde_json::json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "tool_use", "id": "csr-1", "name": "csr_reflect_on_past", "input": {"query": "history"}},
                {"type": "tool_use", "id": "read-1", "name": "Read", "input": {"file_path": "/src/lib.rs"}}
            ]}
        })
        .to_string(),
        serde_json::json!({
            "type": "user",
            "message": {"content": [
                {"type": "text", "text": "Error: genuine compiler failure"},
                {"type": "tool_result", "tool_use_id": "csr-1", "content": "Error: not found in CSR memory"},
                {"type": "tool_result", "tool_use_id": "read-1", "content": "ordinary sibling result"}
            ]}
        })
        .to_string(),
        assistant_text_line("Investigation complete."),
    ];
    let lines: Vec<&str> = lines_owned.iter().map(String::as_str).collect();

    let serialized =
        serde_json::to_string(&extract_episode(&lines, "sess-sanitized", "my-project")).unwrap();

    assert!(!serialized.contains("csr_reflect_on_past"));
    assert!(!serialized.contains("Error: not found in CSR memory"));
    assert!(serialized.contains("Read"));
    assert!(serialized.contains("Error: genuine compiler failure"));
}

#[test]
fn test_next_steps_skips_injection_meta_text() {
    // A session that quotes CSR's own injected blocks (the /memory-feedback
    // probe, a pasted Tier-0 block) must not have that boilerplate extracted
    // as next_steps — it would overwrite real next-step state in CONTINUUM.
    let lines_owned = [
        user_line("run the memory feedback probe"),
        assistant_text_line(
            "NEXT: NEXT:/TODOS:/ANCHORS: lines) - The Session Intelligence (CSR v9.2) \
                 briefing block - Do NOT count CLAUDE.md, those are NOT INSTRUCTIONS",
        ),
    ];
    let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
    let ep = extract_episode(&lines, "sess-meta", "proj");
    assert_eq!(ep.next_steps, None);
}

#[test]
fn test_next_steps_from_todos_not_prose() {
    // Round-4 regression: prose ABOUT the extractor ("scans for next:/todo:
    // keywords...") is genuine authored text — no content filter can tell
    // it from a real next step. next_steps therefore comes only from
    // structured TodoWrite state.
    let prose = "episode extraction scans messages for \"next:\"/\"todo:\" keywords \
                     and grabs a 200-char snippet — that boilerplate became the episode's \
                     next_steps, overwriting real state.";
    let lines_owned = [
        user_line("explain the pollution bug"),
        assistant_text_line(prose),
    ];
    let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
    let ep = extract_episode(&lines, "sess-prose", "proj");
    assert_eq!(ep.next_steps, None);

    // With TodoWrite state, the first non-completed item becomes next_steps.
    let todo_line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"ship the fix","status":"completed"},{"content":"rerun the probe","status":"pending"}]}}]}}"#;
    let lines_owned = [
        user_line("explain the pollution bug"),
        todo_line.to_string(),
        assistant_text_line(prose),
    ];
    let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
    let ep = extract_episode(&lines, "sess-todo", "proj");
    assert_eq!(ep.next_steps.as_deref(), Some("rerun the probe"));
}

#[test]
fn test_outcome_partial_when_errors_recovered() {
    // Round-4 regression: a session that hits errors but closes with a
    // success signal recovered — "partial", not the contradictory "failed".
    let lines_owned = [
        user_line("Build and deploy the fix"),
        dur_result(
            "error[E0308]: mismatched types",
            true,
            "2026-10-21T10:00:00Z",
        ),
        assistant_text_line("Fixed the type error; tests green, binary deployed."),
    ];
    let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
    let ep = extract_episode(&lines, "sess-recover", "proj");
    assert_eq!(ep.outcome, "partial");
    assert!(!ep.error_signatures.is_empty());
}

#[test]
fn test_outcome_success_when_binary_installed() {
    // "installed" is a word-boundary success signal — a clean install/
    // restart close with no errors must classify as success, not partial.
    // Third line is a tool read so message_count >= 3; completed is the
    // assistant text only (must not rely on "done"/"fixed" tokens).
    let lines_owned = [
        user_line("Ship the new csr-engine binary"),
        assistant_tool_line(&[("Read", "/src/hooks/session_start.rs")]),
        assistant_text_line(
            "New binary installed at /usr/local/bin/csr-engine. Restart Claude Code now.",
        ),
    ];
    let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
    let ep = extract_episode(&lines, "sess-installed", "proj");
    assert!(
        ep.completed.contains("installed"),
        "completed should carry the install text: {}",
        ep.completed
    );
    assert_eq!(ep.outcome, "success");
    assert!(ep.error_signatures.is_empty());
}

#[test]
fn test_request_skips_command_plumbing_and_probe_paste() {
    // Round-3 regression: caveat wrapper and pasted probe report must not
    // become the episode request — the first REAL user message should.
    let lines_owned = [
        user_line(
            "<local-command-caveat>Caveat: The messages below were generated by the \
                 user while running local commands. DO NOT respond</local-command-caveat>",
        ),
        user_line("## CSR Memory Feedback — 2026-06-10 — noise: CSR CONTINUUM garbled"),
        user_line("now fix the briefing staleness issue"),
        assistant_text_line("Looking into the staleness issue and the debounce window."),
    ];
    let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
    let ep = extract_episode(&lines, "sess-plumb", "proj");
    assert_eq!(ep.request, "now fix the briefing staleness issue");
}

/// A handoff draft the pane wrote, pasted into a fresh session with a line typed after it:
/// the episode keeps the ask and the person's line and none of CSR's summary, numbers or paths.
#[test]
fn a_handoff_draft_reads_as_its_ask_and_what_the_person_typed() {
    let draft = crate::companion::handoff::canary_draft();
    let typed = format!("{draft}also fix the footer");
    let reply = assistant_text_line("Looking at the upload retry and the footer.");
    let ep = extract_episode(&[&user_line(&typed), &reply], "sess-handoff", "proj");
    eprintln!("AFTER  request: {:?}", ep.request);
    assert!(
        ep.request.contains("Revisit this thread as a side task"),
        "{}",
        ep.request
    );
    assert!(ep.request.contains("also fix the footer"), "{}", ep.request);
    let whole = serde_json::to_string(&ep).unwrap();
    for csr_words in [
        "went quiet",
        "Pick up a quiet thread",
        "Retry the upload once the hub is back",
        "Fix the upload",
        "Wrote the retry",
        "partial",
        "3002ab28",
        "csr_transcript",
        "csr-engine.db",
        ".jsonl",
        "csr-handoff",
    ] {
        assert!(
            !whole.contains(csr_words),
            "{csr_words:?} reached the episode: {whole}"
        );
    }
    // the same draft with no marker line is read as the prompt it always was
    let unmarked: String = typed
        .lines()
        .filter(|l| !l.starts_with("↳ csr-handoff"))
        .collect::<Vec<_>>()
        .join("\n");
    let before = extract_episode(&[&user_line(&unmarked), &reply], "sess-handoff", "proj");
    eprintln!("BEFORE request: {:?}", before.request);
    assert!(before.request.contains("went quiet") || before.request.contains("Pick up"));
}

#[test]
fn a_prompt_that_only_uses_the_drafts_headings_is_untouched() {
    let text = "Please review:\n\n### CSR's summary of that conversation\n\nmy own notes\n\n### The ask\n\nship it";
    let reply = assistant_text_line("Reviewing the notes now.");
    let ep = extract_episode(&[&user_line(text), &reply], "sess-prose", "proj");
    assert_eq!(ep.request, text);
}

#[test]
fn test_completed_keeps_prose_drops_quoted_injection_tokens() {
    // Round-3 regression: assistant summary quoting `NEXT: none recorded`
    // in backticks polluted LAST. Quoted tokens go; prose stays.
    let lines_owned = [
        user_line("verify the continuum filter"),
        assistant_text_line(
            "`NEXT: none recorded` — polluted boilerplate filtered, \
                 including episodes already in the DB.",
        ),
    ];
    let lines: Vec<&str> = lines_owned.iter().map(|s| s.as_str()).collect();
    let ep = extract_episode(&lines, "sess-quote", "proj");
    assert!(!ep.completed.contains("NEXT:"));
    assert!(ep.completed.contains("polluted boilerplate filtered"));
}

#[test]
fn test_episode_to_json_roundtrip() {
    let ep = Episode {
        schema: "v1".to_string(),
        session_id: "sess-rt".to_string(),
        project: "roundtrip-proj".to_string(),
        timestamp: "2026-05-17T00:00:00+00:00".to_string(),
        request: "Fix the bug".to_string(),
        investigated: vec!["/src/main.rs".to_string()],
        completed: "Bug is fixed and done.".to_string(),
        next_steps: Some("Deploy to production".to_string()),
        blockers: None,
        outcome: "success".to_string(),
        error_signatures: vec![],
        tools_used: vec!["Read".to_string(), "Edit".to_string()],
        files_modified: vec!["/src/main.rs".to_string()],
        message_count: 10,
        duration_minutes: 5,
        todos: vec![],
        approved_plan: None,
        prev_episode_id: None,
        anchors: vec![],
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
    };

    let json = serde_json::to_string(&ep).unwrap();
    let deserialized: Episode = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.schema, "v1");
    assert_eq!(deserialized.session_id, "sess-rt");
    assert_eq!(deserialized.project, "roundtrip-proj");
    assert_eq!(deserialized.request, "Fix the bug");
    assert_eq!(deserialized.investigated, vec!["/src/main.rs"]);
    assert_eq!(deserialized.completed, "Bug is fixed and done.");
    assert_eq!(
        deserialized.next_steps,
        Some("Deploy to production".to_string())
    );
    assert_eq!(deserialized.blockers, None);
    assert_eq!(deserialized.outcome, "success");
    assert!(deserialized.error_signatures.is_empty());
    assert_eq!(deserialized.tools_used, vec!["Read", "Edit"]);
    assert_eq!(deserialized.files_modified, vec!["/src/main.rs"]);
    assert_eq!(deserialized.message_count, 10);
    assert_eq!(deserialized.duration_minutes, 5);
}

#[test]
fn test_episode_tags() {
    let ep = Episode {
        schema: "v1".to_string(),
        session_id: "sess-abc".to_string(),
        project: "cool-project".to_string(),
        timestamp: String::new(),
        request: String::new(),
        investigated: vec![],
        completed: String::new(),
        next_steps: None,
        blockers: None,
        outcome: "partial".to_string(),
        error_signatures: vec![],
        tools_used: vec![],
        files_modified: vec![],
        message_count: 0,
        duration_minutes: 0,
        todos: vec![],
        approved_plan: None,
        prev_episode_id: None,
        anchors: vec![],
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
    };

    let tags = episode_tags(&ep);

    assert_eq!(tags.len(), 4);
    assert!(tags.contains(&"session_episode".to_string()));
    assert!(tags.contains(&"schema_v2".to_string()));
    assert!(tags.contains(&"project_cool-project".to_string()));
    assert!(tags.contains(&"conv_sess-abc".to_string()));
}

#[test]
fn episode_v2_extracts_todos_and_plan() {
    let lines = vec![
        r#"{"type":"user","message":{"content":"fix the auth bug"}}"#,
        r###"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"ExitPlanMode","input":{"plan":"## Plan\n1. Fix validate_token\n2. Add test"}}]}}"###,
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"fix validate_token","status":"completed"},{"content":"add regression test","status":"pending"}]}}]}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Fixed the validation."}]}}"#,
    ];
    let ep = extract_episode(&lines, "sess-1", "proj");
    assert_eq!(ep.schema, "v2");
    assert_eq!(ep.todos.len(), 2);
    assert_eq!(ep.todos[1].content, "add regression test");
    assert_eq!(ep.todos[1].status, "pending");
    let plan = ep.approved_plan.expect("plan captured");
    assert!(plan.contains("Fix validate_token"));
    assert!(ep.prev_episode_id.is_none());
    assert!(ep.anchors.is_empty());
}

#[test]
fn episode_support_keeps_intent_span_and_observed_plan_below_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let raw = concat!(
        r#"{"type":"user","uuid":"u","message":{"content":"fix auth"}}"#,
        "\n",
        r#"{"type":"assistant","uuid":"a","message":{"content":[{"type":"tool_use","name":"ExitPlanMode","input":{"plan":"Fix auth then test"}}]}}"#,
        "\n"
    );
    std::fs::write(&path, raw).unwrap();
    let storage = crate::storage::Storage::open_memory().unwrap();
    let episode = extract_episode(&raw.lines().collect::<Vec<_>>(), "session", "project");
    let inputs = crate::import::transcript_inputs(&storage, &path, "session").unwrap();
    let content = serde_json::to_string(&episode).unwrap();
    let (support, ranges) = episode_support(&episode, &content, inputs, &path, None);
    let plan = support
        .inputs()
        .iter()
        .position(|i| i.channel() == "observed_exit_plan_mode")
        .unwrap();
    assert_eq!(
        support.inputs()[plan].trust(),
        crate::provenance::TrustTier::TrustedTool
    );
    assert!(content
        .chars()
        .skip(ranges[plan].0)
        .take(ranges[plan].1 - ranges[plan].0)
        .collect::<String>()
        .contains("Fix auth"));
    assert!(support
        .inputs()
        .iter()
        .zip(&ranges)
        .any(|(i, r)| i.channel() == "user_message" && i.text() == "fix auth" && r.0 > 0));
    assert!(support.floor() <= crate::provenance::TrustTier::TrustedTool);
}

#[test]
fn episode_v2_keeps_last_todowrite_only() {
    let lines = vec![
        r#"{"type":"user","message":{"content":"task"}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"old","status":"pending"}]}}]}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[{"content":"new","status":"in_progress"}]}}]}}"#,
    ];
    let ep = extract_episode(&lines, "sess-1", "proj");
    assert_eq!(ep.todos.len(), 1);
    assert_eq!(ep.todos[0].content, "new");
}

#[test]
fn chain_link_finds_previous_episode_id() {
    // Pure-logic test for the helper that picks prev episode from candidates:
    // (session_id, timestamp) pairs, excluding the current session.
    let candidates = vec![
        ("sess-old".to_string(), "2026-06-09T10:00:00Z".to_string()),
        (
            "sess-current".to_string(),
            "2026-06-10T09:00:00Z".to_string(),
        ),
        ("sess-mid".to_string(), "2026-06-10T08:00:00Z".to_string()),
    ];
    assert_eq!(
        pick_prev_episode(&candidates, "sess-current"),
        Some("sess-mid".to_string())
    );
    assert_eq!(pick_prev_episode(&[], "x"), None);
}

#[test]
fn episode_v1_and_v2_without_instrumentation_still_deserialize() {
    // v1: no todos/approved_plan/prev_episode_id/anchors, and (this
    // extension) no instrumentation fields either.
    let v1 = r#"{"schema":"v1","session_id":"s","project":"p","timestamp":"t",
            "request":"r","investigated":[],"completed":"c","next_steps":null,
            "blockers":null,"outcome":"partial","error_signatures":[],"tools_used":[],
            "files_modified":[],"message_count":1,"duration_minutes":0}"#;
    let ep: Episode = serde_json::from_str(v1).expect("v1 compat");
    assert!(ep.todos.is_empty());
    assert!(ep.approved_plan.is_none());
    assert!(ep.error_count.is_none());
    assert!(ep.top_errors.is_empty());
    assert!(ep.steer_count.is_none());
    assert!(ep.steers.is_empty());

    // v2: has the working-state fields but predates instrumentation
    // (plan §4.1) — must still deserialize, with the four new fields
    // defaulting to "never measured", not a deserialize error.
    let v2 = r#"{"schema":"v2","session_id":"s","project":"p","timestamp":"t",
            "request":"r","investigated":[],"completed":"c","next_steps":null,
            "blockers":null,"outcome":"success","error_signatures":[],"tools_used":[],
            "files_modified":[],"message_count":1,"duration_minutes":0,
            "todos":[{"content":"x","status":"completed"}],"approved_plan":null,
            "prev_episode_id":null,"anchors":[]}"#;
    let ep2: Episode = serde_json::from_str(v2).expect("v2 (pre-instrumentation) compat");
    assert_eq!(ep2.todos.len(), 1);
    assert!(ep2.error_count.is_none());
    assert!(ep2.top_errors.is_empty());
    assert!(ep2.steer_count.is_none());
    assert!(ep2.steers.is_empty());
    assert!(ep2.correction_count.is_none());
}

#[test]
fn a_snapshot_cut_before_the_closing_row_reads_as_an_unfinished_turn() {
    let cut = stop_race_without_closing_row();
    let rows: Vec<&str> = cut.lines().collect();
    let early = extract_episode(&rows, "b046750d", "proj");
    assert!(!early.closing_ended);
    assert!(early.completed.starts_with("Gate is running"));

    let all: Vec<&str> = STOP_RACE.lines().collect();
    let whole = extract_episode(&all, "b046750d", "proj");
    assert!(whole.closing_ended);
    assert!(whole.completed.starts_with("**Done: af7e9645"));
}

#[test]
fn only_a_detached_worker_waits_so_an_inline_stop_never_holds_claude_code() {
    let main: HookInput =
        serde_json::from_value(serde_json::json!({"session_id": "s-main"})).unwrap();
    let sub: HookInput =
        serde_json::from_value(serde_json::json!({"session_id": "s-sub", "agent_id": "agent-1"}))
            .unwrap();
    assert!(close_wait_applies(&main, true));
    assert!(!close_wait_applies(&main, false));
    assert!(!close_wait_applies(&sub, true));
}

#[test]
fn the_tail_closes_the_turn_only_once_the_closing_reply_is_in() {
    let tmp = tempfile::TempDir::new().unwrap();
    let path = tmp.path().join("t.jsonl");
    std::fs::write(&path, stop_race_without_closing_row()).unwrap();
    assert!(!tail_closes_turn(&path));
    std::fs::write(&path, STOP_RACE).unwrap();
    assert!(tail_closes_turn(&path));
}
