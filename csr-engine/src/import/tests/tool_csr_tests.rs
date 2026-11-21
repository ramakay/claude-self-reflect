use super::*;

#[test]
fn sidechain_events_cannot_exceed_parent_context_floor() {
    use crate::provenance::TrustTier;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-child.jsonl");
    std::fs::write(
        &path,
        r#"{"type":"user","uuid":"child-user","isSidechain":true,"message":{"content":[{"type":"text","text":"I am a child user-shaped prompt"}]}}"#,
    )
    .unwrap();
    let parent = ParentContext {
        floor: TrustTier::External,
        event_id: Some("parent-spawn".into()),
    };

    let parsed = parse_jsonl_file_with_stats_and_parent(&path, "project", Some(&parent)).unwrap();
    let event = parsed
        .evidence
        .values()
        .flat_map(|evidence| &evidence.events)
        .next()
        .unwrap();
    assert_eq!(event.trust_tier, TrustTier::External);
    assert_eq!(event.parent_event_id.as_deref(), Some("parent-spawn"));
}

#[test]
fn chunk_author_prefers_user_then_assistant() {
    use Speaker::*;
    assert_eq!(chunk_author(&[ToolResult, Assistant, User]), User);
    assert_eq!(chunk_author(&[ToolResult, Assistant]), Assistant);
    assert_eq!(chunk_author(&[ToolResult, ToolResult]), ToolResult);
    assert_eq!(chunk_author(&[]), ToolResult);
}

#[test]
fn chunk_is_sidechain_any_message_or_agent_prefix() {
    assert!(chunk_is_sidechain(&[false, true], "conv-abc"));
    assert!(chunk_is_sidechain(&[false, false], "agent-123"));
    assert!(!chunk_is_sidechain(&[false], "conv-abc"));
    assert!(!chunk_is_sidechain(&[], "conv-abc"));
    assert!(chunk_is_sidechain(&[], "agent-xyz"));
}

#[test]
fn test_normalize_project_name() {
    assert_eq!(
        normalize_project_name("-Users-someone-projects-claude-self-reflect"),
        "claude-self-reflect"
    );
    assert_eq!(
        normalize_project_name("-Users-name-projects-my-project"),
        "my-project"
    );
    assert_eq!(normalize_project_name("my-project"), "my-project");
    assert_eq!(normalize_project_name(""), "");
    assert_eq!(
        normalize_project_name("/Users/name/.claude/projects/-Users-name-projects-foo"),
        "foo"
    );
}

#[test]
fn test_is_csr_agent_prompt_skips_self_transcripts() {
    assert!(is_csr_agent_prompt(
        "You are CSR Episode Analyst. Generate a brief, actionable session briefing."
    ));
    assert!(is_csr_agent_prompt(
        "You are summarizing a coding session for future context restoration"
    ));
    // Real user work is never misclassified.
    assert!(!is_csr_agent_prompt(
        "Fix the V3 retry storm in csr-engine/src/daemon/mod.rs"
    ));
    // Merely quoting the prompt far into the message is not a meta-transcript
    // (only the leading window is tested).
    let quoted = format!("{}You are CSR Episode Analyst", "x".repeat(300));
    assert!(!is_csr_agent_prompt(&quoted));
    // Ratification extractor transcripts: `claude -p -` records the literal
    // "-" stdin marker as the first line before the piped prompt.
    assert!(is_csr_agent_prompt(
        "-\n# Ratification Dialog-Act Extraction\n\nYou are extracting OBSERVABLE dialog-acts"
    ));
    assert!(is_csr_agent_prompt(
        "# Ratification Dialog-Act Extraction\n\nYou are extracting"
    ));
    // A real message that merely starts with a dash stays importable.
    assert!(!is_csr_agent_prompt("- fix the daemon\n- ship release"));
    // The narrator preamble is a side-session; a bare sentinel is not. Lessons
    // output legitimately leads with one, and a user may open a session with it.
    assert!(is_csr_agent_prompt(
        "[[CSR:RECAP]] machine-marker: this is a CSR narrator side-session.\nYou are a post-turn narrator."
    ));
    assert!(!is_csr_agent_prompt(
        "[[CSR:LESSONS]]\n- stop punting decisions (seen 4× across 3 sessions)\n\nadd these to CLAUDE.md"
    ));
    assert!(!is_csr_agent_prompt(
        "[[CSR:RECAP]] is the marker I asked about"
    ));
    // A recap pasted into a real session carries the sentinel at the end.
    assert!(!is_csr_agent_prompt(
        "recap [2h ago]: fixed the bug: shipped it. [[CSR:RECAP]]\n\nwhy did this regress?"
    ));
}

