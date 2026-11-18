use super::*;

// --- conclusion_preview tests ---

#[test]
fn test_conclusion_preview_short_passthrough() {
    assert_eq!(
        conclusion_preview("Done. Fixed the bug.", 120),
        "Done. Fixed the bug."
    );
}

#[test]
fn test_conclusion_preview_keeps_both_ends() {
    // updated: claim-centric extraction replaces head/tail elision —
    // first sentence is the verdict; when short, append the distinct last
    // sentence (filler middle is dropped, not mid-cut).
    let text = format!(
        "Done. Answers to both interjections first. {} Final verdict: reimport confirmed, 52754 chunks live.",
        "filler sentence about intermediate steps. ".repeat(10)
    );
    let result = conclusion_preview(&text, 120);
    assert!(result.len() <= 120, "over budget: {result}");
    assert!(
        result.starts_with("Done."),
        "verdict (first sentence) lost: {result}"
    );
    assert!(
        result.contains("52754 chunks live."),
        "closing-state sentence lost: {result}"
    );
    assert!(
        result.contains(" … "),
        "expected verdict + ellipsis + last: {result}"
    );
    assert!(
        !result.contains("filler sentence"),
        "middle filler should be elided: {result}"
    );
}

#[test]
fn test_conclusion_preview_unicode_safe() {
    // updated: claim-centric falls back to word/char-boundary truncate when
    // there is no sentence structure; must not panic on non-ASCII.
    let content = "\u{1f600}".repeat(60);
    let result = conclusion_preview(&content, 20);
    assert!(result.chars().count() <= 24);
    assert!(result.ends_with("..."));
}

#[test]
fn test_conclusion_preview_restart_installed_no_table_garble() {
    // Real failure: head/tail elision mid-cut the verdict and glued a
    // markdown table fragment onto LAST. Claim-centric must keep the bold
    // restart sentence + installed closing state, never table pipes.
    let content = "**Yes — restart Claude Code now.** New binary installed at \
             /usr/local/bin/csr-engine (rm+cp, dodged codesign SIGKILL gotcha)...\n\n\
             | step | result |\n|------|--------|\n| build | ok |\n";
    let result = conclusion_preview(content, 120);
    assert!(
        result.contains("restart Claude Code"),
        "verdict lost: {result}"
    );
    assert!(
        result.contains("New binary installed"),
        "closing state lost: {result}"
    );
    assert!(
        !result.contains('|'),
        "table fragment leaked into LAST: {result}"
    );
    assert!(
        !result.contains("n start inject"),
        "old mid-word garble pattern: {result}"
    );
    assert!(
        !result.contains("**"),
        "bold markers should be stripped: {result}"
    );
}

#[test]
fn test_conclusion_preview_strips_bold_markers() {
    let content = "**Done, all tests pass.** Some other trailing sentence here.";
    // Force claim-centric path (overflow) so bold span is extracted.
    let result = conclusion_preview(content, 40);
    assert!(
        !result.contains("**"),
        "bold markers must be stripped: {result}"
    );
    assert!(
        result.contains("Done, all tests pass"),
        "bold inner text lost: {result}"
    );
}

#[test]
fn test_conclusion_preview_word_boundary_no_punctuation() {
    // No `.`/`!`/`?` — fall back to word-boundary truncate, never mid-word.
    let content = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima \
             mike november oscar papa quebec romeo sierra tango uniform victor whiskey";
    let result = conclusion_preview(content, 50);
    assert!(
        result.ends_with("..."),
        "expected truncate suffix: {result}"
    );
    assert!(result.len() <= 50, "over budget: {result}");
    // Everything before "..." must be a clean prefix ending on a full word.
    let body = result.trim_end_matches("...");
    assert!(
        !body.ends_with(char::is_alphanumeric) || content.starts_with(body),
        "truncated body must be a word-aligned prefix: {result}"
    );
    assert!(
        content.starts_with(body.trim_end()),
        "must not invent/glue text: {result}"
    );
    // No mid-word cut: either body is empty, or next char in source is space/EOS.
    let trimmed = body.trim_end();
    if !trimmed.is_empty() {
        let next = content.as_bytes().get(trimmed.len()).copied();
        assert!(
            next.is_none_or(|b| b == b' '),
            "mid-word cut at {}: {result}",
            trimmed.len()
        );
    }
}

// --- strip_preamble tests (Bug 4) ---

