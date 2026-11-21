use super::*;

#[test]
fn edits_alone_are_no_receipt() {
    let edit = |id: &str, path: &str| {
        assistant_named_tool("Edit", id, serde_json::json!({"file_path": path}))
    };
    // A session that only edited files finished nothing a receipt proves.
    let ep = extract(&[
        typed_line("tighten the recap composer", "typed"),
        edit("e1", "/repo/src/hooks/stop.rs"),
        tool_result_bound("e1", "The file has been updated"),
    ]);
    assert!(ep.completions.is_empty());
    assert!(crate::hooks::recap::displayed_receipts(&ep.completions).is_empty());
}

/// A session that asks for `ask`, runs `steps`, and closes with `closing`.
#[test]
fn a_success_word_past_the_shown_sentence_still_counts_for_the_outcome() {
    let first = format!("{}.", "word ".repeat(32).trim_end());
    let closing = format!(
        "{first} The input validation bug is fixed and then {}",
        "x ".repeat(100)
    );
    let ep = closing_run(vec![], &closing);
    assert_eq!(ep.completed, first);
    assert!(ep.closing_success, "{}", ep.completed);
}

#[test]
fn a_session_that_committed_composes_a_recap_from_the_commit() {
    let ep = closing_run(
        vec![bash_step(
            "c",
            "git commit -q -m 'fix(recap): name the commit'",
            "[main 1a2b3c4] fix(recap): name the commit\n 1 file changed",
            false,
        )],
        "Committed.",
    );
    let got = crate::hooks::recap::compose_recap(&ep, &no_feeds(), "now").unwrap();
    assert_eq!(
        got,
        "recap [now]: run the release checks and report. Latest: fix(recap): name the commit (1a2b3c4). [[CSR:RECAP]]"
    );
    // Everything it says is re-import suppressed.
    assert!(crate::extraction::provenance::is_csr_emission(&got));
    assert!(crate::extraction::provenance::extractable(&got).is_none());
}

#[test]
fn a_commit_made_under_a_later_ask_is_not_shown_beside_the_earlier_one() {
    let ep = extract(&[
        typed_line("run the release checks and report", "typed"),
        assistant_stop_line("The checks pass.", "end_turn"),
        typed_line("now land the fix for the stale lock in the daemon", "typed"),
        assistant_named_tool(
            "Bash",
            "c",
            serde_json::json!({"command": "git commit -m 'fix(daemon): stale lock'"}),
        ),
        tool_result_bound("c", "[main 1a2b3c4] fix(daemon): stale lock"),
        assistant_stop_line("Committed.", "end_turn"),
    ]);
    assert_eq!(
        ep.completions,
        vec![receipt("fix(daemon): stale lock", "1a2b3c4")]
    );
    assert_eq!(ep.anchor_receipts, Some(vec![]));
    let got = crate::hooks::recap::compose_recap(&ep, &no_feeds(), "now").unwrap();
    let head = got.lines().next().unwrap_or_default();
    assert!(head.starts_with("recap [now]: run the release checks and report."));
    assert!(!head.contains("1a2b3c4"), "{got}");
    // The commit is session evidence and is shown on its own Settled line.
    assert!(
        got.contains("Settled: fix(daemon): stale lock (1a2b3c4)"),
        "{got}"
    );
    // The commit stays on the episode for the companion's own reading.
    assert_eq!(
        crate::hooks::recap::displayed_receipts(&ep.completions).len(),
        1
    );
}

#[test]
fn a_task_created_under_an_earlier_ask_is_not_the_later_asks_result() {
    let task = |name: &str, id: &str, input: serde_json::Value, result: &str| {
        vec![
            assistant_named_tool(name, id, input),
            tool_result_bound(id, result),
        ]
    };
    let mut rows = vec![typed_line(
        "plan the daemon rewrite for the stale lock",
        "typed",
    )];
    rows.extend(task(
        "TaskCreate",
        "t1",
        serde_json::json!({"subject": "Review the lock fix", "description": "d"}),
        "Task #1 created successfully: Review the lock fix",
    ));
    rows.push(assistant_stop_line("Planned.", "end_turn"));
    rows.push(typed_line(
        "now run the full release checks and report",
        "typed",
    ));
    rows.extend(task(
        "TaskUpdate",
        "t2",
        serde_json::json!({"taskId": "1", "status": "completed"}),
        "Updated task #1 status",
    ));
    rows.push(assistant_stop_line("Done.", "end_turn"));
    let ep = extract(&rows);
    assert_eq!(
        ep.completions,
        vec![receipt("Review the lock fix", "task #1 completed")]
    );
    // The task was created for the first ask, not the one that closed it.
    assert_eq!(ep.anchor_receipts, Some(vec![0]));
}

