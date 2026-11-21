use super::*;

#[test]
fn continuum_fallback_names_the_latest_task_not_the_first_prompt() {
    let mut ep = minimal_episode(
        "anthropics codemod, when github links are bought up or disccussed, can",
        "",
        Some("continue work"),
    );
    ep.recap_evidence_version = Some(crate::hooks::stop::RECAP_EVIDENCE_VERSION);
    ep.anchor_request = Some("How do we show cost as % of max plan".into());
    ep.message_count = 30_393;
    let block = format_tier0_block(&ep, &[], "3h ago");
    assert!(
        block.starts_with("CSR CONTINUUM [3h ago]: How do we show cost as % of max plan\n"),
        "{block}"
    );
    assert!(!block.contains("anthropics codemod"), "{block}");
    // The provenance guard still recognises the emitted fallback.
    assert!(crate::extraction::provenance::is_csr_emission(&block));
    assert!(crate::extraction::provenance::extractable(&block).is_none());
}

#[test]
fn continuum_fallback_withholds_a_stale_first_prompt() {
    let mut ep = minimal_episode(
        "anthropics codemod, when github links are bought up or disccussed, can",
        "",
        Some("continue work"),
    );
    // Legacy episode from a long session: its first prompt is stale.
    ep.message_count = 30_393;
    let legacy = format_tier0_block(&ep, &[], "3h ago");
    // Current episode whose latest task has no clean rendering.
    ep.recap_evidence_version = Some(crate::hooks::stop::RECAP_EVIDENCE_VERSION);
    let current = format_tier0_block(&ep, &[], "3h ago");
    for block in [legacy, current] {
        assert!(
            block.starts_with("CSR CONTINUUM [3h ago]: (no clean intent)\n"),
            "{block}"
        );
        assert!(!block.contains("anthropics codemod"), "{block}");
        assert!(crate::extraction::provenance::is_csr_emission(&block));
    }
}

#[test]
fn episode_index_names_the_latest_task_and_stays_a_csr_emission() {
    let mut ep = minimal_episode(
        "anthropics codemod, when github links are bought up or disccussed, can",
        "Shipped the pane",
        None,
    );
    ep.message_count = 30_393;
    ep.recap_evidence_version = Some(crate::hooks::stop::RECAP_EVIDENCE_VERSION);
    ep.anchor_request = Some("How do we show cost as % of max plan".into());
    let current = format_episode_index(&[(ep.clone(), "3h ago".into())]);
    assert!(
        current.contains("- [3h ago] How do we show cost as % of max plan — "),
        "{current}"
    );
    assert!(!current.contains("anthropics codemod"), "{current}");
    // Legacy long session: the stale first prompt is withheld.
    ep.recap_evidence_version = None;
    ep.anchor_request = None;
    let legacy = format_episode_index(&[(ep, "3h ago".into())]);
    assert!(
        legacy.contains("- [3h ago] (no clean intent) — "),
        "{legacy}"
    );
    // The provenance guard still owns every line of the block.
    for block in [current, legacy] {
        assert!(crate::extraction::provenance::is_csr_emission(&block));
        assert!(crate::extraction::provenance::strip_csr_emissions(&block)
            .trim()
            .is_empty());
    }
}

#[test]
fn headless_sessions_are_never_a_projects_previous_session() {
    // `claude -p` children (entrypoint sdk-*) store episodes too: 19 in
    // the live DB, the newest of 4 projects. They must not anchor the
    // recap, CONTINUUM or EPISODE INDEX.
    let storage = crate::storage::Storage::open_memory().unwrap();
    for (session, entrypoint) in [
        ("human", Some("cli")),
        ("unrecorded", None),
        ("headless", Some("sdk-cli")),
    ] {
        let mut ep = minimal_episode("Fix the auth middleware regression", "", None);
        ep.session_id = session.into();
        ep.project = "proj".into();
        storage
            .insert_reflection(
                &format!("ep-{session}"),
                &serde_json::to_string(&ep).unwrap(),
                &crate::hooks::stop::episode_tags(&ep),
                &[0.0; 4],
            )
            .unwrap();
        if let Some(entrypoint) = entrypoint {
            storage
                .set_conversation_entrypoint(session, entrypoint)
                .unwrap();
        }
    }
    let mut sessions: Vec<String> = recent_project_episodes(&storage, "proj", 10)
        .into_iter()
        .map(|(ep, _)| ep.session_id)
        .collect();
    sessions.sort();
    // An unrecorded entrypoint is unknown, not headless: it stays.
    assert_eq!(sessions, ["human", "unrecorded"]);
}

#[test]
fn learnt_target_skips_own_sid_on_startup_keeps_it_on_resume_and_is_none_elsewhere() {
    let list = lineage_episodes(&["me", "previous", "older"]);
    let pick = |input: &HookInput| learnt_target(&list, input).map(|(e, _)| e.session_id.as_str());
    assert_eq!(pick(&learnt_input(Some("startup"), "me")), Some("previous"));
    assert_eq!(pick(&learnt_input(None, "me")), Some("previous"));
    assert_eq!(
        pick(&learnt_input(Some("startup"), "brand-new")),
        Some("me")
    );
    assert_eq!(pick(&learnt_input(Some("resume"), "me")), Some("me"));
    assert_eq!(pick(&learnt_input(Some("resume"), "new-sid")), Some("me"));
    for source in ["compact", "clear", "other"] {
        assert_eq!(pick(&learnt_input(Some(source), "x")), None, "{source}");
    }
    let mut subagent = learnt_input(Some("startup"), "x");
    subagent.agent_id = Some("agent-1".into());
    assert_eq!(pick(&subagent), None);
    assert!(learnt_target(&[], &learnt_input(Some("resume"), "x")).is_none());
    let only_self = lineage_episodes(&["me"]);
    assert!(learnt_target(&only_self, &learnt_input(Some("startup"), "me")).is_none());
}