/// The signature sits in its own paragraph, which the search sanitizer
/// deletes. The skip has to fire on the raw text, through the real parser.
#[test]
fn agent_transcripts_are_skipped_whole_by_the_parser() {
    let dir = tempfile::tempdir().unwrap();
    let ratification = write_transcript(
        dir.path(),
        "ratification.jsonl",
        "-\n# Ratification Dialog-Act Extraction\n\nYou are extracting OBSERVABLE dialog-acts.\n\nUSER: keep the daemon",
    );
    let narrator = write_transcript(
        dir.path(),
        "narrator.jsonl",
        "[[CSR:RECAP]] machine-marker: this is a CSR narrator side-session, never corpus material.\nYou are a post-turn rubber-duck narrator.\n\nTRANSCRIPT EXCERPT:\nUSER: keep the daemon",
    );
    let analyst = write_transcript(
        dir.path(),
        "analyst.jsonl",
        "You are CSR Episode Analyst. Generate a brief.\n\nEpisode 1: shipped the fix",
    );
    for path in [&ratification, &narrator, &analyst] {
        assert_eq!(
            classify_transcript(path),
            TranscriptKind::Agent,
            "{}",
            path.display()
        );
        let parsed = parse_jsonl_file_with_stats(path, "project").unwrap();
        assert!(parsed.chunks.is_empty(), "{}", path.display());
        assert!(parsed.evidence.is_empty(), "{}", path.display());
    }
}

/// Rows Claude Code injects are never the opening prompt, in either direction.
#[test]
fn injected_rows_never_decide_whether_a_transcript_is_an_agent_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let row = |text: &str, meta: bool| {
        serde_json::json!({
            "type": "user",
            "isMeta": meta,
            "timestamp": "2026-09-15T00:00:00Z",
            "message": {"content": text}
        })
    };
    // A real session whose injected first row happens to lead with a signature.
    let real = write_rows(
        dir.path(),
        "real.jsonl",
        &[
            row(
                "You are summarizing a coding session for the statusline",
                true,
            ),
            row("Why did the daemon regress after this shipped?", false),
        ],
    );
    assert_eq!(classify_transcript(&real), TranscriptKind::Real);
    assert!(!parse_jsonl_file_with_stats(&real, "project")
        .unwrap()
        .chunks
        .is_empty());
    // An agent transcript behind an injected caveat row.
    let agent = write_rows(
        dir.path(),
        "agent.jsonl",
        &[
            row(
                "Caveat: the messages below were generated by local commands.",
                true,
            ),
            row(
                "-\n# Ratification Dialog-Act Extraction\n\nUSER: keep the daemon",
                false,
            ),
        ],
    );
    assert_eq!(classify_transcript(&agent), TranscriptKind::Agent);
    assert!(parse_jsonl_file_with_stats(&agent, "project")
        .unwrap()
        .chunks
        .is_empty());
    // The same rows from an older harness that set neither flag.
    let unflagged = write_rows(
        dir.path(),
        "unflagged.jsonl",
        &[
            row("<local-command-caveat>Caveat: generated by local commands.</local-command-caveat>", false),
            row("This session is being continued from a previous conversation that ran out of context.", false),
            row("[[CSR:RECAP]] machine-marker: narrator side-session.\nYou are a narrator.", false),
        ],
    );
    assert_eq!(classify_transcript(&unflagged), TranscriptKind::Agent);
    // Pasted harness output followed by a typed request IS the opening prompt,
    // whatever a later row leads with. So is a slash command.
    for (name, opening) in [
        ("pasted.jsonl", "<local-command-stdout>error details</local-command-stdout>\n\nHelp me debug why the narrator produces this error."),
        ("slash.jsonl", "<command-name>/compact</command-name>"),
    ] {
        let real = write_rows(
            dir.path(),
            name,
            &[
                row(opening, false),
                row("[[CSR:RECAP]] machine-marker: narrator side-session.\nYou are a narrator.", false),
            ],
        );
        assert_eq!(classify_transcript(&real), TranscriptKind::Real, "{name}");
    }
    assert!(parse_jsonl_file_with_stats(&unflagged, "project")
        .unwrap()
        .chunks
        .is_empty());
    assert_eq!(
        classify_transcript(&dir.path().join("gone.jsonl")),
        TranscriptKind::Unreadable
    );
}

#[test]
fn real_session_quoting_csr_output_is_still_imported() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_transcript(
        dir.path(),
        "real.jsonl",
        "recap [2h ago]: fixed the bug: shipped it. [[CSR:RECAP]]\n\nWhy did the daemon regress after this shipped?",
    );
    assert_eq!(classify_transcript(&path), TranscriptKind::Real);
    let parsed = parse_jsonl_file_with_stats(&path, "project").unwrap();
    assert!(!parsed.chunks.is_empty());
    let content = parsed
        .chunks
        .iter()
        .map(|chunk| chunk.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(content.contains("Why did the daemon regress after this shipped?"));
    // label, never delete: the quoted recap is stored as the person typed it
    assert!(content.contains("[[CSR:RECAP]]"));
    assert!(contamination::has_csr_text(&content));
}

#[test]
fn test_generate_chunk_id_deterministic() {
    let id1 = generate_chunk_id("conv-abc", 0);
    let id2 = generate_chunk_id("conv-abc", 0);
    let id3 = generate_chunk_id("conv-abc", 1);
    assert_eq!(id1, id2);
    assert_ne!(id1, id3);
}

