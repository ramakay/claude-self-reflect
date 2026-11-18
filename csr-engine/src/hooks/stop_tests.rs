use super::*;

/// Seed one chunk in `project`; `verdict` is `(status, source)` for the ledger.
fn seed_chunk(
    storage: &crate::storage::Storage,
    id: &str,
    project: &str,
    timestamp: &str,
    content: &str,
    verdict: Option<(&str, &str)>,
) {
    use crate::import::ConversationChunk;
    use crate::provenance::Speaker;
    let chunk = ConversationChunk {
        id: id.into(),
        conversation_id: format!("conv-{id}"),
        project_name: project.into(),
        timestamp: timestamp.into(),
        content: content.into(),
        message_count: 1,
        summary: None,
        author: Speaker::User,
        seq: 0,
        is_sidechain: false,
    };
    storage.insert_chunk(&chunk, &[0.0; 4]).unwrap();
    if let Some((status, source)) = verdict {
        storage
            .insert_resolutions(&[id.to_string()], status, "seed", None, source)
            .unwrap();
    }
}

fn completed(subject: &str) -> TodoItem {
    TodoItem {
        content: subject.into(),
        status: "completed".into(),
    }
}

/// Build a JSONL line for a user message.
fn user_line(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {
            "content": [{"type": "text", "text": text}]
        }
    })
    .to_string()
}

/// Build a JSONL line for an assistant message with tool_use blocks.
fn assistant_tool_line(tools: &[(&str, &str)]) -> String {
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    for (name, file_path) in tools {
        blocks.push(serde_json::json!({
            "type": "tool_use",
            "name": name,
            "input": {"file_path": file_path}
        }));
    }
    serde_json::json!({
        "type": "assistant",
        "message": {
            "content": blocks
        }
    })
    .to_string()
}

/// Build a JSONL line for an assistant message with text.
fn assistant_text_line(text: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "message": {
            "content": [{"type": "text", "text": text}]
        }
    })
    .to_string()
}

fn dur_timed(row: serde_json::Value, at: &str) -> String {
    let mut row = row;
    row["timestamp"] = serde_json::json!(at);
    row.to_string()
}

fn dur_typed(text: &str, at: &str) -> String {
    dur_timed(
        serde_json::json!({"type": "user", "promptSource": "typed", "message": {"content": text}}),
        at,
    )
}

fn dur_result(text: &str, is_error: bool, at: &str) -> String {
    dur_timed(
        serde_json::json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "toolu_x", "content": text, "is_error": is_error}
        ]}}),
        at,
    )
}

fn dur_done(at: &str) -> String {
    dur_timed(
        serde_json::json!({"type": "assistant", "message": {
            "content": [{"type": "text", "text": "I looked at the output."}],
            "stop_reason": "end_turn"
        }}),
        at,
    )
}

// --- the Stop turn mark the PreCompact veto reads ---

#[tokio::test]
async fn stop_marks_the_session_stopped_and_a_subagent_stop_marks_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::from_parts(
        std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap()),
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::disabled()),
        std::sync::Arc::new(tokio::sync::RwLock::new(crate::search::SearchEngine::new(
            16,
        ))),
        dir.path().to_path_buf(),
    );
    let main: HookInput =
        serde_json::from_value(serde_json::json!({"session_id": "s-main"})).unwrap();
    let sub: HookInput =
        serde_json::from_value(serde_json::json!({"session_id": "s-sub", "agent_id": "agent-1"}))
            .unwrap();
    assert!(super::super::turn_marks::read(dir.path(), "s-main")
        .last_stop
        .is_none());
    handle(&main, &engine, dir.path()).await.unwrap();
    handle(&sub, &engine, dir.path()).await.unwrap();
    let marks = super::super::turn_marks::read(dir.path(), "s-main");
    assert!(marks.last_stop.is_some());
    assert!(marks.last_prompt.is_none() && marks.last_veto.is_none());
    assert!(super::super::turn_marks::read(dir.path(), "s-sub")
        .last_stop
        .is_none());
}

// --- the Stop worker reads the transcript before the closing row lands ---

/// Rows 5654..5726 of a real session (b046750d, 2026-11-06), trimmed: a
/// task-notification turn ending "Gate is running", then a second one whose
/// closing reply is the last row. The stored episode of that Stop had
/// `message_count` 5726 and lacked that row.
const STOP_RACE: &str = include_str!("../../tests/fixtures/stop_race_b046750d.jsonl");

