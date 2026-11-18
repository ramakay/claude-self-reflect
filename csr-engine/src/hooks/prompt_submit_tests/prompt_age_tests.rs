use super::*;

/// Through the production adapter: a real repository on disk, real `git`,
/// decode from `/`, several `import_state` rows for one id.
#[tokio::test]
async fn the_production_rule_recalls_a_subdirectory_session_of_a_real_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let (repo, tools) = (root.join("projects/repo"), root.join("projects/repo-tools"));
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    std::fs::create_dir_all(&tools).unwrap();
    let git_init = |dir: &Path| {
        let mut cmd = std::process::Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&k);
            }
        }
        cmd.arg("init")
            .arg("-q")
            .arg(dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if !git_init(&repo) || !git_init(&tools) {
        return; // git unavailable in this environment
    }
    let projects_dir = root.join("cc");
    let folder =
        |dir: &Path| crate::search::cross_project::encode_project_folder(&dir.to_string_lossy());
    let in_sub = |name: &str| {
        format!(
            "{}/{}/{name}",
            projects_dir.display(),
            folder(&repo.join("sub"))
        )
    };
    let in_tools = |name: &str| format!("{}/{}/{name}", projects_dir.display(), folder(&tools));

    let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
    let mut search = crate::search::SearchEngine::new(8);
    for (id, conversation_id, label) in [
        ("root-chunk", "s-root", "repo"),
        ("sub-chunk", "s-sub", "repo-sub"),
        ("overwritten-chunk", "s-sub", "elsewhere"),
        ("tools-chunk", "s-tools", "repo-tools"),
        ("shared-stem-chunk", "journal", "repo-sub"),
    ] {
        let chunk = crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: conversation_id.into(),
            project_name: label.into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            content: format!("work recorded under {label}"),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[1.0, 0.0]).unwrap();
        search.insert_chunk(chunk.id.clone(), vec![1.0, 0.0]);
    }
    for (file_path, conversation_id) in [
        (in_sub("s-sub.jsonl"), "s-sub"),
        (in_tools("s-tools.jsonl"), "s-tools"),
        (in_sub("w1/journal.jsonl"), "journal"),
        (in_tools("w2/journal.jsonl"), "journal"),
    ] {
        storage
            .upsert_import_state_explicit(&file_path, conversation_id, 1, "0")
            .unwrap();
    }
    let engine = crate::engine::Engine::from_parts(
        storage,
        std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap()),
        std::sync::Arc::new(tokio::sync::RwLock::new(search)),
        projects_dir,
    );
    let mut ids: Vec<String> = search_chunks_with_vec(
        &engine,
        "",
        &[1.0, 0.0],
        10,
        0.0,
        "repo",
        None,
        &repo.join("sub"),
        &mut SearchEvidence::default(),
    )
    .await
    .into_iter()
    .filter_map(|result| result.memory_id)
    .collect();
    ids.sort();
    assert_eq!(ids, ["root-chunk", "sub-chunk"]);
}

#[tokio::test]
async fn a_conversation_the_repository_test_rejects_stays_out() {
    let (ids, _) = recall_across_labels("repo", &IMPORT_ROWS, &[]).await;
    assert_eq!(ids, ["root-chunk"]);
}

#[tokio::test]
async fn no_recorded_transcript_path_means_no_repository_test_at_all() {
    // Differing labels but nothing in import_state: nothing to decide on.
    let (ids, seen) = recall_across_labels("repo", &[], &[("sub-session", "repo-sub")]).await;
    assert_eq!(ids, ["root-chunk"]);
    assert!(seen.is_none());
}

#[tokio::test]
async fn an_unscoped_search_never_consults_the_repository_test() {
    let (ids, seen) = recall_across_labels("", &IMPORT_ROWS, &[]).await;
    assert_eq!(ids, ["other-chunk", "root-chunk", "sub-chunk"]);
    assert!(seen.is_none());
}

#[test]
fn prompt_ancestry_excludes_only_demoted_chunk_in_shared_conversation() {
    use crate::storage::ancestry::{AncestryLabel, AncestryState};

    let label = |conversation_id: &str| AncestryLabel {
        conversation_id: conversation_id.into(),
        state: AncestryState::Shipped,
        release_tag: Some("v1.0.0".into()),
        releases_behind: 5,
        repository: "/repo".into(),
        refreshed_at: "2026-08-06T12:00:00Z".into(),
    };
    let labels = [("shared".into(), label("shared"))].into_iter().collect();
    let chunks = vec![
        ("chunk-demoted".to_string(), "shared".to_string()),
        ("chunk-clean".to_string(), "shared".to_string()),
    ];
    let validity = [(
        "chunk-demoted".into(),
        crate::mcp::tools::ConvValidity {
            demote: true,
            note: "stale".into(),
            csr_only_demote: false,
        },
    )]
    .into_iter()
    .collect();

    let releases = ancestry_releases_for_prompt(labels, &chunks, &validity);

    assert!(!releases.contains_key("chunk-demoted"));
    assert_eq!(releases.get("chunk-clean"), Some(&5));
}

// --- episode recency ranking (Route B stale-anchor fix) ---

#[test]
fn fresh_episode_outranks_stale_higher_score() {
    // Live failure 2026-07-08: a 3-week-old prompt-echo reflection (0.56)
    // beat the 2-hour-old prior session (0.46), so PICKUP anchored stale
    // state. Candidates are (raw_score, age_days).
    let cands = vec![(0.56, 21.0), (0.46, 0.1)];
    assert_eq!(pick_episode(&cands), Some(1));
}

