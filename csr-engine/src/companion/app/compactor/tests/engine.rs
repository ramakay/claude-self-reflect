//! The hooks against the scripted Claude Code: the removed `compact.test.ts` from "the hooks,
//! against the engine" on, and the three marker tests of the removed `companion.test.ts`.

use super::kit::{
    agent, forward, has, same, strings, usual_plan, width, Kit, Point, Setup, GO_ON, KEEP_GOING,
    LANDED, MARKER,
};
use crate::companion::{
    app::compactor::rules::{
        notice_line, Keep, COUNTDOWN_MS, KEEP_HEADER, KEEP_MAX, NOTICE_HEADER,
    },
    relay::dispatch::testing::until,
};
use serde_json::{json, Value};
use std::sync::Arc;

fn git_of(rows: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    rows.to_vec()
}

fn told_of(kit: &Kit) -> String {
    kit.compacted()
        .last()
        .cloned()
        .flatten()
        .unwrap_or_default()
}

fn typing(key: &str) -> Value {
    json!({ "origin": { "kind": "composer" }, "text": "", "cursor": 0, "start": 0, "end": 0,
        "inputText": key, "key": { "key": key } })
}

fn task(id: &str, kind: &str, description: &str) -> Value {
    json!({ "id": id, "type": kind, "status": "running", "description": description })
}

fn lost_rows() -> Vec<Value> {
    vec![
        json!({ "text": "Wait for agent ab763f58e5be44c02's timeout-fix report.", "state": "lost",
            "waiting": true, "hidden": null, "later": false }),
        json!({ "text": "Try the cache countdown in the masthead.", "state": "lost",
            "waiting": false, "hidden": "later", "later": true }),
    ]
}

#[tokio::test]
async fn a_forwarded_dream_s_reasoning_never_reaches_the_summary_as_the_owner_s_rule() {
    let kit = Kit::auto(300_000.);
    kit.typed(&forward()).await;
    kit.phone_sent(KEEP_GOING).await;
    kit.turn("Still going.", &[agent()]).await;
    assert_eq!(kit.compactions(), 1);
    let told = told_of(&kit);
    assert!(!told.contains("disable retries"));
    assert!(!told.contains("The owner's rules"));
}

// ---------------------------------------------------------------- the hooks, against the engine

#[tokio::test]
async fn below_the_band_no_plan_no_pane_py_no_band_no_compaction() {
    let kit = Kit::auto(150_000.);
    kit.typed(GO_ON).await;
    kit.turn(LANDED, &[]).await;
    assert!(kit.out.plans.lock().unwrap().is_empty());
    assert!(kit.compacts().is_empty());
    assert!(kit.compacted().is_empty());
    assert!(kit.plan().is_none());
    let band = kit.band().await;
    assert!(!band.has_like("Compact") && band.labels.is_empty());
}

#[tokio::test]
async fn in_the_band_not_at_a_seam_the_plan_row_says_what_it_waits_for_and_nothing_acts() {
    let kit = Kit::auto(214_000.);
    kit.typed(GO_ON).await;
    kit.turn("Launched the review agent.", &[agent()]).await;
    let want = "Compact planned · 214k of your 181k–269k band · pays back in ~8 API calls \
        (~1 prompt) · waiting: 1 agent running";
    let plan = kit.plan().unwrap();
    assert!(has(&plan, json!({ "state": "planned", "text": want })));
    assert_eq!(
        *kit.out.plans.lock().unwrap(),
        [("sess-1".to_string(), 214_000)]
    );
    let logged = kit.compacts();
    assert_eq!(logged.len(), 1);
    let want = json!({ "event": "plan", "act": "plan", "mode": "attended", "ctx_before": 214_000,
        "payback": 8.2, "per_turn": 17, "wait": "1 agent running" });
    assert!(has(&logged[0], want));
    assert!(has(
        &logged[0]["seam"],
        json!({ "is": false, "running": 1 })
    ));
    assert!(kit.compacted().is_empty());
    assert!(!kit.band().await.has_like("Compacting in"));
}

#[tokio::test]
async fn a_compaction_that_would_not_pay_back_within_15_turns_is_not_planned() {
    let mut plan = usual_plan(214_000);
    plan["payback"] = json!(300); // 300 requests at 17 a turn: 17.6 turns
    let kit = Kit::new(Setup {
        plan: Some(plan),
        ..Setup::auto(214_000.)
    });
    kit.typed(GO_ON).await;
    kit.turn(LANDED, &[]).await;
    assert!(kit.plan().is_none());
    assert!(kit.compacted().is_empty());
}

