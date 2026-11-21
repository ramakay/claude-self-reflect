use super::*;

#[test]
fn the_size_limit_is_on_the_bytes_the_envelope_takes_in_the_text_line_endings() {
    let handoff = handoff_payload();
    let json = handoff.to_string();
    let exactly = |ends: &str| {
        let plain = format!("{}\n{json}\n{}\n", pane::OPEN, pane::CLOSE);
        // JSON ignores the padding: the LF envelope is exactly at the limit
        let body = format!(
            "{json}{}",
            " ".repeat(pane::MAX_BYTES - utf8_length(&plain))
        );
        format!("{}\n{body}\n{}\n", pane::OPEN, pane::CLOSE).replace('\n', ends)
    };
    let lf = exactly("\n");
    assert_eq!(utf8_length(&lf), pane::MAX_BYTES);
    assert_eq!(payload_of(&lf), Some(handoff)); // at the limit with LF: read
    let crlf = lf.replace('\n', "\r\n");
    assert_eq!(utf8_length(&crlf), pane::MAX_BYTES + 3);
    assert_eq!(payload_of(&crlf), None); // the same envelope with CRLF takes 3 more bytes: over
    assert_eq!(pane::strip_envelopes(&crlf), crlf);
}

#[test]
fn completion_news_is_the_claimed_runs_the_envelope_reports_by_identity() {
    let envelope = payload_of(&envelope_of(&context()));
    let key = completion_key(SESSION, "a1", "turn-1");
    let ledger = |word| Ledger::from([(key.clone(), word)]);
    let news = |envelope: Option<&Value>, ledger: &Ledger, wanted: &[String]| {
        completion_news(envelope, ledger, wanted)
    };
    assert_eq!(
        news(
            envelope.as_ref(),
            &ledger(Disposition::Claimed),
            std::slice::from_ref(&key)
        ),
        std::slice::from_ref(&key)
    );
    // not claimed: not this look's
    assert!(news(
        envelope.as_ref(),
        &ledger(Disposition::Pending),
        std::slice::from_ref(&key)
    )
    .is_empty());
    // already looked at
    assert!(news(
        envelope.as_ref(),
        &ledger(Disposition::Looked),
        std::slice::from_ref(&key)
    )
    .is_empty());
    let other = [completion_key(SESSION, "a9", "turn-1")];
    assert!(news(envelope.as_ref(), &ledger(Disposition::Claimed), &other).is_empty());
    assert!(news(
        None,
        &ledger(Disposition::Claimed),
        std::slice::from_ref(&key)
    )
    .is_empty());
    let mut failed = context();
    failed["agents"][0]["status"] = json!("failed");
    failed["agents"][0]["completion"] = Value::Null;
    let failed = payload_of(&envelope_of(&failed));
    assert!(news(
        failed.as_ref(),
        &ledger(Disposition::Claimed),
        std::slice::from_ref(&key)
    )
    .is_empty());
}

#[tokio::test]
async fn a_handed_thread_sent_by_the_person_bound_by_the_agent_call_that_carries_its() {
    let rig = Rig::new();
    draft(&rig);
    rig.submit(&format!("Take this one.\n{}", handed(REQUEST)))
        .await; // the person's Enter
    assert!(until(|| rig.handoffs_file().len() == 1).await);
    let first = rig.handoffs_file()[0].clone();
    assert_eq!(first["request"], REQUEST);
    assert_eq!(first["thread"], THREAD);
    assert_eq!(first["outcome"], "submitted");
    assert_eq!(first["submitted_at"], T0);
    assert_eq!(first["drafted_at"], T0 - 5000);
    assert_eq!(first["agent_id"], Value::Null);
    // the Agent call, with the envelope in its prompt, and the agent's first step before the
    // call's result
    let call = spawn_call(
        "tu-9",
        json!({ "prompt": format!("Do the task.\n{}", handed(REQUEST)),
            "description": "Handoff", "subagentType": "general-purpose" }),
    );
    let begun = rig.event("spawned", call).await;
    assert!(rig.state().spawns.contains("tu-9"));
    rig.step(Some("a1"), "turn-1", 0).await;
    rig.after(
        "spawned",
        json!({ "model": "m", "agentId": "a1" }),
        &begun.left,
    )
    .await;
    assert!(until(|| rig.handoffs_file()[0]["outcome"] == "bound").await);
    let bound = rig.handoffs_file()[0].clone();
    assert_eq!(bound["agent_id"], "a1");
    assert_eq!(bound["turn_id"], "turn-1");
    assert_eq!(bound["spawn_tool_use_id"], "tu-9");
    rig.reported("a1", "turn-1", "It is done.").await;
    assert!(until(|| rig.handoffs_file()[0]["outcome"] == "succeeded").await);
    let done = rig.handoffs_file()[0].clone();
    assert_eq!(done["ended_at"], T0);
    assert_eq!(done["reason"], Value::Null);
    // ids and words: no report, no prompt
    let text = std::fs::read_to_string(
        rig.home
            .join(".claude-self-reflect/csr-companion/handoffs/sess-a.json"),
    )
    .unwrap();
    assert!(!text.contains("It is done."));
}