#[test]
fn learnt_block_carries_the_line_without_a_recap_and_honours_the_kill_switch() {
    use crate::storage::recap_feeds::ConsumptionMode;
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    std::env::remove_var("CSR_NO_LEARNT");
    let storage = crate::storage::Storage::open_memory().unwrap();
    let list = lineage_episodes(&["previous-session-id"]);
    seed_learnt_note(&storage, "previous-session-id", "a-note");
    let input = learnt_input(Some("startup"), "new");

    // The recap abstains (empty text); the line is still there.
    let (line, session) = learnt_block(
        &input,
        &list,
        &storage,
        "p",
        "",
        ConsumptionMode::AnnotateOnly,
    )
    .expect("a note is evidence");
    assert_eq!(session, "previous-session-id");
    assert!(line.starts_with("learnt ["), "{line}");
    assert!(
        line.contains("1 memory note: a-note (previous, 2026-10-16). [[CSR:LEARNT]]\n"),
        "{line}"
    );
    assert_eq!(line.matches('\n').count(), 1);

    std::env::set_var("CSR_NO_LEARNT", "1");
    let off = learnt_block(
        &input,
        &list,
        &storage,
        "p",
        "",
        ConsumptionMode::AnnotateOnly,
    );
    std::env::set_var("CSR_NO_LEARNT", "TRUE");
    let off_upper = learnt_block(
        &input,
        &list,
        &storage,
        "p",
        "",
        ConsumptionMode::AnnotateOnly,
    );
    std::env::remove_var("CSR_NO_LEARNT");
    assert!(off.is_none() && off_upper.is_none());

    // No evidence for the target: no line, zero bytes.
    let bare = lineage_episodes(&["no-notes-session"]);
    assert!(learnt_block(&input, &bare, &storage, "p", "", ConsumptionMode::Full).is_none());
}

#[test]
fn learnt_block_feed_failure_is_an_absent_line_not_an_error() {
    use crate::storage::recap_feeds::ConsumptionMode;
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    std::env::remove_var("CSR_NO_LEARNT");
    let storage = crate::storage::Storage::open_memory().unwrap();
    storage
        .with_connection(|conn| {
            conn.execute_batch("DROP TABLE memory_registry; DROP TABLE session_registry;")?;
            Ok(())
        })
        .unwrap();
    let list = lineage_episodes(&["previous-session-id"]);
    let input = learnt_input(Some("resume"), "new");
    assert!(learnt_block(&input, &list, &storage, "p", "", ConsumptionMode::Full).is_none());
}

#[test]
fn the_learnt_line_is_session_framing_for_the_focus_file() {
    assert!(is_session_framing_line(
        "learnt [yesterday]: 17 memory notes: a, +16 more (6ca2a4d5, 2026-10-16). [[CSR:LEARNT]]"
    ));
    assert!(!is_session_framing_line("learning how to ship"));
}

#[test]
fn a_compaction_leads_with_its_own_session_not_a_concurrent_one() {
    let list = lineage_episodes(&["concurrent", "me", "older"]);
    let mut compact = learnt_input(Some("compact"), "me");
    assert_eq!(compact_own_session(&compact).as_deref(), Some("me"));
    let mut promoted = list.clone();
    promote_predecessor(&mut promoted, compact_own_session(&compact).as_deref());
    assert_eq!(session_ids(&promoted), ["me", "concurrent", "older"]);
    // Only a compaction by the main session: not startup, resume, clear or a subagent.
    for source in ["startup", "resume", "clear"] {
        assert_eq!(
            compact_own_session(&learnt_input(Some(source), "me")),
            None,
            "{source}"
        );
    }
    assert_eq!(compact_own_session(&learnt_input(None, "me")), None);
    compact.agent_id = Some("agent-1".into());
    assert_eq!(compact_own_session(&compact), None);
}

#[test]
fn promote_predecessor_is_a_stable_move_to_front_that_drops_nothing() {
    let mut list = lineage_episodes(&["concurrent", "a", "old", "b"]);
    promote_predecessor(&mut list, Some("old"));
    assert_eq!(session_ids(&list), ["old", "concurrent", "a", "b"]);
    // The age label travels with its episode.
    assert_eq!(list[0].1, "old ago");

    for none_case in [None, Some("unknown-session")] {
        let mut list = lineage_episodes(&["concurrent", "a", "old"]);
        promote_predecessor(&mut list, none_case);
        assert_eq!(session_ids(&list), ["concurrent", "a", "old"]);
    }
    let mut empty = Vec::new();
    promote_predecessor(&mut empty, Some("old"));
    assert!(empty.is_empty());
}

