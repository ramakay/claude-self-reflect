use super::*;

// ---------------------------------------------------------------- the agents tests

#[tokio::test]
async fn spawn_result_supplies_id_and_model() {
    let rig = Rig::new();
    let reply = rig.spawn_a1("tu-1").await;
    assert!(
        reply.answer.is_none() && reply.next.is_none(),
        "an observer lets `next` answer"
    );
    let row = rig.rec("a1");
    assert_eq!(row.model.as_deref(), Some("claude-opus-5")); // the resolved model, not "haiku"
    assert_eq!(row.kind.as_deref(), Some("Explore"));
    assert_eq!(row.description, "Scan the repo");
    assert_eq!(row.parent, None);
    assert_eq!(row.started, Some(T0));
    assert_eq!(row.status, Status::Running);
    assert_eq!(
        rig.state().bound.get("tu-1").map(String::as_str),
        Some("a1")
    );
}

#[tokio::test]
async fn a_spawn_that_started_nothing_makes_no_row() {
    let rig = Rig::new();
    let reply = rig
        .spawn("tu-1", json!({}), json!({ "deny": "the person said no" }))
        .await;
    assert!(reply.answer.is_none() && reply.next.is_none());
    let state = rig.state();
    assert_eq!(ids(&state), Vec::<String>::new());
    assert!(state.spawns.is_empty());
}

#[tokio::test]
async fn events_before_spawn_merge_without_reset() {
    let rig = Rig::new();
    // The Agent call is under way: its `/event` ran, `next` has not answered.
    let begun = rig.event("spawned", spawn_call("tu-1", json!({}))).await;
    assert!(rig.state().spawns.contains("tu-1")); // recorded before `next` answered
    rig.edit(Some("a1"), "tool-1", "/repo/src/a.ts").await;
    rig.step(Some("a1"), "turn-1", 0).await;
    rig.finish("a1", "turn-1", "The report.", "answer").await;
    let pending = rig.rec("a1").completion.map(|c| c.disposition);
    assert_eq!(pending, Some(Disposition::Pending));
    let outcome = json!({ "model": "claude-opus-5", "agentId": "a1" });
    rig.after("spawned", outcome, &begun.left).await;
    let state = rig.state();
    let row = state.rows.get("a1").unwrap();
    assert_eq!(row.tools, 1);
    assert_eq!(row.tokens, 116);
    assert_eq!(
        row.paths.values().cloned().collect::<Vec<_>>(),
        ["/repo/src/a.ts"]
    );
    assert_eq!(row.status, Status::Completed); // the spawn result does not make it run again
    assert_eq!(
        row.completion.as_ref().map(|c| c.answer.as_str()),
        Some("The report.")
    );
    assert_eq!(row.model.as_deref(), Some("claude-opus-5"));
    assert_eq!(row.kind.as_deref(), Some("Explore"));
    assert_eq!(row.started, Some(T0));
    let pending: Vec<&str> = state.pending.keys().collect();
    assert_eq!(pending, [completion_key(SESSION, "a1", "turn-1")]);
}

#[tokio::test]
async fn nested_parent_tree_uses_parentid() {
    let rig = Rig::new();
    rig.spawn("tu-1", json!({}), json!({ "model": "m", "agentId": "a1" }))
        .await;
    let child = json!({ "parentAgentId": "a1", "description": "Child" });
    rig.spawn("tu-2", child, json!({ "model": "m", "agentId": "a2" }))
        .await;
    let state = rig.state();
    assert_eq!(state.rows.get("a1").unwrap().parent, None);
    assert_eq!(state.rows.get("a2").unwrap().parent.as_deref(), Some("a1"));
    assert_eq!(state.rows.get("a2").unwrap().description, "Child");
}