#[test]
fn two_tasks_with_one_subject_each_belong_to_their_own_ask() {
    let create = |id: &str, n: u32| {
        vec![
            assistant_named_tool(
                "TaskCreate",
                id,
                serde_json::json!({"subject": "Run tests", "description": "d"}),
            ),
            tool_result_bound(id, &format!("Task #{n} created successfully: Run tests")),
        ]
    };
    let complete = |id: &str, n: u32| {
        vec![
            assistant_named_tool(
                "TaskUpdate",
                id,
                serde_json::json!({"taskId": n.to_string(), "status": "completed"}),
            ),
            tool_result_bound(id, "Updated task"),
        ]
    };
    let mut rows = vec![typed_line(
        "plan the daemon rewrite for the stale lock",
        "typed",
    )];
    rows.extend(create("a1", 1));
    rows.push(assistant_stop_line("Planned.", "end_turn"));
    rows.push(typed_line(
        "now run the full release checks and report",
        "typed",
    ));
    rows.extend(create("b1", 2));
    rows.extend(complete("b2", 2));
    rows.push(assistant_stop_line("Done.", "end_turn"));
    let ep = extract(&rows);
    assert_eq!(
        ep.completions,
        vec![receipt("Run tests", "task #2 completed")]
    );
    // Task 2 was created and finished under the second ask; the first
    // ask's recap must not claim it.
    assert_eq!(ep.anchor_receipts, Some(vec![]));
    assert_eq!(
        ep.compact_anchor.as_ref().and_then(|a| a.receipts.clone()),
        Some(vec![0])
    );
    // The same holds for a rewritten todo list.
    let write = |id: &str, status: &str| {
        vec![
            assistant_named_tool(
                "TodoWrite",
                id,
                serde_json::json!({"todos": [{"content": "Run tests", "status": status}]}),
            ),
            tool_result_bound(id, "ok"),
        ]
    };
    let mut rows = vec![typed_line(
        "plan the daemon rewrite for the stale lock",
        "typed",
    )];
    rows.extend(write("w1", "pending"));
    rows.push(assistant_stop_line("Planned.", "end_turn"));
    rows.push(typed_line(
        "now run the full release checks and report",
        "typed",
    ));
    rows.extend(write("w2", "pending"));
    rows.extend(write("w3", "completed"));
    rows.push(assistant_stop_line("Done.", "end_turn"));
    let ep = extract(&rows);
    assert_eq!(ep.completions, vec![receipt("Run tests", "todo completed")]);
    assert_eq!(ep.anchor_receipts, Some(vec![]));
}

#[test]
fn a_session_with_no_receipt_composes_no_completed_clause() {
    // A question answered from reads: nothing was committed, edited or
    // finished, so the recap says what was asked and nothing more.
    let ep = closing_run(
        vec![bash_step(
            "r",
            "git log --oneline -3",
            "1a2b3c4 older",
            false,
        )],
        "The last three commits are listed.",
    );
    assert!(ep.completions.is_empty());
    let got = crate::hooks::recap::compose_recap(&ep, &no_feeds(), "now").unwrap();
    assert_eq!(
        got,
        "recap [now]: run the release checks and report. [[CSR:RECAP]]"
    );
    // And with no typed request at all there is nothing to compose.
    let mut empty = ep.clone();
    empty.request = String::new();
    empty.anchor_request = None;
    empty.latest_ask = None;
    empty.completed = String::new();
    assert!(crate::hooks::recap::compose_recap(&empty, &no_feeds(), "now").is_none());
}

#[test]
fn a_failed_call_that_was_run_again_and_passed_is_not_a_failure() {
    let ep = closing_run(
        vec![
            bash_step(
                "a",
                "cd csr-engine && cargo test --lib 2>&1 | tail -5",
                "Exit code 101",
                true,
            ),
            bash_step(
                "b",
                "cd csr-engine && cargo test --lib",
                "test result: ok",
                false,
            ),
        ],
        "The tests pass after the fix.",
    );
    assert!(!ep.late_errors && !ep.final_failed);
    assert_eq!(ep.outcome, "success");
    assert_eq!(ep.outcome_version, Some(OUTCOME_VERSION));
    // The same failure with no second run is a failure the session ended on.
    let ep = closing_run(
        vec![bash_step("a", "cargo test --lib", "Exit code 101", true)],
        "The tests are red.",
    );
    assert!(ep.late_errors && ep.final_failed);
    assert_eq!(ep.outcome, "failed");
    // A different operation of the same tool does not clear it.
    let ep = closing_run(
        vec![
            bash_step("a", "cargo test --lib", "Exit code 101", true),
            bash_step("b", "cargo build --release", "Finished", false),
        ],
        "Built.",
    );
    // A failed test run is not cleared by a build that passed: the tests
    // still fail.
    assert!(ep.late_errors && ep.final_failed);
    assert_eq!(ep.outcome, "failed");
}

#[test]
fn a_failed_edit_is_cleared_by_a_later_edit_of_the_same_file() {
    let edit = |id: &str, path: &str, failed: bool| {
        vec![
            assistant_named_tool("Edit", id, serde_json::json!({"file_path": path})),
            if failed {
                error_result(id, "String to replace not found")
            } else {
                tool_result_bound(id, "The file has been updated")
            },
        ]
    };
    let ep = closing_run(
        vec![edit("a", "/r/a.rs", true), edit("b", "/r/a.rs", false)],
        "Done.",
    );
    assert_eq!(ep.outcome, "success");
    let ep = closing_run(
        vec![edit("a", "/r/a.rs", true), edit("b", "/r/b.rs", false)],
        "Done.",
    );
    // The failed edit stays on the record; the session went on and finished.
    assert!(ep.late_errors && !ep.final_failed);
    assert_eq!(ep.outcome, "success");
}

#[test]
fn a_probe_that_finds_nothing_is_not_a_failure() {
    let ep = closing_run(
        vec![
            bash_step("a", "grep -rn flaky_test src/", "", true),
            bash_step("b", "ls /tmp/missing 2>&1", "No such file", true),
            bash_step(
                "c",
                "curl -sf https://example.test/health",
                "Exit code 22",
                true,
            ),
            bash_step("d", "git log --oneline -3 | grep nothing", "", true),
        ],
        "Nothing matches.",
    );
    assert!(!ep.late_errors && !ep.final_failed, "{ep:?}");
    assert_eq!(ep.outcome, "success");
}