#[tokio::test]
async fn attended_at_a_seam_a_20_s_countdown_with_compact_now_and_cancel_then_the() {
    let git = git_of(&[
        ("rev-parse --show-toplevel", "/repo"),
        ("status --porcelain", " M hooks/compact.ts\n?? bin/x.py"),
        ("log --format=%h @{u}..HEAD", "ea7d83f"),
        ("rev-parse --short HEAD", "ea7d83f"),
        ("symbolic-ref -q --short HEAD", "study/dreamer"),
    ]);
    let said = |e: &Value| {
        let committing = e["command"]
            .as_str()
            .is_some_and(|c| c.starts_with("git commit"));
        if committing {
            "[study/dreamer 6369e6b] wip"
        } else {
            ""
        }
        .to_string()
    };
    let kit = Kit::new(Setup {
        git,
        said: Some(Arc::new(said)),
        ..Setup::auto(214_000.)
    });
    for file in ["/repo/hooks/compact.ts", "/repo/bin/x.py"] {
        let call =
            json!({ "tool": "Write", "file_path": file, "content": "x", "tool_use_id": file });
        kit.tool(call).await;
    }
    // this session committed here
    let commit =
        json!({ "tool": "Bash", "command": "git commit -m wip", "tool_use_id": "commit-1" });
    kit.tool(commit).await;
    kit.typed("Never use --no-verify here. Carry on with the plan.")
        .await;
    kit.turn(LANDED, &[]).await;
    let band = kit.band().await;
    assert!(band.has("Compacting in 20s · 214k · commit 6369e6b landed"));
    assert!(band.has_button("Compact now"));
    assert!(band.has_button("Cancel"));
    assert_eq!(kit.plan().unwrap()["state"], "countdown");
    assert!(kit.compacted().is_empty());

    kit.advance(COUNTDOWN_MS).await;
    assert!(until(|| kit.events().contains(&"compact".to_string())).await);
    assert_eq!(kit.compactions(), 1);
    let instructions = told_of(&kit);
    assert!(instructions.starts_with(KEEP_HEADER));
    assert!(width(&instructions) <= KEEP_MAX);
    assert!(instructions.contains("branch study/dreamer at ea7d83f"));
    assert!(instructions.contains("2 uncommitted files (hooks/compact.ts, bin/x.py)"));
    assert!(instructions.contains("not pushed: ea7d83f"));
    assert!(instructions.contains("Never use --no-verify here."));
    assert_eq!(
        kit.events(),
        strings(&["plan", "countdown", "start", "compact"])
    );
    let last = kit.last();
    let want = json!({ "by": "countdown", "mode": "attended", "outcome": "compacted",
        "ctx_before": 214_000, "ctx_after": 21_000, "floor": 65_000, "payback": 8.2,
        "per_turn": 17 });
    assert!(has(&last, want));
    assert!(last["took_ms"].is_number());
    let seam = json!({ "is": true, "why": "commit 6369e6b landed", "commit": "6369e6b" });
    assert!(has(&last["seam"], seam));
    let notice = "CSR compacted at 214k → 65k · pays back in ~8 API calls (~1 prompt) · kept: \
        unfinished work";
    assert_eq!(kit.appended(), [notice]);
    assert_eq!(kit.plan().unwrap()["state"], "none");
    assert!(!kit.band().await.has_like("Compacting in"));
}

#[tokio::test]
async fn t_0_cancels_no_compaction_and_at_the_same_context_it_keeps_waiting_for_20k_more() {
    let kit = Kit::auto(214_000.);
    kit.typed(GO_ON).await;
    kit.turn(LANDED, &[]).await;
    kit.press("compact-cancel").await;
    assert!(!kit.band().await.has_like("Compacting in"));
    kit.advance(COUNTDOWN_MS).await;
    assert!(kit.compacted().is_empty());
    let want = json!({ "event": "cancel", "by": "press", "ctx_before": 214_000 });
    assert!(has(&kit.last(), want));
    assert!(kit.plan_text().contains("cancelled · waits for 234k"));

    kit.turn(LANDED, &[]).await; // the same context: still waiting
    assert!(!kit.band().await.has_like("Compacting in"));
    assert!(kit.plan_text().contains("cancelled · waits for 234k"));
}

#[tokio::test]
async fn t_1_on_the_band_compacts_at_once_before_the_countdown_runs_out() {
    let kit = Kit::auto(214_000.);
    kit.typed(GO_ON).await;
    kit.turn(LANDED, &[]).await;
    kit.press("compact-now").await;
    assert!(until(|| kit.compactions() > 0).await);
    assert!(until(|| kit.events().contains(&"compact".to_string())).await);
    assert_eq!(kit.compactions(), 1);
    assert!(has(
        &kit.last(),
        json!({ "event": "compact", "by": "press" })
    ));
    kit.advance(COUNTDOWN_MS).await;
    assert_eq!(kit.compactions(), 1); // the countdown went with it
}