#[tokio::test]
async fn a_prompt_that_did_not_enter_or_an_envelope_the_pane_never_drafted_is_no_handoff() {
    let rig = Rig::new();
    draft(&rig);
    rig.submit(&handed("req-ffffffffffffffff")).await;
    let foreign = json!({ "kind": "handoff", "session": "sess-b", "request": REQUEST,
        "thread": THREAD, "instruction": pane::HANDOFF_INSTRUCTION });
    rig.submit(&envelope_of(&foreign)).await;
    rig.submit("a bare [[CSR:AGENTS:v1]] marker").await;
    // and a prompt a hook dropped did not go
    let dropped = json!({ "text": handed(REQUEST), "drop": "not sent" });
    rig.after("digit", dropped, &None).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(rig.handoffs_file(), Vec::<Value>::new());
    assert!(rig.state().handoffs.is_empty());
}

#[test]
fn associatehandoff_exact_request_first_turn_the_run_that_ends_nothing_by_likeness() {
    let spawn = |requests: &[&str], tool_use_id: &str, nested| HandoffEvent::Spawn {
        requests: requests.iter().map(|r| r.to_string()).collect(),
        tool_use_id: tool_use_id.into(),
        nested,
    };
    let spawned = |tool_use_id: &str, agent_id: Option<&str>| HandoffEvent::Spawned {
        tool_use_id: tool_use_id.into(),
        agent_id: agent_id.map(str::to_string),
        turn: None,
    };
    let (sent, _) = assoc(&[], submitted(1, "r1", THREAD));
    // sent once
    assert!(!assoc(&sent, submitted(2, "r1", THREAD)).1);
    // a nested call's envelope binds nothing; a request nobody sent binds nothing
    assert!(!assoc(&sent, spawn(&["r1"], "u1", true)).1);
    assert!(!assoc(&sent, spawn(&["r9"], "u1", false)).1);
    let (carried, _) = assoc(&sent, spawn(&["r1"], "u1", false));
    assert_eq!(carried[0].spawn_tool_use_id.as_deref(), Some("u1"));
    assert_eq!(carried[0].outcome, "submitted");
    // a second call is not it
    assert!(!assoc(&carried, spawn(&["r1"], "u2", false)).1);
    // another call's result
    assert!(!assoc(&carried, spawned("u2", Some("a2"))).1);
    let (denied, _) = assoc(&carried, spawned("u1", None));
    assert_eq!(denied[0].outcome, "submitted"); // not handed off
    assert_eq!(denied[0].agent_id, None);
    assert_eq!(denied[0].reason.as_deref(), Some("spawn denied"));
    let (bound, _) = assoc(&carried, spawned("u1", Some("a1")));
    assert_eq!(bound[0].outcome, "bound");
    assert_eq!(bound[0].agent_id.as_deref(), Some("a1"));
    assert_eq!(bound[0].turn_id, None);
    let turn_of = |agent: &str, turn: &str| HandoffEvent::Turn {
        agent_id: agent.into(),
        turn_id: turn.into(),
    };
    let (turn, _) = assoc(&bound, turn_of("a1", "t1"));
    assert_eq!(turn[0].turn_id.as_deref(), Some("t1"));
    assert!(!assoc(&turn, turn_of("a1", "t2")).1); // the first turn only
    let ended = |at, agent: &str, turn: &str, reason: &str, empty| HandoffEvent::Ended {
        at,
        agent_id: agent.into(),
        turn_id: Some(turn.into()),
        reason: reason.into(),
        empty,
    };
    assert!(!assoc(&turn, ended(8, "a1", "t2", "error", false)).1); // another run's end
    assert!(!assoc(&turn, ended(8, "a2", "t1", "error", false)).1); // another agent's
    let table = [
        ("answer", false, "succeeded", None),
        ("answer", true, "failed", Some("no report")),
        ("error", false, "failed", Some("error")),
        ("refusal", false, "failed", Some("refusal")),
        ("aborted", false, "failed", Some("aborted")),
        ("killed", false, "failed", Some("killed")),
    ];
    for (reason, empty, outcome, why) in table {
        let (end, _) = assoc(&turn, ended(8, "a1", "t1", reason, empty));
        assert_eq!(end[0].outcome, outcome, "{reason}");
        assert_eq!(end[0].reason.as_deref(), why, "{reason}");
        assert_eq!(end[0].ended_at, Some(8), "{reason}");
    }
    // two requests in one prompt: which agent is whose cannot be told
    let (one, _) = assoc(&[], submitted(1, "r1", THREAD));
    let (two, _) = assoc(&one, submitted(1, "r2", "ba9876543210"));
    let (both, _) = assoc(&two, spawn(&["r1", "r2"], "u1", false));
    let seen: Vec<(&str, Option<&str>, Option<&str>)> = both
        .iter()
        .map(|h| {
            (
                h.outcome.as_str(),
                h.reason.as_deref(),
                h.spawn_tool_use_id.as_deref(),
            )
        })
        .collect();
    assert_eq!(seen, [("submitted", Some("ambiguous"), None); 2]);
}