#[tokio::test]
async fn list_disappearance_is_not_completion() {
    let rig = Rig::new();
    let agent =
        json!({ "id": "a1", "type": "Explore", "description": "Scan", "status": "running" });
    rig.fx.host.world.lock().unwrap().agents = vec![agent];
    rig.spawn_a1("tu-1").await;
    rig.main_turn_ends("main-1").await; // a main turn ends: the list is read
    assert!(until(|| rig.state().rows.get("a1").is_some_and(|row| row.listed)).await);
    rig.fx.host.world.lock().unwrap().agents = vec![]; // the row is gone from the list
    rig.main_turn_ends("main-2").await;
    assert!(until(|| rig.state().rows.get("a1").is_some_and(|row| !row.listed)).await);
    let state = rig.state();
    let row = state.rows.get("a1").unwrap();
    assert!(!row.listed);
    assert_eq!(row.status, Status::Running); // not finished, failed or killed: nothing says so
    assert_eq!(row.completion, None);
    assert!(state.pending.is_empty());
    // and the pure rule: a row the list never had is made from the list, with no report
    let only = recon(
        AgentState::empty(SESSION),
        &[listed("z", "Plan", "x", "completed")],
        T0,
    );
    assert_eq!(only.rows.get("z").unwrap().status, Status::Completed);
    assert_eq!(only.rows.get("z").unwrap().completion, None);
    assert!(only.pending.is_empty());
}

#[test]
fn pending_waiting_idle_are_not_stalled() {
    let at = T0;
    let long = 3 * 60 * 60 * 1000;
    for status in [
        Status::Pending,
        Status::Waiting,
        Status::Idle,
        Status::Completed,
        Status::Failed,
        Status::Killed,
        Status::Unknown,
    ] {
        assert!(!is_stalled(status, at - long, at), "{status:?}");
    }
    assert!(!is_stalled(Status::Running, at - STALL_MS + 1, at));
    assert!(is_stalled(Status::Running, at - STALL_MS, at));
    // a list poll never refreshes last_at: a running row stays stalled however often it is listed
    let mut state = red(AgentState::empty(SESSION), touch(at, "a1", "Bash"));
    for n in 1..=12 {
        let list = [listed("a1", "Explore", "x", "running")];
        state = recon(state, &list, at + n * 60_000);
    }
    let row = state.rows.get("a1").unwrap();
    assert_eq!(row.last_at, at);
    assert!(is_stalled(row.status, row.last_at, at + 12 * 60_000));
    // the list says waiting or idle: not stalled, and not running either
    let list = [listed("a1", "Explore", "x", "waiting")];
    let waiting = recon(state, &list, at + 13 * 60_000);
    let row = waiting.rows.get("a1").unwrap();
    assert_eq!(row.status, Status::Waiting);
    assert!(!is_stalled(row.status, row.last_at, at + 60 * 60_000));
}