#[tokio::test]
async fn typing_or_sending_a_prompt_cancels_the_countdown_a_lone_digit_for_the_band_does() {
    let kit = Kit::auto(214_000.);
    kit.typed(GO_ON).await;
    kit.turn(LANDED, &[]).await;
    kit.edit(typing("1")).await;
    // a band hotkey on its way
    assert!(kit.band().await.has_like("Compacting in"));
    kit.edit(typing("w")).await;
    assert!(!kit.band().await.has_like("Compacting in"));
    assert!(has(
        &kit.last(),
        json!({ "event": "cancel", "by": "typing" })
    ));

    kit.set_ctx(224_000.); // 10k more: not enough after a cancel
    kit.turn(LANDED, &[]).await;
    assert!(!kit.band().await.has_like("Compacting in"));
    kit.set_ctx(236_000.); // 22k more, at a seam: armed again
    kit.turn(LANDED, &[]).await;
    assert!(kit.band().await.has_like("Compacting in 20s · 236k"));
    kit.typed("one more thing").await;
    assert!(!kit.band().await.has_like("Compacting in"));
    let want = json!({ "event": "cancel", "by": "prompt", "ctx_before": 236_000 });
    assert!(has(&kit.last(), want));
    kit.advance(COUNTDOWN_MS).await;
    assert!(kit.compacted().is_empty());
}

#[tokio::test]
async fn a_turn_that_starts_during_the_countdown_ends_it_the_next_turn_end_decides_again() {
    let kit = Kit::auto(214_000.);
    kit.typed(GO_ON).await;
    kit.turn(LANDED, &[]).await;
    kit.notify("turn_start", json!({ "text": "", "turnId": "tn-1" }))
        .await;
    assert!(!kit.band().await.has_like("Compacting in"));
    assert!(has(
        &kit.last(),
        json!({ "event": "skip", "reason": "a turn started" })
    ));
    kit.advance(COUNTDOWN_MS).await;
    assert!(kit.compacted().is_empty());
}

#[tokio::test]
async fn past_the_high_edge_it_acts_at_the_next_turn_end_seam_or_not_a_countdown_attended() {
    let kit = Kit::auto(280_000.);
    kit.typed(GO_ON).await;
    kit.turn("Started two agents.", &[agent()]).await;
    assert!(kit.band().await.has_like("Compacting in 20s · 280k"));
    let countdown = kit
        .compacts()
        .into_iter()
        .find(|row| row["event"] == "countdown");
    assert!(has(&countdown.unwrap(), json!({ "forced": true })));
    kit.press("compact-cancel").await;

    kit.phone_sent(KEEP_GOING).await;
    // cancelled at 280k: past the edge too, it waits for 20k more
    kit.turn("Still running.", &[agent()]).await;
    assert!(kit.compacted().is_empty());
    kit.set_ctx(302_000.);
    kit.turn("Still running.", &[agent()]).await;
    assert_eq!(kit.compactions(), 1);
    let running = "Running in the background: subagent a427c126 (Explore CapCut draft automation)";
    assert!(told_of(&kit).contains(running));
    let want = json!({ "event": "compact", "by": "unattended", "mode": "unattended",
        "forced": true });
    assert!(has(&kit.last(), want));
}

#[tokio::test]
async fn a_question_waits_for_the_owner_unattended_past_the_high_edge_it_no_longer_holds() {
    let kit = Kit::auto(280_000.);
    kit.typed(GO_ON).await;
    kit.turn("Both gates pass. Should I push the study branch?", &[])
        .await;
    assert!(!kit.band().await.has_like("Compacting in"));
    assert!(kit.plan_text().contains("waiting: your answer"));

    kit.phone_sent(KEEP_GOING).await;
    kit.set_ctx(230_000.); // in the band, from the phone: a question still holds
    kit.turn("Should I push the study branch?", &[]).await;
    assert!(kit.compacted().is_empty());
    kit.set_ctx(290_000.); // past the edge: it compacts, and the question is kept for the owner
    kit.turn("Should I push the study branch?", &[]).await;
    assert_eq!(kit.compactions(), 1);
    assert!(told_of(&kit).contains("Waiting on the owner: Should I push the study branch?"));
    assert!(kit.appended()[0].contains("kept: 1 open decision"));
}

#[tokio::test]
async fn unattended_at_a_seam_in_the_band_it_compacts_at_once_no_countdown_and_leaves_one() {
    let kit = Kit::auto(214_000.);
    kit.phone_sent(KEEP_GOING).await;
    kit.turn(LANDED, &[]).await;
    assert_eq!(kit.compactions(), 1);
    assert_eq!(kit.events(), strings(&["plan", "start", "compact"]));
    assert!(!kit.band().await.has_like("Compacting in"));
    let notice = kit.appended();
    assert_eq!(notice.len(), 1);
    assert!(notice[0].starts_with(NOTICE_HEADER));
    assert_eq!(
        notice[0],
        "CSR compacted at 214k → 65k · pays back in ~8 API calls (~1 prompt)"
    );
}