#[test]
fn resuming_promotes_before_truncation_and_without_a_predecessor_is_unchanged() {
    let storage = crate::storage::Storage::open_memory().unwrap();
    for (i, session) in ["oldest", "middle", "newest"].iter().enumerate() {
        let mut ep = minimal_episode("Fix the auth middleware regression", "", None);
        ep.session_id = (*session).into();
        ep.project = "proj".into();
        let tags = crate::hooks::stop::episode_tags(&ep);
        storage
            .insert_reflection(
                &format!("ep-{session}"),
                &serde_json::to_string(&ep).unwrap(),
                &tags,
                &[0.0; 4],
            )
            .unwrap();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE reflections SET timestamp = ?1 WHERE id = ?2",
                    rusqlite::params![format!("2026-10-1{i}T10:00:00Z"), format!("ep-{session}")],
                )?;
                Ok(())
            })
            .unwrap();
    }
    let plain = recent_project_episodes(&storage, "proj", 10);
    assert_eq!(session_ids(&plain), ["newest", "middle", "oldest"]);
    // Even a predecessor beyond the truncation limit leads the list.
    let resumed = recent_project_episodes_resuming(&storage, "proj", 1, Some("oldest"));
    assert_eq!(session_ids(&resumed), ["oldest"]);
    let resumed = recent_project_episodes_resuming(&storage, "proj", 10, Some("middle"));
    assert_eq!(session_ids(&resumed), ["middle", "newest", "oldest"]);
    let same = recent_project_episodes_resuming(&storage, "proj", 10, None);
    assert_eq!(session_ids(&same), session_ids(&plain));
}

#[test]
fn the_latest_episode_leads_however_busy_the_sibling_projects_and_cache_rows_are() {
    // Replay trace: `LIKE '%project_anukriti%'` also matched
    // `project_Anukriti-Campaigns` and `project_anukriti-website`, and the
    // 50-row window held no anukriti episode at all.
    let storage = crate::storage::Storage::open_memory().unwrap();
    insert_episode(&storage, "proj", "older", "2026-10-10T10:00:00Z");
    insert_episode(&storage, "proj", "latest", "2026-10-11T10:00:00Z");
    for i in 0..30 {
        // Sibling projects whose names start with this one, one in another case.
        insert_episode(
            &storage,
            "Proj-Campaigns",
            &format!("sib-{i}"),
            &format!("2026-10-12T10:{i:02}:00Z"),
        );
        insert_row(
            &storage,
            &format!("cache-sib-{i}"),
            "{}",
            &[
                "context_cache".to_string(),
                "project_proj-csr-engine".to_string(),
            ],
            &format!("2026-10-12T11:{i:02}:00Z"),
        );
    }
    for i in 0..60 {
        // The project's own cache and narrative rows, all newer.
        let kind = if i % 2 == 0 {
            "context_cache"
        } else {
            "narrative_extracted_v3"
        };
        insert_row(
            &storage,
            &format!("own-{i}"),
            "{}",
            &[
                kind.to_string(),
                "conv_latest".to_string(),
                "project_proj".to_string(),
            ],
            &format!("2026-10-13T10:{i:02}:00Z"),
        );
    }
    // The old substring window holds no episode of this project.
    let window = storage.get_reflections_by_tag("project_proj", 50).unwrap();
    assert!(!window.iter().any(|(_, _, tags, _)| {
        tags.iter().any(|t| t == "session_episode") && tags.iter().any(|t| t == "project_proj")
    }));
    let episodes = recent_project_episodes(&storage, "proj", 10);
    assert_eq!(session_ids(&episodes), ["latest", "older"]);
    // The sibling's own lookup is unaffected.
    assert_eq!(
        recent_project_episodes(&storage, "Proj-Campaigns", 50).len(),
        30
    );
}

#[test]
fn the_episode_lookup_matches_the_project_tag_exactly_and_case_sensitively() {
    let storage = crate::storage::Storage::open_memory().unwrap();
    insert_episode(&storage, "proj", "lower", "2026-10-10T10:00:00Z");
    insert_episode(&storage, "Proj", "upper", "2026-10-11T10:00:00Z");
    insert_episode(&storage, "proj-website", "prefixed", "2026-10-12T10:00:00Z");
    insert_episode(&storage, "my-proj", "suffixed", "2026-10-13T10:00:00Z");
    // 50 newer episodes of the same name in another case: a
    // case-insensitive match fills the whole window with them.
    for i in 0..50 {
        insert_episode(
            &storage,
            "Proj",
            &format!("upper-{i}"),
            &format!("2026-10-15T10:{i:02}:00Z"),
        );
    }
    assert_eq!(
        session_ids(&recent_project_episodes(&storage, "proj", 10)),
        ["lower"]
    );
    let upper = recent_project_episodes(&storage, "Proj", 10);
    assert_eq!(upper.len(), 10);
    assert_eq!(session_ids(&upper)[0], "upper-49");
    assert_eq!(
        session_ids(&recent_project_episodes(&storage, "proj-website", 10)),
        ["prefixed"]
    );
    assert!(recent_project_episodes(&storage, "pro", 10).is_empty());
    // Only episode rows count: a newer non-episode row of the project is not one.
    insert_row(
        &storage,
        "story",
        "{}",
        &["session_story".to_string(), "project_proj".to_string()],
        "2026-10-14T10:00:00Z",
    );
    assert_eq!(
        session_ids(&recent_project_episodes(&storage, "proj", 10)),
        ["lower"]
    );
}

/// SessionStart episode selection skips an episode whose text has a
/// detector hit (selection only; the stored reflection is untouched).
#[test]
fn episode_selection_skips_an_episode_whose_text_is_csr_text() {
    let storage = crate::storage::Storage::open_memory().unwrap();
    for (session, request, completed) in [
        ("csr", RULE_TEXT, "Fixed the auth middleware regression"),
        (
            "clean",
            "Fix the auth middleware regression",
            "Fixed the token check",
        ),
        (
            "csr-completed",
            "Fix the auth middleware regression",
            RULE_TEXT,
        ),
    ] {
        let mut ep = minimal_episode(request, completed, None);
        ep.session_id = session.into();
        ep.project = "proj".into();
        storage
            .insert_reflection(
                &format!("ep-{session}"),
                &serde_json::to_string(&ep).unwrap(),
                &crate::hooks::stop::episode_tags(&ep),
                &[0.0; 4],
            )
            .unwrap();
    }
    let sessions: Vec<String> = recent_project_episodes(&storage, "proj", 10)
        .into_iter()
        .map(|(ep, _)| ep.session_id)
        .collect();
    assert_eq!(sessions, ["clean"]);
}