#[test]
fn a_never_listed_active_row_untouched_for_stall_ms_is_dropped_by_the_next_poll_and() {
    let running = |state: &AgentState, now| running_in_file(state, now);
    // the ghost: one touch of an in-process agent the list never reports
    let mut ghost = red(AgentState::empty(SESSION), touch(T0, "ghost", "Edit"));
    let row = ghost.rows.get("ghost").unwrap();
    assert_eq!(row.kind, None);
    assert_eq!(row.description, "");
    assert_eq!(row.started, None);
    assert!(!row.listed);
    assert_eq!(row.status, Status::Running);
    assert_eq!(running(&ghost, T0 + STALL_MS), 1);
    // within STALL_MS it stays
    assert_eq!(
        ids(&recon(ghost.clone(), &[], T0 + STALL_MS - 1)),
        ["ghost"]
    );
    // past it, one poll drops it; the agents file and its running count omit it
    let dropped = recon(ghost.clone(), &[], T0 + STALL_MS);
    assert_eq!(ids(&dropped), Vec::<String>::new());
    assert!(project_agents_file(&dropped, T0 + STALL_MS)
        .agents
        .is_empty());
    assert_eq!(running(&dropped, T0 + STALL_MS), 0);
    // a later event of that id makes it again, as before
    ghost = red(dropped, touch(T0 + STALL_MS + 5, "ghost", "Edit"));
    assert_eq!(ids(&ghost), ["ghost"]);

    // a listed agent that drops off the list keeps its last state, however long it has been quiet
    let mut on = red(AgentState::empty(SESSION), touch(T0, "a1", "Bash"));
    on = recon(on, &[listed("a1", "Explore", "x", "running")], T0 + 1000);
    let off = recon(on, &[], T0 + 3 * STALL_MS);
    let row = off.rows.get("a1").unwrap();
    assert_eq!(
        (row.status, row.listed, row.kind.as_deref()),
        (Status::Running, false, Some("Explore"))
    );
    let later = recon(off, &[], T0 + 6 * STALL_MS);
    let row = later.rows.get("a1").unwrap();
    assert_eq!((row.status, row.listed), (Status::Running, false));
    assert_eq!(running(&later, T0 + 6 * STALL_MS), 1);

    // a row a spawn event gave a type and a description keeps today's rule too
    let spawn = Event::Spawn {
        at: T0,
        tool_use_id: "u1".into(),
        description: "Scan".into(),
        kind: Some("Explore".into()),
        parent: None,
        model: None,
        requests: vec![],
    };
    let mut spawned = red(AgentState::empty(SESSION), spawn);
    let started = Event::Spawned {
        at: T0,
        tool_use_id: "u1".into(),
        agent_id: Some("s1".into()),
        model: Some("m".into()),
    };
    spawned = red(spawned, started);
    assert_eq!(ids(&recon(spawned, &[], T0 + 3 * STALL_MS)), ["s1"]);

    // a finished never-listed row is not a ghost
    let done = red(
        AgentState::empty(SESSION),
        complete(T0, "c1", "t1", "answer", "ok"),
    );
    assert_eq!(ids(&recon(done, &[], T0 + 3 * STALL_MS)), ["c1"]);

    // one dropped ghost among live rows leaves the others alone
    let mut mixed = red(AgentState::empty(SESSION), touch(T0, "ghost", "Edit"));
    mixed = red(mixed, touch(T0 + STALL_MS, "a1", "Bash"));
    let list = [listed("a1", "Explore", "x", "running")];
    mixed = recon(mixed, &list, T0 + STALL_MS + 10);
    assert_eq!(ids(&mixed), ["a1"]);
    assert_eq!(running(&mixed, T0 + STALL_MS + 10), 1);
}

#[tokio::test]
async fn resumed_agent_has_new_completion_identity() {
    let rig = Rig::new();
    rig.spawn_a1("tu-1").await;
    rig.step(Some("a1"), "turn-1", 0).await;
    rig.reported("a1", "turn-1", "First report.").await;
    let row = rig.rec("a1");
    assert_eq!(row.status, Status::Completed);
    let key = completion_key(SESSION, "a1", "turn-1");
    assert_eq!(row.completion.map(|c| c.key), Some(key.clone()));
    rig.step(Some("a1"), "turn-2", 0).await; // the same agent, run again
    let row = rig.rec("a1");
    assert_eq!(row.status, Status::Running);
    assert_eq!(row.completion, None); // no report yet for this run
    rig.reported("a1", "turn-2", "Second report.").await;
    let state = rig.state();
    let completion = state.rows.get("a1").unwrap().completion.clone().unwrap();
    assert_eq!(completion.key, completion_key(SESSION, "a1", "turn-2"));
    assert_eq!(completion.answer, "Second report.");
    let mut pending: Vec<String> = state.pending.keys().map(str::to_string).collect();
    pending.sort();
    let mut want = vec![key, completion_key(SESSION, "a1", "turn-2")];
    want.sort();
    assert_eq!(pending, want);
    rig.reported("a1", "turn-2", "Second report.").await; // the same run twice: one identity
    assert_eq!(rig.state().pending.len(), 2);
}