#[test]
fn test_extract_tool_context() {
    // Message with tool_use blocks
    let msg: serde_json::Value = serde_json::json!({
        "type": "assistant",
        "message": {
            "content": [
                {
                    "type": "tool_use",
                    "name": "Read",
                    "input": {"file_path": "/Users/me/projects/foo/src/engine.rs"}
                },
                {
                    "type": "tool_use",
                    "name": "Bash",
                    "input": {"command": "cargo test --release"}
                },
                {
                    "type": "tool_use",
                    "name": "Grep",
                    "input": {"pattern": "dump_to_disk"}
                },
                {
                    "type": "text",
                    "text": "Let me check the files."
                }
            ]
        }
    });

    let ctx = extract_tool_context(&msg);
    assert!(ctx.contains("[Read: src/engine.rs]"));
    assert!(ctx.contains("[Bash: cargo test --release]"));
    assert!(ctx.contains("[Grep: dump_to_disk]"));
    // text blocks should not appear in tool context
    assert!(!ctx.contains("Let me check"));
}

#[test]
fn test_extract_tool_context_empty() {
    // Message with only text, no tools
    let msg: serde_json::Value = serde_json::json!({
        "type": "assistant",
        "message": {
            "content": [
                {"type": "text", "text": "Hello world"}
            ]
        }
    });
    assert!(extract_tool_context(&msg).is_empty());
}

#[test]
fn csr_tool_call_and_bound_result_are_suppressed_but_siblings_remain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("csr-filter.jsonl");
    let lines = [
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-08-06T12:00:00Z",
            "message": {"content": [
                {
                    "type": "tool_use",
                    "id": "csr-call",
                    "name": "mcp__claude-self-reflect__csr_reflect_on_past",
                    "input": {"query": "SECRET RETRIEVAL QUERY"}
                },
                {
                    "type": "tool_use",
                    "id": "read-call",
                    "name": "Read",
                    "input": {"file_path": "/repo/src/kept.rs"}
                }
            ]}
        }),
        serde_json::json!({
            "type": "user",
            "timestamp": "2026-08-06T12:00:01Z",
            "message": {"content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "csr-call",
                    "content": "SECRET RETRIEVAL RESULT"
                },
                {
                    "type": "tool_result",
                    "tool_use_id": "read-call",
                    "content": "KEPT FILE RESULT"
                }
            ]}
        }),
    ];
    std::fs::write(
        &path,
        lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();

    let parsed = parse_jsonl_file_with_stats(&path, "test").unwrap();
    assert_eq!(parsed.suppression.csr_tool_blocks_suppressed, 2);
    let content = parsed
        .chunks
        .into_iter()
        .map(|chunk| chunk.content)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!content.contains("SECRET RETRIEVAL QUERY"));
    assert!(!content.contains("SECRET RETRIEVAL RESULT"));
    // Marker-only assistant messages are not embedded (D7); the substantive
    // sibling tool_result still survives as a searchable chunk.
    assert!(content.contains("KEPT FILE RESULT"));
}