#[test]
fn stale_episode_still_reachable_when_only_match() {
    // Old work must stay resumable — recency weights the ordering; past
    // the chunk window it takes a strong match (see episode_may_anchor).
    assert_eq!(pick_episode(&[(0.56, 21.0)]), Some(0));
    assert_eq!(pick_episode(&[(0.70, 72.0)]), Some(0));
}

#[test]
fn a_weak_resemblance_to_months_old_work_is_not_a_pickup() {
    // The candidates the pasted hand-off list drew on 2026-10-10 (raw,
    // age in days); only the 20.6-day-old one was young enough, and it
    // fired at 0.515 on an unrelated episode. The person's own words in
    // that prompt score 0.328 against the best episode.
    let pasted = [
        (0.542, 72.39),
        (0.536, 95.68),
        (0.526, 26.13),
        (0.507, 121.24),
    ];
    assert_eq!(pick_episode(&pasted), None);
    assert_eq!(pick_episode(&[(0.328, 6.37)]), None);
    // Inside the chunk window the raw gate alone decides, as before.
    assert_eq!(pick_episode(&[(0.515, 20.56)]), Some(0));
    // Unknown age keeps the raw gate only.
    assert_eq!(pick_episode(&[(0.50, f32::INFINITY)]), Some(0));
}

#[test]
fn below_min_similarity_never_anchors() {
    assert_eq!(pick_episode(&[(0.44, 0.1)]), None);
    assert_eq!(pick_episode(&[]), None);
}

#[test]
fn zero_age_effective_score_is_raw() {
    assert!((effective_episode_score(0.5, 0.0) - 0.5).abs() < 1e-6);
}

#[test]
fn equal_age_preserves_score_order() {
    assert_eq!(pick_episode(&[(0.46, 1.0), (0.56, 1.0)]), Some(1));
}

#[test]
fn symbol_overlap_detects_anchored_names() {
    let anchors = vec![
        ("s1".to_string(), "validate_token".to_string()),
        ("s1".to_string(), "main".to_string()), // short/common — must be ignored
    ];
    assert_eq!(
        symbol_overlap("the validate_token check is wrong", &anchors),
        Some(("s1".to_string(), "validate_token".to_string()))
    );
    assert_eq!(symbol_overlap("main thing to do today", &anchors), None);
    assert_eq!(symbol_overlap("unrelated prompt", &anchors), None);
}

#[test]
fn test_extract_file_paths() {
    let content = "Modified files:\n- src/auth.rs\n- Cargo.toml\n- some text";
    let files = extract_file_paths(content);
    assert!(files.contains(&"src/auth.rs".to_string()));
    assert!(files.contains(&"Cargo.toml".to_string()));
}

#[test]
fn test_extract_error_patterns() {
    let content = "Log:\nError: connection refused\ninfo: compiling\nerror[E0308]: type mismatch";
    let errors = extract_error_patterns(content);
    assert_eq!(errors.len(), 2);
    assert!(errors[0].contains("connection refused"));
    assert!(errors[1].contains("E0308"));
}

#[test]
fn test_slash_command_detection() {
    // Verify our fast-path logic
    assert!("/help".starts_with('/'));
    assert!("/commit".starts_with('/'));
    assert!(!"fix the bug".starts_with('/'));
}

#[test]
fn test_min_prompt_length() {
    assert!("short".len() < MIN_PROMPT_LENGTH);
    assert!("fix the authentication timeout bug".len() >= MIN_PROMPT_LENGTH);
}

#[test]
fn test_max_chunk_age_constant() {
    // Verify the age gate is set to 21 days
    assert_eq!(MAX_CHUNK_AGE_DAYS, 21);
    // Reflections should NOT be gated (only chunks)
    // This is enforced by the filter only being in search_chunks_with_vec,
    // not in search_reflections_with_vec.
}

#[test]
fn test_self_referential_noise_detected() {
    assert!(is_self_referential_noise(
        "This proves the session_start_hook.py successfully retrieved"
    ));
    assert!(is_self_referential_noise(
        "Current Ralph State: Iteration: 198"
    ));
    assert!(is_self_referential_noise(
        "hook success: session-end completed"
    ));
    assert!(is_self_referential_noise(
        "CSR engine ready. Session: abc123"
    ));
    assert!(is_self_referential_noise(
        "Test in hooks_integration module"
    ));
    assert!(is_self_referential_noise("proves the hook works correctly"));
}

#[test]
fn test_project_scope_reflection_filter_logic() {
    // Reflections tagged for a different project should be filtered
    let tags_other = ["project_anukriti".to_string(), "session_story".to_string()];
    let project = "csr";
    let project_tag = format!("project_{}", project);
    let has_project_tags = tags_other.iter().any(|t| t.starts_with("project_"));
    assert!(has_project_tags && !tags_other.contains(&project_tag));

    // Legacy reflections without project tags should pass through
    let tags_legacy = ["session_story".to_string()];
    let has_project_tags_legacy = tags_legacy.iter().any(|t| t.starts_with("project_"));
    assert!(!has_project_tags_legacy); // no filter applied

    // Same-project reflections should pass
    let tags_same = ["project_csr".to_string(), "session_story".to_string()];
    let has_project_tags_same = tags_same.iter().any(|t| t.starts_with("project_"));
    assert!(has_project_tags_same && tags_same.contains(&project_tag));
}