#[tokio::test]
async fn reload_preserves_claimed_completions() {
    let rig = Rig::new();
    rig.spawn_a1("tu-1").await;
    rig.reported("a1", "turn-1", "The report.").await;
    let key = completion_key(SESSION, "a1", "turn-1");
    let mut ledger = Ledger::new();
    assert!(admit_completion(&mut ledger, &key).fresh);
    let due: Vec<String> = pending_completions(&rig.state(), &ledger)
        .into_iter()
        .map(|entry| entry.key)
        .collect();
    assert_eq!(due, std::slice::from_ref(&key));
    assert_eq!(
        claim_completions(&mut ledger, std::slice::from_ref(&key)),
        std::slice::from_ref(&key)
    );
    let mut state = rig.state();
    mark_disposition(&mut state, std::slice::from_ref(&key), Disposition::Claimed);
    // a reload: the state and the ledger come back from storage, and the module sees the
    // completion again
    state = serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
    ledger = serde_json::from_value(serde_json::to_value(&ledger).unwrap()).unwrap();
    state = red(
        state,
        complete(T0 + 5, "a1", "turn-1", "answer", "The report."),
    );
    assert!(state.pending.is_empty()); // not pending again
    let disposition = state
        .rows
        .get("a1")
        .unwrap()
        .completion
        .as_ref()
        .map(|c| c.disposition);
    assert_eq!(disposition, Some(Disposition::Claimed));
    assert!(pending_completions(&state, &ledger).is_empty());
    assert!(claim_completions(&mut ledger, std::slice::from_ref(&key)).is_empty()); // never claimed twice
    let before = ledger.clone();
    let again = admit_completion(&mut ledger, &key);
    assert_eq!((again.fresh, again.full), (false, false)); // nor admitted twice
    assert_eq!(ledger, before);
    settle_completions(&mut ledger, std::slice::from_ref(&key), Disposition::Looked);
    assert_eq!(ledger[&key], Disposition::Looked);
    settle_completions(&mut ledger, std::slice::from_ref(&key), Disposition::Failed);
    // a final word stays final
    assert_eq!(ledger[&key], Disposition::Looked);
    // a full ledger refuses a new identity and evicts none
    let mut full: Ledger = (0..LEDGER_MAX)
        .map(|i| (format!("k{i}"), Disposition::Looked))
        .collect();
    assert!(admit_completion(&mut full, "new").full);
    assert_eq!(full.len(), LEDGER_MAX);
}

#[tokio::test]
async fn observer_failure_never_repeats_next() {
    let rig = Rig::new();
    // The store cannot be opened: every observer fails where it reads or writes the state.
    let blocker = rig.fx.dir.path().join("csr-companion");
    std::fs::write(&blocker, "not a folder").unwrap();
    let clean = |reply: &Reply| reply.answer.is_none() && reply.next.is_none();
    let reply = rig.edit(Some("a1"), "tool-1", "/repo/src/a.ts").await;
    assert!(clean(&reply)); // the observer answers nothing: the tool's own result stands
    assert!(clean(&rig.edit(None, "tool-2", "/repo/src/a.ts").await));
    assert!(clean(&rig.spawn_a1("tu-1").await));
    rig.step(Some("a1"), "turn-1", 0).await;
    assert!(clean(
        &rig.finish("a1", "turn-1", "Found it.", "answer").await
    ));
    std::fs::remove_file(&blocker).unwrap();
    assert_eq!(ids(&rig.state()), Vec::<String>::new()); // and nothing was recorded
}

#[tokio::test]
async fn tracking_works_with_dreams_and_compaction_off() {
    let rig = Rig::new();
    rig.options(false, "off");
    rig.spawn_a1("tu-1").await;
    rig.edit(Some("a1"), "tool-1", "/repo/src/b.ts").await;
    rig.edit(None, "tool-2", "/repo/src/b.ts").await; // the main loop wrote the same file
    rig.step(Some("a1"), "turn-1", 0).await;
    rig.reported("a1", "turn-1", "Done.").await;
    let state = rig.state();
    let row = state.rows.get("a1").unwrap();
    assert_eq!(row.tools, 1);
    assert_eq!(row.tokens, 116);
    assert_eq!(row.status, Status::Completed);
    let main = state
        .main
        .get(&fingerprint("/repo/src/b.ts"))
        .map(String::as_str);
    assert_eq!(main, Some("/repo/src/b.ts"));
    assert_eq!(state.pending.len(), 1);
}