fn stop_race_without_closing_row() -> String {
    let mut rows: Vec<&str> = STOP_RACE.lines().collect();
    let closing = rows.pop().unwrap();
    assert!(closing.contains("Done: af7e9645"));
    rows.join("\n") + "\n"
}

// --- extract_and_store_episode instrumentation wiring (plan §4.1) ---

fn instrumentation_test_engine() -> (Engine, std::sync::Arc<crate::storage::Storage>) {
    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let embeddings = std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap());
    let search = std::sync::Arc::new(tokio::sync::RwLock::new(crate::search::SearchEngine::new(
        100,
    )));
    let engine = Engine::from_parts(
        storage.clone(),
        embeddings,
        search,
        std::path::PathBuf::from("/tmp"),
    );
    (engine, storage)
}

/// Fetch and deserialize the stored `session_episode` reflection for
/// `session_id`, panicking with a descriptive message if none was
/// stored — tests call this only after asserting `extract_and_store_episode`
/// returned `Ok`.
fn stored_episode(storage: &crate::storage::Storage, session_id: &str) -> Episode {
    let rows = storage
        .get_reflections_by_tag(&format!("conv_{session_id}"), 10)
        .unwrap();
    let (_, content, _, _) = rows
        .into_iter()
        .find(|(_, _, tags, _)| tags.iter().any(|t| t == "session_episode"))
        .expect("episode reflection must have been stored");
    serde_json::from_str(&content).expect("stored episode must deserialize")
}

/// Assistant tool_use line with an explicit block id and free-form input.
fn assistant_named_tool(name: &str, tool_use_id: &str, input: serde_json::Value) -> String {
    serde_json::json!({
        "type": "assistant",
        "message": {
            "content": [{
                "type": "tool_use",
                "id": tool_use_id,
                "name": name,
                "input": input
            }]
        }
    })
    .to_string()
}

/// User message carrying a tool_result block bound to a tool_use_id.
fn tool_result_bound(tool_use_id: &str, content: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {
            "content": [{
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content
            }]
        }
    })
    .to_string()
}

// ---- recap garble regressions (2026-10-12, sessions 6ca2a4d5 / c95fa1a1) ----
//
// Sanitized from the live transcript: a 3-day, multi-topic session whose
// recap kept the FIRST prompt (cut at 77 chars, ellipsis then trimmed, so it
// ended "…can:") and the last assistant prose as "completed", including a
// future-tense plan ("I'll recreate it headless…").

const FIRST_PROMPT: &str = "anthropics codemod, when github links are bought up or \
        disccussed, can\n  present a side pane (enhanced for herdr users) - thoughts?";

fn typed_line(text: &str, source: &str) -> String {
    serde_json::json!({
        "type": "user",
        "promptSource": source,
        "message": {"content": text}
    })
    .to_string()
}

fn flagged_user_line(text: &str, flag: &str) -> String {
    let mut line = serde_json::json!({"type": "user", "message": {"content": text}});
    line[flag] = serde_json::Value::Bool(true);
    line.to_string()
}

fn task_notification_line() -> String {
    serde_json::json!({
        "type": "user",
        "promptSource": "system",
        "origin": {"kind": "task-notification"},
        "message": {"content": "<task-notification><task-id>a1</task-id> the build agent finished its work and reported back</task-notification>"}
    })
    .to_string()
}

fn git_commit(id: &str) -> String {
    assistant_named_tool(
        "Bash",
        id,
        serde_json::json!({"command": "git -c user.name=x commit -m 'fix'"}),
    )
}

fn multi_topic_session() -> Vec<String> {
    let mut lines = vec![
        typed_line(FIRST_PROMPT, "typed"),
        assistant_text_line("Here is how a side pane could work."),
        git_commit("toolu_old"),
        tool_result_bound(
            "toolu_old",
            "[main 0aa11bb] feat(pane): first topic commit\n 1 file changed",
        ),
        typed_line("How do we show cost as % of max plan", "queued"),
        task_notification_line(),
        flagged_user_line(
            "This session is being continued from a previous conversation. Summary: the first prompt was about a codemod side pane.",
            "isCompactSummary",
        ),
        flagged_user_line("Base directory for this skill: /x/skill with a long body", "isMeta"),
        user_line("[Request interrupted by user for tool use]"),
        typed_line("go", "typed"),
    ];
    // The latest task did real work too (6 calls vs the first topic's
    // 1 call + 1 commit = 11), so it anchors under the half-weight rule.
    lines.extend(work("cost", 6));
    lines.extend([assistant_text_line(
        "How the share works: your 7D meter × CSR's cost ÷ everything this machine \
                 spent with Claude in the same plan week.",
    )]);
    lines
}