#[test]
fn an_unresolved_test_failure_survives_a_later_unrelated_success() {
    let commit = || bash_step("c", "git commit -q -m wip", "[main 1a2b3c4] wip", false);
    let red = |id: &str, command: &str| bash_step(id, command, "Exit code 101", true);
    let green = |id: &str, command: &str| bash_step(id, command, "ok", false);
    // Commit WIP, fail the tests, format successfully, stop.
    let ep = closing_run(
        vec![commit(), red("t", "cargo test"), green("f", "cargo fmt")],
        "Committed; the tests still fail.",
    );
    assert!(ep.final_failed && ep.closing_receipt);
    assert_eq!(ep.outcome, "partial");
    // Without a commit the same ending is a failure.
    let ep = closing_run(
        vec![red("t", "cargo test"), green("f", "cargo fmt")],
        "The tests still fail.",
    );
    assert_eq!(ep.outcome, "failed");
    // The same tests run again and passing resolve it.
    let ep = closing_run(
        vec![
            commit(),
            red("t", "cargo test"),
            green("f", "cargo fmt"),
            green("g", "cargo test"),
        ],
        "Fixed and green.",
    );
    assert!(!ep.final_failed);
    assert_eq!(ep.outcome, "success");
    // A failed build or lint stays open the same way.
    for command in [
        "cargo clippy -- -D warnings",
        "cargo build --release",
        "npm test",
    ] {
        let ep = closing_run(
            vec![commit(), red("t", command), green("f", "cargo fmt")],
            "Done.",
        );
        assert_eq!(ep.outcome, "partial", "{command}");
    }
}

#[test]
fn a_test_pass_clears_only_the_failure_of_the_same_target_and_directory() {
    let commit = || bash_step("c", "git commit -q -m wip", "[main 1a2b3c4] wip", false);
    let red = |id: &str, command: &str| bash_step(id, command, "Exit code 101", true);
    let green = |id: &str, command: &str| bash_step(id, command, "ok", false);
    // A different test target is a different test.
    let ep = closing_run(
        vec![
            commit(),
            red("a", "cargo test --test broken"),
            green("b", "cargo test --test healthy"),
        ],
        "Done.",
    );
    assert!(ep.final_failed);
    assert_eq!(ep.outcome, "partial");
    // So is a different filter, or the same command in another directory.
    for (failing, passing) in [
        ("cargo test parser", "cargo test lexer"),
        ("cd /repo/a && cargo test", "cd /repo/b && cargo test"),
        ("cargo test -p engine", "cargo test -p companion"),
    ] {
        let ep = closing_run(
            vec![commit(), red("a", failing), green("b", passing)],
            "Done.",
        );
        assert_eq!(ep.outcome, "partial", "{failing} then {passing}");
    }
    // The same target run again and passing clears it, whatever the noise
    // flags, redirections and pipes around it.
    for (failing, passing) in [
        ("cargo test --test broken", "cargo test --test broken"),
        (
            "cargo test --release --test broken 2>&1 | tail -5",
            "cargo test --release --test broken -q -j4 --color always > /tmp/out.log",
        ),
        ("cd /repo/a && cargo test", "cd /repo/a && cargo test"),
    ] {
        let ep = closing_run(
            vec![commit(), red("a", failing), green("b", passing)],
            "Fixed.",
        );
        assert!(!ep.final_failed, "{failing} then {passing}");
        assert_eq!(ep.outcome, "success", "{failing} then {passing}");
    }
}

#[test]
fn a_pass_under_another_build_profile_does_not_clear_a_failure() {
    let commit = || bash_step("c", "git commit -q -m wip", "[main 1a2b3c4] wip", false);
    let red = |id: &str, command: &str| bash_step(id, command, "Exit code 101", true);
    let green = |id: &str, command: &str| bash_step(id, command, "ok", false);
    for (failing, passing) in [
        ("cargo test", "cargo test --release"),
        ("cargo test --release", "cargo test"),
        ("cargo test", "cargo test -r"),
        ("cargo test", "cargo test --profile ci"),
        ("cargo test", "cargo test --features slow"),
        ("cargo test", "cargo test --all-features"),
        ("cargo test", "cargo test --no-default-features"),
        ("cargo test", "cargo test --target aarch64-apple-darwin"),
    ] {
        let ep = closing_run(
            vec![commit(), red("a", failing), green("b", passing)],
            "Done.",
        );
        assert!(ep.final_failed, "{failing} then {passing}");
        assert_eq!(ep.outcome, "partial", "{failing} then {passing}");
    }
    // Output-shaping flags and redirections are still noise.
    let ep = closing_run(
        vec![
            commit(),
            red("a", "cargo test --release"),
            green("b", "cargo test -r -q -j 4 --color=always > /tmp/o 2>&1"),
        ],
        "Fixed.",
    );
    assert_eq!(ep.outcome, "success");
}

#[test]
fn the_directory_is_the_one_at_the_verification_command_not_the_end_of_the_chain() {
    let commit = || bash_step("c", "git commit -q -m wip", "[main 1a2b3c4] wip", false);
    let red = |id: &str, command: &str| bash_step(id, command, "Exit code 101", true);
    let green = |id: &str, command: &str| bash_step(id, command, "ok", false);
    // A pass in /repo/b, with a trailing cd back to /repo/a, is not a pass in a.
    let ep = closing_run(
        vec![
            commit(),
            red("a", "cd /repo/a && cargo test"),
            green("b", "cd /repo/b && cargo test && cd /repo/a"),
        ],
        "Done.",
    );
    assert_eq!(ep.outcome, "partial");
    // A cd after the command does not move a failure either.
    let key = |command: &str, cwd: Option<&str>| {
        call_signature("Bash", &serde_json::json!({"command": command}), cwd).failure_key
    };
    assert_eq!(
        key("cd /repo/a && cargo test && cd /repo/b", None),
        key("cd /repo/a && cargo test", None)
    );
    assert_ne!(
        key("cd /repo/b && cargo test && cd /repo/a", None),
        key("cd /repo/a && cargo test", None)
    );
}

