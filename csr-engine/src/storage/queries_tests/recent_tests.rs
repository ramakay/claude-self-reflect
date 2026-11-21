use super::*;

#[test]
fn recent_chunks_include_family_namespace_and_exclude_unrelated_sibling() {
    // Hermetic: resolves family membership against a tempdir root instead
    // of the real `$HOME/projects`, so this passes regardless of what
    // exists on the machine running it (was previously gated on
    // `$HOME/projects/claude-self-reflect` genuinely existing, which
    // fails on CI runners — see codex review finding on queries.rs:4648).
    let projects_root = tempfile::tempdir().unwrap();
    std::fs::create_dir(projects_root.path().join("claude-self-reflect")).unwrap();
    // A genuine sibling repo whose directory exists stays out of family —
    // this is the case the family gate exists for, unlike an exact-match
    // query (which would also exclude it, trivially, since the name
    // differs outright).
    std::fs::create_dir(projects_root.path().join("claude-self-reflect-sibling")).unwrap();

    let conn = mem();
    insert_recent(
        &conn,
        "conv-family",
        "claude-self-reflect-csr-engine",
        "2026-09-20T10:00:00Z",
        "family member content",
        false,
        "conversation",
    );
    insert_recent(
        &conn,
        "conv-sibling",
        "claude-self-reflect-sibling",
        "2026-09-20T11:00:00Z",
        "unrelated sibling content",
        false,
        "conversation",
    );

    let projects =
        recent_work_family_projects_at_root(&conn, "claude-self-reflect", projects_root.path())
            .unwrap();
    assert!(
        projects
            .iter()
            .any(|p| p == "claude-self-reflect-csr-engine"),
        "family member must resolve into the project family: {projects:?}"
    );
    assert!(
        !projects.iter().any(|p| p == "claude-self-reflect-sibling"),
        "sibling repo whose directory exists must not leak into the family: {projects:?}"
    );

    let chunks = get_recent_chunks_in_projects(&conn, 10, &projects).unwrap();
    let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
    assert!(
        ids.contains(&"conv-family"),
        "family member must be included: {ids:?}"
    );
    assert!(
        !ids.contains(&"conv-sibling"),
        "unrelated sibling must not leak in: {ids:?}"
    );
}

#[test]
fn recent_chunks_exclude_sidechains_and_order_by_last_activity() {
    let conn = mem();
    // Started earliest but touched most recently -> must rank first.
    insert_recent(
        &conn,
        "conv-old-start",
        "proj-recent",
        "2026-09-01T00:00:00Z",
        "first message",
        false,
        "conversation",
    );
    conn.execute(
        "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, message_count, is_sidechain, source)
             VALUES ('conv-old-start-c1', 'conv-old-start', 'proj-recent', '2026-09-20T00:00:00Z', 'later message', 1, 0, 'conversation')",
        [],
    )
    .unwrap();
    insert_recent(
        &conn,
        "conv-newer-start",
        "proj-recent",
        "2026-09-10T00:00:00Z",
        "started later, never touched again",
        false,
        "conversation",
    );
    insert_recent(
        &conn,
        "agent-sidechain",
        "proj-recent",
        "2026-09-21T00:00:00Z",
        "subagent chatter",
        true,
        "conversation",
    );

    let chunks = get_recent_chunks(&conn, 10, Some("proj-recent")).unwrap();
    let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();

    assert!(
        !ids.contains(&"agent-sidechain"),
        "sidechain conversation must be excluded: {ids:?}"
    );
    assert_eq!(
        ids[0], "conv-old-start",
        "ordering must follow last activity (max timestamp), not first-chunk start: {ids:?}"
    );
    // Preview stays the *first* chunk's content even though the display
    // timestamp is last-activity.
    let top = &chunks[0];
    assert_eq!(top.content, "first message");
    assert_eq!(top.timestamp, "2026-09-20T00:00:00Z");
}

#[test]
fn recent_chunks_collapse_same_second_codex_burst_and_count_limit_once() {
    let conn = mem();
    insert_recent(
        &conn,
        "codex-a",
        "proj-burst",
        "2026-09-20T10:00:00Z",
        "burst member a",
        false,
        "codex_rollout",
    );
    insert_recent(
        &conn,
        "codex-b",
        "proj-burst",
        "2026-09-20T10:00:00.500Z",
        "burst member b",
        false,
        "codex_rollout",
    );
    insert_recent(
        &conn,
        "codex-c",
        "proj-burst",
        "2026-09-20T10:00:00.900Z",
        "burst member c",
        false,
        "codex_rollout",
    );
    insert_recent(
        &conn,
        "solo-later",
        "proj-burst",
        "2026-09-20T11:00:00Z",
        "solo conversation",
        false,
        "conversation",
    );

    // limit=2 must still return both slots: the solo row and the
    // collapsed burst, not truncate the burst away.
    let chunks = get_recent_chunks(&conn, 2, Some("proj-burst")).unwrap();
    assert_eq!(chunks.len(), 2, "collapsed burst must count as one slot");
    assert_eq!(chunks[0].conversation_id, "solo-later");
    assert!(
        chunks[1].content.starts_with("[+2 parallel]"),
        "burst of 3 same-second codex rows collapses to +2: {}",
        chunks[1].content
    );
}