/// Context selection skips a chunk the detector labels (CSR's own pane
/// sends); a person's sentence that only mentions the words stays eligible.
#[test]
fn predictive_context_skips_chunks_with_a_detector_hit() {
    assert!(is_self_referential_noise(
        "A standing rule from 10-12: “Keep receipts”"
    ));
    assert!(is_self_referential_noise(
        "CSR remembered: “Use component ownership”."
    ));
    assert!(!is_self_referential_noise(
        "My mother said it best: `A standing rule from 10-12: “Keep receipts”` is worth keeping in the docs."
    ));
}

#[test]
fn test_non_noise_content_passes() {
    assert!(!is_self_referential_noise(
        "Fix the authentication timeout bug"
    ));
    assert!(!is_self_referential_noise(
        "Docker compose memory issue resolved"
    ));
    assert!(!is_self_referential_noise(
        "Use batch embedding for 3x speedup"
    ));
    // "proves the" without hook/session/integration should NOT be filtered (F6 fix)
    assert!(!is_self_referential_noise(
        "This proves the approach works for authentication"
    ));
}

// --- is_continuation_prompt boundaries (Route A classifier) ---
// Route A only handles no-signal prompts recency-style; content prompts
// are covered by Route B semantic correlation, so these asserts test the
// boundary (hijack prevention), not the phrase list.

#[test]
fn harness_turns_are_recognised_and_typed_prompts_are_not() {
    for harness in [
        "<system-reminder>\n[SYSTEM NOTIFICATION - NOT USER INPUT]\n<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n</task-notification>\n</system-reminder>",
        "[SYSTEM NOTIFICATION - NOT USER INPUT] background job finished",
        "[queued] [SYSTEM NOTIFICATION - NOT USER INPUT] background job finished",
        "<local-command-caveat>Caveat: generated by local commands</local-command-caveat>",
        "<cross-session-message from=\"uds:/tmp/x.sock\" from-name=\"peer\">feedback for you</cross-session-message>",
    ] {
        assert!(is_harness_turn(harness), "{harness}");
    }
    for typed in [
        "why does the task notification hook fire twice?",
        "fix the statusline badge count",
        "continue",
    ] {
        assert!(!is_harness_turn(typed), "{typed}");
    }
}

#[test]
fn early_exits_skip_the_index_and_searchable_prompts_do_not() {
    assert_eq!(early_exit(""), Some(EarlyExit::Silent));
    assert_eq!(
        early_exit("<task-notification>\n<status>completed</status>\n</task-notification>"),
        Some(EarlyExit::Silent)
    );
    assert_eq!(early_exit("/compact"), Some(EarlyExit::Silent));
    assert_eq!(early_exit("ok"), Some(EarlyExit::Silent));
    assert_eq!(early_exit("continue"), Some(EarlyExit::LineagePickup));
    // A harness turn that happens to open with a continuation phrase stays silent.
    assert_eq!(
        early_exit("continue <task-notification>done</task-notification>"),
        Some(EarlyExit::Silent)
    );
    assert!(exits_before_search(None));
    assert!(exits_before_search(Some("continue")));
    // Short but not a stock phrase: the intent classifier may route it to
    // the Explore arm, which searches reflections.
    for searched in [
        "where is auth?",
        "keep at it",
        "fix the statusline badge count",
    ] {
        assert_eq!(early_exit(searched), None, "{searched}");
        assert!(!exits_before_search(Some(searched)), "{searched}");
    }
}

#[test]
fn a_slash_prompt_gets_no_injection_whatever_follows_the_slash() {
    for slash in [
        "/compact",
        "  /review the auth module and tell me what is wrong with it",
        "\n\t/skill-name with a long argument that would otherwise be searched",
        "/Users/me/projects/app/src/main.rs fails to build, why does it do that?",
    ] {
        assert_eq!(early_exit(slash), Some(EarlyExit::Silent), "{slash}");
        assert_eq!(early_exit_for(None, slash), Some(EarlyExit::Silent));
        assert_eq!(early_exit_for(Some("user"), slash), Some(EarlyExit::Silent));
        assert!(exits_before_search(Some(slash)), "{slash}");
    }
    // A slash later in the prompt is a path, not a command.
    assert_eq!(early_exit("why does src/main.rs fail to build?"), None);
}

#[test]
fn the_source_field_names_a_harness_turn_when_it_is_present() {
    for harness in [
        "system",
        "sdk",
        "loop_wakeup",
        "schedule_wakeup",
        " System ",
        "SDK",
    ] {
        assert_eq!(harness_source(Some(harness)), Some(true), "{harness}");
    }
    for typed in [
        "user",
        "prompt",
        "typed",
        "queued",
        "suggestion_accepted",
        "USER",
    ] {
        assert_eq!(harness_source(Some(typed)), Some(false), "{typed}");
    }
    assert_eq!(harness_source(None), None);
    assert_eq!(harness_source(Some("")), None);
    assert_eq!(harness_source(Some("   ")), None);
}

#[test]
fn a_harness_source_is_silent_even_for_a_searchable_prompt() {
    let searchable = "fix the statusline badge count";
    assert_eq!(early_exit(searchable), None);
    for harness in ["system", "sdk", "loop_wakeup", "schedule_wakeup"] {
        assert_eq!(
            early_exit_for(Some(harness), searchable),
            Some(EarlyExit::Silent),
            "{harness}"
        );
    }
    // A typed source leaves the text rules in charge.
    assert_eq!(early_exit_for(Some("user"), searchable), None);
}