#[test]
fn directories_are_compared_lexically_after_dot_and_dotdot() {
    let key = |command: &str, cwd: Option<&str>| {
        call_signature("Bash", &serde_json::json!({"command": command}), cwd).failure_key
    };
    let here = key("cargo test", Some("/repo"));
    assert_eq!(key("cd .. && cargo test", Some("/repo/tests")), here);
    assert_eq!(key("cd ./tests/.. && cargo test", Some("/repo")), here);
    assert_eq!(key("cd /repo/tests/../ && cargo test", None), here);
    assert_eq!(key("cargo test", Some("/repo/")), here);
    assert_ne!(key("cd tests && cargo test", Some("/repo")), here);
    assert_ne!(key("cd ../other && cargo test", Some("/repo")), here);
}

#[test]
fn a_commit_a_merge_or_an_install_lifts_a_failed_ending_to_partial() {
    let failing = || bash_step("a", "cargo test --lib", "Exit code 101", true);
    let ep = closing_run(vec![failing()], "Red.");
    assert_eq!(ep.outcome, "failed");
    // The session then committed what it had.
    let ep = closing_run(
        vec![
            failing(),
            bash_step("c", "git commit -m wip", "[main 1a2b3c4] wip", false),
        ],
        "Committed the work in progress.",
    );
    assert!(ep.closing_receipt && ep.final_failed);
    assert_eq!(ep.outcome, "partial");
    // Or installed the build, and the failure it ended on is a later install.
    let ep = closing_run(
        vec![
            bash_step("i", "csr-engine hook install --apply", "installed", false),
            bash_step("f", "cargo test --lib", "Exit code 101", true),
        ],
        "Red after the install.",
    );
    assert!(ep.final_failed && ep.closing_receipt);
    assert_eq!(ep.outcome, "partial");
    // A merged PR proves progress the same way.
    let merge = "gh pr merge 76 --squash 2>&1 | tail -3; gh pr view 76 --json state,mergeCommit -q '.state+\" \"+.mergeCommit.oid'";
    let ep = closing_run(
        vec![
            failing(),
            bash_step(
                "m",
                merge,
                "MERGED a8f845882e5068d3eed6d2ca160b182a49c23b06",
                false,
            ),
        ],
        "Merged.",
    );
    assert!(ep.closing_receipt && ep.final_failed);
    assert_eq!(ep.outcome, "partial");
}

#[test]
fn a_session_cut_off_or_interrupted_is_partial() {
    // The last row is a tool result: the turn never finished.
    let mut rows = vec![typed_line("run the release checks and report", "typed")];
    rows.extend(bash_step("a", "cargo test --lib", "test result: ok", false));
    let ep = extract(&rows);
    assert!(!ep.closing_ended);
    assert_eq!(ep.outcome, "partial");
    // The person interrupted it.
    rows.push(
        serde_json::json!({"type": "user", "message": {"content": [
            {"type": "text", "text": "[Request interrupted by user]"}
        ]}})
        .to_string(),
    );
    let ep = extract(&rows);
    assert!(!ep.closing_ended);
    assert_eq!(ep.outcome, "partial");
}

#[test]
fn a_failure_under_an_earlier_request_does_not_follow_the_session() {
    let mut rows = vec![typed_line("run the release checks and report", "typed")];
    rows.extend(bash_step("a", "cargo test --lib", "Exit code 101", true));
    rows.push(assistant_stop_line("Red.", "end_turn"));
    rows.push(typed_line(
        "what is still running on this machine?",
        "typed",
    ));
    rows.extend(bash_step("b", "ps aux | head", "PID ...", false));
    rows.push(assistant_stop_line("Nothing of ours.", "end_turn"));
    let ep = extract(&rows);
    assert!(!ep.late_errors && !ep.final_failed);
    assert_eq!(ep.outcome, "success");
}

#[test]
fn call_signatures_name_the_command_that_matters() {
    let key = |command: &str| {
        call_signature("Bash", &serde_json::json!({"command": command}), None).failure_key
    };
    assert_eq!(
        key("cd csr-engine && cargo test --lib 2>&1 | tail -5").as_deref(),
        Some("verify:cargo test --lib @csr-engine")
    );
    assert_eq!(
        key("export A=1; time cargo build --release").as_deref(),
        Some("verify:cargo build --release @")
    );
    assert_eq!(key("git push origin main").as_deref(), Some("git push"));
    assert_eq!(
        key("python3 - <<'EOF'\nprint(1)\nEOF").as_deref(),
        Some("python3")
    );
    assert_eq!(key("./gate.sh --quick").as_deref(), Some("gate.sh"));
    assert_eq!(key("grep -rn x src | head"), None);
    assert_eq!(key("git log --oneline -1 && git status"), None);
    assert_eq!(key("cd /tmp && ls"), None);
    let installs = |command: &str| {
        call_signature("Bash", &serde_json::json!({"command": command}), None).installs
    };
    for yes in [
        "./install-on-phone.sh",
        "cargo install --path .",
        "csr-engine hook install --apply",
        "bash scripts/install.sh",
        "npm install -g @openai/codex",
        "xcrun devicectl device install app --device X App.app",
    ] {
        assert!(installs(yes), "{yes}");
    }
    for no in [
        "git commit -m \"install fix\"",
        "echo install",
        "grep -r install .",
        "cat install.sh",
        "cargo test install_paths",
    ] {
        assert!(!installs(no), "{no}");
    }
    assert_eq!(
        call_signature("Read", &serde_json::json!({"file_path": "/r/a.rs"}), None).failure_key,
        None
    );
}