#[test]
fn test_strip_preamble_implement() {
    assert_eq!(
        strip_preamble("Implement the following plan: Fix the timeline"),
        "Fix the timeline"
    );
}

#[test]
fn test_strip_preamble_execute() {
    assert_eq!(
        strip_preamble("Execute the following plan: Phase 5 work"),
        "Phase 5 work"
    );
}

#[test]
fn test_strip_preamble_markdown_header() {
    assert_eq!(
        strip_preamble("## Phase 3 Implementation"),
        "Phase 3 Implementation"
    );
}

#[test]
fn test_strip_preamble_no_match() {
    assert_eq!(strip_preamble("Normal text here"), "Normal text here");
}

#[test]
fn test_strip_preamble_prefix_then_header() {
    assert_eq!(
        strip_preamble("Implement the following plan: # Big Plan"),
        "Big Plan"
    );
}

// --- is_displayable tests (threshold=6 or has enrichment) ---

#[test]
fn test_is_meta_session_probe_run() {
    // Round-5 regression: a /memory-feedback probe run must not surface as
    // CONTINUED FROM or RECENT SESSIONS. Its summary is the probe command.
    let session = SessionInfo {
        conversation_id: "probe".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-06-10T10:00:00Z".to_string(),
        total_messages: 8,
        chunk_count: 2,
        summary: Some(
            "<command-message>memory-feedback</command-message> CSR Memory Feedback \
                 Probe You are reporting on the quality of the memory context"
                .to_string(),
        ),
        enrichment: None,
    };
    assert!(is_meta_session(&session));
}

#[test]
fn test_is_meta_session_real_work_is_not_meta() {
    let session = SessionInfo {
        conversation_id: "real".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-06-10T10:00:00Z".to_string(),
        total_messages: 30,
        chunk_count: 5,
        summary: Some("fix the briefing staleness bug in session_briefing.rs".to_string()),
        enrichment: None,
    };
    assert!(!is_meta_session(&session));
}

#[test]
fn test_is_meta_session_no_summary_not_meta() {
    let session = SessionInfo {
        conversation_id: "nosum".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-06-10T10:00:00Z".to_string(),
        total_messages: 30,
        chunk_count: 5,
        summary: None,
        enrichment: Some("Edited src/main.rs".to_string()),
    };
    assert!(!is_meta_session(&session));
}

#[test]
fn test_story_leads_with_question_filtered() {
    // Round-6 regression: story-path "Past session" lines led with the raw
    // opening question + log noise instead of narrating work.
    assert!(story_leads_with_question(
        "What were we discussing recently?\nFixed some things."
    ));
    assert!(story_leads_with_question(
        "typng this on console produces this why? csr-engine\nCSR startup: 92ms"
    ));
    // A declarative work story is kept.
    assert!(!story_leads_with_question(
        "Fixed self-pollution across injection paths. Added provenance module."
    ));
}

#[test]
fn test_weak_anchor_recall_question_no_work_title() {
    // Round-6 regression: "What were we discussing recently?" is a real
    // prompt (passes provenance) but a bare question with no work title.
    let session = SessionInfo {
        conversation_id: "recall".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-06-11T10:00:00Z".to_string(),
        total_messages: 30,
        chunk_count: 5,
        summary: Some("What were we discussing recently?".to_string()),
        enrichment: None,
    };
    assert!(is_weak_continuity_anchor(&session));
}

#[test]
fn test_weak_anchor_typo_debug_question() {
    let session = SessionInfo {
        conversation_id: "typo".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-06-11T10:00:00Z".to_string(),
        total_messages: 8,
        chunk_count: 2,
        summary: Some("typng this on console produces this why? csr-engine".to_string()),
        enrichment: None,
    };
    assert!(is_weak_continuity_anchor(&session));
}

#[test]
fn test_strong_anchor_imperative_prompt() {
    // Imperative work request is a fine anchor even without enrichment.
    let session = SessionInfo {
        conversation_id: "work".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-06-11T10:00:00Z".to_string(),
        total_messages: 30,
        chunk_count: 5,
        summary: Some("fix the auth bug in login.rs".to_string()),
        enrichment: None,
    };
    assert!(!is_weak_continuity_anchor(&session));
}