#[test]
fn a_failed_or_refused_or_interrupted_turn_is_terminal_and_never_pending() {
    let mut state = AgentState::empty(SESSION);
    state = red(state, complete(T0, "a1", "t1", "error", "partial"));
    state = red(state, complete(T0, "a2", "t1", "refusal", "no"));
    state = red(state, complete(T0, "a3", "t1", "aborted", ""));
    state = red(state, complete(T0, "a4", "t1", "answer", "   "));
    let rows: Vec<&AgentRec> = (1..=4)
        .map(|n| state.rows.get(&format!("a{n}")).unwrap())
        .collect();
    let statuses: Vec<Status> = rows.iter().map(|row| row.status).collect();
    assert_eq!(
        statuses,
        [
            Status::Failed,
            Status::Failed,
            Status::Killed,
            Status::Completed
        ]
    );
    let words: Vec<Option<Disposition>> = rows
        .iter()
        .map(|row| row.completion.as_ref().map(|c| c.disposition))
        .collect();
    assert_eq!(words, [Some(Disposition::Skipped); 4]);
    // an answer with no text is "no report"
    assert_eq!(
        rows[3].completion.as_ref().map(|c| c.answer.as_str()),
        Some("")
    );
    assert!(state.pending.is_empty());
}

#[test]
fn tool_and_step_observations_are_counted_once() {
    let mut state = AgentState::empty(SESSION);
    let tool = || Event::Tool {
        at: T0,
        agent_id: Some("a1".into()),
        tool_use_id: "u1".into(),
        tool: "Edit".into(),
        ok: true,
        path: Some("/r/x.ts".into()),
    };
    state = red(red(state, tool()), tool());
    let stepped = || Event::Step {
        at: T0,
        agent_id: "a1".into(),
        turn_id: "t".into(),
        index: 0.into(),
        answer: "hi".into(),
        tokens: 50,
    };
    state = red(red(state, stepped()), stepped());
    assert_eq!(state.rows.get("a1").unwrap().tools, 1);
    assert_eq!(state.rows.get("a1").unwrap().tokens, 50);
    assert_eq!(
        step_key(SESSION, Some("a1"), "t", &0.into()),
        r#"["sess-a","a1","t",0]"#
    );
    assert_eq!(
        step_key(SESSION, None, "t", &3.into()),
        r#"["sess-a",null,"t",3]"#
    );
    // a failed or denied edit is not a write
    let denied = Event::Tool {
        at: T0,
        agent_id: Some("a1".into()),
        tool_use_id: "u2".into(),
        tool: "Write".into(),
        ok: false,
        path: Some("/r/y.ts".into()),
    };
    state = red(state, denied);
    let paths: Vec<String> = state
        .rows
        .get("a1")
        .unwrap()
        .paths
        .values()
        .cloned()
        .collect();
    assert_eq!(paths, ["/r/x.ts"]);
}

#[test]
fn paths_are_normalized_and_a_state_is_keyed_by_its_session() {
    assert_eq!(
        normalize_path("src/../lib/./a.ts", "/repo/"),
        "/repo/lib/a.ts"
    );
    assert_eq!(normalize_path("/a//b/../c", "/x"), "/a/c");
}

#[test]
fn redaction_by_agents_ts_is_the_helper_s_table_and_then_some() {
    let say = |text: &str| redact_agent_value(&json!(text));
    for (input, expected) in TABLE {
        assert_eq!(say(input), json!(expected), "{input}");
    }
    let hash = say("hash 0123456789abcdef0123456789abcdef01234567 end");
    assert_eq!(hash, json!("hash [redacted] end"));
    // under 32 digits: a commit, not a secret
    assert_eq!(
        say("a 0123456789abcdef short"),
        json!("a 0123456789abcdef short")
    );
    assert_eq!(
        say("STRIPE_KEY=live_abc\nnext line"),
        json!("STRIPE_KEY=[redacted]\nnext line")
    );
    assert_eq!(
        say("export SIGNING_SECRET: plain\nok"),
        json!("export SIGNING_SECRET: [redacted]\nok")
    );
    assert_eq!(
        say("\x1b[31mred\x1b[0m \x1b]0;title\x07 bell\x07 ok"),
        json!("red  bell ok")
    );
    let nested = redact_agent_value(&json!({ "a": ["password=hunter2"], "n": 3 }));
    assert_eq!(nested, json!({ "a": ["password=[redacted]"], "n": 3 }));
    let twice = redact_agent_value(&say("password=hunter2 and sk-abcdefghijklmnopqrstuv"));
    assert_eq!(twice, json!("password=[redacted] and [redacted]"));
}