#[test]
fn sidechain_rows_never_touch_recap_state() {
    let ep = extract(&[
        typed_line("Fix login validation", "typed"),
        assistant_stop_line("editing", "tool_use"),
        // A legacy in-file subagent: its turn end, prompt, commit and task
        // completion are not the main session's.
        sidechain(assistant_stop_line("subagent done", "end_turn")),
        sidechain(typed_line("Refactor the whole billing module", "typed")),
        sidechain(git_commit("toolu_side")),
        sidechain(tool_result_bound(
            "toolu_side",
            "[main 5e5e5e5] refactor(billing): subagent commit",
        )),
        sidechain(assistant_named_tool(
            "TodoWrite",
            "toolu_side_todo",
            serde_json::json!({"todos": [{"content": "Billing", "status": "completed"}]}),
        )),
        sidechain(tool_result_bound(
            "toolu_side_todo",
            "Todos have been modified successfully",
        )),
        typed_line("only for the signup form", "queued"),
    ]);
    assert_eq!(
        ep.anchor_request.as_deref(),
        Some("Fix login validation only for the signup form")
    );
    assert!(ep.completions.is_empty(), "{:?}", ep.completions);
    // tools_used keeps its existing behaviour: sidechain tools still count.
    assert!(ep.tools_used.iter().any(|t| t == "Bash"));
}

#[test]
fn anchor_is_the_heaviest_segment_when_the_latest_is_light() {
    let mut lines = segment("build me a vox explainer video for the above", "a", 10);
    lines.extend(segment(
        "Will the audio cause linkedin to not play it",
        "b",
        1,
    ));
    let ep = extract(&lines);
    assert_eq!(
        ep.anchor_request.as_deref(),
        Some("build me a vox explainer video for the above")
    );
}

#[test]
fn an_unclean_anchor_falls_back_to_the_next_heaviest() {
    let long = format!("{} end", "word ".repeat(40));
    let mut lines = segment(&long, "a", 20);
    lines.extend(segment(
        "build me a vox explainer video for the above",
        "b",
        8,
    ));
    lines.extend(segment(
        "Will the audio cause linkedin to not play it",
        "c",
        1,
    ));
    assert_eq!(
        extract(&lines).anchor_request.as_deref(),
        Some("build me a vox explainer video for the above")
    );
    // No second segment with work: abstain.
    let mut lines = segment(&long, "a", 20);
    lines.push(typed_line(
        "Will the audio cause linkedin to not play it",
        "typed",
    ));
    assert_eq!(extract(&lines).anchor_request, None);
}

#[test]
fn a_lost_and_found_pane_paste_is_not_a_task() {
    // Session 3002ab28: the only typed prompt was a pane send text.
    let ep = extract(&[
        typed_line(
            "Lost and found: “The IG post time is a soft follow-up. Then ask for the release go-ahead (allowed, since publishing needs approval).” went quiet after 19:38. Revisit it as a side task: finish or close it in a few steps without derailing the current work, then tell me in one line.",
            "typed",
        ),
        assistant_stop_line("Closed: it was already live.", "end_turn"),
    ]);
    assert_eq!(ep.anchor_request, None);
}

#[test]
fn only_a_quick_mid_turn_follow_up_joins_the_task_text() {
    // this-ai-life 76e2ddd2: an addition queued 24 minutes into the work
    // stays in the segment (its work counts) but not in the anchor text.
    let mut lines = vec![typed_at(
        "in the pigment skill, can we add a vox style - review this gh repo for style etc and add it https://github.com/example/vox-director",
        "typed",
        "2026-10-11T18:52:19.078Z",
    )];
    lines.extend(work("a", 5));
    lines.push(typed_at(
        "ok one more now its called anatomy - use the anatomy repo as the second reference for this style",
        "queued",
        "2026-10-11T19:16:28.448Z",
    ));
    lines.extend(work("b", 5));
    lines.push(assistant_stop_line("Done.", "end_turn"));
    lines.extend(segment(
        "tell csr to go ahead with the jukebox build",
        "c",
        1,
    ));
    assert_eq!(
        extract(&lines).anchor_request.as_deref(),
        Some("in the pigment skill, can we add a vox style - review this gh repo for style etc and add it https://github.com/example/vox-director")
    );

    // Case 1 (lines 28444/28462): a split message 24 s apart still joins.
    let ep = extract(&[
        typed_at(
            "How do we show cost as % of mac",
            "queued",
            "2026-10-11T21:26:26.000Z",
        ),
        assistant_stop_line("remote-cancel", "stop_sequence"),
        typed_at(
            "% of max plan if they have it or cents",
            "queued",
            "2026-10-11T21:26:50.000Z",
        ),
    ]);
    assert_eq!(
        ep.anchor_request.as_deref(),
        Some("How do we show cost as % of mac % of max plan if they have it or cents")
    );
}

// ---- round 4 pivot: anchor = the first task (reviewer labels 9/11) ----

#[test]
fn pivot_first_task_anchors_whatever_work_follows() {
    let mut lines = segment(
        "whats the next song to attempt, with trap sophistication",
        "a",
        1,
    );
    lines.extend(segment("thats a copyright trap what else", "b", 30));
    assert_eq!(
        extract(&lines).anchor_request.as_deref(),
        Some("whats the next song to attempt, with trap sophistication")
    );
}

#[test]
fn pivot_unclean_first_task_falls_back_to_the_heaviest_segment() {
    let long = format!("{} end", "word ".repeat(40));
    let mut lines = segment(&long, "a", 2);
    lines.extend(segment("clean up the duplicate main layouts", "b", 12));
    lines.push(typed_line("commit the cleanup now and tag it", "typed"));
    lines.extend(work("c", 1));
    lines.extend(committed("c", "1a2b3c4", "fix(app): one main layout"));
    lines.push(assistant_stop_line("Committed.", "end_turn"));
    // 12 calls vs 2 calls + one receipt (2 + COMMIT_WEIGHT = 12, later wins).
    assert_eq!(
        extract(&lines).anchor_request.as_deref(),
        Some("commit the cleanup now and tag it")
    );
    // No segment with work besides the unclean first: abstain.
    let mut lines = segment(&long, "a", 20);
    lines.push(typed_line(
        "Will the audio cause linkedin to not play it",
        "typed",
    ));
    assert_eq!(extract(&lines).anchor_request, None);
}