/// Import drops a tool result by the tool's name and nothing else. A result
/// that holds a complete search envelope plus the analysis of it is a
/// person's or Claude's own text and is imported intact.
#[test]
fn a_tool_result_holding_an_envelope_and_analysis_is_imported_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("envelope-analysis.jsonl");
    let envelope = "\u{1f3af} RESULTS: 1 matches (high relevance, top score: 0.900)\n\u{26a1} PERFORMANCE: 6ms (1 collection searched)\n\n<search>\n  <summary count=\"1\" relevance=\"high\" top-score=\"0.900\">\n    <preview>x</preview>\n  </summary>\n  <meta>\n    <q>q</q>\n  </meta>\n  <results>\n    <r rank=\"1\">\n      <excerpt><![CDATA[x]]></excerpt>\n    </r>\n  </results>\n</search>\n";
    let result =
        format!("{envelope}Decision: retain SQLite, because it removed the network hops.\n");
    let bare = "[mcp__claude_self_reflect__csr_why]\nFix: use SQLite.";
    let why =
        "WHY: storage\n\nNo evidence chain found.\n\nRoot cause: migration 42 wrote the lock.";
    let lines = [
        serde_json::json!({
            "type": "assistant", "timestamp": "2026-10-08T12:00:00Z",
            "message": {"content": [
                {"type": "tool_use", "id": "a", "name": "Bash", "input": {"command": "probe.sh"}},
                {"type": "tool_use", "id": "b", "name": "Bash", "input": {"command": "echo"}},
                {"type": "tool_use", "id": "c", "name": "Bash", "input": {"command": "why.sh"}}
            ]}
        }),
        serde_json::json!({
            "type": "user", "timestamp": "2026-10-08T12:00:01Z",
            "message": {"content": [
                {"type": "tool_result", "tool_use_id": "a", "content": result},
                {"type": "tool_result", "tool_use_id": "b", "content": bare},
                {"type": "tool_result", "tool_use_id": "c", "content": why}
            ]}
        }),
    ];
    std::fs::write(
        &path,
        lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let parsed = parse_jsonl_file_with_stats(&path, "test").unwrap();
    assert_eq!(parsed.suppression.csr_tool_blocks_suppressed, 0);
    let content = parsed
        .chunks
        .into_iter()
        .map(|chunk| chunk.content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(content.contains("Decision: retain SQLite"), "{content}");
    assert!(content.contains("Fix: use SQLite."), "{content}");
    assert!(content.contains("Root cause: migration 42"), "{content}");
}

/// A pane-capture dump is search-side only: it sinks in ranking but import
/// keeps it, so a wrong verdict costs rank, never data.
#[test]
fn a_terminal_recording_in_a_tool_result_is_kept_at_import() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pane-capture.jsonl");
    let frames = "24 20.706 9 \u{276f} why did we drop Docker and Qdrant f\n25 20.87 9 \u{276f} why did we drop Docker and Qdrant for\n26 21.034 9 \u{276f} why did we drop Docker and Qdrant for a\n27 21.2 9 \u{276f} why did we drop Docker and Qdrant for a s\n28 21.367 9 \u{276f} why did we drop Docker and Qdrant for a sin\n29 21.532 9 \u{276f} why did we drop Docker and Qdrant for a singl\n30 21.697 9 \u{276f} why did we drop Docker and Qdrant for a single";
    let lines = [
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-10-11T12:00:00Z",
            "message": {"content": [
                {"type": "tool_use", "id": "dump", "name": "Bash", "input": {"command": "python3 dump_frames.py"}}
            ]}
        }),
        serde_json::json!({
            "type": "user",
            "timestamp": "2026-10-11T12:00:01Z",
            "message": {"content": [
                {"type": "tool_result", "tool_use_id": "dump", "content": frames}
            ]}
        }),
    ];
    std::fs::write(
        &path,
        lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    assert!(contamination::is_terminal_replay(frames));
    let parsed = parse_jsonl_file_with_stats(&path, "test").unwrap();
    assert_eq!(parsed.suppression.csr_tool_blocks_suppressed, 0);
    let content = parsed
        .chunks
        .into_iter()
        .map(|chunk| chunk.content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(content.contains("20.706"), "{content}");
}

#[test]
fn marker_only_text_detection() {
    assert!(is_marker_only_text("[Edit: foo.md]"));
    assert!(!is_marker_only_text(
        "[Edit: foo.md]\nFixed the parser bug."
    ));
    assert!(is_marker_only_text("[Read] [Bash]"));
    assert!(is_marker_only_text(
        "[Read: src/engine.rs] [Bash: cargo test --release]"
    ));
    assert!(!is_marker_only_text("Fixed the parser bug."));
    assert!(!is_marker_only_text(""));
}

/// A real text block is never dropped, whatever its text looks like. Only a
/// message with no text block at all (nothing but rendered tool markers) is.
#[test]
fn an_assistant_text_block_that_looks_like_a_tool_marker_is_imported_byte_identical() {
    for text in ["[TODO]", "[csr_reflect_on_past: Preserve my rollback plan]"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("looks-like-a-marker.jsonl");
        let message = serde_json::json!({
            "type": "assistant",
            "uuid": "a1",
            "timestamp": "2026-10-13T00:00:00Z",
            "message": {"content": [{"type": "text", "text": text}]}
        });
        std::fs::write(&path, message.to_string()).unwrap();
        let parsed = parse_jsonl_file_with_stats(&path, "project").unwrap();
        let stored: Vec<&str> = parsed.chunks.iter().map(|c| c.content.as_str()).collect();
        assert_eq!(stored, [text], "{text} was dropped at import");
        // the reparse decision is the same one
        assert!(
            !drops_as_marker_only(&message, text),
            "{text} dropped at reparse"
        );
    }
    // a tool_use-only message is dropped as before
    let tool_only = serde_json::json!({
        "type": "assistant",
        "message": {"content": [{"type": "tool_use", "id": "e", "name": "Edit",
                                  "input": {"file_path": "/repo/a.md"}}]}
    });
    assert!(drops_as_marker_only(&tool_only, "[Edit: a.md]"));
    // the same text from the person is never a marker
    let user = serde_json::json!({"type": "user", "message": {"content": "[TODO]"}});
    assert!(!drops_as_marker_only(&user, "[TODO]"));
}

#[test]
fn marker_only_assistant_message_produces_no_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("marker-only.jsonl");
    let marker_only = serde_json::json!({
        "type": "assistant",
        "timestamp": "2026-08-08T12:00:00Z",
        "message": {"content": [
            {
                "type": "tool_use",
                "id": "edit-1",
                "name": "Edit",
                "input": {"file_path": "/repo/tasks/2026-08-08-v10.1-release-gate.md"}
            }
        ]}
    });
    std::fs::write(&path, marker_only.to_string()).unwrap();

    let parsed = parse_jsonl_file_with_stats(&path, "test").unwrap();
    assert!(
        parsed.chunks.is_empty(),
        "marker-only assistant message must not produce a searchable chunk, got: {:?}",
        parsed
            .chunks
            .iter()
            .map(|c| c.content.as_str())
            .collect::<Vec<_>>()
    );

    // Same Edit tool_use plus real prose must still produce a chunk.
    let with_prose = serde_json::json!({
        "type": "assistant",
        "timestamp": "2026-08-08T12:00:01Z",
        "message": {"content": [
            {
                "type": "tool_use",
                "id": "edit-2",
                "name": "Edit",
                "input": {"file_path": "/repo/tasks/2026-08-08-v10.1-release-gate.md"}
            },
            {
                "type": "text",
                "text": "Fixed the parser bug."
            }
        ]}
    });
    let path2 = dir.path().join("marker-plus-prose.jsonl");
    std::fs::write(&path2, with_prose.to_string()).unwrap();
    let parsed2 = parse_jsonl_file_with_stats(&path2, "test").unwrap();
    assert!(!parsed2.chunks.is_empty());
    let content = parsed2
        .chunks
        .iter()
        .map(|c| c.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        content.contains("Fixed the parser bug."),
        "prose next to a tool marker must survive embedding, got: {content}"
    );
}