#[test]
fn test_weak_anchor_v3_user_request_is_question() {
    // The fc16f91d bug: V3 enrichment exists, but its "## User Request" is
    // the user's opening question, which session_title echoes as the title.
    // Enrichment-present must not imply work-titled.
    let session = SessionInfo {
        conversation_id: "v3q".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-06-11T10:00:00Z".to_string(),
        total_messages: 30,
        chunk_count: 8,
        summary: Some("What were we discussing recently?".to_string()),
        enrichment: Some(
            "## User Request\nWhat were we discussing recently?\n\n## Search Summary\nstuff"
                .to_string(),
        ),
    };
    assert!(is_weak_continuity_anchor(&session));
}

#[test]
fn test_strong_anchor_question_but_has_work_title() {
    // A question opener but enrichment shows real edits → work title, keep.
    let session = SessionInfo {
        conversation_id: "qwork".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-06-11T10:00:00Z".to_string(),
        total_messages: 30,
        chunk_count: 5,
        summary: Some("why is the build failing?".to_string()),
        enrichment: Some(
            "[Heuristic] Project: test\nTools: Edit, Read\nFiles: main.rs, lib.rs".to_string(),
        ),
    };
    assert!(!is_weak_continuity_anchor(&session));
}

#[test]
fn test_is_displayable_good_session() {
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 50,
        chunk_count: 3,
        summary: Some("Did some work".to_string()),
        enrichment: None,
    };
    assert!(is_displayable(&session));
}

#[test]
fn test_is_displayable_too_few_messages() {
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 1,
        chunk_count: 1,
        summary: Some("Short".to_string()),
        enrichment: None,
    };
    assert!(!is_displayable(&session));
}

#[test]
fn test_is_displayable_five_messages_no_enrichment() {
    // 5 messages is below threshold (6) and no enrichment → not displayable
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 5,
        chunk_count: 2,
        summary: Some("Quick chat".to_string()),
        enrichment: None,
    };
    assert!(!is_displayable(&session));
}

#[test]
fn test_is_displayable_six_messages_with_summary() {
    // 6 messages meets threshold → displayable if has summary
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 6,
        chunk_count: 2,
        summary: Some("Debugging session".to_string()),
        enrichment: None,
    };
    assert!(is_displayable(&session));
}

#[test]
fn test_is_displayable_empty_summary_no_enrichment() {
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 50,
        chunk_count: 3,
        summary: Some("   ".to_string()),
        enrichment: None,
    };
    assert!(!is_displayable(&session));
}

#[test]
fn test_is_displayable_enrichment_only() {
    // Enrichment present → always displayable regardless of message count
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 3,
        chunk_count: 1,
        summary: None,
        enrichment: Some("[Heuristic] Project: test\nTools: Edit".to_string()),
    };
    assert!(is_displayable(&session));
}

#[test]
fn test_is_displayable_enriched_micro_session_filtered() {
    // A single prompt/reply exchange (e.g. a manual `claude -p "say exactly: ..."`
    // test invocation) gets heuristic enrichment like any session, but injecting
    // it as a "past session" is self-referential noise — enrichment alone must
    // not rescue a session below the micro threshold.
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 2,
        chunk_count: 1,
        summary: None,
        enrichment: Some("[Heuristic] Project: test\nTools: ".to_string()),
    };
    assert!(!is_displayable(&session));
}

// --- session_title tests ---

#[test]
fn test_session_title_strips_preamble_no_enrichment() {
    // When no enrichment, falls back to summary with preamble stripped
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 50,
        chunk_count: 2,
        summary: Some("Implement the following plan: ## Fix the timeline bugs".to_string()),
        enrichment: None,
    };
    let title = session_title(&session);
    assert!(!title.contains("Implement the following plan"));
    assert!(title.contains("Fix the timeline bugs"));
}

#[test]
fn test_session_title_prefers_enrichment_over_summary() {
    // When enrichment has v3 summary, use it instead of raw prompt
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 50,
        chunk_count: 2,
        summary: Some("Please fix the auth bug and make it work".to_string()),
        enrichment: Some(
            "## Search Summary\nFixed authentication timeout by increasing JWT expiry\n\n## Other"
                .to_string(),
        ),
    };
    let title = session_title(&session);
    assert!(title.contains("authentication timeout"));
    assert!(!title.contains("Please fix"));
}