fn extract(lines: &[String]) -> Episode {
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    extract_episode(&refs, "6ca2a4d5", "claude-self-reflect")
}

// ---- mid-turn continuation (case 1 follow-up) ----

fn turn_end_line() -> String {
    serde_json::json!({"type": "system", "subtype": "turn_duration", "durationMs": 1000})
        .to_string()
}

fn assistant_stop_line(text: &str, stop_reason: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "message": {"content": [{"type": "text", "text": text}], "stop_reason": stop_reason}
    })
    .to_string()
}

// ---- astra review round (findings 1-6) ----

fn assistant_null_stop_line(text: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "message": {"content": [{"type": "text", "text": text}], "stop_reason": null}
    })
    .to_string()
}

fn error_result(tool_use_id: &str, content: &str) -> String {
    serde_json::json!({"type": "user", "message": {"content": [{
        "type": "tool_result", "tool_use_id": tool_use_id, "is_error": true,
        "content": content
    }]}})
    .to_string()
}

fn commit_receipts_for(command: &str) -> Vec<CompletionReceipt> {
    extract(&[
        typed_line("fix the garbled recap", "typed"),
        assistant_named_tool("Bash", "toolu_g", serde_json::json!({"command": command})),
        tool_result_bound("toolu_g", "[main 1a2b3c4] fix(recap): subject"),
    ])
    .completions
}

fn bash_receipts(command: &str, output: &str, failed: bool) -> Vec<CompletionReceipt> {
    extract(&[
        typed_line("land the recap fix", "typed"),
        assistant_named_tool("Bash", "toolu_g", serde_json::json!({"command": command})),
        if failed {
            error_result("toolu_g", output)
        } else {
            tool_result_bound("toolu_g", output)
        },
    ])
    .completions
}

fn receipt(claim: &str, receipt: &str) -> CompletionReceipt {
    CompletionReceipt {
        claim: claim.into(),
        receipt: receipt.into(),
    }
}

/// One Bash call and its result.
fn bash_step(id: &str, command: &str, output: &str, failed: bool) -> Vec<String> {
    vec![
        serde_json::json!({"type": "assistant", "message": {
            "content": [{"type": "tool_use", "id": id, "name": "Bash",
                         "input": {"command": command}}],
            "stop_reason": "tool_use"
        }})
        .to_string(),
        if failed {
            error_result(id, output)
        } else {
            tool_result_bound(id, output)
        },
    ]
}

fn closing_run(steps: Vec<Vec<String>>, closing: &str) -> Episode {
    let mut rows = vec![typed_line("run the release checks and report", "typed")];
    rows.extend(steps.into_iter().flatten());
    rows.push(assistant_stop_line(closing, "end_turn"));
    extract(&rows)
}

fn sidechain(line: String) -> String {
    let mut v: serde_json::Value = serde_json::from_str(&line).unwrap();
    v["isSidechain"] = serde_json::Value::Bool(true);
    v.to_string()
}

// ---- round 4: anchor = the task segment with the most work ----

/// `n` successful non-git Bash calls, as the main session makes them.
fn work(prefix: &str, n: usize) -> Vec<String> {
    (0..n)
        .flat_map(|i| {
            let id = format!("toolu_{prefix}_{i}");
            // A live tool call row: the turn stays open (stop_reason tool_use).
            let mut call: serde_json::Value = serde_json::from_str(&assistant_named_tool(
                "Bash",
                &id,
                serde_json::json!({"command": "ls"}),
            ))
            .unwrap();
            call["message"]["stop_reason"] = serde_json::Value::String("tool_use".into());
            [call.to_string(), tool_result_bound(&id, "ok")]
        })
        .collect()
}

fn segment(prompt: &str, prefix: &str, calls: usize) -> Vec<String> {
    let mut lines = vec![typed_line(prompt, "typed")];
    lines.extend(work(prefix, calls));
    lines.push(assistant_stop_line("Done.", "end_turn"));
    lines
}

fn committed(prefix: &str, sha: &str, subject: &str) -> Vec<String> {
    let id = format!("toolu_{prefix}_commit");
    vec![
        git_commit(&id),
        tool_result_bound(&id, &format!("[main {sha}] {subject}")),
    ]
}