#[test]
fn unresolved_tool_result_is_kept() {
    let msg = serde_json::json!({
        "type": "user",
        "message": {"content": [{
            "type": "tool_result",
            "tool_use_id": "missing-call",
            "content": "UNKNOWN RESULT MUST STAY"
        }]}
    });

    assert_eq!(extract_tool_results(&msg), "UNKNOWN RESULT MUST STAY");
}

#[test]
fn bare_csr_name_requires_matching_server_identity() {
    let csr = serde_json::json!({
        "type": "assistant",
        "message": {"content": [{
            "type": "tool_use",
            "id": "csr-bare",
            "name": "reflect_on_past",
            "server_name": "claude-self-reflect",
            "input": {"query": "SUPPRESSED"}
        }]}
    });
    let unrelated = serde_json::json!({
        "type": "assistant",
        "message": {"content": [{
            "type": "tool_use",
            "id": "other-bare",
            "name": "reflect_on_past",
            "server_name": "another-server",
            "input": {"query": "KEPT"}
        }]}
    });
    let (sanitized, stats) = sanitize_messages_for_search(&[csr, unrelated]);

    assert!(extract_tool_context(&sanitized[0]).is_empty());
    assert_eq!(
        extract_tool_context(&sanitized[1]),
        "[reflect_on_past: KEPT]"
    );
    assert_eq!(stats.csr_tool_blocks_suppressed, 1);
}

#[test]
fn memory_manifest_system_reminder_is_scrubbed_without_user_prose() {
    let msg = serde_json::json!({
        "type": "user",
        "message": {"content": [{
            "type": "text",
            "text": "Please fix the importer.\n<system-reminder>CSR ENDLESS MEMORY ACTIVE — every past session in this project is indexed and searchable.\nRetrieved memory payload.</system-reminder>\nKeep this sentence."
        }]}
    });

    // The shared sanitizer removes exactly the wrapper span; every other
    // byte of the person's text is untouched.
    let (sanitized, stats) = sanitize_messages_for_search(&[msg]);
    assert_eq!(stats.csr_hook_wrappers_scrubbed, 1);
    assert_eq!(
        extract_message_text(&sanitized[0]),
        "Please fix the importer.\n\nKeep this sentence."
    );
}

#[test]
fn csr_discussion_outside_user_reminder_is_kept() {
    let user = serde_json::json!({
        "type": "user",
        "message": {"content": [{
            "type": "text",
            "text": "Discuss CSR PICKUP — without treating this prose as injected."
        }]}
    });
    let assistant = serde_json::json!({
        "type": "assistant",
        "message": {"content": [{
            "type": "text",
            "text": "<system-reminder>CSR PICKUP — quoted by the assistant</system-reminder>"
        }]}
    });

    let (sanitized, stats) = sanitize_messages_for_search(&[user, assistant.clone()]);
    assert_eq!(stats.csr_hook_wrappers_scrubbed, 0);
    assert_eq!(
        extract_message_text(&sanitized[0]),
        "Discuss CSR PICKUP — without treating this prose as injected."
    );
    // The harness wraps what it injects into the user's turn, so the
    // assistant's quoted wrapper is not cut as one (stats stay 0). It is
    // CSR text, though, and a derived artifact does not learn it.
    assert_eq!(stats.csr_hook_wrappers_scrubbed, 0);
    assert_eq!(extract_message_text(&sanitized[1]), "");
    // The stored conversation keeps it, byte for byte.
    let mut chunks = CsrMessageSanitizer::for_chunks();
    let mut quoted = assistant.clone();
    sanitize_message_for_search(&mut quoted, &mut chunks);
    assert_eq!(
        extract_message_text(&quoted),
        "<system-reminder>CSR PICKUP — quoted by the assistant</system-reminder>"
    );
}