#[test]
fn test_session_title_caveat_detected_as_noise() {
    // Caveat text from /clear commands should be detected as noise
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 10,
        chunk_count: 2,
        summary: Some("<local-command-caveat>Caveat: The messages below were generated by the user while running local commands</local-command-caveat>".to_string()),
        enrichment: None,
    };
    let title = session_title(&session);
    assert!(!title.contains('<'));
    assert!(!title.contains("Caveat"));
    assert_eq!(title, "(session)");
}

#[test]
fn test_session_title_caveat_with_enrichment_fallback() {
    // When caveat noise is detected, fall back to enrichment-based title
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 10,
        chunk_count: 2,
        summary: Some(
            "<local-command-caveat>Caveat: The messages below</local-command-caveat>".to_string(),
        ),
        enrichment: Some(
            "[Heuristic] Project: test\nTools: Edit, Read\nFiles: main.rs, lib.rs".to_string(),
        ),
    };
    let title = session_title(&session);
    assert!(title.contains("Edited"));
    assert!(title.contains("main.rs"));
}

#[test]
fn test_session_title_no_summary() {
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 10,
        chunk_count: 2,
        summary: None,
        enrichment: None,
    };
    assert_eq!(session_title(&session), "(no summary)");
}

#[test]
fn test_session_title_system_reminder_with_enrichment() {
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 20,
        chunk_count: 3,
        summary: Some(
            "<system-reminder>Note: this session was started from context</system-reminder>"
                .to_string(),
        ),
        enrichment: Some("[Heuristic] Project: test\nTools: Bash, Read".to_string()),
    };
    let title = session_title(&session);
    // Enrichment is now preferred over summary (not just a fallback)
    assert!(title.contains("Session using"));
    assert!(title.contains("Bash"));
}

// --- enrichment parsing + inference tests (Bug 6) ---

#[test]
fn test_parse_enrichment_full() {
    let enrichment = "[Heuristic] Project: csr\nMessages: 603 (227 user)\nTools: TaskCreate, Edit, Bash, Read\nFiles: mod.rs, engine.rs\nHad errors: yes";
    let fields = parse_enrichment(enrichment);
    assert_eq!(fields.tools, vec!["TaskCreate", "Edit", "Bash", "Read"]);
    assert_eq!(fields.files, vec!["mod.rs", "engine.rs"]);
    assert!(fields.has_errors);
    assert!(fields.has_edit_tool);
}

#[test]
fn test_parse_enrichment_no_errors() {
    let enrichment =
        "[Heuristic] Project: csr\nMessages: 100 (50 user)\nTools: Read, Grep\nFiles: main.rs";
    let fields = parse_enrichment(enrichment);
    assert!(!fields.has_errors);
    assert!(!fields.has_edit_tool);
}

#[test]
fn test_infer_from_enrichment_errors_with_edit() {
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 100,
        chunk_count: 5,
        summary: None,
        enrichment: Some(
            "[Heuristic] Project: test\nTools: Edit, Bash\nFiles: main.rs\nHad errors: yes"
                .to_string(),
        ),
    };
    let result = infer_next_action_from_session(&session);
    assert!(result.contains("Fix errors"));
    assert!(result.contains("edits were in progress"));
}

#[test]
fn test_infer_from_enrichment_errors_no_edit() {
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 100,
        chunk_count: 5,
        summary: None,
        enrichment: Some(
            "[Heuristic] Project: test\nTools: Read, Grep\nHad errors: yes".to_string(),
        ),
    };
    let result = infer_next_action_from_session(&session);
    assert!(result.contains("Investigate and fix errors"));
}

#[test]
fn test_infer_from_enrichment_files() {
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 50,
        chunk_count: 3,
        summary: None,
        enrichment: Some(
            "[Heuristic] Project: test\nTools: Edit, Read\nFiles: session_start.rs, queries.rs"
                .to_string(),
        ),
    };
    let result = infer_next_action_from_session(&session);
    assert!(result.contains("Continue work on"));
    assert!(result.contains("session_start.rs"));
}

#[test]
fn test_infer_from_enrichment_large_session() {
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 500,
        chunk_count: 10,
        summary: None,
        enrichment: Some(
            "[Heuristic] Project: test\nMessages: 500 (200 user)\nTools: Read, Grep".to_string(),
        ),
    };
    let result = infer_next_action_from_session(&session);
    assert!(result.contains("Large session"));
}