/// The fragment block (what `CSR_NO_RECAP=1` forces) never renders a line
/// that is CSR text.
#[test]
fn fragments_do_not_render_csr_text() {
    let ep = minimal_episode(
        "Fix the auth middleware regression",
        RULE_TEXT,
        Some(RULE_TEXT),
    );
    let block = format_tier0_block_with_recap(
        &ep,
        &[],
        "2h ago",
        None,
        crate::provenance::TrustTier::UserHistory,
    );
    assert!(!block.contains("Keep receipts"), "{block}");
    let request = minimal_episode(RULE_TEXT, "Fixed the token check", None);
    let block = format_tier0_block_with_recap(
        &request,
        &[],
        "2h ago",
        None,
        crate::provenance::TrustTier::UserHistory,
    );
    assert!(!block.contains("Keep receipts"), "{block}");
}

/// Continuity (CONTINUED FROM, RECENT SESSIONS) never anchors on a session
/// whose summary or rendered title is CSR text.
#[test]
fn continuity_skips_a_session_whose_title_is_csr_text() {
    let session = |summary: Option<&str>, enrichment: Option<&str>| SessionInfo {
        conversation_id: "s".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-06-10T10:00:00Z".to_string(),
        total_messages: 30,
        chunk_count: 5,
        summary: summary.map(String::from),
        enrichment: enrichment.map(String::from),
    };
    assert!(is_meta_session(&session(Some(RULE_TEXT), None)));
    assert!(is_meta_session(&session(
        Some("fix the briefing staleness bug"),
        Some(&format!("## User Request\n{RULE_TEXT}\n"))
    )));
    assert!(!is_meta_session(&session(
        Some("fix the briefing staleness bug"),
        None
    )));
}

#[test]
fn startup_and_resume_get_the_open_block_and_nothing_else_does() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    std::env::remove_var("CSR_NO_RECAP");
    let storage = crate::storage::Storage::open_memory().unwrap();
    let mut ep = minimal_episode("ask", "done", None);
    ep.session_id = "prev-session".into();
    let (_tmp, mut input) = session_with_summary("prev-session");
    for source in [Some("resume"), Some("startup"), None] {
        input.source = source.map(String::from);
        let block = open_block(&input, &storage, &ep).expect("block");
        assert!(block.starts_with(crate::extraction::provenance::OPEN_BLOCK_HEADER));
        assert!(
            block.contains("- Still need to check receipt:true"),
            "{block}"
        );
        assert!(!block.contains("csr_transcript"), "{block}");
    }
    for source in [Some("compact"), Some("clear")] {
        input.source = source.map(String::from);
        assert!(open_block(&input, &storage, &ep).is_none(), "{source:?}");
    }
    // another session's summary is not read, and the kill switch holds
    input.source = Some("resume".into());
    let mut other = ep.clone();
    other.session_id = "someone-else".into();
    assert!(open_block(&input, &storage, &other).is_none());
    std::env::set_var("CSR_NO_RECAP", "1");
    assert!(open_block(&input, &storage, &ep).is_none());
    std::env::remove_var("CSR_NO_RECAP");
}

#[test]
fn a_new_or_resumed_session_leads_with_the_newest_ask_not_the_sessions_first() {
    let ep = ep_with_newer_ask();
    for source in [None, Some("startup"), Some("resume")] {
        let taken = recap_episode(&ep, source, Some("some-other-session"));
        assert_eq!(
            taken.anchor_request.as_deref(),
            Some("what are we waiting for do we have a ready product?"),
            "source {source:?}"
        );
    }
    // /clear and a compaction of another session keep the stored anchor.
    for source in [Some("clear"), Some("compact")] {
        let taken = recap_episode(&ep, source, Some("some-other-session"));
        assert_eq!(
            taken.anchor_request.as_deref(),
            Some("spawn a worker to fix the old feature"),
            "source {source:?}"
        );
    }
}

#[test]
fn a_briefing_older_than_the_last_turn_or_with_stale_ages_is_dropped() {
    let aged = "- **Latest (1m ago):** Request to fix the handoff.";
    let undated = "- **Latest:** Request to fix the handoff.";
    let mut ep = minimal_episode("ask", "done", None);
    ep.watermark = Some("2026-11-06T01:46:01.577Z".into());
    let now = at("2026-11-06T14:57:19Z");
    // written yesterday, before the last turn
    assert!(!briefing_still_true(
        undated,
        "2026-11-05T20:00:00Z",
        Some(&ep),
        &now
    ));
    // written after the last turn but hours ago, naming ages
    assert!(!briefing_still_true(
        aged,
        "2026-11-06T02:00:00Z",
        Some(&ep),
        &now
    ));
    // written after the last turn, no ages named: kept
    assert!(briefing_still_true(
        undated,
        "2026-11-06T02:00:00Z",
        Some(&ep),
        &now
    ));
    // written minutes ago, ages still true: kept
    assert!(briefing_still_true(
        aged,
        "2026-11-06T14:55:00Z",
        Some(&ep),
        &now
    ));
    // no readable write time: dropped
    assert!(!briefing_still_true(undated, "???", Some(&ep), &now));
    // no lead episode: only the ages rule applies
    assert!(briefing_still_true(
        undated,
        "2026-11-05T20:00:00Z",
        None,
        &now
    ));
}