#[test]
fn sanitized_messages_keep_user_prose_and_sibling_blocks_out_of_v3_contamination() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secondary-pipelines.jsonl");
    let lines = [
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-08-06T12:00:00Z",
            "message": {"content": [
                {
                    "type": "tool_use",
                    "id": "csr-call",
                    "name": "mcp__claude_self_reflect__csr_reflect_on_past",
                    "input": {"query": "CSR QUERY MUST DISAPPEAR"}
                },
                {
                    "type": "tool_use",
                    "id": "read-call",
                    "name": "Read",
                    "input": {"file_path": "/repo/src/kept.rs"}
                }
            ]}
        }),
        serde_json::json!({
            "type": "human",
            "timestamp": "2026-08-06T12:00:01Z",
            "message": {"content": [
                {
                    "type": "text",
                    "text": "USER PROSE BEFORE\n<system-reminder>CSR PICKUP — CSR WRAPPER MUST DISAPPEAR</system-reminder>\nUSER PROSE AFTER"
                },
                {
                    "type": "tool_result",
                    "tool_use_id": "csr-call",
                    "content": "CSR RESULT MUST DISAPPEAR"
                },
                {
                    "type": "tool_result",
                    "tool_use_id": "read-call",
                    "content": "SIBLING RESULT MUST STAY"
                }
            ]}
        }),
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-08-06T12:00:02Z",
            "message": {"content": [{"type": "text", "text": "Implemented kept.rs successfully"}]}
        }),
    ];
    std::fs::write(
        &path,
        lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();

    let messages = parse_jsonl_messages_for_search(&path).unwrap();
    let serialized = serde_json::to_string(&messages).unwrap();
    let result = crate::extraction::extract_v3(&messages);
    let v3 = format!("{}\n{}", result.search_index, result.context_cache);

    for forbidden in [
        "CSR QUERY MUST DISAPPEAR",
        "CSR RESULT MUST DISAPPEAR",
        "CSR WRAPPER MUST DISAPPEAR",
    ] {
        assert!(!serialized.contains(forbidden));
        assert!(!v3.contains(forbidden));
    }
    for retained in [
        "USER PROSE BEFORE",
        "USER PROSE AFTER",
        "kept.rs",
        "SIBLING RESULT MUST STAY",
    ] {
        assert!(
            serialized.contains(retained),
            "sanitized input lost {retained}"
        );
    }
    assert!(v3.contains("USER PROSE BEFORE"));
    assert!(v3.contains("USER PROSE AFTER"));
    // Only the wrapper span went from the person's block.
    assert!(
        serialized.contains("USER PROSE BEFORE\\n\\nUSER PROSE AFTER"),
        "{serialized}"
    );
}

#[test]
fn rows_the_harness_wrote_into_the_user_channel_are_not_indexed_as_prose() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("harness-rows.jsonl");
    let user_row = |text: &str, extra: serde_json::Value| {
        let mut row = serde_json::json!({
            "type": "user",
            "timestamp": "2026-09-19T21:00:00Z",
            "message": {"role": "user", "content": text}
        });
        row.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        row
    };
    let lines = [
        user_row(
            "TYPED PROMPT trace the prompt-submit latency",
            serde_json::json!({"origin": {"kind": "human"}, "promptSource": "typed"}),
        ),
        user_row(
            "<task-notification>\n<task-id>NOTIFICATION MUST DISAPPEAR</task-id>\n</task-notification>",
            serde_json::json!({
                "origin": {"kind": "task-notification"},
                "promptSource": "system",
                "turnOrigin": "task_notification"
            }),
        ),
        // Subagent transcripts carry the origin without a promptSource.
        user_row(
            "Another Claude session sent a message: PEER MESSAGE MUST DISAPPEAR",
            serde_json::json!({"origin": {"kind": "peer"}, "isMeta": true}),
        ),
        user_row(
            "[Cross-session idle notice] IDLE NOTICE MUST DISAPPEAR",
            serde_json::json!({"promptSource": "system", "isMeta": true}),
        ),
        user_row(
            "The coordinator sent a message while you were working:\nCOORDINATOR ORDER stop and report",
            serde_json::json!({"origin": {"kind": "coordinator"}}),
        ),
        serde_json::json!({
            "type": "user",
            "timestamp": "2026-09-19T21:00:05Z",
            "origin": {"kind": "task-notification"},
            "promptSource": "system",
            "message": {"content": [
                {"type": "text", "text": "ARRAY NOTIFICATION MUST DISAPPEAR"},
                {"type": "tool_result", "tool_use_id": "read-call", "content": "SIBLING RESULT MUST STAY"}
            ]}
        }),
    ];
    std::fs::write(
        &path,
        lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();

    let serialized =
        serde_json::to_string(&parse_jsonl_messages_for_search(&path).unwrap()).unwrap();
    let indexed = parse_jsonl_file(&path, "project")
        .unwrap()
        .into_iter()
        .map(|chunk| chunk.content)
        .collect::<Vec<_>>()
        .join("\n");

    for forbidden in [
        "NOTIFICATION MUST DISAPPEAR",
        "PEER MESSAGE MUST DISAPPEAR",
        "IDLE NOTICE MUST DISAPPEAR",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "sanitizer kept {forbidden}"
        );
        assert!(!indexed.contains(forbidden), "index kept {forbidden}");
    }
    for retained in [
        "TYPED PROMPT",
        "COORDINATOR ORDER",
        "SIBLING RESULT MUST STAY",
    ] {
        assert!(serialized.contains(retained), "sanitizer lost {retained}");
        assert!(indexed.contains(retained), "index lost {retained}");
    }
}