#[test]
fn test_infer_from_enrichment_fallback_to_keyword() {
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: "2026-02-15T10:00:00Z".to_string(),
        total_messages: 50,
        chunk_count: 3,
        summary: Some("Phase 4 implementation".to_string()),
        enrichment: None,
    };
    let result = infer_next_action_from_session(&session);
    assert!(result.contains("Phase 4"));
}

// --- enrichment_display tests ---

#[test]
fn test_enrichment_display_full() {
    let enrichment = "[Heuristic] Project: csr\nTools: Edit, Bash, Read\nFiles: mod.rs, engine.rs\nHad errors: yes";
    let display = enrichment_display(enrichment).unwrap();
    assert!(display.contains("Tools: Edit, Bash, Read"));
    assert!(display.contains("Files: mod.rs, engine.rs"));
    assert!(display.contains("Had errors"));
}

#[test]
fn test_enrichment_display_no_structured_data() {
    let enrichment = "[Heuristic] Project: csr\nMessages: 10 (5 user)";
    assert!(enrichment_display(enrichment).is_none());
}

#[test]
fn test_enrichment_display_v3_format() {
    let enrichment = "```markdown\n## Search Summary\nImplemented Phase 4 code-aware search with tree-sitter AST.\n\n## Problem-Solution Mapping\n**Request**: stuff";
    let display = enrichment_display(enrichment).unwrap();
    assert!(display.contains("Implemented Phase 4"));
    assert!(display.contains("tree-sitter"));
}

#[test]
fn test_enrichment_display_v3_long_truncates() {
    let long_summary = "A".repeat(120);
    let enrichment = format!("## Search Summary\n{long_summary}\n\n## Other");
    let display = enrichment_display(&enrichment).unwrap();
    assert!(display.ends_with("..."));
    assert!(display.len() <= 85); // 80 + "..."
}

#[test]
fn test_extract_v3_summary_empty() {
    assert!(extract_v3_summary("## Search Summary\n\n## Other").is_none());
}

#[test]
fn test_extract_v3_summary_user_request() {
    let enrichment = "## User Request\n\"Fix the session start hook bugs\"\n\"Review the injection output\"\n\n## Solution Pattern\ncreation: file.md";
    let display = extract_v3_summary(enrichment).unwrap();
    assert!(display.contains("Fix the session start hook bugs"));
}

#[test]
fn test_enrichment_display_v3_user_request_fallback() {
    // When v3 has no Search Summary, falls back to User Request
    let enrichment =
        "## User Request\n\"Implement the new feature\"\n\n## Solution Pattern\ncreation: file.md";
    let display = enrichment_display(enrichment).unwrap();
    assert!(display.contains("Implement the new feature"));
}

// --- format_session_line tests ---

#[test]
fn test_format_session_line_with_enrichment() {
    let now = Utc::now();
    let ts = (now - chrono::Duration::hours(2))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: ts,
        total_messages: 293,
        chunk_count: 6,
        summary: Some("Some work".to_string()),
        enrichment: Some(
            "[Heuristic] Project: test\nTools: Edit, Read\nFiles: main.rs".to_string(),
        ),
    };
    let line = format_session_line(&session, &now);
    assert!(line.contains("2h ago"));
    assert!(line.contains("293 msgs"));
    assert!(line.contains("Tools: Edit, Read"));
    assert!(line.contains("Files: main.rs"));
}

#[test]
fn test_format_session_line_summary_fallback() {
    let now = Utc::now();
    let ts = (now - chrono::Duration::days(1))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: ts,
        total_messages: 90,
        chunk_count: 2,
        summary: Some("Phase 3 HNSW persistence work".to_string()),
        enrichment: None,
    };
    let line = format_session_line(&session, &now);
    assert!(line.contains("yesterday"));
    assert!(line.contains("90 msgs"));
    assert!(line.contains("Phase 3 HNSW persistence work"));
}

#[test]
fn test_format_session_line_strips_preamble() {
    let now = Utc::now();
    let ts = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let session = SessionInfo {
        conversation_id: "abc".to_string(),
        project_name: "test".to_string(),
        timestamp: ts,
        total_messages: 50,
        chunk_count: 2,
        summary: Some("Implement the following plan: Fix the bugs".to_string()),
        enrichment: None,
    };
    let line = format_session_line(&session, &now);
    assert!(line.contains("Fix the bugs"));
    assert!(!line.contains("Implement the following plan"));
}

// --- ContinuedSession formatting tests ---