// 10-12 21:17: when CSR compacts while you are away, the line the phone shows says what waits for
// you in Lost and found, so the threads stay out of the summary and you still know to look.
#[tokio::test]
async fn the_notice_counts_the_threads_waiting_on_you_in_lost_and_found_set_aside_quiet() {
    let lost = vec![
        json!({ "text": "Wait for agent ab763f58e5be44c02's report.", "state": "lost",
            "waiting": true, "hidden": null, "later": false, "rank": 0 }),
        json!({ "text": "Push the study branch when you say go.", "state": "open",
            "waiting": true, "hidden": null, "later": false, "rank": 2 }),
        json!({ "text": "Approve the install?", "state": "lost", "waiting": true,
            "hidden": "later", "later": true, "rank": 0 }),
        json!({ "text": "Say go on the guard.", "state": "answered", "waiting": true,
            "hidden": null, "later": false, "rank": 4 }),
        json!({ "text": "Try the cache countdown in the masthead.", "state": "lost",
            "waiting": false, "hidden": null, "later": false, "rank": 1 }),
    ];
    let kit = Kit::new(Setup {
        lost,
        ..Setup::auto(300_000.)
    });
    kit.phone_sent(KEEP_GOING).await;
    kit.turn("Still going.", &[agent()]).await;
    let notice = "CSR compacted at 300k → 65k · pays back in ~8 API calls (~1 prompt) · kept: 1 \
        running agent · 2 waiting in Lost and found";
    assert_eq!(kit.appended(), [notice]);
    let logged = kit
        .compacts()
        .into_iter()
        .find(|row| row["event"] == "compact");
    assert!(has(&logged.unwrap(), json!({ "lost_waiting": 2 })));
    let none = || Keep {
        running: vec![],
        decisions: vec![],
        work: vec![],
        rules: vec![],
    };
    let line = notice_line(300_000., Some(65_000.), None, &none(), 1);
    assert_eq!(
        line,
        "CSR compacted at 300k → 65k · 1 waiting in Lost and found"
    );
    let line = notice_line(300_000., Some(65_000.), None, &none(), 0);
    assert_eq!(line, "CSR compacted at 300k → 65k");
}

#[tokio::test]
async fn ten_minutes_without_a_prompt_at_the_terminal_is_unattended_no_countdown() {
    let kit = Kit::auto(214_000.);
    kit.typed(GO_ON).await;
    kit.advance(11. * 60_000.).await;
    let done = "<task-notification><status>completed</status></task-notification>";
    kit.submitted("task-notification", done).await;
    kit.turn(LANDED, &[]).await;
    assert_eq!(kit.compactions(), 1);
    let want = json!({ "event": "compact", "mode": "unattended", "by": "unattended" });
    assert!(has(&kit.last(), want));
}

// 10-12 20:53: the keep list brought back three Lost and found threads from 10-11 and 10-12
// morning, one waiting on agent ab763f58e5be44c02, long gone. A later summary had dropped them.
// 10-13 04:50: (1) the keep list named shells b02336hzp, b81nce4ei and bpd9kf6h3 as running; they
// were a subagent's, and Claude Code's own task list showed one main-thread shell. (3) It carried
// "7 uncommitted files (src/engine.rs, mcp/mod.rs, .agents/, ...)" from another session in the
// same checkout; this one never touched them.
#[tokio::test]
async fn the_keep_list_holds_the_main_thread_s_tasks_and_this_session_s_changes_never_a() {
    let git = git_of(&[
        ("rev-parse --show-toplevel", "/repo"),
        (
            "status --porcelain",
            " M src/engine.rs\n M mcp/mod.rs\n?? .agents/\n M hooks/compact.ts",
        ),
        ("log --format=%h @{u}..HEAD", "fdaba23"),
        ("rev-parse --short HEAD", "fdaba23"),
        ("symbolic-ref -q --short HEAD", "main"),
    ]);
    let said = |e: &Value| {
        if e["tool"] == "Agent" {
            return "Async agent launched successfully.\nagentId: a66d38d8d43ee1dad (internal ID)"
                .to_string();
        }
        if e["run_in_background"] != json!(true) {
            return String::new();
        }
        let id = e["tool_use_id"].as_str().unwrap_or("");
        let id = if id == "main-shell" { "bmain0001" } else { id };
        format!("Command running in background with ID: {id}")
    };
    let kit = Kit::new(Setup {
        git,
        said: Some(Arc::new(said)),
        ..Setup::auto(300_000.)
    });
    let launch = json!({ "tool": "Agent", "description": "scrub review", "prompt": "x",
        "tool_use_id": "agent-1" });
    kit.tool(launch).await;
    let build = json!({ "tool": "Bash", "command": "make", "run_in_background": true,
        "description": "main build", "tool_use_id": "main-shell" });
    kit.tool(build).await;
    let theirs = ["b02336hzp", "b81nce4ei", "bpd9kf6h3"];
    for id in theirs {
        // the subagent's own shells
        let sleep = json!({ "tool": "Bash", "command": "sleep 60", "run_in_background": true,
            "agentId": "a5f814bf6883609c3", "tool_use_id": id });
        kit.tool(sleep).await;
    }
    let edit = json!({ "tool": "Edit", "file_path": "/repo/hooks/compact.ts", "old_string": "a",
        "new_string": "b", "tool_use_id": "edit-1" });
    kit.tool(edit).await;
    let mut tasks = vec![
        task("a66d38d8d43ee1dad", "subagent", "scrub review"),
        task("bmain0001", "shell", "main build"),
    ];
    tasks.extend(theirs.iter().map(|id| task(id, "shell", "subagent shell")));
    kit.phone_sent(KEEP_GOING).await;
    kit.turn_listing("Still going.", tasks).await;
    let told = told_of(&kit);
    assert!(told.contains("subagent a66d38d8d43ee1dad"));
    assert!(told.contains("shell bmain0001"));
    for id in theirs {
        assert!(!told.contains(id));
    }
    // the file this session wrote, no other
    assert!(told.contains("1 uncommitted file (hooks/compact.ts)"));
    assert!(!told.contains("engine.rs"));
    assert!(!told.contains("not pushed")); // no commit of this session's here
    let start = kit
        .compacts()
        .into_iter()
        .find(|row| row["event"] == "start")
        .unwrap();
    assert_eq!(start["keep"]["running"].as_array().map(Vec::len), Some(2));
}