#[test]
fn an_absent_source_falls_back_to_the_text_heuristic() {
    let notification =
        "<task-notification>\n<status>completed</status>\n</task-notification> and then some more words";
    assert_eq!(early_exit_for(None, notification), Some(EarlyExit::Silent));
    assert_eq!(
        early_exit_for(Some(""), notification),
        Some(EarlyExit::Silent)
    );
    assert_eq!(early_exit_for(None, "fix the statusline badge count"), None);
    assert_eq!(
        early_exit_for(None, "continue"),
        Some(EarlyExit::LineagePickup)
    );
}

#[test]
fn a_prompt_that_is_only_the_ask_and_the_envelope_has_nothing_to_search() {
    let ask = "Lost and found: “Run source-gone validation” went quiet after 14:30. Hand it to a background subagent with a five-part spec (objective, files, interfaces, constraints, verification), keep working on the current task, and report its result when it lands.";
    for fixture in crate::eval::contamination::agents_document()
        .fixtures
        .into_values()
    {
        let prompt = format!("{ask}\n{}", fixture.text);
        assert_eq!(early_exit(&prompt), Some(EarlyExit::Silent), "{prompt}");
        // the envelope alone is CSR's too
        assert_eq!(early_exit(&fixture.text), Some(EarlyExit::Silent));
    }
    // the person's words around it still search, on those words only
    let typed = format!(
        "fix the statusline badge count\n{ask}\n{}",
        crate::eval::contamination::agents_document().fixtures["handoff"].text
    );
    assert_eq!(early_exit(&typed), None);
    assert_eq!(
        crate::extraction::provenance::strip_csr_emissions(&typed),
        "fix the statusline badge count"
    );
}

#[test]
fn continuation_prompt_boundaries() {
    // Bare and lightly decorated forms fire.
    assert!(is_continuation_prompt("continue"));
    assert!(is_continuation_prompt("  Resume.  "));
    assert!(is_continuation_prompt("continue with the rerank work"));
    // Contains the verb but doesn't open with it — never hijacked.
    assert!(!is_continuation_prompt("the tests continue to fail on CI"));
    // Opens with the verb but is a long, self-scoped instruction (>60 chars).
    assert!(!is_continuation_prompt(
        "continue refactoring the session_start module and then rewrite the anchor verification logic"
    ));
    assert!(!is_continuation_prompt("fix the auth bug"));
    assert!(!is_continuation_prompt("/continue"));
}

// --- pickup echo guard (stale episode whose request restates the prompt) ---

#[test]
fn is_pickup_echo_rejects_near_verbatim_match() {
    // Containment either direction, both sides long enough — the episode
    // was itself just asking the same question, not state to resume.
    assert!(is_pickup_echo(
        "what's the status of the sessionstart debugging work",
        "status of the sessionstart debugging work",
    ));
    assert!(is_pickup_echo(
        "status of the sessionstart debugging work",
        "what's the status of the sessionstart debugging work",
    ));
}

#[test]
fn an_episode_whose_latest_task_restates_the_prompt_is_an_echo() {
    let mut ep = code_map_episode();
    ep.request = "anthropics codemod side pane thoughts for herdr users".into();
    ep.message_count = 30_393;
    ep.recap_evidence_version = Some(crate::hooks::stop::RECAP_EVIDENCE_VERSION);
    ep.anchor_request = Some("How do we show cost as % of max plan".into());
    assert!(is_episode_echo("how do we show cost as % of max plan", &ep));
    // The first prompt still counts, as before.
    assert!(is_episode_echo(
        "anthropics codemod side pane thoughts for herdr users",
        &ep
    ));
    assert!(!is_episode_echo("fix the login validation for signup", &ep));

    let mut handed = HandedOnce::default();
    handed.note_episode(&ep);
    assert!(!handed.admit(None, true, "How do we show cost as % of max plan, again"));
}

#[test]
fn is_pickup_echo_accepts_unrelated_or_paraphrase() {
    // Genuine content / paraphrase against a different episode request —
    // no containment, so not an echo.
    assert!(!is_pickup_echo(
        "fix the auth timeout bug",
        "status of the sessionstart debugging work",
    ));
    // Too short on either side is coincidence, not a signal.
    assert!(!is_pickup_echo(
        "short",
        "status of the sessionstart debugging work"
    ));
    assert!(!is_pickup_echo(
        "what's the status of the sessionstart debugging work",
        "short",
    ));
}

#[test]
fn format_semantic_pickup_has_no_continuum_banner() {
    // Route B topic matches must never render the lineage CONTINUUM block.
    let out = format_semantic_pickup(&code_map_episode(), "2d ago", 0.52);
    assert!(out.contains("CSR PICKUP"));
    assert!(
        !out.contains("CSR CONTINUUM"),
        "semantic pickup must not render lineage CONTINUUM banner: {out}"
    );
    assert!(out.contains("csr_reflect_on_past(\"conv_abc123\")"));
}

#[test]
fn code_map_lists_files_with_anchor_counts_and_lookup() {
    let out = format_code_map(&code_map_episode(), "2d ago").unwrap();
    assert!(out.starts_with("CSR CODE MAP"));
    assert!(out.contains("src/radio/RadioSheet.swift"));
    assert!(out.contains("1 anchor"));
    assert!(out.contains("outcome=partial"));
    assert!(out.contains("csr_reflect_on_past(\"conv_abc123\")"));
    assert!(out.contains("Read these before mapping"));
}

#[test]
fn code_map_none_when_no_files() {
    let mut ep = code_map_episode();
    ep.files_modified.clear();
    assert!(format_code_map(&ep, "2d ago").is_none());
}