#[test]
fn test_continued_session_format_with_enrichment() {
    let cont = ContinuedSession {
        conversation_id: "abc-123".to_string(),
        age_label: "12m ago".to_string(),
        title: "Seedance API video generation".to_string(),
        enrichment_line: "Tools: Edit, Bash | Files: api/seedance.ts, lib/video.ts".to_string(),
        raw_enrichment: Some(
            "[Heuristic] Project: test\nTools: Edit, Bash\nFiles: api/seedance.ts, lib/video.ts"
                .to_string(),
        ),
        last_working_on: Some("editing api/seedance.ts, lib/video.ts".to_string()),
    };
    let header = cont.format_header();
    assert!(header.contains("SESSION CONTINUITY DETECTED"));
    assert!(header.contains("CONTINUED FROM [12m ago]"));
    assert!(header.contains("Seedance API video generation"));
    assert!(header.contains("Tools: Edit, Bash"));
    assert!(header.contains("Last working on: editing"));
    // Suggested line should NOT be present (removed as noise)
    assert!(!header.contains("Suggested:"));
}

#[test]
fn test_continued_session_format_minimal() {
    let cont = ContinuedSession {
        conversation_id: "xyz".to_string(),
        age_label: "5m ago".to_string(),
        title: "Quick fix".to_string(),
        enrichment_line: String::new(),
        raw_enrichment: None,
        last_working_on: None,
    };
    let header = cont.format_header();
    assert!(header.contains("CONTINUED FROM [5m ago]"));
    assert!(header.contains("Quick fix"));
    assert!(!header.contains("Last working on"));
    assert!(!header.contains("Suggested:"));
}

#[test]
fn test_continued_session_skips_redundant_enrichment() {
    // When enrichment_line equals title, don't repeat it
    let cont = ContinuedSession {
        conversation_id: "abc".to_string(),
        age_label: "3m ago".to_string(),
        title: "Edited main.rs, lib.rs".to_string(),
        enrichment_line: "Edited main.rs, lib.rs".to_string(),
        raw_enrichment: None,
        last_working_on: Some("editing main.rs, lib.rs".to_string()),
    };
    let header = cont.format_header();
    // Title appears once in CONTINUED FROM line
    let count = header.matches("Edited main.rs, lib.rs").count();
    assert_eq!(count, 1, "enrichment should not repeat title");
}

#[test]
fn test_extract_last_working_on_v3_preferred_over_files() {
    // v3 intent should win even when heuristic files are available
    let enrichment = "## Search Summary\nImplemented retry logic for polling endpoint\n\n## Other\n[Heuristic] Project: test\nTools: Edit, Read\nFiles: main.rs";
    let result = extract_last_working_on(enrichment);
    assert!(result.unwrap().contains("retry logic"));
}

#[test]
fn test_extract_last_working_on_heuristic_fallback() {
    // When no v3 summary, fall back to edited files with "editing" prefix
    let enrichment =
        "[Heuristic] Project: test\nTools: Edit, Read\nFiles: session_start.rs, queries.rs";
    let result = extract_last_working_on(enrichment).unwrap();
    assert!(result.starts_with("editing "));
    assert!(result.contains("session_start.rs"));
    assert!(result.contains("queries.rs"));
}

#[test]
fn test_extract_last_working_on_no_edits() {
    let enrichment = "[Heuristic] Project: test\nTools: Read, Grep\nFiles: main.rs";
    // No Edit tool and no v3 summary → None
    let result = extract_last_working_on(enrichment);
    assert!(result.is_none());
}

#[test]
fn test_extract_last_working_on_v3_only() {
    let enrichment = "## Search Summary\nImplemented retry logic for polling endpoint\n\n## Other";
    let result = extract_last_working_on(enrichment);
    assert!(result.unwrap().contains("retry logic"));
}