#[tokio::test]
async fn the_agents_file_is_written_at_once_then_at_most_every_five_seconds_terminal() {
    let rig = Rig::new();
    rig.options(false, "auto");
    rig.spawn_a1("tu-1").await;
    assert_eq!(rig.writes(), 1);
    let path = rig
        .home
        .join(".claude-self-reflect/csr-companion/agents/sess-a.json");
    assert!(path.exists());
    // through the helper's writer: its strings redacted, then serialized
    for n in 0..10 {
        rig.edit(Some("a1"), &format!("tool-{n}"), &format!("/repo/f{n}.ts"))
            .await;
    }
    rig.step(Some("a1"), "turn-1", 0).await;
    rig.reported("a1", "turn-1", "The report.").await;
    rig.advance(FILE_EVERY_MS as f64 - 1.).await;
    assert_eq!(rig.writes(), 1); // the burst is held back
    rig.advance(1.).await;
    assert!(until(|| rig.writes() == 2).await);
    rig.advance(FILE_EVERY_MS as f64 * 3.).await;
    assert_eq!(rig.writes(), 2); // and it was one write: nothing changed since
    let text = rig.file_text().unwrap();
    let file: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(file["session"], SESSION);
    assert_eq!(file["agents"][0]["tools"], 10);
    // the terminal event reached the file within the interval
    assert_eq!(file["agents"][0]["status"], "completed");
    assert_eq!(file["agents"][0]["completion"]["answer"], "The report.");
    assert!(utf8_length(&text) <= FILE_BYTES);
}

#[tokio::test]
async fn a_session_id_that_could_name_another_file_is_never_written() {
    let rig = Rig::build(World::default(), "../../etc/x", true);
    rig.spawn_a1("tu-1").await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(rig.writes(), 0);
    assert!(!rig.home.join(".claude-self-reflect").exists());
    assert!(!rig.home.join("etc").exists());
}

#[test]
fn planwrite_the_first_write_at_once_the_rest_one_trailing_write_a_lost_timer() {
    let mut state = red(AgentState::empty(SESSION), touch(T0, "a1", "Bash"));
    assert_eq!(plan_write(&mut state, T0).0, Act::Now);
    assert_eq!(
        plan_write(&mut state, T0 + 1000),
        (Act::Later, FILE_EVERY_MS - 1000)
    );
    // a timer is due: this change is covered by its write
    assert_eq!(plan_write(&mut state.clone(), T0 + 2000).0, Act::None);
    // the timer never fired (a reload): due is in the past
    assert_eq!(
        plan_write(&mut state.clone(), T0 + FILE_EVERY_MS + 1).0,
        Act::Now
    );
    // no agents, no file
    assert_eq!(plan_write(&mut AgentState::empty(SESSION), T0).0, Act::None);
}