#[tokio::test]
async fn another_session_s_changes_alone_no_unfinished_work_section_at_all() {
    let git = git_of(&[
        ("rev-parse --show-toplevel", "/repo"),
        (
            "status --porcelain",
            " M src/engine.rs\n M mcp/mod.rs\n?? .agents/",
        ),
        ("log --format=%h @{u}..HEAD", "fdaba23"),
        ("rev-parse --short HEAD", "fdaba23"),
        ("symbolic-ref -q --short HEAD", "main"),
    ]);
    let kit = Kit::new(Setup {
        git,
        ..Setup::auto(300_000.)
    });
    kit.phone_sent(KEEP_GOING).await;
    kit.turn("Still going.", &[agent()]).await;
    let told = told_of(&kit);
    assert!(!told.contains("Unfinished work"));
    assert!(!told.contains("branch main"));
}

#[tokio::test]
async fn lost_and_found_stays_out_of_the_keep_list_waiting_or_set_aside_for_later_no() {
    let kit = Kit::new(Setup {
        lost: lost_rows(),
        ..Setup::auto(300_000.)
    });
    kit.phone_sent(KEEP_GOING).await;
    kit.turn("Still going.", &[agent()]).await;
    assert_eq!(kit.compactions(), 1);
    let told = told_of(&kit);
    assert!(!told.contains("ab763f58e5be44c02"));
    assert!(!told.contains("Waiting on the owner"));
    assert!(!told.contains("Try the cache countdown"));
    assert_eq!(kit.last()["kept"]["decisions"], 0);
}

// 10-12: "if you trigger the compact, make it a card ... and stream the info as needed": the
// pane's card reads these.
#[tokio::test]
async fn the_card_streams_what_it_keeps_is_logged_before_the_summary_runs_and_the_result() {
    let git = git_of(&[
        ("rev-parse --show-toplevel", "/repo"),
        ("status --porcelain", " M src/engine.rs"),
        ("rev-parse --short HEAD", "fdaba23"),
        ("symbolic-ref -q --short HEAD", "main"),
    ]);
    let kit = Kit::new(Setup {
        git,
        ..Setup::auto(300_000.)
    });
    let edit = json!({ "tool": "Edit", "file_path": "/repo/src/engine.rs", "old_string": "a",
        "new_string": "b", "tool_use_id": "edit-1" });
    kit.tool(edit).await;
    kit.phone_sent(KEEP_GOING).await;
    kit.turn("Still going.", &[agent()]).await;
    let events = kit.events();
    let start = events.iter().position(|event| event == "start").unwrap();
    let compact = events.iter().position(|event| event == "compact").unwrap();
    assert!(start < compact);
    let rows = json!({ "running": ["subagent a427c126 · Explore CapCut draft automation"],
        "decisions": [], "work": ["branch main at fdaba23", "1 uncommitted file (src/engine.rs)"],
        "rules": [] });
    let logged = kit.compacts();
    let want = json!({ "why": "over 269k · 1 agent kept", "ctx_before": 300_000 });
    assert!(has(&logged[start], want));
    assert!(same(&logged[start]["keep"], &rows));
    assert_eq!(logged[compact]["why"], "over 269k · 1 agent kept");
    assert!(same(&logged[compact]["keep"], &rows));
}

#[tokio::test]
async fn a_pane_py_that_does_not_answer_never_stops_the_high_edge_past_it_from_the_phone() {
    let kit = Kit::new(Setup {
        plan: Some(Value::Null),
        ..Setup::auto(300_000.)
    });
    kit.phone_sent(KEEP_GOING).await;
    kit.turn("Still going.", &[agent()]).await;
    assert_eq!(kit.compactions(), 1);
    let want = json!({ "event": "compact", "forced": true, "payback": null });
    assert!(has(&kit.last(), want));
    assert_eq!(
        kit.appended()[0],
        "CSR compacted at 300k → 21k · kept: 1 running agent"
    );
}