#[test]
fn session_start_marks_off_branch_receipts_and_never_names_the_dirty_tree() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    std::env::remove_var("CSR_NO_RECAP");
    let (repo, on_main, on_side) = repo_with_side_commit();
    for name in ["a.rs", "b.rs", "c.rs", "d.rs"] {
        std::fs::write(repo.path().join(name), "changed").unwrap();
    }
    let storage = crate::storage::Storage::open_memory().unwrap();
    let mut ep = minimal_episode("Fix the auth middleware regression", "Fixed it", None);
    ep.completions = vec![
        crate::hooks::stop::CompletionReceipt {
            claim: "fix(search): rig sink".into(),
            receipt: on_side[..7].to_string(),
        },
        crate::hooks::stop::CompletionReceipt {
            claim: "chore: base".into(),
            receipt: on_main[..7].to_string(),
        },
    ];
    let verdicts = vec![("validate_token".to_string(), AnchorVerdict::Intact)];
    let block = format_session_start_tier0_with_native(
        &storage,
        &ep.project,
        &ep,
        &verdicts,
        "9m ago",
        None,
        None,
        None,
        RepoProbe::Cwd(repo.path()),
    );
    assert!(
        block.contains(&format!(
            "fix(search): rig sink ({}, not on main)",
            &on_side[..7]
        )),
        "{block}"
    );
    assert!(
        block.contains(&format!("chore: base ({})", &on_main[..7])),
        "{block}"
    );
    // Four files are changed in the tree: the block says nothing about them.
    assert!(!block.contains("Uncommitted"), "{block}");
    assert!(
        block.contains("ANCHORS (last session's files): 1 intact, 0 modified\n"),
        "{block}"
    );

    // Detached HEAD names no branch.
    git_in(repo.path(), &["checkout", "-q", "--detach", &on_main]);
    let block = format_session_start_tier0_with_native(
        &storage,
        &ep.project,
        &ep,
        &verdicts,
        "9m ago",
        None,
        None,
        None,
        RepoProbe::Cwd(repo.path()),
    );
    assert!(
        block.contains(&format!("({}, not on HEAD)", &on_side[..7])),
        "{block}"
    );

    // Not a repository: no marks, no dirty line, and the block still composes.
    let plain = tempfile::tempdir().unwrap();
    let block = format_session_start_tier0_with_native(
        &storage,
        &ep.project,
        &ep,
        &verdicts,
        "9m ago",
        None,
        None,
        None,
        RepoProbe::Cwd(plain.path()),
    );
    assert!(
        !block.contains("not on") && !block.contains("Uncommitted:"),
        "{block}"
    );
    assert!(block.starts_with("recap [9m ago]: "), "{block}");
}

#[test]
fn a_dirty_tree_adds_no_uncommitted_line_and_no_status_call() {
    // The tree has changes and the episode has no commit receipt: nothing is
    // read from git, so the block cannot name a path.
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    std::env::remove_var("CSR_NO_RECAP");
    let (repo, _, _) = repo_with_side_commit();
    for name in ["a.rs", "b.rs", "c.rs"] {
        std::fs::write(repo.path().join(name), "changed").unwrap();
    }
    let storage = crate::storage::Storage::open_memory().unwrap();
    let ep = minimal_episode("Fix the auth middleware regression", "Fixed it", None);
    let block = format_session_start_tier0_with_native(
        &storage,
        &ep.project,
        &ep,
        &[],
        "2h ago",
        None,
        None,
        None,
        RepoProbe::Cwd(repo.path()),
    );
    assert!(!block.contains("Uncommitted"), "{block}");
    assert!(!block.contains("a.rs"), "{block}");
    assert!(
        crate::hooks::recap_git::probe(repo.path(), &[]).is_none(),
        "no commit receipt, no git call"
    );
}

#[test]
fn the_briefing_header_names_when_it_was_written() {
    let tier = crate::provenance::TrustTier::Unknown;
    let head = format_briefing_written(tier, "Body.", "7m ago");
    assert!(
        head.starts_with(&format!(
            "[briefing: model-derived, trust={tier}, written 7m ago, ages below as of then]\n"
        )),
        "{head}"
    );
    // The label is the header only; the old header is still the fallback.
    assert!(format_briefing(tier, "Body.")
        .starts_with(&format!("[briefing: model-derived, trust={tier}]\n")));
    // Both headers are recognised as CSR's own on re-import.
    for header in [head, format_briefing(tier, "Body.")] {
        assert!(
            crate::import::contamination::has_csr_text(&header),
            "{header}"
        );
    }
}