#[test]
fn pivot_completed_is_the_sessions_latest_receipts_tied_only_to_a_latest_anchor() {
    let mut lines = segment("clean up the duplicate main layouts", "a", 3);
    lines.push(typed_line("and ship the radio reorder too", "typed"));
    lines.extend(committed("b", "5e5e5e5", "fix(app): one main layout"));
    lines.extend(committed(
        "c",
        "6f6f6f6",
        "feat(radio): sundarakanda second",
    ));
    lines.extend(committed("d", "7a7a7a7", "chore: release notes"));
    lines.push(assistant_stop_line("Done.", "end_turn"));
    let ep = extract(&lines);
    assert_eq!(
        ep.anchor_request.as_deref(),
        Some("clean up the duplicate main layouts")
    );
    let receipts: Vec<&str> = ep.completions.iter().map(|c| c.receipt.as_str()).collect();
    assert_eq!(receipts, ["5e5e5e5", "6f6f6f6", "7a7a7a7"]);
    // The anchor is not the latest segment: the receipts are not its.
    assert!(ep.completions_untied);

    let mut lines = segment("clean up the duplicate main layouts", "a", 3);
    lines.extend(committed("b", "5e5e5e5", "fix(app): one main layout"));
    let ep = extract(&lines);
    assert!(!ep.completions_untied);
}

#[test]
fn an_over_long_first_task_anchors_on_its_clause_not_a_fallback() {
    let mut lines = segment(
        "This ia new Reycital challenge - there are two videos here /Users/r/Downloads/IMG_8550.mov /Users/r/Downloads/IMG_8546.mov - 8550 is the melodica, which is the primary melody and 8546 is the piano - your mission if you chose to take it on is to import both into logic.",
        "a",
        5,
    );
    lines.extend(segment("stopped check now", "b", 40));
    assert_eq!(
        extract(&lines).anchor_request.as_deref(),
        Some("This ia new Reycital challenge")
    );
}

#[test]
fn a_short_fallback_abstains_on_every_path() {
    // The first task has no clean rendering and no clause to cut; the
    // heaviest other segment is a 3-word side request: abstain.
    let long = format!("{} end", "word ".repeat(40));
    let mut lines = segment(&long, "a", 5);
    lines.extend(segment("stopped check now", "b", 40));
    let ep = extract(&lines);
    assert_eq!(ep.anchor_request, None);
    // The recap, CONTINUUM fallback and EPISODE INDEX (which the pickup
    // renders through) all read the same anchor.
    assert_eq!(crate::hooks::recap::recap_intent(&ep), None);
    let continuum = crate::hooks::session_start::format_tier0_block(&ep, &[], "now");
    assert!(
        continuum.starts_with("CSR CONTINUUM [now]: (no clean intent)\n"),
        "{continuum}"
    );
    let index = crate::hooks::session_start::format_episode_index(&[(ep.clone(), "now".into())]);
    assert!(index.contains("- [now] (no clean intent) — "), "{index}");
    assert!(!index.contains("stopped check now"), "{index}");

    // Six words or more: the fallback stands.
    let mut lines = segment(&long, "a", 5);
    lines.extend(segment("stopped, check the render log now", "b", 40));
    assert_eq!(
        extract(&lines).anchor_request.as_deref(),
        Some("stopped, check the render log now")
    );
}

#[test]
fn a_csr_agent_transcript_has_no_task() {
    // CSR's own narrator children open with an agent-prompt signature;
    // their later rows are CSR's inputs, never the person's task. Before
    // this, the clause cut turned "TRANSCRIPT EXCERPT: ..." into an anchor.
    let ep = extract(&[
        typed_line(
            "[[CSR:RECAP]] machine-marker: this is a CSR narrator side-session, never corpus material.\nYou are a post-turn rubber-duck narrator for a coding agent.",
            "typed",
        ),
        assistant_stop_line("ok", "end_turn"),
        typed_line(
            "TRANSCRIPT EXCERPT: cal link targets: True missing: [] headings identical: True lines new 145 old 145 lines over 200 chars: 17 longest 481 273 5, plus the rest of the excerpt that runs on",
            "typed",
        ),
    ]);
    assert_eq!(ep.anchor_request, None);
    assert!(ep.completions.is_empty());
}

#[test]
fn astra_r5_1_a_forwarded_note_holding_a_recap_is_never_a_note() {
    let ep = extract(&[typed_line(
        "Here is a note offered by a side agent:\n\n> **recap [1h ago]: Fix login validation. Latest: fix(auth): preserve form errors until the next submitted validation request (abc1234); test(auth): cover retries and stale error clearing when the validation request changes (def5678). [[CSR:RECAP]]**",
        "typed",
    )]);
    assert_eq!(ep.anchor_request, None);
}

#[test]
fn astra_r5_2_an_excluded_late_prompt_does_not_carry_its_follow_up_in() {
    let ep = extract(&[
        typed_at(
            "Fix the checkout validation bug",
            "typed",
            "2026-10-12T12:00:00Z",
        ),
        typed_at(
            "Also build the billing dashboard",
            "queued",
            "2026-10-12T12:24:00Z",
        ),
        typed_at("and show failed invoices", "queued", "2026-10-12T12:24:30Z"),
    ]);
    assert_eq!(
        ep.anchor_request.as_deref(),
        Some("Fix the checkout validation bug")
    );
}

#[test]
fn astra_r5_3_earlier_receipts_never_read_as_a_fallback_anchors_result() {
    let long = "word ".repeat(40);
    let mut lines = vec![typed_line(&long, "typed")];
    lines.extend(committed("a", "abc1234", "fix(auth): validate checkout"));
    lines.push(assistant_stop_line("Done.", "end_turn"));
    lines.push(typed_line(
        "Review the new billing dashboard implementation",
        "typed",
    ));
    lines.extend(work("b", 1));
    lines.push(assistant_stop_line("Reviewed.", "end_turn"));
    let ep = extract(&lines);
    let got = crate::hooks::recap::compose_recap(&ep, &no_feeds(), "now").unwrap();
    assert_eq!(
        got.lines().next().unwrap_or_default(),
        "recap [now]: Review the new billing dashboard implementation. [[CSR:RECAP]]"
    );
    // The earlier commit is shown as a session fact, never as this ask's result.
    assert!(
        got.contains("Settled: fix(auth): validate checkout (abc1234)"),
        "{got}"
    );
}