#[test]
fn code_map_caps_at_five_files() {
    let mut ep = code_map_episode();
    ep.files_modified = (0..9).map(|i| format!("src/file_{i}.swift")).collect();
    let out = format_code_map(&ep, "1h ago").unwrap();
    assert_eq!(out.matches("src/file_").count(), 5);
}

#[test]
fn code_map_anchor_count_respects_path_segment_boundary() {
    // "Ring.swift" must NOT match the anchor for ".../ChannelRing.swift" —
    // that's a basename substring collision, not the same file.
    // "RadioSheet.swift" (relative, no dir) must still match the anchor's
    // "src/radio/RadioSheet.swift" (absolute-ish) — that's a genuine
    // path-segment-boundary suffix match across abs/rel styles.
    let mut ep = code_map_episode();
    ep.files_modified = vec!["Ring.swift".into(), "RadioSheet.swift".into()];
    ep.anchors = vec![
        crate::extraction::anchors::FunctionAnchor {
            file: "src/radio/ChannelRing.swift".into(),
            node_kind: "file".into(),
            name: "ChannelRing.swift".into(),
            body_hash: "h1".into(),
        },
        crate::extraction::anchors::FunctionAnchor {
            file: "src/radio/RadioSheet.swift".into(),
            node_kind: "file".into(),
            name: "RadioSheet.swift".into(),
            body_hash: "h2".into(),
        },
    ];
    let out = format_code_map(&ep, "2d ago").unwrap();
    let ring_line = out
        .lines()
        .find(|l| l.trim_start().starts_with("Ring.swift"))
        .expect("Ring.swift line present");
    assert!(
        !ring_line.contains("anchor"),
        "basename substring collision must not count: {ring_line}"
    );
    let sheet_line = out
        .lines()
        .find(|l| l.trim_start().starts_with("RadioSheet.swift"))
        .expect("RadioSheet.swift line present");
    assert!(
        sheet_line.contains("1 anchor"),
        "boundary match across abs/rel paths must still count: {sheet_line}"
    );
}

/// The file slice is read by file name through the index: a prompt naming
/// `stop.rs` gets `hooks/stop.rs`, never `nonstop.rs`, whose name only ends
/// in the same letters (the `LIKE '%stop.rs'` scan matched both).
#[test]
fn graph_slice_for_a_named_file_never_takes_a_file_whose_name_only_ends_alike() {
    let storage = crate::storage::Storage::open_memory().unwrap();
    let node = |id: &str, file: &str, name: &str| crate::storage::codegraph::NodeRow {
        id: id.into(),
        project: "proj".into(),
        file: file.into(),
        kind: "function".into(),
        name: name.into(),
        ..Default::default()
    };
    for (id, file, name) in [
        ("n_other", "/repo/src/nonstop.rs", "aaa_elsewhere"),
        ("n_stop", "/repo/src/hooks/stop.rs", "handle_stop"),
        ("n_main", "/repo/src/main.rs", "main_loop"),
    ] {
        storage.upsert_code_node(&node(id, file, name)).unwrap();
    }
    let edge = |src: &str, dst: &str, file: &str| crate::storage::codegraph::EdgeRow {
        src_id: src.into(),
        dst_id: dst.into(),
        kind: "calls".into(),
        src_file: file.into(),
        resolved: 1,
        weight: 1.0,
        ..Default::default()
    };
    storage
        .replace_code_file_edges(
            "proj",
            "/repo/src/main.rs",
            &[edge("n_main", "n_stop", "/repo/src/main.rs")],
        )
        .unwrap();
    storage
        .replace_code_file_edges(
            "proj",
            "/repo/src/nonstop.rs",
            &[edge("n_other", "n_other", "/repo/src/nonstop.rs")],
        )
        .unwrap();

    let slices = build_graph_slices(&storage, "look at stop.rs", &["stop.rs".into()], "proj");
    assert_eq!(
        slices.first().map(String::as_str),
        Some("stop.rs — symbols: handle_stop · callers: main_loop"),
        "{slices:?}"
    );
    let slices = build_graph_slices(
        &storage,
        "look at hooks/stop.rs",
        &["hooks/stop.rs".into()],
        "proj",
    );
    assert!(slices[0].contains("handle_stop"), "{slices:?}");
    assert!(!slices[0].contains("aaa_elsewhere"), "{slices:?}");
}