#[test]
fn the_full_recap_block_is_recognised_so_it_never_re_imports() {
    use crate::hooks::recap::{DreamClause, RecapContext, RecapFeeds, SettledFact};
    let mut ep = minimal_episode(
        "Review previous sesison - create a simple unwall for me where we are and how we get to 10.1.",
        "Fixed it",
        Some("Retest the send path in kitty.rs"),
    );
    ep.completions = vec![crate::hooks::stop::CompletionReceipt {
        claim: "fix(search): rig sink on the keyword get_more page".into(),
        receipt: "d696838".into(),
    }];
    ep.outcome_version = Some(crate::hooks::stop::OUTCOME_VERSION);
    ep.closing_ended = false;
    // `Next:` names the dream's file, so SessionStart shows it.
    let feeds = RecapFeeds {
        settled: vec![SettledFact {
            claim: "schema is stable".into(),
            receipt: "810283b".into(),
            status: "resolved".into(),
            source: "user_confirmed".into(),
        }],
        still_open: vec![],
        retired_while_away: vec![],
        corrections: vec![],
        open_proposals: 0,
        native_warn: None,
        top_dream: Some(DreamClause {
            label: "send in pane/kitty.rs".into(),
            verdict: "was superseded".into(),
            receipt: "921d63f".into(),
            date: "2026-10-30".into(),
            file: "src/pane/kitty.rs".into(),
            symbol: Some("send".into()),
        }),
    };
    let ctx = RecapContext {
        off_head: ["d696838", "810283b", "921d63f"]
            .iter()
            .map(|r| r.to_string())
            .collect(),
        head_label: "main".into(),
        session_is_live: false,
    };
    let recap = crate::hooks::recap::compose_recap_in_context(&ep, &feeds, "9m ago", &ctx)
        .expect("composes")
        .text;
    let verdicts = vec![
        ("send".to_string(), AnchorVerdict::Intact),
        ("recv".to_string(), AnchorVerdict::Modified),
    ];
    let block = format_tier0_block_with_recap(
        &ep,
        &verdicts,
        "9m ago",
        Some(&recap),
        crate::provenance::TrustTier::Unknown,
    );
    // Every new line of the grammar is in the block under test.
    for needle in [
        "Dreamt: send in pane/kitty.rs was superseded (2026-10-30, 921d63f, not on main).",
        "(d696838, not on main)",
        "\nLast: Fixed it (conv s).\n",
        "Now: last turn did not finish (conv s).",
        "ANCHORS (last session's files): 1 intact, 1 modified (recv)\n",
        "[[CSR:RECAP]]",
    ] {
        assert!(block.contains(needle), "{needle}\n{block}");
    }
    assert!(!block.contains("Uncommitted"), "{block}");
    // A block stored by an older build, with its `Uncommitted:` line, is still one recap hit.
    let legacy = block.replace(
        "ANCHORS (last session's files)",
        "Uncommitted: 5 files (a.rs, b.rs, c.rs, +2 more)\nANCHORS (last session's files)",
    );
    assert_ne!(legacy, block);
    let hits = crate::import::contamination::detect(&legacy);
    assert_eq!(hits.len(), 1, "{hits:?}\n{legacy}");
    assert!(
        crate::import::contamination::strip_hits(&legacy)
            .trim()
            .is_empty(),
        "{legacy}"
    );
    // The guard recognises it, with the sentinel and, as a re-paste may lose it, without.
    assert!(
        crate::extraction::provenance::is_csr_emission(&block),
        "{block}"
    );
    let unsentineled = block.replace("[[CSR:RECAP]]", "");
    assert!(
        crate::extraction::provenance::is_csr_emission(&unsentineled),
        "{unsentineled}"
    );
    // The scrubber removes the whole block as ONE recap hit, trailer lines included.
    let hits = crate::import::contamination::detect(&block);
    assert_eq!(hits.len(), 1, "{hits:?}\n{block}");
    assert_eq!(hits[0].formatter, "recap");
    assert!(
        crate::import::contamination::strip_hits(&block)
            .trim()
            .is_empty(),
        "{block}"
    );
    // ...and leaves a person's own words around it alone.
    let pasted = format!("My notes:\n{block}\nthe end");
    assert_eq!(
        crate::import::contamination::strip_hits(&pasted).trim(),
        "My notes:\n\nthe end"
    );
    // The old ANCHORS grammar, still in every stored transcript, is recognised too.
    let old = "recap [4m ago]: Fixed the importer. [[CSR:RECAP]]\nSettled: a (b).\nANCHORS: 1 intact, 0 modified since checkpoint\nFull state: csr_reflect_on_past(\"conv_s\")\n";
    assert_eq!(crate::import::contamination::detect(old).len(), 1);
    assert!(
        crate::import::contamination::strip_hits(old)
            .trim()
            .is_empty(),
        "{old}"
    );
}