#[test]
fn test_extract_message_text_user_type() {
    // "user" type messages should work the same as "human"
    let msg: serde_json::Value = serde_json::json!({
        "type": "user",
        "message": {
            "content": [
                {"type": "text", "text": "Fix the chunking bug"}
            ]
        }
    });
    assert_eq!(extract_message_text(&msg), "Fix the chunking bug");
}

#[test]
fn test_strip_private_tags() {
    assert_eq!(strip_private_tags("hello world"), "hello world");
    assert_eq!(
        strip_private_tags("before <private>secret key ABC123</private> after"),
        "before [private] after"
    );
    assert_eq!(
        strip_private_tags("no <private>one</private> and <private>two</private> tags"),
        "no [private] and [private] tags"
    );
}

#[test]
fn test_strip_private_unclosed_tag() {
    // S-3 fix: unclosed <private> tag should redact to end of input
    assert_eq!(
        strip_private_tags("before <private>secret leaked here"),
        "before [private]"
    );
}

#[test]
fn test_extract_message_text_strips_private() {
    let msg: serde_json::Value = serde_json::json!({
        "type": "user",
        "message": {
            "content": [
                {"type": "text", "text": "My API key is <private>sk-abc123</private> please use it"}
            ]
        }
    });
    let text = extract_message_text(&msg);
    assert!(!text.contains("sk-abc123"));
    assert!(text.contains("[private]"));
}

#[test]
fn import_redacts_secrets_from_every_chunk_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret-session.jsonl");
    let secrets = FakeSecrets::new();
    write_secret_transcript(&path, &secrets);

    let parsed = parse_jsonl_file_with_stats(&path, "proj").unwrap();
    let stored = parsed
        .chunks
        .iter()
        .map(|chunk| chunk.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for secret in secrets.all() {
        assert!(!stored.contains(secret), "a secret reached a chunk");
    }
    for kind in [
        "github_token",
        "anthropic_key",
        "aws_access_key",
        "stripe_key",
    ] {
        assert!(
            stored.contains(&format!("[secret redacted: {kind}]")),
            "missing marker for {kind}: {stored}"
        );
        assert_eq!(parsed.suppression.secrets.get(kind), 1, "{kind}");
    }
    assert_eq!(parsed.suppression.secrets.total(), 4);
}

#[test]
fn object_keys_are_redacted_like_values() {
    let token = format!("{}{}", "ghp_", "K".repeat(36));
    let other = format!("{}{}", "ghp_", "L".repeat(36));
    let mut value = serde_json::json!({
        "accounts": {token.clone(): "first", other.clone(): "second", "plain": 1},
        "list": [{token.clone(): "nested"}],
    });
    let mut counts = crate::redact::SecretCounts::default();
    redact_content_secrets(&mut value, &mut counts);
    let text = value.to_string();
    assert!(!text.contains("ghp_"), "{text}");
    assert_eq!(counts.get("github_token"), 3);
    let accounts = value["accounts"].as_object().unwrap();
    // Two keys that redact to the same marker must not overwrite each other.
    assert_eq!(accounts.len(), 3, "{text}");
    assert!(accounts.values().any(|v| v == "first"));
    assert!(accounts.values().any(|v| v == "second"));
}

#[test]
fn a_renamed_key_never_lands_on_a_key_that_was_already_there() {
    let aws = format!("AKIA{}", "B".repeat(16));
    let marker = "[secret redacted: aws_access_key]";
    // `AKIA...` sorts before the marker, so it is visited first.
    let mut value = serde_json::json!({
        "accounts": {aws.clone(): "credential", marker: "existing", "z": 1},
    });
    let mut counts = crate::redact::SecretCounts::default();
    redact_content_secrets(&mut value, &mut counts);
    let accounts = value["accounts"].as_object().unwrap();
    assert_eq!(accounts.len(), 3, "{value}");
    assert_eq!(accounts[marker], "existing");
    assert!(accounts.values().any(|v| v == "credential"));
}

#[test]
fn kill_switch_restores_unredacted_import() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret-session.jsonl");
    let secrets = FakeSecrets::new();
    write_secret_transcript(&path, &secrets);

    let _off = crate::redact::DisabledForTest::set(true);
    let parsed = parse_jsonl_file_with_stats(&path, "proj").unwrap();
    let stored = parsed
        .chunks
        .iter()
        .map(|chunk| chunk.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for secret in secrets.all() {
        assert!(stored.contains(secret), "kill switch should keep the text");
    }
    assert!(parsed.suppression.secrets.is_empty());
    assert!(!stored.contains("[secret redacted"));
}