#[test]
fn astra_r5_5_a_wrapped_csr_agent_opening_still_has_no_task() {
    let ep = extract(&[
        typed_line(
            "<system-reminder>CSR ENDLESS MEMORY ACTIVE\npast context</system-reminder>\n\nYou are CSR Episode Analyst. Generate a brief.",
            "typed",
        ),
        assistant_stop_line("ok", "end_turn"),
        typed_line("Summarize the release notes for the narrator.", "typed"),
    ]);
    assert_eq!(ep.anchor_request, None);
}

#[test]
fn astra_r5_7_commit_receipts_outrank_todo_receipts() {
    let todo_write = |id: &str, items: Vec<&str>| {
        let todos: Vec<serde_json::Value> = items
            .iter()
            .map(|c| serde_json::json!({"content": c, "status": "completed"}))
            .collect();
        vec![
            assistant_named_tool("TodoWrite", id, serde_json::json!({ "todos": todos })),
            tool_result_bound(id, "Todos have been modified successfully"),
        ]
    };
    let mut lines = vec![typed_line("ship the auth fixes", "typed")];
    lines.extend(committed("a", "1111111", "fix(auth): one"));
    lines.extend(committed("b", "2222222", "fix(auth): two"));
    lines.extend(todo_write(
        "toolu_t",
        vec!["Write regression tests", "Run gates"],
    ));
    let got = crate::hooks::recap::compose_recap(&extract(&lines), &no_feeds(), "now").unwrap();
    assert_eq!(
        got,
        "recap [now]: ship the auth fixes. Latest: fix(auth): one (1111111); fix(auth): two (2222222). [[CSR:RECAP]]"
    );
    // Five or more todo receipts never evict the stored commits.
    let mut lines = vec![typed_line("ship the auth fixes", "typed")];
    lines.extend(committed("a", "1111111", "fix(auth): one"));
    lines.extend(committed("b", "2222222", "fix(auth): two"));
    lines.extend(todo_write(
        "toolu_t",
        vec![
            "a1 task", "a2 task", "a3 task", "a4 task", "a5 task", "a6 task",
        ],
    ));
    let receipts: Vec<String> = extract(&lines)
        .completions
        .iter()
        .map(|c| c.receipt.clone())
        .collect();
    assert!(receipts.contains(&"1111111".to_string()), "{receipts:?}");
    assert!(receipts.contains(&"2222222".to_string()), "{receipts:?}");
}

#[test]
fn astra_r5_8_failed_task_creation_never_becomes_next() {
    let dreamer = "What if memory notes included their originating session?\n\n\
                       CSR's dreamer asked this while you worked. It is a question, not a task.\n\n\
                       > Notes should show their origin.";
    let ep = extract(&[
        typed_line(dreamer, "typed"),
        sidechain(assistant_named_tool(
            "TaskCreate",
            "toolu_side_create",
            serde_json::json!({"subject": "Publish package"}),
        )),
        sidechain(error_result("toolu_side_create", "permission denied")),
    ]);
    assert!(ep.todos.is_empty(), "{:?}", ep.todos);
    assert_eq!(ep.next_steps, None);
    let got = crate::hooks::recap::compose_recap(&ep, &no_feeds(), "now").unwrap();
    assert!(!got.contains("Next:"), "{got}");
    assert!(!got.contains("todos open"), "{got}");
    // The main session too: a create that failed is no task.
    let ep = extract(&[
        typed_line("publish the package", "typed"),
        assistant_named_tool(
            "TaskCreate",
            "toolu_create",
            serde_json::json!({"subject": "Publish package"}),
        ),
        error_result("toolu_create", "permission denied"),
    ]);
    assert!(ep.todos.is_empty(), "{:?}", ep.todos);
}

#[test]
fn astra_r5_9_a_crlf_dreamer_prompt_keeps_its_headline() {
    let ep = extract(&[typed_line(
        "What if memory notes included their originating session?\r\n\r\nCSR's dreamer asked this while you worked. It is a question, not a task.\r\n\r\n> Notes should show their origin.",
        "typed",
    )]);
    assert_eq!(
        ep.anchor_request.as_deref(),
        Some("note: What if memory notes included their originating session?")
    );
}
#[test]
fn a_handoff_envelope_in_a_prompt_does_not_reach_the_derived_request_or_tasks() {
    let ask = "Lost and found: “Run source-gone validation” went quiet after 14:30. Hand it to a background subagent with a five-part spec (objective, files, interfaces, constraints, verification), keep working on the current task, and report its result when it lands.";
    let envelope = crate::eval::contamination::agents_document()
        .fixtures
        .remove("handoff")
        .expect("handoff fixture")
        .text;
    let row = |text: &str| {
        serde_json::json!({
            "type": "user",
            "timestamp": "2026-10-13T10:00:00Z",
            "message": {"role": "user", "content": text},
        })
    };
    // the pane's send alone is CSR's, not a task
    assert!(typed_prompt(&row(&format!("{ask}\n{envelope}"))).is_none());
    // with the person's words, only the words are the task
    let typed = typed_prompt(&row(&format!("fix the importer first\n{ask}\n{envelope}")))
        .expect("the person's words are a prompt");
    assert_eq!(typed.prose, "fix the importer first");
    // and the episode built from the transcript carries none of it
    let lines = [
        row("fix the importer first").to_string(),
        row(&format!("{ask}\n{envelope}")).to_string(),
    ];
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let episode = extract_episode(&refs, "sid", "proj");
    let derived = serde_json::to_string(&episode).unwrap();
    for leaked in [
        "\\\"kind\\\":\\\"handoff\\\"",
        "[[CSR:AGENTS",
        "req-2026-10-13-0001",
    ] {
        assert!(
            !derived.contains(leaked),
            "{leaked} reached the episode: {derived}"
        );
    }
}