#[tokio::test]
async fn a_private_writer_that_fails_never_stops_a_compaction_the_plan_and_marker_files() {
    let kit = Kit::new(Setup {
        private_fails: Some(Arc::new(|_, _| true)),
        ..Setup::auto(300_000.)
    });
    kit.phone_sent(KEEP_GOING).await;
    kit.turn("Still going.", &[agent()]).await;
    assert_eq!(kit.compactions(), 1);
    assert!(has(&kit.last(), json!({ "event": "compact" })));
}

// The marker is cleared in the compaction hook's finally. Here rm leaves the marker, so it is
// overwritten as ended, and that write fails.
#[tokio::test]
async fn a_marker_that_cannot_be_cleared_leaves_exactly_one_compaction() {
    let kit = Kit::new(Setup {
        private_fails: Some(Arc::new(|path, text| {
            path.contains("/compacting/") && text.contains("ended")
        })),
        ..Setup::auto(300_000.)
    });
    let messages = json!([{ "role": "user", "text": "one", "toolUses": [] },
        { "role": "assistant", "text": "done", "toolUses": [] }]);
    let args = json!({ "trigger": "plugin", "instructions": "keep", "messages": messages });
    kit.compact(args).await.unwrap();
    let writes = kit.out.writes.lock().unwrap().clone();
    assert_eq!(
        writes
            .iter()
            .filter(|path| path.contains("/compacting/"))
            .count(),
        2
    );
    assert_eq!(kit.compactions(), 1);
}

#[tokio::test]
async fn a_compaction_a_hook_vetoes_is_logged_as_a_skip_and_the_plan_says_so() {
    let kit = Kit::new(Setup {
        skip: Some("another plugin keeps this one"),
        ..Setup::auto(214_000.)
    });
    kit.phone_sent(KEEP_GOING).await;
    kit.turn(LANDED, &[]).await;
    let want = json!({ "event": "skip", "outcome": "skipped",
        "reason": "another plugin keeps this one" });
    assert!(has(&kit.last(), want));
    assert!(kit.plan_text().contains("the compaction did not run"));
    assert!(kit.appended().is_empty());
}

#[tokio::test]
async fn never_in_a_subagent_s_turn_and_never_in_a_claude_p_child() {
    let kit = Kit::auto(290_000.);
    kit.phone_sent(KEEP_GOING).await;
    kit.turn_of(LANDED, "sub-1").await;
    assert!(kit.compacted().is_empty());
    assert!(kit.compacts().is_empty());
}

#[tokio::test]
async fn a_claude_p_child_draws_nowhere_and_never_plans_or_compacts() {
    let kit = Kit::new(Setup {
        headless: true,
        ..Setup::auto(290_000.)
    });
    kit.phone_sent(KEEP_GOING).await;
    kit.turn(LANDED, &[]).await;
    assert!(kit.compacted().is_empty());
    assert!(kit.compacts().is_empty());
    assert!(kit.plan().is_none());
}

#[tokio::test]
async fn suggest_the_band_offers_it_and_only_a_press_compacts() {
    let kit = Kit::new(Setup {
        setting: Some("suggest"),
        ..Setup::auto(214_000.)
    });
    kit.phone_sent(KEEP_GOING).await;
    kit.turn(LANDED, &[]).await;
    assert!(kit.compacted().is_empty());
    let offer = "Compact now? · 214k · pays back in ~8 API calls (~1 prompt) · commit 6369e6b \
        landed";
    assert!(kit.band().await.has(offer));
    assert!(kit.plan_text().contains("waiting: your press"));
    kit.advance(5. * 60_000.).await;
    assert!(kit.compacted().is_empty());
    kit.press("compact-now").await;
    assert!(until(|| kit.compactions() > 0).await);
    assert!(until(|| kit.events().contains(&"compact".to_string())).await);
    let want = json!({ "event": "compact", "by": "press", "setting": "suggest" });
    assert!(has(&kit.last(), want));
}

#[tokio::test]
async fn off_no_plan_no_band_no_compaction_however_far_past_the_band() {
    let kit = Kit::new(Setup {
        setting: Some("off"),
        ..Setup::auto(400_000.)
    });
    kit.phone_sent(KEEP_GOING).await;
    kit.turn(LANDED, &[]).await;
    assert!(kit.out.plans.lock().unwrap().is_empty());
    assert!(kit.compacted().is_empty());
    assert!(kit.plan().is_none());
}