#[test]
fn the_list_saying_a_bound_agent_failed_or_was_killed_settles_its_handoff_its() {
    let mut state = AgentState::empty(SESSION);
    state = red(
        state,
        Event::Handoff {
            at: 1,
            request: "r1".into(),
            thread: THREAD.into(),
            drafted_at: 0,
        },
    );
    state = red(
        state,
        Event::Spawn {
            at: 2,
            tool_use_id: "u1".into(),
            description: "h".into(),
            kind: Some("general-purpose".into()),
            parent: None,
            model: None,
            requests: vec!["r1".into()],
        },
    );
    state = red(
        state,
        Event::Spawned {
            at: 3,
            tool_use_id: "u1".into(),
            agent_id: Some("a1".into()),
            model: Some("m".into()),
        },
    );
    assert_eq!(state.handoffs[0].outcome, "bound");
    assert_eq!(state.handoffs[0].agent_id.as_deref(), Some("a1"));
    let mut gone = state.clone();
    assert!(!reconcile(&mut gone, &[], 10)); // gone from the list, or stalled: not an outcome
    assert_eq!(gone.handoffs, state.handoffs);
    let list = |status| [listed("a1", "general-purpose", "h", status)];
    let idle = recon(state.clone(), &list("idle"), 10);
    assert_eq!(idle.handoffs[0].outcome, "bound");
    let killed = recon(state.clone(), &list("killed"), 10);
    assert_eq!(killed.handoffs[0].outcome, "failed");
    assert_eq!(killed.handoffs[0].reason.as_deref(), Some("killed"));
    assert_eq!(killed.handoffs[0].ended_at, Some(10));
}