#[test]
fn recent_chunks_collapse_codex_burst_spanning_several_seconds() {
    // Reproduces the live-DB 01a0bc57 burst: 4 rollouts at
    // 2026-09-20T01:04:05.6xx plus one at 01:04:00.802 — 5 seconds
    // apart, so the old same-UTC-second rule saw it as two rows.
    let conn = mem();
    insert_recent(
        &conn,
        "codex-head",
        "proj-burst2",
        "2026-09-20T01:04:00.802Z",
        "burst head",
        false,
        "codex_rollout",
    );
    for (i, ms) in ["05.601", "05.622", "05.640", "05.658"].iter().enumerate() {
        insert_recent(
            &conn,
            &format!("codex-sibling-{i}"),
            "proj-burst2",
            &format!("2026-09-20T01:04:{ms}Z"),
            "burst sibling",
            false,
            "codex_rollout",
        );
    }
    insert_recent(
        &conn,
        "solo-later",
        "proj-burst2",
        "2026-09-20T02:00:00Z",
        "solo conversation",
        false,
        "conversation",
    );

    let chunks = get_recent_chunks(&conn, 6, Some("proj-burst2")).unwrap();
    assert_eq!(
        chunks.len(),
        2,
        "the whole 5-row burst plus the solo conversation must collapse to 2 rows: {:?}",
        chunks
            .iter()
            .map(|c| &c.conversation_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(chunks[0].conversation_id, "solo-later");
    assert!(
        chunks[1].content.starts_with("[+4 parallel]"),
        "5-row burst spanning 5s collapses to +4: {}",
        chunks[1].content
    );
}

#[test]
fn recent_chunks_burst_tail_truncated_by_fetch_window_gets_exact_count() {
    // Solo rows are newer than the 25-member burst, so limit=4 (3 solos +
    // 1 collapsed burst row) is already satisfied by the *first* fetch
    // window (target=16: 3 solo + 13 burst rows) — the widening retry in
    // get_recent_chunks_in_projects never fires because `collapsed.len()
    // >= limit` is already true. Before the follow-up-count fix, the
    // burst's window-based scan stopped at the fetch-window edge (13
    // members captured of 25) and reported "[+12 parallel]" instead of
    // the true "[+24 parallel]" — this is the regression this test
    // guards (finding on queries.rs `collapse_codex_bursts`). Contrast
    // with `recent_chunks_widening_retry_pulls_solos_behind_head_burst`
    // below, which puts the burst ahead of the solos so the widening
    // retry itself is what gets exercised.
    let conn = mem();
    let burst_len = 25;
    for i in 0..burst_len {
        // Sub-second spacing: the whole burst sits well inside the
        // collapse window regardless of chaining order.
        insert_recent(
            &conn,
            &format!("codex-big-{i}"),
            "proj-bigburst",
            &format!("2026-09-20T03:00:00.{:03}Z", i * 30),
            "burst member",
            false,
            "codex_rollout",
        );
    }
    for i in 0..3 {
        insert_recent(
            &conn,
            &format!("solo-{i}"),
            "proj-bigburst",
            &format!("2026-09-20T04:0{i}:00Z"),
            "solo conversation",
            false,
            "conversation",
        );
    }

    let chunks = get_recent_chunks(&conn, 4, Some("proj-bigburst")).unwrap();
    let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
    assert_eq!(
        chunks.len(),
        4,
        "limit must still be honoured despite an oversized burst filling the first window: {ids:?}"
    );
    assert!(
        chunks[..3]
            .iter()
            .all(|c| c.conversation_id.starts_with("solo-")),
        "the 3 solo rows (last-activity order) must surface ahead of the collapsed burst: {ids:?}"
    );
    assert_eq!(
        chunks[3].content, "[+24 parallel] burst member",
        "the burst must report its exact true size (25 members = +24), not just the 13 \
             that fit inside the first fetch window"
    );
}

#[test]
fn recent_chunks_widening_retry_pulls_solos_behind_head_burst() {
    // The burst is *newer* than the solo rows this time, so the first
    // fetch window (target=16 for limit=4) is entirely burst members and
    // collapses to a single row (1 < limit=4) — this is what actually
    // forces get_recent_chunks_in_projects's widening retry to run and
    // fetch a wider window that reaches the solo rows behind the burst.
    // (The sibling test above, with solos ahead, satisfies `limit`
    // before the retry ever fires — it does not exercise this path.)
    let conn = mem();
    let burst_len = 25;
    for i in 0..burst_len {
        insert_recent(
            &conn,
            &format!("codex-big-{i}"),
            "proj-headburst",
            &format!("2026-09-20T04:00:00.{:03}Z", i * 30),
            "burst member",
            false,
            "codex_rollout",
        );
    }
    for i in 0..3 {
        insert_recent(
            &conn,
            &format!("solo-{i}"),
            "proj-headburst",
            &format!("2026-09-20T03:0{i}:00Z"),
            "solo conversation",
            false,
            "conversation",
        );
    }

    let chunks = get_recent_chunks(&conn, 4, Some("proj-headburst")).unwrap();
    let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
    assert_eq!(
        chunks.len(),
        4,
        "limit must be honoured once the widening retry pulls in the solo rows: {ids:?}"
    );
    for i in 0..3 {
        let want = format!("solo-{i}");
        assert!(
            ids.contains(&want.as_str()),
            "solo-{i} must be reached by the widening retry: {ids:?}"
        );
    }
    let burst_row = chunks
        .iter()
        .find(|c| c.conversation_id.starts_with("codex-big-"))
        .expect("the collapsed burst row must still be present");
    assert_eq!(
        burst_row.content, "[+24 parallel] burst member",
        "the full 25-member burst must be counted exactly once the retry fetches it whole"
    );
}

#[test]
fn recent_chunks_exclude_headless_sdk_cli_entrypoint_conversation() {
    // W3-B1 review#2 (HIGH), 2nd pass: exclusion must be driven by a
    // *positive* headless signal (the transcript's own `entrypoint`),
    // never by absence from `session_registry` — the old mechanism hid
    // real sessions `history.jsonl` never happened to record. A
    // registered interactive session, an unregistered-but-real
    // `entrypoint=cli` session, and a `entrypoint=sdk-cli` headless probe
    // all coexist here; only the `sdk-cli` one is dropped.
    let conn = mem();
    insert_registered_session(
        &conn,
        "interactive-a",
        "proj-registry",
        "2026-09-20T09:00:00Z",
        "2026-09-20T09:05:00Z",
    );
    insert_recent(
        &conn,
        "interactive-a",
        "proj-registry",
        "2026-09-20T10:00:00Z",
        "real user prompt",
        false,
        "conversation",
    );
    set_conversation_entrypoint(&conn, "interactive-a", "cli").unwrap();

    insert_recent(
        &conn,
        "probe-b",
        "proj-registry",
        "2026-09-20T09:03:00Z",
        "Reply with exactly the word PROBE-DONE",
        false,
        "conversation",
    );
    set_conversation_entrypoint(&conn, "probe-b", "sdk-cli").unwrap();

    let chunks = get_recent_chunks(&conn, 6, Some("proj-registry")).unwrap();
    let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();

    assert!(
        ids.contains(&"interactive-a"),
        "a real cli-entrypoint session must still surface: {ids:?}"
    );
    assert!(
        !ids.contains(&"probe-b"),
        "a sdk-cli-entrypoint headless probe conversation must be excluded: {ids:?}"
    );
}

#[test]
fn recent_chunks_surface_unregistered_cli_entrypoint_despite_nonempty_registry() {
    // W3-B1 review#2 (HIGH): the exact regression the reviewer found live
    // — `entrypoint=cli`, but `history.jsonl`/`session_registry` never
    // recorded it (six real whats-next sessions did this). A non-empty
    // registry (containing an unrelated session) must not make this one
    // read as headless.
    let conn = mem();
    insert_registered_session(
        &conn,
        "some-other-registered-session",
        "proj-registry-lag",
        "2026-09-20T09:00:00Z",
        "2026-09-20T09:05:00Z",
    );
    insert_recent(
        &conn,
        "brand-new-live",
        "proj-registry-lag",
        "2026-09-20T12:00:00Z",
        "What directory are you operating on",
        false,
        "conversation",
    );
    set_conversation_entrypoint(&conn, "brand-new-live", "cli").unwrap();

    let chunks = get_recent_chunks(&conn, 6, Some("proj-registry-lag")).unwrap();
    let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
    assert!(
        ids.contains(&"brand-new-live"),
        "an unregistered real cli-entrypoint session must surface despite a non-empty registry: {ids:?}"
    );
}

#[test]
fn recent_chunks_fail_open_when_entrypoint_unknown() {
    // No `conversation_entrypoint` row at all (pre-dates the column, or
    // the source file is gone) must never be read as headless — only an
    // explicit `sdk-*` value is a headless signal.
    let conn = mem();
    insert_recent(
        &conn,
        "conv-a",
        "proj-empty-registry",
        "2026-09-20T09:00:00Z",
        "real user prompt",
        false,
        "conversation",
    );

    let chunks = get_recent_chunks(&conn, 6, Some("proj-empty-registry")).unwrap();
    let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
    assert!(
        ids.contains(&"conv-a"),
        "a conversation with no recorded entrypoint must fail open, not be hidden: {ids:?}"
    );
}

#[test]
fn recent_chunks_exclude_csr_emission_when_entrypoint_unknown() {
    // W3-B1 review#2 (HIGH): the fallback net for rows with no recorded
    // entrypoint at all — CSR's own emission echoed back must still be
    // excluded on that basis, exactly as before.
    let conn = mem();
    insert_recent(
        &conn,
        "narrator-echo",
        "proj-emission",
        "2026-09-20T09:00:00Z",
        "You are CSR Episode Analyst reviewing a transcript",
        false,
        "conversation",
    );
    insert_recent(
        &conn,
        "conv-real",
        "proj-emission",
        "2026-09-20T09:01:00Z",
        "real user prompt",
        false,
        "conversation",
    );

    let chunks = get_recent_chunks(&conn, 6, Some("proj-emission")).unwrap();
    let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
    assert!(
        !ids.contains(&"narrator-echo"),
        "a CSR-emission preview with no entrypoint recorded must still be excluded: {ids:?}"
    );
    assert!(
        ids.contains(&"conv-real"),
        "a real session in the same batch must still surface: {ids:?}"
    );
}

#[test]
fn recent_work_family_excludes_headless_scratchpad_namespace() {
    let conn = mem();
    insert_recent(
        &conn,
        "conv-main",
        "myproj",
        "2026-09-20T09:00:00Z",
        "real content",
        false,
        "conversation",
    );
    insert_recent(
        &conn,
        "conv-scratch",
        "myproj-scratchpad-abc123",
        "2026-09-20T10:00:00Z",
        "headless workflow cwd content",
        false,
        "conversation",
    );

    // Only the base dir exists -- the scratchpad namespace is a runtime
    // worktree/cwd alias, not a checked-out sibling repo, so it must
    // pass the alias check (`is_alias_at_root`) on its own merits before
    // the scratchpad filter even gets a chance to exclude it.
    let projects_root = tempfile::tempdir().unwrap();
    std::fs::create_dir(projects_root.path().join("myproj")).unwrap();

    assert!(
        crate::search::cross_project::same_project_family_at_root(
            "myproj",
            "myproj-scratchpad-abc123",
            projects_root.path(),
        ),
        "sanity: the fixture must actually resolve as a family alias so this test can tell \
             the scratchpad filter apart from the alias check itself"
    );

    let projects =
        recent_work_family_projects_at_root(&conn, "myproj", projects_root.path()).unwrap();
    assert!(
        !projects.iter().any(|p| p.contains("-scratchpad-")),
        "a headless scratchpad namespace must never join the family: {projects:?}"
    );
}

#[test]
fn recent_work_family_projects_with_home_none_returns_exact_scope_only() {
    // Deterministic regardless of this machine's real `$HOME`: a
    // candidate that would otherwise be a plausible family member (same
    // hyphenated prefix) must still be excluded when there is no
    // resolvable home directory to form a projects root against.
    let conn = mem();
    insert_recent(
        &conn,
        "conv-a",
        "myproj",
        "2026-09-20T09:00:00Z",
        "content",
        false,
        "conversation",
    );
    insert_recent(
        &conn,
        "conv-b",
        "myproj-csr-engine",
        "2026-09-20T09:00:00Z",
        "content",
        false,
        "conversation",
    );

    let projects = recent_work_family_projects_with_home(&conn, "myproj", None).unwrap();
    assert_eq!(
        projects,
        vec!["myproj".to_string()],
        "no resolvable home directory must mean exact scope only: {projects:?}"
    );
}

#[test]
fn recent_chunks_preview_skips_codex_harness_boilerplate_first_chunk() {
    // W3-B1 review#2 finding: a still-polluted Codex rollout's chunk 0 is
    // harness boilerplate (`<recommended_plugins>...`); the preview must
    // skip to the first real chunk instead of showing the boilerplate.
    let conn = mem();
    insert_chunk_with_source(
        &conn,
        &ConversationChunk {
            id: "codex-polluted-c0".into(),
            conversation_id: "codex-polluted".into(),
            project_name: "proj-preview".into(),
            timestamp: "2026-09-20T09:00:00Z".into(),
            content: "<recommended_plugins>\nHere is a list of plugins\n</recommended_plugins>"
                .into(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult,
            seq: 0,
            is_sidechain: false,
        },
        &[0.1, 0.2],
        "codex_rollout",
    )
    .unwrap();
    insert_chunk_with_source(
        &conn,
        &ConversationChunk {
            id: "codex-polluted-c1".into(),
            conversation_id: "codex-polluted".into(),
            project_name: "proj-preview".into(),
            timestamp: "2026-09-20T09:00:01Z".into(),
            content: "real user request about the recall bug".into(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult,
            seq: 1,
            is_sidechain: false,
        },
        &[0.1, 0.2],
        "codex_rollout",
    )
    .unwrap();

    let chunks = get_recent_chunks(&conn, 6, Some("proj-preview")).unwrap();
    let top = chunks
        .iter()
        .find(|c| c.conversation_id == "codex-polluted")
        .expect("the polluted conversation must still surface");
    assert_eq!(
        top.content, "real user request about the recall bug",
        "the preview must skip the harness-boilerplate first chunk: {}",
        top.content
    );
}

#[test]
fn recent_chunks_preview_skips_multi_chunk_codex_harness_span() {
    // W3-B1 review#2 (MEDIUM), 2nd pass: the real `codex:01a0c5df` shape —
    // only chunk 0 starts with a harness marker, but the injected block
    // (CSR's chunker re-splits it at a flat char budget with no idea it
    // was boilerplate) continues, unmarked, across several more chunks
    // before its closing tag turns up, and only THEN does the real user
    // prompt begin. A per-chunk-only noisy check (the old behaviour)
    // picks the first unmarked continuation chunk — pure boilerplate —
    // as the preview, exactly as it did live.
    let conn = mem();
    let mut rows: Vec<(String, usize, String)> = vec![(
        "harness-span-c0".to_string(),
        0,
        "<recommended_plugins>\nHere is a list of plugins, continued below".to_string(),
    )];
    for seq in 1..11usize {
        rows.push((
            format!("harness-span-c{seq}"),
            seq,
            format!("- Consensus (app-{seq:x}…@openai-curated-remote) plugin entry"),
        ));
    }
    rows.push((
        "harness-span-c11".to_string(),
        11,
        "...the rest of the plugin list </recommended_plugins>".to_string(),
    ));
    rows.push((
        "harness-span-c12".to_string(),
        12,
        "dont look at the literature install graphify".to_string(),
    ));

    for (id, seq, content) in &rows {
        insert_chunk_with_source(
            &conn,
            &ConversationChunk {
                id: id.clone(),
                conversation_id: "harness-span".into(),
                project_name: "proj-preview".into(),
                timestamp: format!("2026-09-20T09:00:{seq:02}Z"),
                content: content.clone(),
                message_count: 1,
                summary: None,
                author: Speaker::ToolResult,
                seq: *seq,
                is_sidechain: false,
            },
            &[0.1, 0.2],
            "codex_rollout",
        )
        .unwrap();
    }

    let chunks = get_recent_chunks(&conn, 6, Some("proj-preview")).unwrap();
    let top = chunks
        .iter()
        .find(|c| c.conversation_id == "harness-span")
        .expect("the polluted conversation must still surface");
    assert_eq!(
        top.content, "dont look at the literature install graphify",
        "the preview must skip the whole multi-chunk harness span, not just chunk 0: {}",
        top.content
    );
}

#[test]
fn recent_chunks_preview_survives_harness_block_closing_and_opening_in_same_chunk() {
    // W3-B1 review#2 (MEDIUM), 3rd pass: the exact live `codex:01a0c5df`
    // shape (verified on a clone of the production DB). Multiple harness
    // blocks are packed back-to-back inside ONE original message with no
    // chunk-boundary alignment: `</recommended_plugins>` (closing block
    // A) and `<INSTRUCTIONS>` (opening block B, the "# AGENTS.md
    // instructions" body) land in the very same stored chunk. A "first
    // closing tag ends the span" rule reopens picking right there — on
    // block B's own boilerplate body (which is CLAUDE.md content quoted
    // as agent instructions, live) — instead of staying closed until
    // block B's own `</INSTRUCTIONS>` shows up several chunks later.
    let conn = mem();
    let rows: Vec<(&str, usize, &str)> = vec![
        ("c0", 0, "<recommended_plugins>\nplugin list starts here"),
        ("c1", 1, "more plugins, no marker of its own"),
        (
            "c2",
            2,
            "</recommended_plugins>\n# AGENTS.md instructions\n<INSTRUCTIONS>\nCore prefs",
        ),
        ("c3", 3, "more instructions body, no marker of its own"),
        ("c4", 4, "closing soon </INSTRUCTIONS>"),
        ("c5", 5, "the real first prompt of the session"),
    ];
    for (id, seq, content) in &rows {
        insert_chunk_with_source(
            &conn,
            &ConversationChunk {
                id: format!("same-chunk-{id}"),
                conversation_id: "same-chunk-span".into(),
                project_name: "proj-preview".into(),
                timestamp: format!("2026-09-20T09:00:{seq:02}Z"),
                content: content.to_string(),
                message_count: 1,
                summary: None,
                author: Speaker::ToolResult,
                seq: *seq,
                is_sidechain: false,
            },
            &[0.1, 0.2],
            "codex_rollout",
        )
        .unwrap();
    }

    let chunks = get_recent_chunks(&conn, 6, Some("proj-preview")).unwrap();
    let top = chunks
        .iter()
        .find(|c| c.conversation_id == "same-chunk-span")
        .expect("the polluted conversation must still surface");
    assert_eq!(
        top.content, "the real first prompt of the session",
        "closing one block and opening the next in the same chunk must not reopen picking early: {}",
        top.content
    );
}

#[test]
fn recent_chunks_preview_skips_local_command_caveat_first_chunk() {
    // A Claude session that opens with a slash command (`/login`) has a
    // `<local-command-caveat>` wrapper as its first chunk (live-DB
    // bd3294f7/d8fe3a96); the preview must skip that wrapper rather than
    // showing the harness caveat text itself.
    let conn = mem();
    insert_registered_session(
        &conn,
        "conv-slash",
        "proj-preview-claude",
        "2026-09-20T09:00:00Z",
        "2026-09-20T09:00:01Z",
    );
    insert_chunk_with_source(
        &conn,
        &ConversationChunk {
            id: "conv-slash-c0".into(),
            conversation_id: "conv-slash".into(),
            project_name: "proj-preview-claude".into(),
            timestamp: "2026-09-20T09:00:00Z".into(),
            content: "<local-command-caveat>Caveat: The messages below were generated \
                          by the user while running local commands. DO NOT respond\
                          </local-command-caveat>"
                .into(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult,
            seq: 0,
            is_sidechain: false,
        },
        &[0.1, 0.2],
        "conversation",
    )
    .unwrap();
    insert_chunk_with_source(
        &conn,
        &ConversationChunk {
            id: "conv-slash-c1".into(),
            conversation_id: "conv-slash".into(),
            project_name: "proj-preview-claude".into(),
            timestamp: "2026-09-20T09:00:01Z".into(),
            content: "/login".into(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult,
            seq: 1,
            is_sidechain: false,
        },
        &[0.1, 0.2],
        "conversation",
    )
    .unwrap();

    let chunks = get_recent_chunks(&conn, 6, Some("proj-preview-claude")).unwrap();
    let top = chunks
        .iter()
        .find(|c| c.conversation_id == "conv-slash")
        .expect("the session must still surface");
    assert_eq!(
        top.content, "/login",
        "the preview must skip the local-command-caveat wrapper: {}",
        top.content
    );
}

#[test]
fn recent_chunks_preview_survives_noncontiguous_rowids_between_own_chunks() {
    // W3-B1 review#2 (LOW): the live-DB `d8fe3a96`/`356b2649` case — the
    // watcher imports incrementally and interleaves conversations, so a
    // session's own chunk 1 can land dozens of rowids after its chunk 0
    // once other conversations' chunks are inserted in between. A
    // `rowid <` range window sized for *contiguous* rowids then holds
    // only chunk 0 (the caveat) and never even sees chunk 1, no matter
    // how large the window is raised. Ranking by `seq` instead must
    // still find chunk 1 regardless of how many unrelated rows separate
    // the two by rowid.
    let conn = mem();
    insert_chunk_with_source(
        &conn,
        &ConversationChunk {
            id: "conv-gap-c0".into(),
            conversation_id: "conv-gap".into(),
            project_name: "proj-preview-gap".into(),
            timestamp: "2026-09-20T09:00:00Z".into(),
            content: "<local-command-caveat>Caveat: The messages below were generated \
                          by the user while running local commands. DO NOT respond\
                          </local-command-caveat>"
                .into(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult,
            seq: 0,
            is_sidechain: false,
        },
        &[0.1, 0.2],
        "conversation",
    )
    .unwrap();
    // 40 unrelated chunks from a different, unrelated conversation land
    // in between by rowid — more than any realistic `rowid <` window,
    // reproducing the live gap (21 rows) at a size that would defeat
    // even the review's own suggested "raise the window" band-aid.
    for i in 0..40 {
        insert_chunk_with_source(
            &conn,
            &ConversationChunk {
                id: format!("filler-{i}"),
                conversation_id: "filler-conv".into(),
                project_name: "proj-preview-gap".into(),
                timestamp: format!("2026-09-20T08:{:02}:00Z", i % 60),
                content: "unrelated filler content".into(),
                message_count: 1,
                summary: None,
                author: Speaker::ToolResult,
                seq: i as usize,
                is_sidechain: false,
            },
            &[0.1, 0.2],
            "conversation",
        )
        .unwrap();
    }
    insert_chunk_with_source(
        &conn,
        &ConversationChunk {
            id: "conv-gap-c1".into(),
            conversation_id: "conv-gap".into(),
            project_name: "proj-preview-gap".into(),
            timestamp: "2026-09-20T09:00:01Z".into(),
            content: "the real first prompt of the session".into(),
            message_count: 1,
            summary: None,
            author: Speaker::ToolResult,
            seq: 1,
            is_sidechain: false,
        },
        &[0.1, 0.2],
        "conversation",
    )
    .unwrap();

    let chunks = get_recent_chunks(&conn, 6, Some("proj-preview-gap")).unwrap();
    let top = chunks
        .iter()
        .find(|c| c.conversation_id == "conv-gap")
        .expect("the session must still surface");
    assert_eq!(
        top.content, "the real first prompt of the session",
        "the preview must find chunk 1 by seq despite a large rowid gap: {}",
        top.content
    );
}

#[test]
fn recent_chunks_exclude_codex_subagent_child_marked_sidechain() {
    // Once the codex_rollout adapter correctly marks a `thread_spawn`
    // child's chunks `is_sidechain = true` (see
    // `import::codex_rollout::backfill_codex_subagent_sidechain`), the
    // existing sidechain exclusion here must keep it from taking its own
    // recent-work slot next to its parent.
    let conn = mem();
    insert_recent(
        &conn,
        "codex:parent-thread",
        "proj-subagent",
        "2026-09-20T10:00:00Z",
        "parent rollout",
        false,
        "codex_rollout",
    );
    insert_recent(
        &conn,
        "codex:child-thread",
        "proj-subagent",
        "2026-09-20T10:00:00.050Z",
        "subagent child rollout",
        true,
        "codex_rollout",
    );

    let chunks = get_recent_chunks(&conn, 6, Some("proj-subagent")).unwrap();
    let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
    assert!(
        ids.contains(&"codex:parent-thread"),
        "the parent rollout must surface: {ids:?}"
    );
    assert!(
        !ids.contains(&"codex:child-thread"),
        "a subagent child marked is_sidechain must never take its own slot: {ids:?}"
    );
}

#[test]
fn codex_burst_key_ts_uses_uuidv7_spawn_time_not_restamped_first_chunk_ts() {
    // Reproduces the live-DB 01a093d5 pair: two rollouts spawned 100ms
    // apart via `thread_spawn` (per their UUIDv7 ids), but the
    // chunk-timestamp restamp rewrote their first-chunk timestamps to
    // the *message* time, 7 minutes apart. Burst grouping must use the
    // id's embedded spawn time, not the restamped chunk timestamp.
    let head_id = "codex:01935e3b-0000-7000-8000-000000000001";
    let sibling_id = "codex:01935e3b-0064-7000-8000-000000000002"; // +0x64 = +100ms
    let head_ts = codex_burst_key_ts(head_id, "2026-09-20T04:16:49.700Z");
    let sibling_ts = codex_burst_key_ts(sibling_id, "2026-09-20T04:23:47.808Z");
    assert!(
        within_burst_window(&head_ts, &sibling_ts),
        "two ids 100ms apart must be treated as the same burst even though their \
             restamped first-chunk timestamps are 7 minutes apart: head={head_ts} sibling={sibling_ts}"
    );
}

#[test]
fn recent_chunks_collapse_codex_burst_by_uuidv7_id_despite_restamped_gap() {
    let conn = mem();
    insert_recent(
        &conn,
        "codex:01935e3b-0000-7000-8000-000000000001",
        "proj-uuidburst",
        "2026-09-20T04:16:49.700Z",
        "burst head",
        false,
        "codex_rollout",
    );
    insert_recent(
        &conn,
        "codex:01935e3b-0064-7000-8000-000000000002",
        "proj-uuidburst",
        "2026-09-20T04:23:47.808Z",
        "burst sibling",
        false,
        "codex_rollout",
    );
    insert_recent(
        &conn,
        "solo-later",
        "proj-uuidburst",
        "2026-09-20T05:00:00Z",
        "solo conversation",
        false,
        "conversation",
    );

    let chunks = get_recent_chunks(&conn, 4, Some("proj-uuidburst")).unwrap();
    let ids: Vec<&str> = chunks.iter().map(|c| c.conversation_id.as_str()).collect();
    assert_eq!(
        chunks.len(),
        2,
        "the UUIDv7-adjacent pair must collapse into one row despite a 7-minute \
             restamped first-chunk gap: {ids:?}"
    );
    assert_eq!(chunks[0].conversation_id, "solo-later");
    assert!(
        chunks[1].content.starts_with("[+1 parallel]"),
        "the pair must report exactly one parallel sibling: {}",
        chunks[1].content
    );
}

#[test]
fn latest_chunk_names_the_conversation_get_most_recent_session_names() {
    let conn = mem();
    let put = |id: &str, conv: &str, project: &str, ts: &str| {
        let mut chunk = test_chunk(id, "body");
        chunk.conversation_id = conv.into();
        chunk.project_name = project.into();
        chunk.timestamp = ts.into();
        insert_chunk(&conn, &chunk, &[0.0; 4]).unwrap();
    };
    put("a1", "conv-a", "p", "2026-10-01T10:00:00Z");
    put("a2", "conv-a", "p", "2026-10-03T10:00:00Z");
    put("b1", "conv-b", "p", "2026-10-02T10:00:00Z");
    put("b2", "conv-b", "p", "2026-10-04T09:00:00Z");
    put("c1", "conv-c", "q", "2026-10-05T10:00:00Z");
    for project in ["p", "q", "all"] {
        let session = get_most_recent_session(&conn, project).unwrap().unwrap();
        assert_eq!(
            latest_chunk_in_project(&conn, project).unwrap(),
            Some((session.conversation_id, session.timestamp)),
            "{project}"
        );
    }
    assert_eq!(latest_chunk_in_project(&conn, "none").unwrap(), None);
    // A tie on the newest timestamp goes to the smallest conversation id.
    put("z1", "conv-z", "p", "2026-10-04T09:00:00Z");
    put("a3", "conv-0", "p", "2026-10-04T09:00:00Z");
    assert_eq!(
        latest_chunk_in_project(&conn, "p").unwrap(),
        Some(("conv-0".into(), "2026-10-04T09:00:00Z".into()))
    );
}

#[test]
fn reflections_by_tag_return_what_the_table_scan_returned() {
    let conn = mem();
    for i in 0..30 {
        let tags: Vec<String> = match i % 3 {
            0 => vec!["session_episode".into(), "conv_abc".into()],
            1 => vec!["session_story".into(), "conv_abc-def".into()],
            _ => vec!["conv_other".into()],
        };
        let id = format!("r{i:02}");
        insert_reflection(
            &conn,
            &id,
            &format!("body {i} {}", "x".repeat(5000)),
            &tags,
            &[0.0; 4],
        )
        .unwrap();
        conn.execute(
            "UPDATE reflections SET timestamp = ?1 WHERE id = ?2",
            params![
                format!("2026-09-{:02}T00:00:{:02}Z", 1 + (i * 7) % 28, i),
                id
            ],
        )
        .unwrap();
    }
    let scan = |tag: &str, limit: i64| -> Vec<(String, String)> {
        let pattern = format!("%{}%", tag.replace('%', "\\%").replace('_', "\\_"));
        let mut stmt = conn
            .prepare(
                "SELECT id, timestamp FROM reflections
                     WHERE tags LIKE ?1 ESCAPE '\\' ORDER BY timestamp DESC LIMIT ?2",
            )
            .unwrap();
        stmt.query_map(params![pattern, limit], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    for (tag, limit) in [
        ("conv_abc", 10),
        ("conv_abc", 3),
        ("session_episode", 50),
        ("CONV_OTHER", 4),
        ("nothing", 5),
    ] {
        let got: Vec<(String, String)> = get_reflections_by_tag(&conn, tag, limit)
            .unwrap()
            .into_iter()
            .map(|(id, _, _, ts)| (id, ts))
            .collect();
        assert_eq!(got, scan(tag, limit as i64), "{tag} {limit}");
    }
}

#[test]
fn batched_episode_rows_match_one_tag_scan_per_conversation() {
    let conn = mem();
    let mut n = 0;
    let mut put = |tags: &[&str], body: &str| {
        n += 1;
        let id = format!("r{n:03}");
        let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
        insert_reflection(&conn, &id, body, &tags, &[0.0; 4]).unwrap();
        conn.execute(
            "UPDATE reflections SET timestamp = ?1 WHERE id = ?2",
            params![
                format!("2026-09-{:02}T{:02}:00:00Z", 1 + (n * 5) % 28, n % 24),
                id
            ],
        )
        .unwrap();
    };
    // An episode, its story and extract, and enough newer rows of one
    // conversation to push its episode out of a 3-row window.
    put(&["session_episode", "conv_aaa"], "{\"episode\":\"aaa\"}");
    put(&["session_story", "conv_aaa"], "story aaa");
    put(
        &["session_episode", "conv_aaa-bbb"],
        "{\"episode\":\"aaa-bbb\"}",
    );
    put(&["session_episode", "CONV_CCC"], "{\"episode\":\"ccc\"}");
    for i in 0..6 {
        put(&["context_cache", "conv_ddd"], &format!("cache {i}"));
    }
    put(&["session_episode", "conv_ddd"], "{\"episode\":\"ddd\"}");
    put(&["narrative_extracted_v3", "conv_eee"], "no episode here");
    let ids = ["aaa", "aaa-bbb", "ccc", "ddd", "eee", "missing", "aaa"];
    for window in [3usize, 10] {
        let batched = episode_rows_by_conversation(&conn, &ids, window).unwrap();
        for id in ids {
            let one: Vec<(String, String)> =
                get_reflections_by_tag(&conn, &format!("conv_{id}"), window)
                    .unwrap()
                    .into_iter()
                    .filter(|(_, _, tags, _)| tags.iter().any(|t| t == "session_episode"))
                    .map(|(_, content, _, ts)| (content, ts))
                    .collect();
            assert_eq!(batched.get(id), Some(&one), "{id} window {window}");
        }
    }
}

#[test]
fn get_chunk_provenance_many_matches_single_row_reads() {
    let conn = mem();
    for (id, author, supersedes) in [
        ("p1", Speaker::User, Some("older".to_string())),
        ("p2", Speaker::Assistant, None),
    ] {
        insert_chunk(&conn, &test_chunk(id, "body"), &[0.0; 4]).unwrap();
        insert_chunk_provenance(
            &conn,
            id,
            &ChunkProvenance {
                author,
                source_conv_id: format!("conv-{id}"),
                supersedes,
            },
        )
        .unwrap();
    }
    insert_chunk(&conn, &test_chunk("p3", "no provenance row"), &[0.0; 4]).unwrap();
    // An unknown author token degrades to ToolResult in both readers.
    conn.execute(
        "UPDATE chunk_provenance SET author = 'martian' WHERE chunk_id = 'p2'",
        [],
    )
    .unwrap();

    let ids = ["p1", "p2", "p3", "missing", "p1"];
    let many = get_chunk_provenance_many(&conn, &ids).unwrap();
    for id in ids {
        assert_eq!(
            many.get(id),
            get_chunk_provenance(&conn, id).unwrap().as_ref(),
            "{id}"
        );
    }
    assert_eq!(many.len(), 2);
    assert_eq!(many["p2"].author, Speaker::ToolResult);
    assert!(get_chunk_provenance_many(&conn, &[]).unwrap().is_empty());
}

#[test]
fn get_reflections_by_ids_matches_single_row_reads() {
    let conn = mem();
    let vec = [0.5f32, 0.5, 0.5, 0.5];
    insert_reflection(&conn, "r1", "first", &["a".to_string()], &vec).unwrap();
    insert_reflection(&conn, "r2", "second", &[], &vec).unwrap();
    insert_reflection(
        &conn,
        "r-ledger",
        "shift text",
        &["narrative_shift".to_string()],
        &vec,
    )
    .unwrap();

    let ids = ["r1", "r2", "r-ledger", "missing", "r1"];
    let many = get_reflections_by_ids(&conn, &ids).unwrap();
    for id in ids {
        assert_eq!(
            many.get(id),
            get_reflection_by_id(&conn, id).unwrap().as_ref(),
            "{id}"
        );
    }
    assert_eq!(many.len(), 2);
    assert!(get_reflections_by_ids(&conn, &[]).unwrap().is_empty());
}

#[test]
fn reflection_readers_exclude_narrative_shift_ledger_rows_while_table_keeps_them() {
    let conn = mem();
    let text = "MEMORY-ARCHIVE shift-quarantine-token shared words in story and ledger";
    let vec = [0.5f32, 0.5, 0.5, 0.5];
    insert_reflection(
        &conn,
        "story-row",
        text,
        &[
            "session_story".to_string(),
            "project_csr".to_string(),
            "conv_x".to_string(),
            "session_x".to_string(),
        ],
        &vec,
    )
    .unwrap();
    // Give the ledger row a newer timestamp AND conv_x/session_x/project_csr
    // tags so LIMIT 1 would pick the ledger row first if SQL failed to filter it.
    insert_reflection(
        &conn,
        "ledger-row",
        text,
        &[
            "narrative_shift".to_string(),
            "shift_project:csr".to_string(),
            "shift_conv:x".to_string(),
            "shift_sig_deadbeefcafebabe".to_string(),
            "project_csr".to_string(),
            "conv_x".to_string(),
            "session_x".to_string(),
            "session_story".to_string(),
        ],
        &vec,
    )
    .unwrap();
    conn.execute(
        "UPDATE reflections SET timestamp = '2099-01-02T00:00:00Z' WHERE id = 'ledger-row'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE reflections SET timestamp = '2099-01-01T00:00:00Z' WHERE id = 'story-row'",
        [],
    )
    .unwrap();

    assert!(get_reflection_by_id(&conn, "story-row").unwrap().is_some());
    assert!(
        get_reflection_by_id(&conn, "ledger-row").unwrap().is_none(),
        "get_reflection_by_id must return None for narrative_shift rows"
    );

    let fts_hits = fts5_search_reflections(&conn, "shift-quarantine-token", 1).unwrap();
    assert_eq!(
        fts_hits
            .iter()
            .map(|(id, _, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["story-row"]
    );

    let prefix_hits =
        fts5_reflection_prefix_matches(&conn, "MEMORY-ARCHIVE shift-quarantine-token", 1).unwrap();
    // The ledger row is quarantined by its tag; the story row is a derived
    // row, which an exact prefix no longer promotes.
    assert!(prefix_hits.is_empty(), "{prefix_hits:?}");

    for tag in ["conv_x", "session_x", "project_csr", "narrative_shift"] {
        let rows = get_reflections_by_tag(&conn, tag, 1).unwrap();
        assert!(
            rows.iter().all(|(id, _, _, _)| id != "ledger-row"),
            "get_reflections_by_tag({tag}) leaked ledger-row: {rows:?}"
        );
    }
    assert_eq!(
        get_reflections_by_tag(&conn, "conv_x", 1).unwrap()[0].0,
        "story-row"
    );

    let two_tag_rows =
        get_reflections_by_two_tags(&conn, "project_csr", "session_story", 1).unwrap();
    assert_eq!(two_tag_rows.len(), 1);
    assert_eq!(two_tag_rows[0].0, "story-row");

    // Row stays in the table for the pane and ledger dedupe.
    let raw_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM reflections WHERE id = 'ledger-row'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(raw_count, 1);
}

#[test]
fn get_ledger_reflections_by_tag_returns_ledger_rows() {
    let conn = mem();
    insert_reflection(
        &conn,
        "ledger-row",
        "[shift: model-derived] a => b - claim",
        &[
            "narrative_shift".to_string(),
            "shift_sig_1234567890abcdef".to_string(),
        ],
        &[0.1f32; 4],
    )
    .unwrap();
    let rows = get_ledger_reflections_by_tag(&conn, "shift_sig_1234567890abcdef", 1).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "ledger-row");
}

#[test]
fn the_session_a_start_continues_from_is_never_a_subagent() {
    // A subagent's transcript is newer than its lead's; CONTINUED FROM must
    // still name the lead, not the worker brief.
    let conn = mem();
    let (lead, agent) = ("01151a75-lead", "agent-a3d77c7f");
    insert_recent(
        &conn,
        lead,
        "proj",
        "2026-11-12T00:10:00Z",
        "lead work",
        false,
        "conversation",
    );
    insert_recent(
        &conn,
        agent,
        "proj",
        "2026-11-12T00:57:00Z",
        "worker brief",
        true,
        "conversation",
    );
    let found = get_most_recent_interactive_session(&conn, "proj")
        .unwrap()
        .unwrap();
    assert_eq!(found.conversation_id, lead);
}