#[test]
fn one_parent_and_seven_children_and_multibyte_reports_stay_within_16_kb_and_12() {
    let mut state = AgentState::empty(SESSION);
    let parent = red(state.clone(), touch(T0, "p", "Bash"))
        .rows
        .get("p")
        .unwrap()
        .clone();
    state.rows.insert(
        "p",
        AgentRec {
            kind: Some("Plan".into()),
            model: Some("opus".into()),
            ..parent
        },
    );
    for n in 0..7 {
        let spawn = Event::Spawn {
            at: T0,
            tool_use_id: format!("t{n}"),
            description: format!("Child {n}"),
            kind: Some("Explore".into()),
            parent: Some("p".into()),
            model: None,
            requests: vec![],
        };
        state = red(state, spawn);
        let spawned = Event::Spawned {
            at: T0,
            tool_use_id: format!("t{n}"),
            agent_id: Some(format!("c{n}")),
            model: Some("opus".into()),
        };
        state = red(state, spawned);
        let report = "界".repeat(700);
        state = red(
            state,
            complete(T0 + n, &format!("c{n}"), "turn-1", "answer", &report),
        );
    }
    let file = project_agents_file(&state, T0 + 1000);
    let mut named: Vec<&str> = file.agents.iter().map(|row| row.id.as_str()).collect();
    named.sort();
    assert_eq!(named, ["c0", "c1", "c2", "c3", "c4", "c5", "c6", "p"]);
    assert_eq!(file.agents[0].id, "p"); // the parent first
    assert_eq!(file.agents.len(), 8);
    assert!(file.agents[1..]
        .iter()
        .all(|row| row.parent.as_deref() == Some("p")));
    let bytes = utf8_length(&serde_json::to_string(&file).unwrap());
    assert!(bytes <= FILE_BYTES, "{bytes}");
    // squeezed, not emptied
    assert!(file
        .agents
        .iter()
        .all(|row| row.completion.as_ref().is_none_or(|c| !c.answer.is_empty())));
    // fourteen agents: twelve rows, the rest counted
    for n in 0..14 {
        state = red(state, touch(T0 + n, &format!("z{n}"), "Bash"));
    }
    let many = project_agents_file(&state, T0 + 1000);
    assert!(many.agents.len() <= 12);
    assert_eq!(many.agents.len() + many.omitted_agents, state.rows.len());
    assert!(utf8_length(&serde_json::to_string(&many).unwrap()) <= FILE_BYTES);
}

#[test]
fn the_envelope_is_read_taken_out_of_the_person_s_words_and_only_a_whole_valid_one() {
    let text = envelope_of(&context());
    assert_eq!(text.split('\n').count(), 4); // three lines and the final newline
    assert!(text.contains("\\u005b\\u005bCSR:AGENTS:v1\\u005d\\u005d"));
    assert_eq!(payload_of(&text), Some(context()));
    let reply = format!("Yes, do that.\n{text}And the importer too.");
    assert_eq!(
        pane::strip_envelopes(&reply),
        "Yes, do that.\nAnd the importer too."
    );
    // the dreamer's own words of a prompt that carries one: the envelope is the machine's
    assert_eq!(own_words(&reply), "Yes, do that.\nAnd the importer too.");
    assert_eq!(own_words(&text), "");
    assert_eq!(pane::parse_envelopes(&format!("{text}x\n{text}")).len(), 2);
    let handoff = handoff_payload();
    assert_eq!(payload_of(&envelope_of(&handoff)), Some(handoff.clone()));
    // CRLF and a byte-order mark are allowed; the rest is left where it is
    assert_eq!(payload_of(&text.replace('\n', "\r\n")), Some(context()));
    assert_eq!(payload_of(&format!("\u{feff}{text}")), Some(context()));
    let body = text.split('\n').nth(1).unwrap().to_string();
    let with = |extra: Value| merge(context(), extra);
    let kept = [
        format!("{}\nnot json\n{}\n", pane::OPEN, pane::CLOSE),
        format!("{}\n{body}\n", pane::OPEN),
        format!("  {text}"),
        format!("> {text}"),
        format!("x: {text}"),
        format!("{body}\n"),
        envelope_of(&with(json!({ "extra": 1 }))),
        envelope_of(&merge(
            handoff.clone(),
            json!({ "instruction": "Do something else." }),
        )),
        format!("{}\n{body}\n{body}\n{}\n", pane::OPEN, pane::CLOSE),
        text.replacen("v1]]", "v2]]", 1),
        envelope_of(&with(json!({ "pad": "x".repeat(20_000) }))),
    ];
    for text in kept {
        assert_eq!(pane::strip_envelopes(&text), text);
        assert_eq!(payload_of(&text), None);
    }
    let prose = "Mind [[CSR:AGENTS:v1]] in prose";
    assert_eq!(own_words(prose), prose);
}