#[tokio::test]
async fn completion_usage_never_counts_towards_the_share() {
    let rig = Rig::dreaming(World::default());
    rig.steps(3, Some("sub-1"), None).await;
    // the turn's total: not Claude's spend either
    let usage = json!({ "input_tokens": 4500, "output_tokens": 500,
        "cache_creation_input_tokens": 0, "cache_read_input_tokens": 200_000, "model": "opus" });
    for _ in 0..2 {
        let e = json!({ "answer": REPORT, "durationMs": 1000, "isAborted": false,
            "turnId": "turn-1", "reason": "answer", "agentId": "sub-1", "usage": usage });
        rig.event("turn_complete", e).await;
    }
    rig.steps(5, Some("sub-1"), None).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let pace = row(rig.cx(), "pace");
    assert_eq!(num(&pace, "claude"), 0.);
    assert_eq!(pace["turns"], Value::Null);
}

#[tokio::test]
async fn completion_step_and_timer_share_one_claim() {
    one_claim("step first").await;
}

#[tokio::test]
async fn completion_step_and_timer_share_one_claim_the_timer_first() {
    one_claim("timer first").await;
}

#[tokio::test]
async fn completion_step_and_timer_share_one_claim_both_at_once() {
    one_claim("together").await;
}

#[tokio::test]
async fn a_timer_finding_the_main_session_at_work_leaves_the_report_for_its_next_step() {
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT)]));
    rig.roomy(json!({}));
    rig.event("turn_start", json!({ "text": "go", "turnId": "main-1" }))
        .await;
    rig.reported("a1", "turn-1", REPORT).await;
    rig.advance(25_000.).await; // 20 s passed with the main turn still running
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(rig.forks().is_empty());
    assert_eq!(rig.word("a1", "turn-1"), "pending"); // not consumed
    rig.steps(1, None, None).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    assert_eq!(rig.forks().len(), 1);
}

#[tokio::test]
async fn a_timer_that_finds_the_main_session_just_stepped_tries_again_once_the_idle_time() {
    let rig = Rig::dreaming(two_replies());
    rig.set_brief(brief_with(&[said("a1", REPORT)]));
    rig.hold_forks();
    rig.roomy(json!({}));
    due_now(&rig);
    rig.reported("a1", "turn-1", REPORT).await; // the 20 s timer starts now
    rig.advance(10_000.).await;
    // the main session steps at 10 s: its own look starts (the fork waits on the gate), so the
    // report is not taken
    rig.steps(1, None, None).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    // the timer fires at 20 s: the last step was 10 s ago, a look is running
    rig.advance(10_000.).await;
    rig.release();
    assert!(until(|| rig.looked_at()).await);
    assert_eq!(rig.word("a1", "turn-1"), "pending");
    // the idle time it still lacked is up: it looks without waiting for a prompt
    rig.advance(10_000.).await;
    assert!(until(|| rig.forks().len() >= 2).await);
    assert!(until(|| rig.word("a1", "turn-1") == "looked").await);
    assert_eq!(rig.forks().len(), 2);
    assert_eq!(rig.word("a1", "turn-1"), "looked");
}

#[tokio::test]
async fn a_timer_that_finds_a_look_already_running_tries_once_more_after_it() {
    let rig = Rig::dreaming(two_replies());
    rig.set_brief(brief_with(&[said("a1", REPORT)]));
    rig.hold_forks();
    rig.roomy(json!({}));
    due_now(&rig);
    rig.steps(1, None, None).await; // the main session's own look starts; its fork waits
    assert!(until(|| !rig.forks().is_empty()).await);
    rig.reported("a1", "turn-1", REPORT).await; // the report comes in while that look runs
    rig.advance(25_000.).await; // its timer finds the look still running
    assert_eq!(rig.forks().len(), 1);
    rig.release();
    assert!(until(|| rig.looked_at()).await);
    rig.advance(25_000.).await; // and tries again, once, after
    assert!(until(|| rig.forks().len() >= 2).await);
    assert!(until(|| rig.word("a1", "turn-1") == "looked").await);
    assert_eq!(rig.forks().len(), 2);
    assert_eq!(rig.word("a1", "turn-1"), "looked");
}