/// A selected clean episode takes a compact anchor only when the anchor's
/// request is not CSR text.
#[test]
fn a_compact_anchor_that_is_csr_text_is_not_taken_after_selection() {
    let mut ep = minimal_episode("Fix the auth middleware regression", "Fixed it", None);
    ep.recap_evidence_version = Some(crate::hooks::stop::RECAP_EVIDENCE_VERSION);
    ep.anchor_request = Some("Fix the auth middleware regression.".into());
    ep.compact_anchor = Some(crate::hooks::stop::CompactAnchor {
        request: RULE_TEXT.into(),
        completions_untied: false,
        receipts: None,
    });
    let taken = recap_episode(&ep, Some("compact"), Some(ep.session_id.as_str()));
    assert_eq!(
        taken.anchor_request.as_deref(),
        Some("Fix the auth middleware regression."),
        "the CSR-text anchor must not replace the clean intent"
    );
    // a clean anchor is still taken
    ep.compact_anchor = Some(crate::hooks::stop::CompactAnchor {
        request: "Now wire the staging endpoint.".into(),
        completions_untied: false,
        receipts: None,
    });
    let taken = recap_episode(&ep, Some("compact"), Some(ep.session_id.as_str()));
    assert_eq!(
        taken.anchor_request.as_deref(),
        Some("Now wire the staging endpoint.")
    );
}

/// The snapshot's receipts belong to the anchor it was taken against. Laid
/// over an episode with a different anchor they would sit under a task they
/// were not made for.
#[test]
fn a_compact_snapshot_replaces_receipts_only_for_the_same_anchor() {
    let ep = stored_episode(Some(EARLIER), None);
    let same = overlay_compact_snapshot(
        &ep,
        snapshot_at(
            Some(LATER),
            Some("Fix the auth middleware regression."),
            None,
        ),
    )
    .expect("a snapshot that is ahead");
    assert_eq!(same.completions[0].receipt, "2222222");
    assert!(same.completions_untied);
    assert_eq!(same.anchor_receipts, Some(vec![0]));
    // A different anchor, or a snapshot that predates the field: the stored
    // receipts stay, the compact anchor still arrives.
    for other in [Some("Something else entirely."), None] {
        let kept =
            overlay_compact_snapshot(&ep, snapshot_at(Some(LATER), other, None)).expect("ahead");
        assert_eq!(kept.completions[0].receipt, "1111111", "{other:?}");
        assert!(!kept.completions_untied, "{other:?}");
        assert_eq!(kept.anchor_receipts, None, "{other:?}");
        assert_eq!(
            kept.compact_anchor.as_ref().map(|a| a.request.as_str()),
            Some("Now wire the staging endpoint.")
        );
    }
}

/// A snapshot taken against a later ask carries receipt indices into its own
/// completions. Over an episode of the earlier ask they would pick that
/// ask's receipts and show them beside the new one.
#[test]
fn a_newer_snapshot_with_another_anchor_does_not_pick_the_old_asks_receipts() {
    let mut ep = stored_episode(Some(EARLIER), None);
    ep.recap_evidence_version = Some(crate::hooks::stop::RECAP_EVIDENCE_VERSION);
    ep.anchor_receipts = Some(vec![0]);
    let mut snapshot = snapshot_at(Some(LATER), Some("Something else entirely."), None);
    snapshot.anchor = Some(crate::hooks::stop::CompactAnchor {
        request: "Now wire the staging endpoint.".into(),
        completions_untied: false,
        receipts: Some(vec![0]),
    });
    let overlaid = overlay_compact_snapshot(&ep, snapshot).expect("ahead");
    let shown = recap_episode(&overlaid, Some("compact"), Some(ep.session_id.as_str()));
    let feeds = crate::hooks::recap::RecapFeeds {
        settled: vec![],
        still_open: vec![],
        retired_while_away: vec![],
        corrections: vec![],
        open_proposals: 0,
        native_warn: None,
        top_dream: None,
    };
    let text = crate::hooks::recap::compose_recap(&shown, &feeds, "1h ago")
        .expect("the anchor still composes");
    let head = text.lines().next().unwrap_or_default();
    assert!(head.contains("Now wire the staging endpoint"), "{text}");
    // Neither receipt is shown beside the intent; the Settled clause is
    // session evidence and sits on its own line.
    assert!(!head.contains("1111111"), "{text}");
    assert!(!head.contains("2222222"), "{text}");
    // The same anchor moves its receipts together with the snapshot's.
    let mut same = snapshot_at(
        Some(LATER),
        Some("Fix the auth middleware regression."),
        None,
    );
    same.anchor = Some(crate::hooks::stop::CompactAnchor {
        request: "Now wire the staging endpoint.".into(),
        completions_untied: false,
        receipts: Some(vec![0]),
    });
    let overlaid = overlay_compact_snapshot(&ep, same).expect("ahead");
    let shown = recap_episode(&overlaid, Some("compact"), Some(ep.session_id.as_str()));
    let text = crate::hooks::recap::compose_recap(&shown, &feeds, "1h ago").expect("composes");
    let head = text.lines().next().unwrap_or_default();
    assert!(
        head.contains("2222222") && !head.contains("1111111"),
        "{text}"
    );
}

/// An ask saved with task A must not be shown after the compact anchor for
/// task B: a snapshot that is ahead replaces the stored ask, absence included.
#[test]
fn a_compact_snapshot_that_is_ahead_replaces_the_stored_last_ask_absence_included() {
    let ep = stored_episode(Some(EARLIER), Some(ASK_OLD));
    let newer = overlay_compact_snapshot(&ep, snapshot_at(Some(LATER), None, Some(ASK_NEW)))
        .expect("ahead");
    assert_eq!(newer.latest_ask.as_deref(), Some(ASK_NEW));
    let none = overlay_compact_snapshot(&ep, snapshot_at(Some(LATER), None, None)).expect("ahead");
    assert!(none.latest_ask.is_none(), "{:?}", none.latest_ask);
}