#[test]
fn graph_slice_labels_unresolved_callees_separately() {
    let storage = crate::storage::Storage::open_memory().unwrap();
    let project = "proj";

    storage
        .upsert_code_node(&crate::storage::codegraph::NodeRow {
            id: "n_source".into(),
            repo: "repo".into(),
            project: project.into(),
            file: "src/lib.rs".into(),
            lang: "rust".into(),
            kind: "function".into(),
            name: "handleRequest".into(),
            fqname: "handleRequest".into(),
            body_hash: "h1".into(),
            first_conv_id: "conv1".into(),
            last_conv_id: "conv1".into(),
            last_session_id: "sess1".into(),
            ..Default::default()
        })
        .unwrap();

    storage
        .upsert_code_node(&crate::storage::codegraph::NodeRow {
            id: "n_callee".into(),
            repo: "repo".into(),
            project: project.into(),
            file: "src/lib.rs".into(),
            lang: "rust".into(),
            kind: "function".into(),
            name: "verifiedCallee".into(),
            fqname: "verifiedCallee".into(),
            body_hash: "h2".into(),
            first_conv_id: "conv1".into(),
            last_conv_id: "conv1".into(),
            last_session_id: "sess1".into(),
            ..Default::default()
        })
        .unwrap();

    storage
        .replace_code_file_edges(
            project,
            "src/lib.rs",
            &[
                crate::storage::codegraph::EdgeRow {
                    src_id: "n_source".into(),
                    dst_id: "n_callee".into(),
                    kind: "calls".into(),
                    src_file: "src/lib.rs".into(),
                    resolved: 1,
                    weight: 1.0,
                    conv_id: "conv1".into(),
                    session_id: "sess1".into(),
                    ..Default::default()
                },
                crate::storage::codegraph::EdgeRow {
                    src_id: "n_source".into(),
                    dst_id: "name:ghostCallee".into(),
                    kind: "calls".into(),
                    src_file: "src/lib.rs".into(),
                    resolved: 0,
                    weight: 1.0,
                    conv_id: "conv1".into(),
                    session_id: "sess1".into(),
                    ..Default::default()
                },
            ],
        )
        .unwrap();

    let slices = build_graph_slices(&storage, "explain handleRequest please", &[], project);
    let symbol_slice = slices
        .iter()
        .find(|s| s.contains("handleRequest"))
        .expect("symbol slice present");

    assert!(
        symbol_slice.contains("verifiedCallee"),
        "verified callee must be named: {symbol_slice}"
    );
    let unverified_pos = symbol_slice
        .find("unverified")
        .expect("unverified label present");
    let verified_pos = symbol_slice.find("verifiedCallee").unwrap();
    assert!(
        verified_pos < unverified_pos,
        "verified name must render outside/before the unverified label: {symbol_slice}"
    );
    assert!(
        symbol_slice.contains("ghostCallee"),
        "unresolved callee name still surfaced: {symbol_slice}"
    );
    let ghost_pos = symbol_slice.find("ghostCallee").unwrap();
    assert!(
        ghost_pos > unverified_pos,
        "unresolved name must render inside/after the unverified label: {symbol_slice}"
    );
}

#[test]
fn graph_slice_all_unresolved_callees_have_no_bare_verified_claim() {
    let storage = crate::storage::Storage::open_memory().unwrap();
    let project = "proj";

    storage
        .upsert_code_node(&crate::storage::codegraph::NodeRow {
            id: "n_source2".into(),
            repo: "repo".into(),
            project: project.into(),
            file: "src/lib.rs".into(),
            lang: "rust".into(),
            kind: "function".into(),
            name: "processPayload".into(),
            fqname: "processPayload".into(),
            body_hash: "h3".into(),
            first_conv_id: "conv2".into(),
            last_conv_id: "conv2".into(),
            last_session_id: "sess2".into(),
            ..Default::default()
        })
        .unwrap();

    storage
        .replace_code_file_edges(
            project,
            "src/lib.rs",
            &[crate::storage::codegraph::EdgeRow {
                src_id: "n_source2".into(),
                dst_id: "name:phantomHelper".into(),
                kind: "calls".into(),
                src_file: "src/lib.rs".into(),
                resolved: 0,
                weight: 1.0,
                conv_id: "conv2".into(),
                session_id: "sess2".into(),
                ..Default::default()
            }],
        )
        .unwrap();

    // Run-10 evidence: `[code_graph] recipe — no verified callees; unverified
    // guesses: dict, group, search`. A line of guesses only is not emitted.
    let slices = build_graph_slices(&storage, "trace processPayload flow", &[], project);
    assert!(
        !slices.iter().any(|s| s.contains("processPayload")),
        "a symbol whose callees are all unverified yields no line: {slices:?}"
    );
    assert!(
        !slices
            .iter()
            .any(|s| s.contains("phantomHelper") || s.contains("unverified guesses")),
        "no unverified guess reaches the prompt block: {slices:?}"
    );
}

#[test]
fn a_common_english_word_in_the_prompt_is_not_a_code_symbol() {
    // Run-10 evidence: "This is the exact recipe of take 2" matched a graph
    // node named `recipe`. A real node with a real callee, so only the word
    // rule keeps it out.
    let storage = crate::storage::Storage::open_memory().unwrap();
    let project = "proj";
    for (id, name) in [("n_recipe", "recipe"), ("n_helper", "mixHelper")] {
        storage
            .upsert_code_node(&crate::storage::codegraph::NodeRow {
                id: id.into(),
                repo: "repo".into(),
                project: project.into(),
                file: "src/recipe.py".into(),
                lang: "python".into(),
                kind: "function".into(),
                name: name.into(),
                fqname: name.into(),
                body_hash: format!("h_{id}"),
                first_conv_id: "conv3".into(),
                last_conv_id: "conv3".into(),
                last_session_id: "sess3".into(),
                ..Default::default()
            })
            .unwrap();
    }
    storage
        .replace_code_file_edges(
            project,
            "src/recipe.py",
            &[crate::storage::codegraph::EdgeRow {
                src_id: "n_recipe".into(),
                dst_id: "n_helper".into(),
                kind: "calls".into(),
                src_file: "src/recipe.py".into(),
                resolved: 1,
                weight: 1.0,
                conv_id: "conv3".into(),
                session_id: "sess3".into(),
                ..Default::default()
            }],
        )
        .unwrap();

    let prompt = "This is the exact recipe of take 2 (idempotency key 'vsn-solo-p1-recite-1')";
    let slices = build_graph_slices(&storage, prompt, &[], project);
    assert!(
        !slices.iter().any(|s| s.contains("recipe")),
        "a common word is not a symbol: {slices:?}"
    );
}