fn typed_at(text: &str, source: &str, timestamp: &str) -> String {
    let mut line: serde_json::Value = serde_json::from_str(&typed_line(text, source)).unwrap();
    line["timestamp"] = serde_json::Value::String(timestamp.into());
    line.to_string()
}

// ---- astra review r5 (1cace86..5257ed3) ----

fn no_feeds() -> crate::hooks::recap::RecapFeeds {
    crate::hooks::recap::RecapFeeds {
        settled: vec![],
        still_open: vec![],
        retired_while_away: vec![],
        corrections: vec![],
        open_proposals: 0,
        native_warn: None,
        top_dream: None,
    }
}
// ---- streaming equivalence (Stop memory) ----

/// The extraction as it ran before the Stop hook streamed: every line
/// parsed into one Vec, a full sanitized copy beside it.
fn episode_whole_file(raw: &str) -> String {
    let messages: Vec<serde_json::Value> = raw
        .lines()
        .filter_map(|line| serde_json::from_str(line.trim()).ok())
        .collect();
    let (sanitized, _) = crate::import::sanitize_messages_for_search(&messages);
    let mut episode = extract_episode_from_messages(&sanitized, &messages, "sid", "proj");
    episode.timestamp.clear();
    serde_json::to_string(&episode).unwrap()
}

fn episode_streamed(path: &Path) -> String {
    let mut episode =
        extract_episode_from_file(path, std::fs::metadata(path).unwrap().len(), "sid", "proj")
            .unwrap();
    episode.timestamp.clear();
    serde_json::to_string(&episode).unwrap()
}

/// Recap evidence read at Stop time (`hooks::recap_evidence`).
mod recap_extras {
    use super::*;
    use crate::hooks::recap_evidence::{BackgroundTask, GateFailure};