/// Snapshot S1, then a newer ask and its Stop episode. If the next
/// PreCompact cannot read its transcript, S1 survives: it must not bring
/// back its older ask or anchor over the newer episode.
#[test]
fn a_snapshot_behind_the_stored_episode_is_ignored_ask_and_anchor_alike() {
    let ep = stored_episode(Some(LATER), Some(ASK_NEW));
    assert!(
        overlay_compact_snapshot(&ep, snapshot_at(Some(EARLIER), None, Some(ASK_OLD))).is_none()
    );
}

/// A slow Stop reader stamps its wall clock later than a PreCompact that
/// read more of the transcript. Only the transcript position decides.
#[test]
fn a_slow_stop_does_not_outrank_a_snapshot_that_read_more() {
    // The episode's own timestamp is the wall clock of the slow Stop.
    let mut ep = stored_episode(Some(EARLIER), Some(ASK_OLD));
    ep.timestamp = "2026-07-08T23:59:00Z".into();
    let out = overlay_compact_snapshot(&ep, snapshot_at(Some(LATER), None, Some(ASK_NEW)))
        .expect("the snapshot read further into the transcript");
    assert_eq!(out.latest_ask.as_deref(), Some(ASK_NEW));
}

/// No clear newer side: the snapshot is ignored and a disagreement on the
/// last ask means no ask at all.
#[test]
fn without_a_clear_newer_side_a_disagreeing_ask_is_dropped() {
    for (stored, snap) in [
        (Some(LATER), Some(LATER)),
        (None, Some(LATER)),
        (Some(LATER), None),
        (Some("yesterday"), Some(LATER)),
    ] {
        let ep = stored_episode(stored, Some(ASK_OLD));
        let out = overlay_compact_snapshot(&ep, snapshot_at(snap, None, Some(ASK_NEW)))
            .expect("a disagreement still changes the episode");
        assert!(out.latest_ask.is_none(), "{stored:?} {snap:?}");
        assert!(out.compact_anchor.is_none(), "the anchor is not overlaid");
        // Agreeing asks: nothing to change.
        assert!(
            overlay_compact_snapshot(&ep, snapshot_at(snap, None, Some(ASK_OLD))).is_none(),
            "{stored:?} {snap:?}"
        );
    }
}

/// Only text that can render decides: a completed todo never renders, so
/// one that reads like CSR text does not drop a clean episode.
#[test]
fn a_completed_csr_text_todo_does_not_drop_a_clean_episode() {
    let storage = crate::storage::Storage::open_memory().unwrap();
    let mut ep = minimal_episode(
        "Fix the auth middleware regression",
        "Fixed the token check",
        None,
    );
    ep.session_id = "clean".into();
    ep.project = "proj".into();
    ep.todos = vec![crate::hooks::stop::TodoItem {
        content: RULE_TEXT.into(),
        status: "completed".into(),
    }];
    storage
        .insert_reflection(
            "ep-clean",
            &serde_json::to_string(&ep).unwrap(),
            &crate::hooks::stop::episode_tags(&ep),
            &[0.0; 4],
        )
        .unwrap();
    let sessions: Vec<String> = recent_project_episodes(&storage, "proj", 10)
        .into_iter()
        .map(|(ep, _)| ep.session_id)
        .collect();
    assert_eq!(sessions, ["clean"]);
    // an open todo does render, so one that is CSR text does drop it
    ep.todos[0].status = "pending".into();
    assert!(episode_is_csr_text(&ep));
}

#[test]
fn astra_r5_4_continued_from_never_names_a_headless_session() {
    let storage = crate::storage::Storage::open_memory().unwrap();
    let now = Utc::now();
    let put = |id: &str, conv: &str, minutes_ago: i64, summary: &str| {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conv.into(),
            project_name: "p".into(),
            timestamp: (now - chrono::Duration::minutes(minutes_ago)).to_rfc3339(),
            content: format!("work recorded for {summary}"),
            message_count: 6,
            summary: Some(summary.into()),
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
    };
    put("h1", "human", 120, "Fix the checkout validation bug");
    put("s1", "headless", 5, "Review the authentication changes");
    storage.set_conversation_entrypoint("human", "cli").unwrap();
    storage
        .set_conversation_entrypoint("headless", "sdk-cli")
        .unwrap();
    let continued = detect_continued_session(&storage, "p", &now).unwrap();
    assert_eq!(continued.conversation_id, "human");
}

#[test]
fn no_recap_kill_switch_forces_fragment_snapshot() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    std::env::set_var("CSR_NO_RECAP", "1");
    let storage = crate::storage::Storage::open_memory().unwrap();
    let ep = minimal_episode(
        "Fix the auth middleware regression",
        "Fixed token validation",
        Some("Deploy to staging"),
    );
    seed_recap_feeds(&storage, &ep);

    let block = format_session_start_tier0_for_session(
        &storage,
        &ep.project,
        &ep,
        &[],
        "2h ago",
        None,
        None,
    );
    std::env::remove_var("CSR_NO_RECAP");

    assert!(block.starts_with("CSR CONTINUUM [2h ago]: Fix the auth middleware regression\n"));
    assert!(block.contains("LAST: Fixed token validation (outcome=success, trust=unknown)"));
    assert!(!block.contains("recap ["));
}