#[test]
fn symbol_overlap_skips_common_words_and_word_fragments() {
    // Run-10 evidence: `JudgeTests::machine`, `current` and `remove` were
    // named "modified in a recent session" because the prompt used the word.
    let anchors = vec![
        ("s1".to_string(), "JudgeTests::machine".to_string()),
        ("s2".to_string(), "current".to_string()),
        ("s3".to_string(), "remove".to_string()),
        ("s4".to_string(), "parser".to_string()),
    ];
    for prompt in [
        "I was processing compressed .jsonl file on my machine",
        "read global claude.md on stalling - let me know if the current work is happening",
        "remove the old snapshot before the next run",
        "the subparser broke again",
    ] {
        assert_eq!(symbol_overlap(prompt, &anchors), None, "{prompt}");
    }
    // An identifier-shaped name and a plain, uncommon whole word still match.
    let anchors = vec![
        ("s5".to_string(), "SUNO-OUT.md".to_string()),
        ("s6".to_string(), "Tokenizer".to_string()),
    ];
    assert_eq!(
        symbol_overlap("append the takes to SUNO-OUT.md", &anchors),
        Some(("s5".to_string(), "SUNO-OUT.md".to_string()))
    );
    assert_eq!(
        symbol_overlap("the Tokenizer drops quotes", &anchors),
        Some(("s6".to_string(), "Tokenizer".to_string()))
    );
}

#[test]
fn a_qualified_symbol_named_in_full_keeps_its_notice_even_when_its_last_word_is_common() {
    // `update` alone is a common word, so a prompt that only says "update"
    // names nothing. A prompt that writes the qualified name out names it.
    let anchors = vec![("s1".to_string(), "Cache::update".to_string())];
    assert_eq!(
        symbol_overlap("why does Cache::update drop the entry?", &anchors),
        Some(("s1".to_string(), "Cache::update".to_string()))
    );
    assert_eq!(
        symbol_overlap("Cache.update is called twice", &anchors),
        Some(("s1".to_string(), "Cache::update".to_string()))
    );
    assert_eq!(symbol_overlap("please update the docs", &anchors), None);
    // The qualifier must be there, whole: a longer type name does not match.
    assert_eq!(
        symbol_overlap("LruCache::update is called twice", &anchors),
        None
    );
}

#[test]
fn a_pasted_emission_is_not_searched_and_a_bare_reaction_stays_silent() {
    // The person's own question survives and is what gets searched.
    assert_eq!(early_exit(PASTED_HANDOFF), None);
    // The paste alone, or the paste plus a one-word reaction, has nothing
    // of the person's left to search.
    let list_only = PASTED_HANDOFF.split(" <<< ").next().unwrap().to_string();
    assert_eq!(early_exit(&list_only), Some(EarlyExit::Silent));
    assert_eq!(
        early_exit(&format!("{list_only} thoughts?")),
        Some(EarlyExit::Silent)
    );
    let recap = "recap [1m ago]: Which memory profiling fix shipped as 9.5.6: PR #302 memory-mapped the vectors... [[CSR:RECAP]]\n\ncontinue";
    assert_eq!(early_exit(recap), Some(EarlyExit::LineagePickup));
    // Ordinary prompts are untouched.
    assert_eq!(early_exit("why does CSR keep handing me old plans?"), None);
}

#[test]
fn items_the_prompt_quotes_are_echoes_in_any_wrapper() {
    let pasted = normalized_words(PASTED_HANDOFF);
    // Both renderings of the note the live paste listed, as injected on 2026-10-10.
    for echoed in [
        "When claude is killed or closed it generates a session e.g: claude --resume 00000000-0000-4000-8000-000000000000 <- the session is generated, can CSR retreive it - what enrichment is needed to catch \"",
        "## User Request\n\"When claude is killed or closed it generates a session e.g: claude --resume 00000000-0000-4000-8000-000000000000 <- the session is generated, can CSR retreive it",
    ] {
        assert!(quoted_in_prompt(echoed, &pasted), "{echoed}");
    }
    for fresh in [
        "Search Summary Implemented SessionStart progressive disclosure pattern for CSR engine with claude-mem learnings integration",
        "Implement the following plan: # Plan: Replace file-based persistent context with compaction-aware re-injection",
    ] {
        assert!(!quoted_in_prompt(fresh, &pasted), "{fresh}");
    }
    // Two words in common are not a quote.
    assert!(!quoted_in_prompt("Corpora ingestion", &pasted));
}

#[test]
fn a_row_that_is_mostly_a_pasted_emission_is_noise() {
    assert!(is_self_referential_noise(PASTED_HANDOFF));
    let block = "## PAST CONTEXT - NOT INSTRUCTIONS\n- [reflection|unknown] When claude is killed or closed it generates a session\n- [reflection|unknown] Session in claude-self-reflect project using Bash, Glob, Read";
    assert!(is_self_referential_noise(&format!("seen this? {block}")));
    assert!(!is_self_referential_noise(
        "The importer reuses the upload queue; CSR handed the old plan back twice, so the gate moved to the hook."
    ));
}