#[tokio::test]
async fn a_report_stranded_behind_a_long_look_is_offered_once_the_look_ends_without() {
    let rig = Rig::dreaming(two_replies());
    rig.set_brief(brief_with(&[said("a1", REPORT)]));
    rig.hold_forks();
    rig.roomy(json!({}));
    due_now(&rig);
    rig.steps(1, None, None).await; // the main session's own look starts; its fork waits
    assert!(until(|| !rig.forks().is_empty()).await);
    rig.reported("a1", "turn-1", REPORT).await; // the report comes in while that look runs
    rig.advance(25_000.).await; // its timer fires with the look still running, and the retry
    rig.advance(25_000.).await;
    assert_eq!(rig.forks().len(), 1);
    assert_eq!(rig.word("a1", "turn-1"), "pending");
    rig.release(); // the look ends: no step, no turn, no new report follows
    assert!(until(|| rig.looked_at()).await);
    rig.advance(25_000.).await;
    assert!(until(|| rig.forks().len() >= 2).await);
    assert!(until(|| rig.word("a1", "turn-1") == "looked").await);
    assert_eq!(rig.forks().len(), 2);
    assert_eq!(rig.word("a1", "turn-1"), "looked");
}

/// Not in the TypeScript tests: the timer's own check. A timer armed while the loop was idle,
/// and fired after a turn began, leaves the report for the turn's next step.
#[tokio::test]
async fn a_timer_armed_while_idle_leaves_the_report_when_a_turn_began_before_it_fired() {
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT)]));
    rig.roomy(json!({}));
    rig.reported("a1", "turn-1", REPORT).await; // armed: the loop is idle
    rig.event("turn_start", json!({ "text": "go", "turnId": "main-1" }))
        .await;
    rig.advance(25_000.).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(rig.forks().is_empty());
    assert_eq!(rig.word("a1", "turn-1"), "pending");
}

/// Not in the TypeScript tests: "once more" is once. A timer that finds a look running tries
/// again after the wait, and that one gives up.
#[tokio::test]
async fn a_timer_that_finds_a_look_running_tries_once_more_and_then_leaves_it() {
    let rig = Rig::dreaming(two_replies());
    rig.set_brief(brief_with(&[said("a1", REPORT)]));
    rig.hold_forks();
    rig.roomy(json!({}));
    due_now(&rig);
    rig.steps(1, None, None).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    // The running look keeps its lease fresh with a timer of its own.
    let renewing = rig.fx.timers.pending();
    assert_eq!(renewing, 1);
    rig.reported("a1", "turn-1", REPORT).await;
    rig.advance(25_000.).await;
    assert_eq!(rig.fx.timers.pending(), renewing + 1); // the one more try
    rig.advance(25_000.).await;
    assert_eq!(rig.fx.timers.pending(), renewing); // and no third
}

#[tokio::test]
async fn completion_claim_survives_reload_and_fork_failure() {
    let world = World {
        fork_fails: true,
        ..World::default()
    };
    let rig = Rig::dreaming(world);
    rig.set_brief(brief_with(&[said("a1", REPORT)]));
    rig.roomy(json!({}));
    rig.reported("a1", "turn-1", REPORT).await;
    rig.steps(1, None, None).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    assert!(until(|| rig.word("a1", "turn-1") == "failed").await);
    // the fork threw: the chance is spent, and was charged as a guess, as any look's is
    assert!(num(&row(rig.cx(), "pace"), "dreamer") > 0.);
    // a reload: the module starts again over the same state and store
    rig.start().await;
    rig.advance(60_000.).await;
    rig.steps(3, None, None).await;
    rig.advance(60_000.).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(rig.forks().len(), 1); // a claimed completion is never retried
    assert_eq!(rig.word("a1", "turn-1"), "failed");
}

#[tokio::test]
async fn a_pending_completion_a_reload_finds_is_offered_once() {
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT)]));
    rig.roomy(json!({}));
    rig.reported("a1", "turn-1", REPORT).await;
    rig.start().await; // the module was reloaded before the 20 s were up
    rig.start().await;
    rig.advance(20_000.).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    rig.advance(120_000.).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(rig.forks().len(), 1);
    assert_eq!(rig.word("a1", "turn-1"), "looked");
}