#[test]
fn question_opener_with_real_work_keeps_anchor() {
    // Live failure 2026-07-07: the round-5-proofs session opened with
    // "did the sessionstart provide you useful info from the last
    // session?" then proved 6 claims — Tier-0 skipped it and anchored on
    // a stale predecessor. A question in the request must not disqualify
    // an episode that carries completed state.
    let ep = minimal_episode(
        "i restarted did the sessionstart provide you useful info from the last session?",
        "Proved all 5 claims plus subagent echo; rerank fixes landed; binary installed.",
        None,
    );
    assert!(episode_carries_state(&ep));
}

#[test]
fn question_opener_with_next_steps_keeps_anchor() {
    let ep = minimal_episode(
        "does the primacy boost survive a restart?",
        "",
        Some("verify MCP rerank live post-restart"),
    );
    assert!(episode_carries_state(&ep));
}

#[test]
fn bare_question_probe_still_skipped() {
    // Recall probes carry no state — the original filter's target.
    let ep = minimal_episode("what were we discussing recently?", "", None);
    assert!(!episode_carries_state(&ep));
}

#[test]
fn declarative_request_qualifies_alone() {
    let ep = minimal_episode("Fix the auth middleware regression", "", None);
    assert!(episode_carries_state(&ep));
}

#[test]
fn thin_continue_request_with_real_work_keeps_anchor() {
    // Live failure 2026-07-08: the session that BUILT the pickup feature
    // was itself skipped — its captured request was literally "continue"
    // (one word, not substantive), so the next "continue" anchored on a
    // stale predecessor. A thin request must not disqualify an episode
    // whose completed state is real.
    let ep = minimal_episode(
        "continue",
        "Done. All four build steps landed, proven live, binary installed.",
        None,
    );
    assert!(episode_carries_state(&ep));
}

#[test]
fn thin_request_with_meta_only_completed_still_skipped() {
    // Probe/command sessions: thin request AND meta-only conclusion.
    let ep = minimal_episode("continue", "", None);
    assert!(!episode_carries_state(&ep));
    let probe = minimal_episode(
        "memory-feedback",
        "## CSR Memory Feedback — 2026-06-10 — session report",
        None,
    );
    assert!(!episode_carries_state(&probe));
}

// --- format_episode_index tests ---