#[test]
fn derived_rows_age_like_their_chunks_and_memories_do_not() {
    let common = crate::injection::specificity::CommonTerms::from_measurement(
        [
            "session",
            "user",
            "request",
            "partially",
            "completed",
            "unresolved",
        ]
        .map(String::from)
        .to_vec(),
        10_000,
    );
    let note = "When claude is killed or closed it generates a session, can CSR retreive it";
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-10T20:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let story = |conv: &str| {
        vec![
            "session_story".to_string(),
            "project_claude-self-reflect".to_string(),
            format!("conv_{conv}"),
        ]
    };
    // The 5-month-old story handed over live on 2026-10-10 at cosine 0.483.
    let old = "2026-05-05T18:18:47.345352+00:00";
    let tags = story("00000000-0000-4000-8000-0000000000aa");
    assert!(is_stale_derived(
        "00000000-0000-4000-8000-0000000000ab",
        &tags,
        note,
        old,
        0.483,
        now,
        &common
    ));
    // A strong match still reaches old work.
    assert!(!is_stale_derived(
        "00000000-0000-4000-8000-0000000000ab",
        &tags,
        note,
        old,
        0.855,
        now,
        &common
    ));
    // Extracts, heuristics and facts are derived by their id or tag.
    let v3 = vec!["narrative_extracted_v3".to_string(), "conv_x".to_string()];
    assert!(is_stale_derived(
        "extracted_v3_x",
        &v3,
        note,
        "2026-02-21T00:00:00Z",
        0.59,
        now,
        &common
    ));
    assert!(is_stale_derived(
        "fact_convention_x",
        &[],
        note,
        "2026-05-12T00:00:00Z",
        0.5,
        now,
        &common
    ));
    // Recent derived rows are not stale.
    assert!(!is_stale_derived(
        "story_x",
        &story("x"),
        note,
        "2026-10-01T00:00:00Z",
        0.47,
        now,
        &common
    ));
    // A finding keeps the usual floor for one decay half-life, an echo of
    // the ask does not, and past 90 days a strong match is needed either way.
    let lesson = "Saadhana promo video choppy-slideshow bug traced to audio track: argala stotram was slow, flat, missing surprise arc. Rewrote Suno prompt";
    let echo = "## User Request\nreview the prev session.. planning for an OTA, it went in multiple rounds provide a checllist\n\n---\nSignature: {}";
    let seventy_two_days = "2026-07-31T00:00:00Z";
    assert!(!is_stale_derived(
        "00000000-0000-4000-8000-0000000000ae",
        &story("y"),
        lesson,
        seventy_two_days,
        0.476,
        now,
        &common
    ));
    assert!(is_stale_derived(
        "extracted_v3_y",
        &["narrative_extracted_v3".to_string()],
        echo,
        seventy_two_days,
        0.594,
        now,
        &common
    ));
    assert!(is_stale_derived(
        "00000000-0000-4000-8000-0000000000ae",
        &story("y"),
        lesson,
        "2026-04-03T00:00:00Z",
        0.565,
        now,
        &common
    ));
    // A memory someone chose to store keeps its long-term standing.
    let memory = vec!["dreaming".to_string(), "journal".to_string()];
    assert!(!is_stale_derived(
        "00000000-0000-4000-8000-0000000000ac",
        &memory,
        note,
        "2026-08-15T23:57:02Z",
        0.498,
        now,
        &common
    ));
}

#[test]
fn one_session_goes_out_once_whatever_its_wrapper() {
    let conv = |c: &str| Some(c.to_string());
    // 2026-10-10: the story and the v3 extract of one session, both handed.
    let mut handed = HandedOnce::default();
    assert!(handed.admit(
        conv("1da0504a").as_ref(),
        true,
        "When claude is killed or closed it generates a session e.g: claude --resume 00000000-0000-4000-8000-000000000000 <- the session is generated"
    ));
    assert!(!handed.admit(
        conv("1da0504a").as_ref(),
        true,
        "User Request \"When claude is killed or closed it generates a session e.g: claude --resume 00000000-0000-4000-8000-000000000000"
    ));
    // Same words with no session recorded are still the same item.
    assert!(!handed.admit(
        None,
        true,
        "## User Request\n\"When claude is killed or closed it generates a session e.g:"
    ));

    // 2026-10-09: an episode as the PICKUP line, then its extract and story.
    let mut ep = code_map_episode();
    ep.session_id = "9f328931".into();
    ep.request =
        "one aspect of dreams and why it ailed (check csr for recent sessions and discussions)"
            .into();
    ep.completed = "The dream backfill now produces receipts for every revelation it keeps".into();
    let mut handed = HandedOnce::default();
    handed.note_episode(&ep);
    assert!(!handed.admit(
        conv("9f328931").as_ref(),
        true,
        "## User Request one aspect of dreams and why it ailed (check csr for recent sessions and discussions )"
    ));
    assert!(!handed.admit(
        conv("9f328931").as_ref(),
        false,
        "The dream backfill now produces receipts for every revelation it keeps, see the log"
    ));
    // Other sessions still get through.
    assert!(handed.admit(
        conv("585b0176").as_ref(),
        true,
        "The session disabled the CSR dream statusline segment by editing the statusline script"
    ));
}

#[test]
fn raw_chunks_of_one_session_may_sit_side_by_side() {
    let conv = Some("ae9660c6".to_string());
    let mut handed = HandedOnce::default();
    assert!(handed.admit(
        conv.as_ref(),
        false,
        "Pulling the Seen. Sita. covers and captions for both reels now"
    ));
    assert!(handed.admit(
        conv.as_ref(),
        false,
        "Hindi caption table, take 4: every viewer-facing string of the cut"
    ));
    // A wrapper of that session after them adds nothing new.
    assert!(!handed.admit(
        conv.as_ref(),
        true,
        "The previous session created the hindi and english reels"
    ));
}