#[tokio::test]
async fn a_countdown_the_person_turns_to_suggest_in_config_becomes_an_offer_turned_off_it() {
    let kit = Kit::auto(214_000.);
    kit.typed(GO_ON).await;
    kit.turn(LANDED, &[]).await;
    assert!(kit.band().await.has_like("Compacting in 20s"));
    kit.config_set("suggest").await;
    // converted before the change resolves
    assert!(kit.band().await.has_like(r"^Compact now\?"));
    kit.advance(COUNTDOWN_MS + 5_000.).await;
    assert!(kit.compacted().is_empty());
    assert!(!kit.band().await.has_like("Compacting in"));
    kit.config_set("off").await;
    assert!(!kit.band().await.has_like(r"^Compact now\?"));
    kit.advance(5. * 60_000.).await;
    assert!(kit.compacted().is_empty());
    assert!(has(
        &kit.last(),
        json!({ "event": "cancel", "by": "setting" })
    ));
}

// Codex r2 #3: Auto, unattended, 220k at a seam: the person turned Auto off while the compaction
// was being prepared (the plan being logged, or the keep list being read from git), and it
// compacted anyway, with the setting off.
async fn flip_while_preparing(at: Point, setting: &'static str) {
    let git = git_of(&[
        ("rev-parse --show-toplevel", "/repo"),
        ("status --porcelain", " M a.ts"),
    ]);
    let kit = Kit::new(Setup {
        git,
        ..Setup::auto(220_000.)
    });
    let hold = kit.hold_at(at);
    kit.phone_sent(KEEP_GOING).await;
    let ended = {
        let kit = kit.clone();
        tokio::spawn(async move { kit.turn(LANDED, &[]).await })
    };
    assert!(until(|| hold.paused()).await);
    kit.config_set(setting).await;
    hold.release();
    ended.await.unwrap();
    assert!(until(|| kit.events().contains(&"skip".to_string())).await);
    assert!(kit.compacted().is_empty());
    let reason = format!("the setting changed to {setting}");
    assert!(has(
        &kit.last(),
        json!({ "event": "skip", "reason": reason })
    ));
}

#[tokio::test]
async fn auto_turned_off_while_the_plan_is_logged_it_never_compacts_and_the_log_says_why() {
    flip_while_preparing(Point::Plan, "off").await;
}

#[tokio::test]
async fn auto_turned_off_while_the_keep_list_is_read_it_never_compacts() {
    flip_while_preparing(Point::GitStatus, "off").await;
}

#[tokio::test]
async fn auto_turned_to_suggest_while_the_plan_is_logged_it_never_compacts_on_its_own() {
    flip_while_preparing(Point::Plan, "suggest").await;
}

// astra r3 #10: Auto, attended, 220k at a seam: Off chosen while the plan was logged still
// installed the countdown, its timer with it; it ran out at zero with nothing to compact. Off
// leaves no countdown and no timer.
#[tokio::test]
async fn off_chosen_while_a_countdown_is_prepared_no_countdown_no_timer_nothing_compacts() {
    let kit = Kit::auto(220_000.);
    let hold = kit.hold_at(Point::Plan);
    kit.typed(GO_ON).await;
    let ended = {
        let kit = kit.clone();
        tokio::spawn(async move { kit.turn(LANDED, &[]).await })
    };
    assert!(until(|| hold.paused()).await);
    kit.config_set("off").await;
    hold.release();
    ended.await.unwrap();
    assert!(until(|| kit.events().contains(&"skip".to_string())).await);
    assert!(!kit.band().await.has_like("Compacting in"));
    assert_eq!(kit.events(), strings(&["plan", "skip"]));
    assert_eq!(kit.last()["reason"], "the setting changed to off");
    kit.advance(COUNTDOWN_MS + 5_000.).await;
    assert!(kit.compacted().is_empty());
    let band = kit.band().await;
    assert!(!band.has_like("Compacting in|Compact now") && band.labels.is_empty());
}

// astra r4 #12: the countdown's plan waited on HOME while Off took the countdown down; it then
// wrote "countdown" over Off's "none", and the pane showed "Compacting in 0s" for up to two
// hours. A plan a newer one overtook writes nothing.
#[tokio::test]
async fn a_countdown_plan_that_finishes_after_off_took_it_down_writes_nothing_over_off() {
    let kit = Kit::auto(220_000.);
    let hold = kit.hold_at(Point::Home);
    kit.typed(GO_ON).await;
    let ended = {
        let kit = kit.clone();
        tokio::spawn(async move { kit.turn(LANDED, &[]).await })
    };
    // the countdown is up, its plan waits on HOME
    assert!(until(|| hold.paused()).await);
    kit.config_set("off").await;
    hold.release();
    ended.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(kit.plan().unwrap()["state"], "none");
    assert_eq!(kit.events(), strings(&["plan", "cancel"]));
    kit.advance(COUNTDOWN_MS + 5_000.).await;
    assert!(kit.compacted().is_empty());
    assert!(!kit.band().await.has_like("Compacting in"));
}