#[test]
fn episode_index_lines_carry_pointer_and_reconciled_outcome() {
    let mut recovery = minimal_episode(
        "prove the five solidity claims live",
        "Done. All claims pass, binary installed.",
        None,
    );
    recovery.outcome = "failed".into(); // stale mislabel + success LAST → shown as partial
    recovery.session_id = "ea57c42a".into();
    let plain = minimal_episode(
        "fix the rerank primacy band",
        "Band anchored at best eligible candidate.",
        None,
    );
    let index = format_episode_index(&[(recovery, "10m ago".into()), (plain, "2h ago".into())]);
    assert!(index.starts_with("EPISODE INDEX"));
    assert!(index.contains("[10m ago] prove the five solidity claims live"));
    assert!(index.contains("(outcome=partial)"));
    assert!(!index.contains("(outcome=failed)"));
    assert!(index.contains(r#"csr_reflect_on_past("conv_ea57c42a")"#));
    assert!(index.contains("[2h ago] fix the rerank primacy band"));
}

#[test]
fn an_episode_stored_before_the_current_outcome_rule_shows_no_label() {
    let mut legacy = minimal_episode("ship the release notes", "Notes shipped.", None);
    legacy.outcome = "failed".into();
    legacy.outcome_version = None;
    legacy.session_id = "0ldlabel".into();
    let mut current = minimal_episode("fix the rerank band", "Band fixed.", None);
    current.outcome = "success".into();
    current.closing_receipt = true;
    let index = format_episode_index(&[
        (legacy.clone(), "1d ago".into()),
        (current, "2d ago".into()),
    ]);
    let legacy_line = index.lines().find(|l| l.contains("0ldlabel")).unwrap();
    assert!(!legacy_line.contains("outcome="), "{legacy_line}");
    assert!(legacy_line.contains("ship the release notes"));
    assert!(index.contains("Band fixed. (outcome=success)"));
    let block = format_tier0_block(&legacy, &[], "1d ago");
    assert!(!block.contains("outcome="), "{block}");
    assert!(block.contains("LAST: Notes shipped. (trust="), "{block}");
    // The index line is still a CSR emission, so it is never re-imported.
    assert!(crate::extraction::provenance::is_csr_emission(&index));
}

#[test]
fn a_success_that_proved_nothing_shows_no_label_but_a_failure_still_does() {
    let mut unproven = minimal_episode("is the voice ready to ship", "Partly.", None);
    unproven.outcome = "success".into();
    unproven.closing_receipt = false;
    assert_eq!(trusted_outcome(&unproven), None);
    unproven.closing_success = true;
    assert_eq!(trusted_outcome(&unproven), Some("success"));
    let mut committed = minimal_episode("land the fix", "Done.", None);
    committed.closing_success = false;
    committed.closing_receipt = true;
    assert_eq!(trusted_outcome(&committed), Some("success"));
    let mut failed = minimal_episode("land the fix", "Red.", None);
    failed.outcome = "failed".into();
    assert_eq!(trusted_outcome(&failed), Some("failed"));
    failed.outcome = "partial".into();
    assert_eq!(trusted_outcome(&failed), Some("partial"));
}

#[test]
fn episode_index_empty_input_emits_nothing() {
    // With only one episode (the CONTINUUM anchor), no index renders at all —
    // a single-line index duplicating the CONTINUUM would be pure noise.
    assert_eq!(format_episode_index(&[]), "");
}

#[test]
fn the_episode_index_gives_up_its_oldest_lines_to_stay_under_the_cap() {
    use crate::hooks::start_budget::{episode_index_within, START_BLOCK_MAX_BYTES};
    let episodes: Vec<(Episode, String)> = (0..4)
        .map(|n| {
            let mut ep = minimal_episode(
                &format!("task number {n} about the index"),
                &format!(
                    "Finished part {n}. {}",
                    "More detail follows here. ".repeat(8)
                ),
                None,
            );
            ep.session_id = format!("sess{n}0000");
            (ep, format!("{n}d ago"))
        })
        .collect();
    let full = format_episode_index(&episodes);
    let tail = MEMORY_MANIFEST.len() + DEEPER_CONTEXT_FOOTER.len() + 1;
    // Plenty of room: the index is whole.
    let (index, named) = episode_index_within(&episodes, None, 0);
    assert_eq!((index.as_str(), named), (full.as_str(), 4));
    // Room for the header and the first two lines only: the two oldest go.
    let lines: Vec<&str> = full.split_inclusive('\n').collect();
    let two = lines[..3].iter().map(|l| l.len()).sum::<usize>();
    let used = START_BLOCK_MAX_BYTES - tail - two - 1;
    let (index, named) = episode_index_within(&episodes, None, used);
    assert_eq!(named, 2);
    assert_eq!(index, lines[..3].concat());
    assert!(index.contains("task number 0") && !index.contains("task number 2"));
    assert!(used + tail + index.len() <= START_BLOCK_MAX_BYTES);
    // A body that leaves no room for one line drops the header with them.
    let (index, named) = episode_index_within(&episodes, None, START_BLOCK_MAX_BYTES - tail);
    assert_eq!((index.as_str(), named), ("", 0));
    // What is kept is still CSR's own text on re-import.
    let (index, _) = episode_index_within(&episodes, None, used);
    assert!(crate::extraction::provenance::is_csr_emission(&index));
}

#[test]
fn an_index_line_names_the_ask_in_twelve_words_like_the_recap_head() {
    let mut ep = minimal_episode("placeholder", "Finished the work.", None);
    ep.recap_evidence_version = Some(crate::hooks::stop::RECAP_EVIDENCE_VERSION);
    ep.anchor_request = Some(
        "Rework the whole recap composer so that the head clause stays short while the rest waits."
            .into(),
    );
    let label = episode_intent_label(&ep);
    assert_eq!(
        label,
        "Rework the whole recap composer so that the head clause stays short\u{2026}"
    );
    let line = format_episode_index(&[(ep, "2d ago".into())]);
    assert!(
        line.contains(&label) && !line.contains("while the rest waits"),
        "{line}"
    );
}

#[test]
fn episode_index_header_is_framing_for_focus_file() {
    assert!(is_session_framing_line(EPISODE_INDEX_HEADER.trim_end()));
}

// --- relative_time_label tests (Bug 2: hour-level granularity) ---

#[test]
fn test_relative_time_just_now() {
    let now = Utc::now();
    let ts = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    assert_eq!(relative_time_label(&ts, &now), "just now");
}

#[test]
fn test_relative_time_minutes_ago() {
    let now = Utc::now();
    let ts = (now - chrono::Duration::minutes(15))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    assert_eq!(relative_time_label(&ts, &now), "15m ago");
}

#[test]
fn test_relative_time_hours_ago() {
    let now = Utc::now();
    let ts = (now - chrono::Duration::hours(3))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    assert_eq!(relative_time_label(&ts, &now), "3h ago");
}

#[test]
fn test_same_day_differentiation() {
    let now = Utc::now();
    let ts_2h = (now - chrono::Duration::hours(2))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let ts_6h = (now - chrono::Duration::hours(6))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let label_2h = relative_time_label(&ts_2h, &now);
    let label_6h = relative_time_label(&ts_6h, &now);
    // Same-day sessions must be distinguishable
    assert_ne!(label_2h, label_6h);
    assert_eq!(label_2h, "2h ago");
    assert_eq!(label_6h, "6h ago");
}

#[test]
fn test_relative_time_yesterday() {
    let now = Utc::now();
    let ts = (now - chrono::Duration::hours(30))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    assert_eq!(relative_time_label(&ts, &now), "yesterday");
}

#[test]
fn test_relative_time_days_ago() {
    let now = Utc::now();
    let ts = (now - chrono::Duration::days(3))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    assert_eq!(relative_time_label(&ts, &now), "3d ago");
}

#[test]
fn test_relative_time_weeks_ago() {
    let now = Utc::now();
    let ts = (now - chrono::Duration::days(14))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    assert_eq!(relative_time_label(&ts, &now), "2w ago");
}

#[test]
fn test_relative_time_months_ago() {
    let now = Utc::now();
    let ts = (now - chrono::Duration::days(45))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    assert_eq!(relative_time_label(&ts, &now), "1mo ago");
}

#[test]
fn test_relative_time_invalid() {
    let now = Utc::now();
    assert_eq!(relative_time_label("not-a-timestamp", &now), "???");
}

// --- compact_preview tests ---

#[test]
fn test_compact_preview_short() {
    assert_eq!(compact_preview("short text", 55), "short text");
}

#[test]
fn test_compact_preview_truncates() {
    let long = "a".repeat(100);
    let result = compact_preview(&long, 55);
    assert!(result.ends_with("..."));
    assert!(result.len() <= 58);
}

#[test]
fn test_compact_preview_strips_newlines() {
    assert_eq!(compact_preview("hello\nworld\nfoo", 55), "hello world foo");
}

#[test]
fn test_compact_preview_unicode_safe() {
    let content = "\u{1f600}".repeat(30);
    let result = compact_preview(&content, 10);
    assert!(result.ends_with("..."));
}