#[test]
fn secret_inside_and_outside_a_private_span_never_leaks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private-session.jsonl");
    let secret = format!("{}{}", "ghp_", "B".repeat(36));
    let line = serde_json::json!({
        "type": "user",
        "uuid": "u-1",
        "timestamp": "2026-10-14T10:00:00Z",
        "message": {"content": format!(
            "open <private>hush {secret}</private> and also {secret} here")}
    });
    std::fs::write(&path, line.to_string()).unwrap();

    let parsed = parse_jsonl_file_with_stats(&path, "proj").unwrap();
    let stored = parsed.chunks[0].content.clone();
    assert!(!stored.contains(&secret));
    assert!(!stored.contains("hush"));
    assert!(stored.contains("[private]"));
    assert!(stored.contains("[secret redacted: github_token]"));
}

#[test]
fn per_file_secret_counts_do_not_double_on_reparse() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret-session.jsonl");
    write_secret_transcript(&path, &FakeSecrets::new());
    let storage = crate::storage::Storage::open(&dir.path().join("test.db")).unwrap();

    for _ in 0..3 {
        let parsed = parse_jsonl_file_with_stats(&path, "proj").unwrap();
        storage
            .mark_file_imported_with_suppression(&path, parsed.chunks.len(), parsed.suppression)
            .unwrap();
    }
    let (total, by_kind) = storage.get_secrets_redacted().unwrap();
    assert_eq!(total, 4);
    assert_eq!(by_kind.len(), 4);
    assert!(by_kind.iter().all(|(_, count)| *count == 1));
}

#[tokio::test]
async fn engine_import_stores_markers_and_counts_them() {
    use crate::embeddings::EmbeddingEngine;
    use crate::search::SearchEngine;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    let project_dir = projects.join("-tmp-proj");
    std::fs::create_dir_all(&project_dir).unwrap();
    let path = project_dir.join("secret-session.jsonl");
    let secrets = FakeSecrets::new();
    write_secret_transcript(&path, &secrets);

    let storage = Arc::new(crate::storage::Storage::open(&dir.path().join("test.db")).unwrap());
    let embeddings = Arc::new(EmbeddingEngine::new().unwrap());
    let search = Arc::new(RwLock::new(SearchEngine::new(64)));
    let engine = crate::engine::Engine::from_parts(storage.clone(), embeddings, search, projects);

    let imported = engine.import_file(&path, "proj").await.unwrap();
    assert!(imported > 0);

    let ids = storage
        .get_chunk_ids_for_conversation("secret-session")
        .unwrap();
    let stored = storage
        .get_chunks_by_ids(&ids)
        .unwrap()
        .into_iter()
        .map(|chunk| chunk.content)
        .collect::<Vec<_>>()
        .join("\n");
    for secret in secrets.all() {
        assert!(!stored.contains(secret), "a secret reached the database");
    }
    assert!(stored.contains("[secret redacted: github_token]"));
    let (total, by_kind) = storage.get_secrets_redacted().unwrap();
    assert_eq!(total, 4);
    assert!(by_kind.contains(&("stripe_key".to_string(), 1)));
}

/// `transcript_inputs_limited` reads a plan file's first `limit` bytes only.
#[test]
fn limited_inputs_cap_a_plan_file() {
    let temp = tempfile::tempdir().unwrap();
    let storage = crate::storage::Storage::open_memory().unwrap();
    let plan = temp.path().join("plan.md");
    std::fs::write(&plan, "ABCDEF").unwrap();
    let full = transcript_inputs(&storage, &plan, "plan:x").unwrap();
    assert!(joined_input_text(&full).contains("ABCDEF"));
    let capped = transcript_inputs_limited(&storage, &plan, "plan:x", 3).unwrap();
    let text = joined_input_text(&capped);
    assert!(text.contains("ABC"), "{text}");
    assert!(!text.contains("ABCD"), "{text}");
    assert!(!text.contains("DEF"), "{text}");
}

/// `transcript_inputs_limited` reads a Codex rollout's first `limit` bytes only.
#[test]
fn limited_inputs_cap_a_codex_rollout() {
    let temp = tempfile::tempdir().unwrap();
    let storage = crate::storage::Storage::open_memory().unwrap();
    let rollout = temp.path().join("rollout-limit.jsonl");
    let first = r#"{"timestamp":"2026-09-01T09:00:02Z","type":"response_item","payload":{"type":"message","id":"user-1","role":"user","content":[{"type":"input_text","text":"first request"}]}}"#;
    let second = r#"{"timestamp":"2026-09-01T09:00:03Z","type":"response_item","payload":{"type":"message","id":"assistant-1","role":"assistant","content":[{"type":"output_text","text":"second reply"}]}}"#;
    std::fs::write(&rollout, format!("{first}\n{second}\n")).unwrap();
    let full = transcript_inputs_limited(&storage, &rollout, "c", u64::MAX).unwrap();
    let text = joined_input_text(&full);
    assert!(
        text.contains("first request") && text.contains("second reply"),
        "{text}"
    );
    let one = transcript_inputs_limited(&storage, &rollout, "c", first.len() as u64 + 1).unwrap();
    let text = joined_input_text(&one);
    assert!(
        text.contains("first request") && !text.contains("second reply"),
        "{text}"
    );
    let none = transcript_inputs_limited(&storage, &rollout, "c", 0).unwrap();
    assert!(
        !joined_input_text(&none).contains("first request"),
        "a zero limit must read nothing"
    );
}