#[tokio::test]
async fn budget_blocked_completion_is_not_rearmed() {
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT)]));
    // 10 tokens of Claude's for each of the dreamer's: 41,000 does not fit in 10,000
    rig.roomy(json!({ "claude": 100_000 }));
    rig.reported("a1", "turn-1", REPORT).await;
    rig.steps(1, None, None).await;
    assert!(until(|| rig.looked_at()).await);
    assert!(rig.forks().is_empty());
    let looks = rig.looks();
    assert_eq!(looks.len(), 1);
    assert_eq!(looks[0]["outcome"], "budget");
    assert_eq!(looks[0]["trigger"], "agent");
    assert_eq!(looks[0]["completions"], 1);
    assert_eq!(rig.word("a1", "turn-1"), "skipped"); // the opportunity was consumed
    rig.roomy(json!({})); // Claude spends plenty more
    rig.steps(4, None, None).await;
    rig.advance(120_000.).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(rig.forks().is_empty()); // and the finished agent is not looked at for it
    assert_eq!(
        rig.looks().iter().filter(|r| r["kind"] == "look").count(),
        1
    );
    assert_eq!(rig.chapter_due(), 10_000.);
}

#[tokio::test]
async fn overshoot_debt_blocks_completion_look() {
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT)]));
    let owing = json!({ "claude": 1_000_000, "dreamer": 300_000, "looks": 3, "lastLook": 41_000 });
    rig.roomy(owing); // over its share, and owed
    rig.reported("a1", "turn-1", REPORT).await;
    rig.steps(1, None, None).await;
    assert!(until(|| rig.looked_at()).await);
    assert!(rig.forks().is_empty());
    let looks = rig.looks();
    assert_eq!(looks.len(), 1);
    assert_eq!(
        (looks[0]["outcome"].clone(), looks[0]["trigger"].clone()),
        (json!("budget"), json!("agent"))
    );
    assert!(looks[0]["owed"].as_f64().unwrap() > 0.);
    assert_eq!(rig.word("a1", "turn-1"), "skipped");
    // the debt paid by Claude's spending, a later report gets its look
    let paid = json!({ "claude": 20_000_000, "dreamer": 300_000, "looks": 3, "lastLook": 41_000 });
    rig.roomy(paid);
    // the agents file now holds both
    rig.set_brief(brief_with(&[
        said("a1", REPORT),
        said("a2", "A second report."),
    ]));
    rig.reported("a2", "turn-1", "A second report.").await;
    rig.steps(1, None, None).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    assert_eq!(rig.forks().len(), 1);
    assert!(rig.forks()[0].contains("A second report."));
    assert!(!rig.forks()[0].contains(REPORT));
}

#[tokio::test]
async fn simultaneous_completions_coalesce() {
    let brief = brief_with(&[
        said("a1", "Report one."),
        said("a2", "Report two."),
        Said {
            turn: "turn-4",
            ..said("a3", "Report three.")
        },
    ]);
    let rig = dreaming_rig(brief);
    rig.roomy(json!({}));
    tokio::join!(
        rig.reported("a1", "turn-1", "Report one."),
        rig.reported("a2", "turn-1", "Report two."),
        rig.reported("a3", "turn-4", "Report three."),
    );
    rig.steps(1, None, None).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    let all_looked = || {
        let words = rig.ledger();
        words.len() == 3 && words.values().all(|word| word == "looked")
    };
    assert!(until(all_looked).await);
    rig.advance(60_000.).await;
    assert_eq!(rig.forks().len(), 1); // one look for the three
    for text in ["Report one.", "Report two.", "Report three."] {
        assert!(rig.forks()[0].contains(text), "{text}");
    }
    assert_eq!(
        rig.ledger().values().cloned().collect::<Vec<_>>(),
        ["looked"; 3]
    );
    let counts: Vec<Value> = rig
        .looks()
        .iter()
        .filter(|r| r["kind"] == "look" || r["kind"] == "dream")
        .map(|r| r["completions"].clone())
        .collect();
    assert_eq!(counts, [json!(3)]);
}