#[tokio::test]
async fn a_session_start_in_the_middle_of_a_countdown_leaves_it_running_one_compaction() {
    let kit = Kit::auto(214_000.);
    kit.typed(GO_ON).await;
    kit.turn(LANDED, &[]).await;
    assert!(kit.band().await.has_like("Compacting in 20s"));
    kit.advance(5_000.).await;
    kit.start().await;
    assert!(kit.band().await.has_like("Compacting in 15s"));
    kit.advance(COUNTDOWN_MS - 5_000.).await;
    assert!(until(|| kit.events().contains(&"compact".to_string())).await);
    assert_eq!(kit.compactions(), 1);
    assert_eq!(
        kit.events(),
        strings(&["plan", "countdown", "start", "compact"])
    );
}

// ---------------------------------------------------------------- the compaction marker

/// `Date.UTC(2026, 9, 9, 12)`, the clock of the removed `companion.test.ts`.
const MARKER_NOW: f64 = 1_791_547_200_000.;

fn conversation() -> Value {
    json!([{ "role": "user", "text": "one", "toolUses": [] },
        { "role": "assistant", "text": "done", "toolUses": [] }])
}

fn marker_kit(compact_fails: bool) -> Kit {
    let kit = Kit::new(Setup {
        compact_fails,
        rm_removes: true,
        ..Setup::auto(0.)
    });
    kit.clock.set(MARKER_NOW);
    kit
}

#[tokio::test]
async fn a_compaction_leaves_a_marker_while_it_runs_and_removes_it_when_it_ends() {
    let kit = marker_kit(false);
    let args = json!({ "trigger": "manual", "messages": conversation() });
    kit.compact(args).await.unwrap();
    let markers = kit.markers.lock().unwrap().clone();
    let seen: Value = serde_json::from_str(markers[0].as_deref().unwrap_or("{}")).unwrap();
    assert!(same(
        &seen,
        &json!({ "at": MARKER_NOW / 1000., "trigger": "manual" })
    ));
    assert!(kit.out.file(MARKER).is_none());
}

#[tokio::test]
async fn the_marker_goes_also_when_the_compaction_fails() {
    let kit = marker_kit(true);
    let args = json!({ "trigger": "auto", "messages": conversation() });
    assert!(kit.compact(args).await.is_err());
    let markers = kit.markers.lock().unwrap().clone();
    assert!(!markers.is_empty());
    for marker in &markers {
        let seen: Value = serde_json::from_str(marker.as_deref().unwrap_or("{}")).unwrap();
        assert_eq!(seen["trigger"], "auto");
    }
    assert!(kit.out.file(MARKER).is_none());
}

#[tokio::test]
async fn a_precompute_or_a_subagent_leaves_no_marker() {
    let kit = marker_kit(false);
    let args = json!({ "trigger": "precompute", "messages": conversation() });
    kit.compact(args).await.unwrap();
    let args = json!({ "trigger": "manual", "agentId": "sub-1", "messages": conversation() });
    kit.compact(args).await.unwrap();
    assert_eq!(*kit.markers.lock().unwrap(), [None, None]);
    let writes = kit.out.writes.lock().unwrap().clone();
    assert!(!writes.iter().any(|path| path.contains("/compacting/")));
}

// ---------------------------------------------------------------- native wins for the warning

fn wins(kit: &Kit) -> Vec<Value> {
    kit.out.wins.lock().unwrap().clone()
}

#[tokio::test]
async fn a_native_auto_compaction_goes_into_the_ledger_with_the_compactors_setting() {
    let kit = marker_kit(false);
    kit.compact(json!({ "trigger": "auto", "messages": conversation() }))
        .await
        .unwrap();
    let got = wins(&kit);
    assert_eq!(got.len(), 1);
    assert!(same(
        &got[0],
        &json!({ "at": MARKER_NOW / 1000., "sid": "sess-1", "compactor": "auto" })
    ));
}

#[tokio::test]
async fn a_native_win_while_the_compactor_was_off_says_off() {
    let kit = Kit::new(Setup {
        setting: Some("off"),
        rm_removes: true,
        ..Setup::auto(0.)
    });
    kit.clock.set(MARKER_NOW);
    kit.compact(json!({ "trigger": "auto", "messages": conversation() }))
        .await
        .unwrap();
    assert_eq!(wins(&kit)[0]["compactor"], "off");
}

#[tokio::test]
async fn a_win_is_noted_also_when_the_native_compaction_fails() {
    let kit = marker_kit(true);
    assert!(kit
        .compact(json!({ "trigger": "auto", "messages": conversation() }))
        .await
        .is_err());
    assert_eq!(wins(&kit).len(), 1);
}

#[tokio::test]
async fn only_an_auto_trigger_of_the_main_loop_is_a_native_win() {
    let kit = marker_kit(false);
    for trigger in ["manual", "plugin", "precompute"] {
        kit.compact(json!({ "trigger": trigger, "messages": conversation() }))
            .await
            .unwrap();
    }
    kit.compact(json!({ "trigger": "auto", "agentId": "sub-1", "messages": conversation() }))
        .await
        .unwrap();
    assert!(wins(&kit).is_empty());
}