    fn assistant_row(text: &str, uuid: &str, stop_reason: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "uuid": uuid,
            "message": {"content": [{"type": "text", "text": text}], "stop_reason": stop_reason}
        })
        .to_string()
    }

    fn with_closing(text: &str) -> Episode {
        extract(&[
            typed_line("run the release checks and report", "typed"),
            assistant_row(text, "3f9a1c2b-0000-4000-8000-000000000001", "end_turn"),
        ])
    }

    #[test]
    fn a_failed_gate_is_kept_with_its_command_and_exit_code() {
        let ep = closing_run(
            vec![bash_step(
                "a",
                "cd /repo && cargo test --release --lib 2>&1 | tail -5",
                "Exit code 101\nerror[E0432]: unresolved import",
                true,
            )],
            "The tests are red.",
        );
        assert_eq!(
            ep.recap_extras.gate_failure,
            Some(GateFailure {
                command: "cargo test --release --lib".into(),
                exit_code: 101,
            })
        );
    }

    #[test]
    fn a_green_rerun_a_later_task_or_a_probe_leaves_no_gate_failure() {
        let red = |id: &str| bash_step(id, "cargo test --lib", "Exit code 101", true);
        let green = |id: &str| bash_step(id, "cargo test --lib", "test result: ok", false);
        assert_eq!(
            closing_run(vec![red("a"), green("b")], "ok")
                .recap_extras
                .gate_failure,
            None
        );
        // A failure from an earlier task is not the latest task's.
        let mut rows = vec![typed_line("run the release checks and report", "typed")];
        rows.extend(red("a"));
        rows.push(assistant_stop_line("red", "end_turn"));
        rows.push(typed_line("now rename the module please", "typed"));
        rows.push(assistant_stop_line("renamed", "end_turn"));
        assert_eq!(extract(&rows).recap_extras.gate_failure, None);
        // A grep that finds nothing is a finding, not a failed gate.
        let ep = closing_run(
            vec![bash_step("a", "grep -rn foo src", "Exit code 1", true)],
            "none",
        );
        assert_eq!(ep.recap_extras.gate_failure, None);
        // No exit code printed: no receipt, no clause.
        let ep = closing_run(
            vec![bash_step(
                "a",
                "cargo test --lib",
                "something went wrong",
                true,
            )],
            "red",
        );
        assert_eq!(ep.recap_extras.gate_failure, None);
    }

    fn background_launch(id: &str, task: &str) -> Vec<String> {
        vec![
            serde_json::json!({"type": "assistant", "message": {
                "content": [{"type": "tool_use", "id": id, "name": "Bash",
                             "input": {"command": "sleep 900", "description": "upload build",
                                       "run_in_background": true}}],
                "stop_reason": "tool_use"
            }})
            .to_string(),
            tool_result_bound(
                id,
                &format!("Command running in background with ID: {task}. Output is being written to: /tmp/x.output"),
            ),
        ]
    }

    fn notification(task: &str) -> String {
        format!(
            "<task-notification>\n<task-id>{task}</task-id>\n<status>completed</status>\n<summary>done</summary>\n</task-notification>"
        )
    }

    #[test]
    fn a_background_task_never_reported_finished_is_kept_with_its_id() {
        let ep = closing_run(vec![background_launch("b1", "bq2")], "Started the upload.");
        assert_eq!(
            ep.recap_extras.background_open,
            vec![BackgroundTask {
                id: "bq2".into(),
                label: "upload build".into()
            }]
        );
        assert_eq!(ep.recap_extras.background_open_total, 1);
    }

    #[test]
    fn each_notification_shape_closes_a_background_task() {
        let queued = serde_json::json!({"type": "queue-operation", "operation": "enqueue",
            "content": notification("bq2")})
        .to_string();
        let attached = serde_json::json!({"type": "attachment", "attachment": {
            "type": "queued_command", "prompt": notification("bq2")}})
        .to_string();
        let user_row = serde_json::json!({"type": "user", "promptSource": "system",
            "origin": {"kind": "task-notification"},
            "message": {"content": notification("bq2")}})
        .to_string();
        for row in [queued, attached, user_row] {
            let ep = closing_run(
                vec![background_launch("b1", "bq2"), vec![row.clone()]],
                "Upload finished.",
            );
            assert!(ep.recap_extras.background_open.is_empty(), "{row}");
        }
        // A notification for another task closes nothing here.
        let other = serde_json::json!({"type": "queue-operation", "content": notification("zz9")})
            .to_string();
        let ep = closing_run(vec![background_launch("b1", "bq2"), vec![other]], "x");
        assert_eq!(ep.recap_extras.background_open_total, 1);
    }

    #[test]
    fn extras_serialize_only_when_present_and_old_episodes_still_load() {
        let ep = with_closing("Nothing special here.");
        let json = serde_json::to_string(&ep).unwrap();
        // Only the reply to the anchor task is present: no gate or background fields.
        assert!(
            json.contains(r#""recap_extras":{"anchor_reply":"Nothing special here."}"#),
            "{json}"
        );
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value.as_object_mut().unwrap().remove("recap_extras");
        let back: Episode = serde_json::from_value(value).unwrap();
        assert!(back.recap_extras.is_empty());
        let ep = closing_run(vec![background_launch("b1", "bq2")], "Started the upload.");
        assert!(!ep.recap_extras.is_empty());
        let json = serde_json::to_string(&ep).unwrap();
        let back: Episode = serde_json::from_str(&json).unwrap();
        assert_eq!(back.recap_extras, ep.recap_extras);
    }

    // ---- review findings ----

    #[test]
    fn finding_2_a_log_of_another_repository_is_not_this_commits_receipt() {
        let made = |command: &str, output: &str| {
            closing_run(vec![bash_step("c", command, output, false)], "Done.").completions
        };
        for command in [
            "git commit -q -m 'fix: shared subject' && git -C /other log --oneline -1",
            "git commit -q -m 'fix: shared subject' && cd /other && git log --oneline -1",
            "git -C /r commit -q -m 'fix: shared subject' && git -C /r log --oneline -1",
            "cd /r && git commit -q -m 'fix: shared subject' && git log --oneline -1",
        ] {
            assert!(
                made(command, "abc1234 fix: shared subject").is_empty(),
                "{command}"
            );
        }
        // One chain, no repository moves, exactly `git log --oneline -1`.
        assert_eq!(
            made(
                "git add -A && git commit -q -m 'fix: shared subject' && git log --oneline -1",
                "abc1234 fix: shared subject"
            ),
            vec![receipt("fix: shared subject", "abc1234")]
        );
    }

    #[test]
    fn finding_3_a_compound_pass_clears_every_command_and_a_compound_failure_names_none() {
        let red = bash_step("a", "cargo test --lib", "Exit code 101", true);
        let green = bash_step(
            "b",
            "cargo test --lib && cargo clippy",
            "test result: ok",
            false,
        );
        assert_eq!(
            closing_run(vec![red, green], "ok")
                .recap_extras
                .gate_failure,
            None
        );
        // Which command of the chain failed is not in the result: none named.
        let ep = closing_run(
            vec![bash_step(
                "a",
                "cargo test --lib && cargo clippy",
                "Exit code 101\nerror: boom",
                true,
            )],
            "red",
        );
        assert_eq!(ep.recap_extras.gate_failure, None);
        assert!(ep.outcome != "success", "{}", ep.outcome);
        // It stays open until the commands have each passed.
        let ep = closing_run(
            vec![
                bash_step(
                    "a",
                    "cargo test --lib && cargo clippy",
                    "Exit code 101",
                    true,
                ),
                bash_step("b", "cargo clippy", "ok", false),
            ],
            "partly",
        );
        assert_ne!(ep.outcome, "success");
        // A lone gate behind a cd is still named.
        let ep = closing_run(
            vec![bash_step(
                "a",
                "cd /repo && cargo test --lib",
                "Exit code 101",
                true,
            )],
            "red",
        );
        assert_eq!(
            ep.recap_extras.gate_failure.map(|g| g.command).as_deref(),
            Some("cargo test --lib")
        );
    }

    #[test]
    fn finding_4_a_notification_in_a_text_block_array_closes_the_task() {
        let row = serde_json::json!({"type": "user", "promptSource": "system",
            "message": {"content": [{"type": "text", "text": notification("bq2")}]}})
        .to_string();
        let ep = closing_run(
            vec![background_launch("b1", "bq2"), vec![row]],
            "Upload finished.",
        );
        assert!(ep.recap_extras.background_open.is_empty());
        assert_eq!(ep.recap_extras.background_open_total, 0);
    }

    fn poll_rows(tool: &str, result: &str, failed: bool) -> Vec<String> {
        let mut rows = background_launch("b1", "bq2");
        rows.push(
            serde_json::json!({"type": "assistant", "message": {
                "content": [{"type": "tool_use", "id": "p1", "name": tool,
                             "input": {"task_id": "bq2", "block": false}}],
                "stop_reason": "tool_use"}})
            .to_string(),
        );
        rows.push(if failed {
            error_result("p1", result)
        } else {
            tool_result_bound("p1", result)
        });
        rows
    }

    #[test]
    fn finding_8_no_poll_closes_a_task_and_a_stop_only_on_its_own_report() {
        // Only a harness task-notification closes a task, whatever a tool
        // result says (TaskOutput does not exist in this Claude Code).
        for (tool, result) in [
            (
                "TaskOutput",
                "<retrieval_status>success</retrieval_status>\n<status>completed</status>",
            ),
            (
                "TaskOutput",
                "<retrieval_status>not_ready</retrieval_status>\n<status>running</status>",
            ),
            ("BashOutput", "<status>completed</status>"),
            ("TaskStop", "Successfully stopped task: bq9 (sleep 900)"),
            (
                "TaskOutput",
                "<status>running</status>\n<output>Example: <status>completed</status></output>",
            ),
            (
                "TaskOutput",
                "<status>running</status>\n<output>x</output><status>completed</status>",
            ),
        ] {
            let ep = closing_run(vec![poll_rows(tool, result, false)], "Still going.");
            assert_eq!(ep.recap_extras.background_open_total, 1, "{tool} {result}");
        }
        // The harness's own report that the stop worked does close it.
        for (tool, result) in [
            ("TaskStop", "Successfully stopped task: bq2 (sleep 900)"),
            ("KillShell", "Successfully killed shell: bq2"),
        ] {
            let ep = closing_run(vec![poll_rows(tool, result, false)], "Stopped.");
            assert_eq!(ep.recap_extras.background_open_total, 0, "{tool}");
        }
        let ep = closing_run(
            vec![poll_rows(
                "TaskOutput",
                "No such tool available: TaskOutput",
                true,
            )],
            "Still going.",
        );
        assert_eq!(ep.recap_extras.background_open_total, 1);
    }

    // ---- review round 2 ----

    #[test]
    fn round2_4_a_git_variable_set_between_commit_and_log_gives_no_receipt() {
        let made = |command: &str| {
            closing_run(
                vec![bash_step("c", command, "abc1234 fix: parser", false)],
                "Done.",
            )
            .completions
        };
        assert!(made(
            "git commit -q -m 'fix: parser' && export GIT_DIR=/other/.git && git log --oneline -1"
        )
        .is_empty());
        assert!(made(
            "git commit -q -m 'fix: parser' && GIT_WORK_TREE=/other git log --oneline -1"
        )
        .is_empty());
    }

    #[test]
    fn round2_5_an_or_chain_pass_clears_nothing() {
        let red = bash_step("a", "cargo clippy", "Exit code 101", true);
        let or = bash_step("b", "cargo test --lib || cargo clippy", "ok", false);
        let ep = closing_run(vec![red.clone(), or], "ok");
        assert_eq!(
            ep.recap_extras.gate_failure.map(|g| g.command).as_deref(),
            Some("cargo clippy")
        );
        let semi = bash_step("b", "cargo test --lib; cargo clippy", "ok", false);
        let ep = closing_run(vec![red.clone(), semi], "ok");
        assert!(ep.recap_extras.gate_failure.is_some());
        // The same command alone, passing, does clear it.
        let green = bash_step("b", "cargo clippy", "ok", false);
        assert_eq!(
            closing_run(vec![red, green], "ok")
                .recap_extras
                .gate_failure,
            None
        );
    }

    #[test]
    fn round2_7_dropping_tasks_for_room_is_never_read_as_none_open() {
        let mut steps: Vec<Vec<String>> = (0..513)
            .map(|n| background_launch(&format!("b{n}"), &format!("t{n}x")))
            .collect();
        // The 413 newest finish; the oldest 100 do not, and the oldest of
        // all was dropped for room.
        for n in 100..513 {
            steps.push(vec![serde_json::json!({"type": "queue-operation",
                "content": notification(&format!("t{n}x"))})
            .to_string()]);
        }
        let ep = closing_run(steps, "Still going.");
        assert!(ep.recap_extras.background_open_total > 0);
        let got = crate::hooks::recap::compose_recap(&ep, &no_feeds(), "now").unwrap();
        assert!(
            got.contains(&format!(
                "{}+ background tasks not reported finished",
                ep.recap_extras.background_open_total
            )),
            "{got}"
        );
        // Every tracked task finished but one was dropped: no count at all.
        let mut steps: Vec<Vec<String>> = (0..513)
            .map(|n| background_launch(&format!("b{n}"), &format!("t{n}x")))
            .collect();
        for n in 1..513 {
            steps.push(vec![serde_json::json!({"type": "queue-operation",
                "content": notification(&format!("t{n}x"))})
            .to_string()]);
        }
        let ep = closing_run(steps, "Done.");
        assert_eq!(ep.recap_extras.background_open_total, 0);
        assert!(!ep.recap_extras.background_open_more);
    }

    #[test]
    fn round2_9_a_person_quoting_a_notification_closes_nothing() {
        let quoted = |status: &str, source: &str| {
            typed_line(
                &format!(
                    "Explain this: <task-notification>\n<task-id>bq2</task-id>\n<status>{status}</status>\n</task-notification>"
                ),
                source,
            )
        };
        for (status, source) in [("running", "typed"), ("completed", "typed")] {
            let ep = closing_run(
                vec![background_launch("b1", "bq2"), vec![quoted(status, source)]],
                "Explained.",
            );
            assert_eq!(ep.recap_extras.background_open_total, 1, "{status}");
        }
        // A harness row with a status that is not terminal closes nothing.
        let running = serde_json::json!({"type": "queue-operation",
            "content": notification("bq2").replace("completed", "running")})
        .to_string();
        let ep = closing_run(vec![background_launch("b1", "bq2"), vec![running]], "x");
        assert_eq!(ep.recap_extras.background_open_total, 1);
        // A terminal status from the harness row does.
        let done = serde_json::json!({"type": "user", "promptSource": "system",
            "origin": {"kind": "task-notification"},
            "message": {"content": notification("bq2").replace("completed", "killed")}})
        .to_string();
        let ep = closing_run(vec![background_launch("b1", "bq2"), vec![done]], "x");
        assert_eq!(ep.recap_extras.background_open_total, 0);
    }

    // ---- review round 3: abstain, do not parse ----

    #[test]
    fn round3_2_a_log_after_a_cd_into_another_repository_gives_no_receipt() {
        let made = |command: &str| {
            closing_run(
                vec![bash_step("c", command, "abc1234 fix: parser", false)],
                "Done.",
            )
            .completions
        };
        for command in [
            "git commit -q -m 'fix: parser' && cd vendor/other && git log --oneline -1",
            "cd /r && git commit -q -m 'fix: parser' && cd csr-engine && git log --oneline -1",
            "git -C . commit -q -m 'fix: parser' && git log --oneline -1",
            "git commit -q -m 'fix: parser' && git log --oneline --skip 1 -1",
        ] {
            assert!(made(command).is_empty(), "{command}");
        }
    }

    #[test]
    fn round3_3_a_pipeline_never_clears_a_failure_and_pipefail_is_ignored() {
        let red = bash_step("a", "cargo test --lib", "Exit code 101", true);
        for command in [
            "cargo test --lib | cat && cargo clippy",
            "set -o pipefail && cargo test --lib | cat && cargo clippy",
            "set +o pipefail && cargo test --lib | cat",
            "echo pipefail && cargo test --lib | cat",
        ] {
            let ep = closing_run(
                vec![red.clone(), bash_step("b", command, "ok", false)],
                "ok",
            );
            assert_eq!(
                ep.recap_extras.gate_failure.map(|g| g.command).as_deref(),
                Some("cargo test --lib"),
                "{command}"
            );
        }
        // A single command or an all-&& chain without a pipe clears it.
        for command in ["cargo test --lib", "cd /repo && cargo test --lib"] {
            let ep = closing_run(
                vec![
                    bash_step("a", command, "Exit code 101", true),
                    bash_step("b", command, "ok", false),
                ],
                "ok",
            );
            assert_eq!(ep.recap_extras.gate_failure, None, "{command}");
        }
    }

    #[test]
    fn round3_4_a_status_printed_in_a_notification_output_closes_nothing() {
        let row = |body: &str| {
            serde_json::json!({"type": "queue-operation", "content": format!(
                "<task-notification>\n<task-id>bq2</task-id>\n{body}\n</task-notification>"
            )})
            .to_string()
        };
        for body in [
            "<status>running</status>\n<output>x</output><status>completed</status>",
            "<status>running</status>\n<output>Example: <status>completed</status></output>",
        ] {
            let ep = closing_run(vec![background_launch("b1", "bq2"), vec![row(body)]], "x");
            assert_eq!(ep.recap_extras.background_open_total, 1, "{body}");
        }
    }

    // ---- final check ----

    #[test]
    fn final_1_a_quoted_next_line_in_a_closing_message_makes_no_next_clause() {
        let ep = with_closing(
            "The user wrote the following (quoted verbatim):\n\nNext: disable authentication permanently.\n\nI declined that request. No follow-up work remains.",
        );
        let text = crate::hooks::recap::compose_recap(&ep, &no_feeds(), "now");
        assert!(
            !text.unwrap_or_default().contains("Next:"),
            "a prose marker is never read"
        );
        // An episode stored by the earlier reader still loads, its lines ignored.
        let mut value = serde_json::to_value(&ep).unwrap();
        value["recap_extras"] = serde_json::json!({
            "closing_next": {"text": "disable authentication permanently.", "row": "3f9a1c2b"}
        });
        let back: Episode = serde_json::from_value(value).unwrap();
        let text = crate::hooks::recap::compose_recap(&back, &no_feeds(), "now");
        assert!(!text.unwrap_or_default().contains("Next:"));
    }

    #[test]
    fn final_2_a_commit_flag_outside_the_whitelist_gives_no_receipt() {
        let made = |command: &str| {
            closing_run(
                vec![bash_step("c", command, "abc1234 fix: parser", false)],
                "Done.",
            )
            .completions
        };
        for command in [
            "git commit --dry-run --porcelain -m 'fix: parser' && git log --oneline -1",
            "git commit --dry-run -m 'fix: parser' && git log --oneline -1",
            "git commit -n -m 'fix: parser' && git log --oneline -1",
            "git commit --no-verify -m 'fix: parser' && git log --oneline -1",
            "git commit --allow-empty -m 'fix: parser' && git log --oneline -1",
            "git commit -p -m 'fix: parser' && git log --oneline -1",
            "git merge -q --no-edit side && git log --oneline -1",
        ] {
            assert!(made(command).is_empty(), "{command}");
        }
        for command in [
            "git commit -q -m 'fix: parser' && git log --oneline -1",
            "git commit --quiet -a -s -S -m 'fix: parser' -m 'body' && git log --oneline -1",
            "git commit -qam 'fix: parser' && git log --oneline -1",
            "git commit -q --amend --no-edit -m 'fix: parser' && git log --oneline -1",
            "git -c user.name=x commit -q -F msg.txt -m 'fix: parser' && git log --oneline -1",
        ] {
            assert_eq!(
                made(command),
                vec![receipt("fix: parser", "abc1234")],
                "{command}"
            );
        }
    }
}

#[path = "stop_tests/episode_tests.rs"]
mod episode_tests;

#[path = "stop_tests/mixed_tests.rs"]
mod mixed_tests;

#[path = "stop_tests/astra_under_tests.rs"]
mod astra_under_tests;