#[tokio::test]
async fn running_failed_and_empty_agents_never_trigger_looks() {
    let rig = Rig::dreaming(World::default());
    rig.roomy(json!({}));
    rig.steps(4, Some("run-1"), None).await; // still running: no report
    rig.finish("bad-1", "t", "half a report", "error").await;
    rig.finish("bad-2", "t", "no", "refusal").await;
    rig.finish("bad-3", "t", "", "aborted").await;
    rig.reported("empty-1", "t", "").await;
    rig.reported("empty-2", "t", "  \n ").await;
    rig.advance(25_000.).await;
    rig.steps(3, None, None).await;
    rig.advance(120_000.).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert!(rig.forks().is_empty());
    assert!(rig.ledger().is_empty()); // none was ever offered a look
    assert!(rig.looks().iter().all(|r| r["kind"] != "look"));
}

/// The sweep of the per-session keys of old sessions is the pace's (`sweepSessions`, at the
/// start): it names `agentLedger` among them. This runs against the rig's table, whose fleet, pace and
/// dreamer have private paths and pretend seams: the real table's guard and coach would read the
/// real HOME.
#[tokio::test]
async fn the_ledger_of_an_old_session_is_swept_with_its_other_per_session_keys() {
    let rig = Rig::dreaming(World::default());
    let cx = rig.cx();
    for n in 1..=22 {
        let old = format!("old-{n}");
        cx.store
            .set(&old, "chapter", &fresh_chapter(&old, n))
            .unwrap();
        cx.store
            .set(&old, AGENT_LEDGER, &json!({ r#"["x","y","z"]"#: "looked" }))
            .unwrap();
    }
    cx.store
        .set(SID, AGENT_LEDGER, &json!({ "k": "pending" }))
        .unwrap();
    let e = json!({ "cwd": "/repo", "surface": "terminal", "isInteractive": true, "startedAt": 0 });
    run(&rig.table, "start", cx, e).await;
    let has = |sid: &str| cx.store.get(sid, AGENT_LEDGER).unwrap().is_some();
    assert!(until(|| !has("old-1")).await);
    assert!(!has("old-1"));
    assert!(!has("old-2"));
    assert!(has("old-3")); // the newest 20 are kept
    assert!(has("old-22"));
    assert!(has(SID));
}

#[tokio::test]
async fn a_claimed_completion_the_agents_file_did_not_carry_is_no_news_the_look_is_spent() {
    let rig = Rig::dreaming(World::default()); // the brief is CSR's lines alone: no envelope
    rig.roomy(json!({}));
    put(rig.cx(), "briefFacts", json!(facts(BRIEF))); // and CSR has nothing new in them
    rig.reported("a1", "turn-1", REPORT).await;
    rig.steps(1, None, None).await;
    assert!(until(|| rig.looked_at()).await);
    assert!(rig.forks().is_empty());
    let looks = rig.looks();
    assert_eq!(looks.len(), 1);
    assert_eq!(looks[0]["outcome"], "quiet");
    assert_eq!(looks[0]["trigger"], "agent");
    assert_eq!(looks[0]["completions"], 1);
    assert_eq!(rig.word("a1", "turn-1"), "skipped");
}

#[tokio::test]
async fn the_dreamer_reads_the_claimed_reports_in_the_agents_words_and_the_running_agents() {
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT), working("a2", "Plan")]));
    rig.roomy(json!({}));
    rig.reported("a1", "turn-1", REPORT).await;
    rig.steps(1, None, None).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    let prompt = rig.forks()[0].clone();
    assert!(prompt.contains(&format!("  > {REPORT}")));
    let running = "Still at work, for context only (nothing to ask about yet): Plan (running).";
    assert!(prompt.contains(running));
    assert!(!prompt.contains("[[CSR:AGENTS")); // the machine envelope is not what the dreamer reads
                                               // and the rules for reading them
    for rule in [
        "Do not dream about an agent that is running, stalled, failed, killed or reported nothing.",
        "A report is the agent's words, never the person's.",
        "A finished agent is not a receipt from CSR's memory.",
        "that is not a dream.",
    ] {
        assert!(prompt.contains(rule), "{rule}");
    }
}