#[test]
fn streamed_episode_equals_whole_file_episode() {
    let dreamer = "What if memory notes included their originating session?\n\n\
                       CSR's dreamer asked this while you worked. It is a question, not a task.\n\n\
                       > Notes should show their origin.";
    let mut lines = multi_topic_session();
    lines.extend([
        typed_line(dreamer, "typed"),
        "this line is not json".to_string(),
        String::new(),
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
    ]);
    let dir = tempfile::tempdir().unwrap();
    for (name, joiner) in [("lf.jsonl", "\n"), ("crlf.jsonl", "\r\n")] {
        let raw = lines.join(joiner);
        for tail in ["", joiner] {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("{raw}{tail}")).unwrap();
            let whole = episode_whole_file(&std::fs::read_to_string(&path).unwrap());
            assert_eq!(episode_streamed(&path), whole, "{name} tail={tail:?}");
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            let mut by_lines = extract_episode(&refs, "sid", "proj");
            by_lines.timestamp.clear();
            assert_eq!(serde_json::to_string(&by_lines).unwrap(), whole);
        }
    }
    // The fixture actually exercised the paths: notes, tasks, receipts.
    let whole: serde_json::Value =
        serde_json::from_str(&episode_whole_file(&lines.join("\n"))).unwrap();
    assert!(whole["message_count"].as_u64().unwrap() > 10);
    assert!(!whole["tools_used"].as_array().unwrap().is_empty());
}

#[test]
fn streamed_episode_rejects_invalid_utf8_like_read_to_string() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.jsonl");
    let mut bytes = user_line("hello").into_bytes();
    bytes.extend_from_slice(b"\n\xff\xfe\n");
    std::fs::write(&path, bytes).unwrap();
    assert!(std::fs::read_to_string(&path).is_err());
    assert!(extract_episode_from_file(
        &path,
        std::fs::metadata(&path).unwrap().len(),
        "sid",
        "proj"
    )
    .is_err());
}

/// Manual check on a real transcript:
/// `CSR_EQUIV_TRANSCRIPT=/path/x.jsonl cargo test --release --lib -- --ignored streamed_episode_real`
#[test]
#[ignore]
fn streamed_episode_real_transcript_equals_whole_file_episode() {
    let path = std::path::PathBuf::from(std::env::var("CSR_EQUIV_TRANSCRIPT").unwrap());
    let whole = episode_whole_file(&std::fs::read_to_string(&path).unwrap());
    let streamed = episode_streamed(&path);
    assert_eq!(streamed, whole);
    println!("equivalent: {} bytes of episode JSON", streamed.len());
}

/// Claude Code appends to the live transcript. Both reads of the Stop hook
/// stop at one snapshot length, so a row (or garbage) appended between
/// them shows up in neither the episode nor the provenance inputs.
#[test]
fn rows_appended_between_the_two_reads_reach_neither() {
    use std::io::Write;
    let base = [
        user_line("fix the importer"),
        assistant_tool_line(&[("Edit", "/src/a.rs")]),
        assistant_text_line("Fixed and complete."),
    ];
    let dir = tempfile::tempdir().unwrap();
    let storage = crate::storage::Storage::open_memory().unwrap();
    let pristine = dir.path().join("pristine.jsonl");
    std::fs::write(&pristine, format!("{}\n", base.join("\n"))).unwrap();
    let mut expected_episode = extract_episode_from_file(&pristine, u64::MAX, "s", "p").unwrap();
    expected_episode.timestamp.clear();
    let expected_episode = serde_json::to_string(&expected_episode).unwrap();

    let appended: [&[u8]; 4] = [
        // a whole new row that would add a tool, a file and a message
        br#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t9","name":"Write","input":{"file_path":"/src/late.rs"}}]}}
"#,
        b"\xff\xfe\n",
        // a failed tool result that instrumentation would count
        br#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"late","is_error":true,"content":"late failure"}]}}
"#,
        // the start of a row still being written
        br#"{"type":"user","message":{"content":"half"#,
    ];
    for tail in appended {
        let live = dir.path().join("live.jsonl");
        std::fs::write(&live, format!("{}\n", base.join("\n"))).unwrap();
        let snapshot_len = std::fs::metadata(&live).unwrap().len();
        // Receipts name the file, so the expected inputs are read from the
        // same path, before anything is appended.
        let expected_inputs = format!(
            "{:?}",
            crate::import::transcript_inputs(&storage, &live, "s").unwrap()
        );
        let scan = |path: &Path, limit: u64| {
            let found = scan_instrumentation(path, limit).unwrap();
            (found.error_count, format!("{:?}", found.top_errors))
        };
        let expected_scan = scan(&live, snapshot_len);
        let inputs =
            crate::import::transcript_inputs_limited(&storage, &live, "s", snapshot_len).unwrap();
        // Claude Code appends between the hook's two reads.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&live)
            .unwrap()
            .write_all(tail)
            .unwrap();
        let mut episode = extract_episode_from_file(&live, snapshot_len, "s", "p").unwrap();
        episode.timestamp.clear();
        assert_eq!(serde_json::to_string(&episode).unwrap(), expected_episode);
        assert_eq!(format!("{inputs:?}"), expected_inputs);
        assert_eq!(scan(&live, snapshot_len), expected_scan);
        // and a provenance read taken after the append is also capped
        let late =
            crate::import::transcript_inputs_limited(&storage, &live, "s", snapshot_len).unwrap();
        assert_eq!(format!("{late:?}"), expected_inputs);
    }
}
