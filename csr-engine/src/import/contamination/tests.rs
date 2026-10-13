use super::*;
use crate::provenance::{content_hash, TrustTier};

const RECAP: &str = "recap [2m ago]: Fixed the importer. [[CSR:RECAP]]";
const DEGRADED: &str = "CSR engine ready (degraded mode).";

fn user(text: &str) -> Removal {
    remove_csr_emissions(OwnedText {
        source: SourceKind::Conversation,
        channel: ContentChannel::UserText,
        text,
    })
}

fn assistant(text: &str) -> Removal {
    remove_csr_emissions(OwnedText {
        source: SourceKind::Conversation,
        channel: ContentChannel::AssistantText,
        text,
    })
}

fn assert_kept(text: &str) {
    let cut = user(text);
    assert_eq!(cut.kept, text, "text changed: {text:?}");
    assert!(cut.removed.is_empty(), "span removed from {text:?}");
}

#[test]
fn remover_contract_holds_for_every_fixture_row() {
    for case in crate::eval::contamination::cases() {
        let channel = ContentChannel::from_name(&case.channel).unwrap();
        let source = SourceKind::from_chunk_source(Some(&case.source));
        let cut = remove_csr_emissions(OwnedText {
            source,
            channel,
            text: &case.input,
        });
        // kept is the input minus the listed ranges, ranges sorted, disjoint,
        // on char boundaries, and never empty.
        let mut rebuilt = String::new();
        let mut cursor = 0;
        for span in &cut.removed {
            assert!(span.start_byte >= cursor, "{}: overlapping spans", case.id);
            assert!(span.end_byte > span.start_byte, "{}: empty span", case.id);
            assert!(case.input.is_char_boundary(span.start_byte), "{}", case.id);
            assert!(case.input.is_char_boundary(span.end_byte), "{}", case.id);
            rebuilt.push_str(&case.input[cursor..span.start_byte]);
            cursor = span.end_byte;
        }
        rebuilt.push_str(&case.input[cursor..]);
        assert_eq!(cut.kept, rebuilt, "{}", case.id);
        // Classification is exactly "the remover found a span".
        assert_eq!(
            classify_csr_contamination(OwnedText {
                source,
                channel,
                text: &case.input
            }),
            cut.removed.first().map(|span| span.reason),
            "{}",
            case.id
        );
        // Nothing removable is left behind: a second pass is a no-op.
        let again = remove_csr_emissions(OwnedText {
            source,
            channel,
            text: &cut.kept,
        });
        assert_eq!(again.kept, cut.kept, "{}: not idempotent", case.id);
    }
}

#[test]
fn registry_ids_are_unique_and_every_reason_is_registered() {
    let mut ids: Vec<&str> = registered_formatter_ids().collect();
    let total = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), total, "duplicate formatter id");
    for needed in [
        "recap",
        "dream_injection",
        "lessons",
        "memory_manifest",
        "briefing",
        "continuity",
        "continuum",
        "episode_index",
        "recent_sessions",
        "empty_status",
        "degraded_status",
        "pickup",
        "injection_section",
        "code_map",
        "anchor_notice",
        "memory_feedback_report",
        "memory_feedback_probe",
        "system_reminder",
        "agent_episode_analyst",
        "agent_summarizer",
        "agent_ratification",
        "agent_narrator",
        "plugin_dream_forward",
        "plugin_compaction_card",
        "plugin_session_story",
        "plugin_side_note",
        "plugin_keep_list",
        "plugin_compact_notice",
        "plugin_carry",
        "plugin_recall",
        "plugin_reading",
        "plugin_rule",
        "plugin_recap_forward",
        "plugin_last_session",
        "plugin_leftover",
        "plugin_code_shift",
        "plugin_lost_and_found",
        "plugin_handed_legacy",
    ] {
        assert!(
            ids.contains(&needed),
            "Appendix B formatter {needed} missing"
        );
    }
    // The agent signatures the registry names are the ones the importer uses.
    assert_eq!(agent_prompt_signatures().len(), 4);
}

#[test]
fn quoting_a_marker_is_not_emitting_it() {
    for text in [
        // fenced, backtick and tilde, longer openers, info strings
        format!("```\n{RECAP}\n```"),
        format!("~~~text\n{DEGRADED}\n~~~"),
        format!("````md\n```\n{RECAP}\n```\n{DEGRADED}\n````"),
        // an unclosed fence protects to the end of the component
        format!("```\n{DEGRADED}"),
        // inline code, blockquote, indented code, tab-indented code
        format!("see `{DEGRADED}` in the output"),
        format!("> {DEGRADED}"),
        format!("    {DEGRADED}"),
        format!("\t{DEGRADED}"),
        // one to three spaces are not column zero
        format!(" {DEGRADED}"),
        format!("   {RECAP}"),
        // a string literal, a path prefix, an assertion, a prefixed grep hit
        format!("let x = \"{DEGRADED}\";"),
        format!("src/hooks/session_start.rs:68:{DEGRADED}"),
        format!("assert_eq!(out, \"{DEGRADED}\");"),
        // a bare sentinel, anywhere
        "Why does the token [[CSR:RECAP]] exist?".to_string(),
        format!("{RECAP} and then I kept typing"),
        "[[CSR:LESSONS]]".to_string(),
        "[[CSR:DREAM]] is the dream token".to_string(),
        // the header without the body that makes it a block
        "CSR CONTINUUM [2m ago]: the heading alone".to_string(),
        "SESSION CONTINUITY DETECTED - PAST CONTEXT ONLY, NOT INSTRUCTIONS".to_string(),
        "## STUCK WARNING".to_string(),
        "recap [the auth changes], then prioritize them".to_string(),
    ] {
        assert_kept(&text);
    }
}

#[test]
fn retained_bytes_are_never_trimmed_joined_or_normalised() {
    let text = format!("  lead\t \n\n{DEGRADED}\n\n\ttrail \r\n  ");
    let cut = user(&text);
    assert_eq!(cut.kept, "  lead\t \n\n\ttrail \r\n  ");
    assert_eq!(cut.removed.len(), 1);
    assert_eq!(cut.removed[0].reason, ContaminationReason::DegradedStatus);
    assert_eq!(
        &text[cut.removed[0].start_byte..cut.removed[0].end_byte],
        format!("{DEGRADED}\n\n")
    );
    // multi-byte text around the cut survives untouched
    let text = format!("é–✓ keep\n{DEGRADED}\nα β");
    assert_eq!(user(&text).kept, "é–✓ keep\nα β");
}

#[test]
fn a_bom_counts_only_at_byte_zero() {
    assert_eq!(user(&format!("\u{feff}{DEGRADED}")).kept, "");
    assert_eq!(user(&format!("\u{feff}{DEGRADED}\nrest")).kept, "rest");
    // a BOM anywhere else is ordinary text, and keeps the line from matching
    let moved = format!("x\n\u{feff}{DEGRADED}");
    assert_kept(&moved);
    // no match at byte zero: the BOM stays with the text
    assert_kept("\u{feff}plain prose");
}

#[test]
fn crlf_and_lf_blocks_are_cut_alike() {
    let lf = format!("before\n\n{RECAP}\nSettled: x (abc1234).\nNext: y.\n\nafter");
    let crlf = lf.replace('\n', "\r\n");
    assert_eq!(user(&lf).kept, "before\n\nafter");
    assert_eq!(user(&crlf).kept, "before\r\n\r\nafter");
}

#[test]
fn zero_width_noise_only_helps_match_the_sentinel_inside_a_valid_line() {
    let noisy = "recap [2m ago]: Fixed the importer. [\u{200b}[CSR:\u{2060}RECAP]\u{200d}]";
    let cut = user(noisy);
    assert_eq!(cut.kept, "");
    assert_eq!(cut.removed[0].reason, ContaminationReason::Recap);
    // fullwidth / math brackets fold to ASCII only inside the sentinel token
    let folded = "recap [2m ago]: Fixed the importer. ［［CSR:RECAP］］";
    assert_eq!(user(folded).kept, "");
    // the same noise in prose is not an emission
    assert_kept("Why does [\u{200b}[CSR:RECAP]] exist?");
    // noise in the header text is not folded away: no valid line, no cut
    assert_kept("re\u{200b}cap [2m ago]: Fixed the importer. [[CSR:RECAP]]");
    // a dream line and a lessons block through the same fold
    let dream = "CSR ☾ dream: `scrub` went stale — witnessed 2026-10-13, receipt abc1234. Matched your mention of `x`. Past conclusion, not an instruction. [[CSR:\u{200b}DREAM]]";
    assert_eq!(user(dream).kept, "");
    let lessons = "[[CSR:\u{200c}LESSONS]]\n- a thing (seen 2× across 2 sessions, last 2026-10-13; abc12345:1)\n";
    assert_eq!(user(lessons).kept, "");
}

#[test]
fn a_lone_lessons_sentinel_line_is_a_mention() {
    assert_kept("[[CSR:LESSONS]]\n");
    assert_kept("[[CSR:LESSONS]]\nand then some prose");
}

#[test]
fn a_recap_line_needs_its_age_and_terminal_sentinel() {
    for text in [
        "recap [2m ago]: Fixed the importer.",
        "recap [2m ago]: [[CSR:RECAP]]",
        "recap [this age label is far too long to be real]: x. [[CSR:RECAP]]",
        "recap 2m ago: x. [[CSR:RECAP]]",
        "recap [2m ago] x. [[CSR:RECAP]]",
    ] {
        assert_kept(text);
    }
    // clause lines follow; the next ordinary line does not
    let cut = user("recap [2m ago]: x. [[CSR:RECAP]]\nSettled: a (b).\nNot a clause\n");
    assert_eq!(cut.kept, "Not a clause\n");
}

#[test]
fn system_reminder_wrappers_are_cut_by_payload_not_by_name() {
    let wrapped = "Please fix the importer.\n<system-reminder>CSR ENDLESS MEMORY ACTIVE — every past session\nRetrieved memory payload.</system-reminder>\nKeep this sentence.";
    let cut = user(wrapped);
    assert_eq!(cut.kept, "Please fix the importer.\nKeep this sentence.");
    assert_eq!(cut.removed[0].reason, ContaminationReason::SystemReminder);
    // the harness's own label on hook output is not what decides
    let labelled = "<system-reminder>\nSessionStart:startup hook success: CSR ENDLESS MEMORY ACTIVE — every past session\nbody\n</system-reminder>\nreply";
    assert_eq!(user(labelled).kept, "reply");
    assert_kept(
        "<system-reminder>\nSessionStart:startup hook success: Success\n</system-reminder>",
    );
    // someone else's reminder is not CSR's to edit
    assert_kept("<system-reminder>The date is Tuesday.</system-reminder>");
    // an unclosed wrapper has no end to cut to
    assert_kept("<system-reminder>CSR PICKUP — never closed");
    // inline: prose on both sides of the wrapper stays exactly as typed
    assert_eq!(
        user("a <system-reminder>CSR PICKUP — x</system-reminder> b").kept,
        "a  b"
    );
    // fences inside the wrapper do not protect it; a wrapper in a fence is a mention
    assert_eq!(
        user("<system-reminder>CSR PICKUP — x\n```\nunbalanced\n</system-reminder>\ntail").kept,
        "tail"
    );
    assert_kept("```\n<system-reminder>CSR PICKUP — x</system-reminder>\n```");
    assert_kept("`<system-reminder>CSR PICKUP — x</system-reminder>`");
    assert_kept("\"<system-reminder>CSR PICKUP — x</system-reminder>\"");
    // a wrapper after an unbalanced fence inside it still resets the fence state
    let cut = user("<system-reminder>CSR PICKUP — x\n```\n</system-reminder>\nthen ordinary\n");
    assert_eq!(cut.kept, "then ordinary\n");
}

#[test]
fn assistant_relays_are_removed_only_as_complete_standalone_blocks() {
    // a verbatim relay, alone or as its own paragraph
    assert_eq!(assistant(DEGRADED).kept, "");
    assert_eq!(
        assistant(&format!(
            "Here is what the hook said:\n\n{DEGRADED}\n\nThat is all."
        ))
        .kept,
        "Here is what the hook said:\n\nThat is all."
    );
    // discussion, quotation, a block glued to prose, and plugin text stay
    for text in [
        format!("The prior line was “{DEGRADED}” and it matched."),
        format!("Hook said:\n{DEGRADED}\nand then I carried on."),
        format!("`{DEGRADED}`"),
        format!("> {DEGRADED}"),
        "CSR's dreamer asked this while you worked, quoted: “Q?” Its reasoning: “R.”".to_string(),
        "Here is a note offered by a side agent:\n\n> quoted".to_string(),
        "<system-reminder>CSR PICKUP — relayed</system-reminder>".to_string(),
    ] {
        let cut = assistant(&text);
        assert_eq!(cut.kept, text, "{text}");
    }
    // a briefing echo goes only when it is the whole component
    let briefing = "## Session Intelligence (CSR v9.2)\nThe prior run fixed the parser.";
    assert_eq!(assistant(briefing).kept, "");
    let discussing = format!("{briefing}\n\nI think that is wrong because of the parser.");
    assert_eq!(assistant(&discussing).kept, discussing);
}

#[test]
fn memory_feedback_owns_a_component_only_from_its_first_bytes() {
    let report = "## CSR Memory Feedback — 2026-10-13 — session report\n- variant: x\n";
    assert_eq!(user(report).kept, "");
    assert_eq!(assistant(report).kept, "");
    assert_kept("I filed a ## CSR Memory Feedback — note yesterday.");
    assert_kept(&format!("Notes first.\n\n{report}"));
    let probe = "<command-message>memory-feedback</command-message>\n<command-name>/memory-feedback</command-name>\nCSR Memory Feedback Probe\nYou are reporting on the quality of the memory context CSR injected in this session.";
    assert_eq!(user(probe).kept, "");
    assert_kept("CSR Memory Feedback Probe is the name of the command.");
}

#[test]
fn structural_channels_are_decided_by_identity_alone() {
    for (channel, reason) in [
        (ContentChannel::CsrToolUse, ContaminationReason::CsrToolUse),
        (
            ContentChannel::CsrToolResult,
            ContaminationReason::CsrToolResult,
        ),
        (ContentChannel::HarnessRow, ContaminationReason::HarnessRow),
    ] {
        for source in [
            SourceKind::Conversation,
            SourceKind::Sidechain,
            SourceKind::CodexRollout,
        ] {
            let cut = remove_csr_emissions(OwnedText {
                source,
                channel,
                text: "plain words",
            });
            assert_eq!((cut.kept.as_str(), cut.removed[0].reason), ("", reason));
        }
        // a plan is a document, never a CSR tool block
        let plan = remove_csr_emissions(OwnedText {
            source: SourceKind::Plan,
            channel,
            text: "plain words",
        });
        assert_eq!(plan.kept, "plain words");
    }
    // nothing to remove from nothing
    assert!(remove_csr_emissions(OwnedText {
        source: SourceKind::Conversation,
        channel: ContentChannel::CsrToolUse,
        text: "",
    })
    .removed
    .is_empty());
}

#[test]
fn blocks_that_follow_one_another_are_cut_together_and_prose_between_stays() {
    let text = format!("{DEGRADED}\n{RECAP}\nmy own line\nCSR: 3 chunks, 1 reflections indexed. No recent sessions for this project.\n");
    let cut = user(&text);
    assert_eq!(cut.kept, "my own line\n");
    assert_eq!(cut.removed.len(), 3, "{:?}", cut.removed);
}

#[test]
fn source_kind_never_defaults_a_missing_source_to_conversation() {
    assert_eq!(SourceKind::from_chunk_source(None), SourceKind::Unknown);
    assert_eq!(SourceKind::from_chunk_source(Some("")), SourceKind::Unknown);
    assert_eq!(
        SourceKind::from_chunk_source(Some("conversation")),
        SourceKind::Conversation
    );
    assert_eq!(
        SourceKind::from_chunk_source(Some("sidechain")),
        SourceKind::Sidechain
    );
    assert_eq!(
        SourceKind::from_chunk_source(Some("codex_rollout")),
        SourceKind::CodexRollout
    );
    assert_eq!(
        SourceKind::from_chunk_source(Some("plan")),
        SourceKind::Plan
    );
    assert_eq!(
        SourceKind::from_chunk_source(Some("telepathy")),
        SourceKind::Unknown
    );
}

// --- stored ownership proof ---

fn event(id: &str, conversation: &str, channel: &str) -> ProvenanceEvent {
    ProvenanceEvent {
        event_id: id.into(),
        conversation_id: conversation.into(),
        message_key: "m1".into(),
        seq: 0,
        channel: channel.into(),
        trust_tier: TrustTier::UserHistory,
        parent_event_id: None,
        receipt_kind: "jsonl".into(),
        receipt_ref: None,
        observed_at: "2026-10-13T00:00:00Z".into(),
    }
}

fn span(chunk: &str, event_id: &str, content: &str) -> ChunkSpan {
    ChunkSpan {
        chunk_id: chunk.into(),
        event_id: event_id.into(),
        start_char: 0,
        end_char: content.chars().count(),
        content_hash: content_hash(content),
    }
}

fn proof(
    source: Option<&str>,
    author: Option<Speaker>,
    spans: &[ChunkSpan],
    events: &[ProvenanceEvent],
    content: &str,
) -> StoredOwnership {
    prove_stored_ownership(StoredChunkEvidence {
        chunk_id: "c1",
        conversation_id: "conv",
        source,
        content,
        aggregate_author: author,
        spans,
        events,
    })
}

#[test]
fn stored_ownership_needs_every_condition() {
    let content = "é CSR engine ready (degraded mode).";
    let e = event("e1", "conv", "user_message");
    let s = span("c1", "e1", content);
    let proven = |source, author, spans: &[ChunkSpan], events: &[ProvenanceEvent]| {
        proof(source, author, spans, events, content)
    };
    assert_eq!(
        proven(
            Some("conversation"),
            Some(Speaker::User),
            std::slice::from_ref(&s),
            std::slice::from_ref(&e)
        ),
        StoredOwnership::Proven(ContentChannel::UserText)
    );
    assert_eq!(
        proven(
            Some("sidechain"),
            None,
            std::slice::from_ref(&s),
            std::slice::from_ref(&e)
        ),
        StoredOwnership::Proven(ContentChannel::UserText)
    );
    let assistant_event = event("e1", "conv", "assistant_message");
    assert_eq!(
        proven(
            Some("conversation"),
            Some(Speaker::Assistant),
            std::slice::from_ref(&s),
            &[assistant_event]
        ),
        StoredOwnership::Proven(ContentChannel::AssistantText)
    );
    // each missing condition leaves the chunk unproven
    let unproven = |outcome: StoredOwnership| matches!(outcome, StoredOwnership::Unproven(_));
    for source in [
        None,
        Some("plan"),
        Some("codex_rollout"),
        Some("unknown"),
        Some(""),
    ] {
        assert!(
            unproven(proven(
                source,
                None,
                std::slice::from_ref(&s),
                std::slice::from_ref(&e)
            )),
            "{source:?}"
        );
    }
    assert!(
        unproven(proven(
            Some("conversation"),
            Some(Speaker::User),
            &[],
            std::slice::from_ref(&e)
        )),
        "no span"
    );
    assert!(
        unproven(proven(
            Some("conversation"),
            None,
            &[s.clone(), s.clone()],
            std::slice::from_ref(&e)
        )),
        "two spans"
    );
    assert!(
        unproven(proven(
            Some("conversation"),
            None,
            std::slice::from_ref(&s),
            &[]
        )),
        "no event"
    );
    assert!(
        unproven(proven(
            Some("conversation"),
            None,
            std::slice::from_ref(&s),
            &[e.clone(), e.clone()]
        )),
        "two events"
    );
    assert!(
        unproven(proven(
            Some("conversation"),
            None,
            std::slice::from_ref(&s),
            &[event("e2", "conv", "user_message")]
        )),
        "span names another event"
    );
    assert!(
        unproven(proven(
            Some("conversation"),
            None,
            std::slice::from_ref(&s),
            &[event("e1", "other", "user_message")]
        )),
        "event in another conversation"
    );
    for channel in [
        "tool_result:Bash",
        "unclassified",
        "codex_user",
        "plan_file",
    ] {
        assert!(
            unproven(proven(
                Some("conversation"),
                None,
                std::slice::from_ref(&s),
                &[event("e1", "conv", channel)]
            )),
            "{channel}"
        );
    }
    let mut short = s.clone();
    short.end_char -= 1;
    assert!(
        unproven(proven(
            Some("conversation"),
            None,
            &[short],
            std::slice::from_ref(&e)
        )),
        "partial span"
    );
    let mut shifted = s.clone();
    shifted.start_char = 1;
    shifted.end_char += 1;
    // a span in the event's coordinates may start past zero; only its length counts
    assert!(!unproven(proven(
        Some("conversation"),
        None,
        &[shifted],
        std::slice::from_ref(&e)
    )));
    let mut altered = s.clone();
    altered.content_hash = content_hash("something else");
    assert!(
        unproven(proven(
            Some("conversation"),
            None,
            &[altered],
            std::slice::from_ref(&e)
        )),
        "hash"
    );
    let mut other_chunk = s.clone();
    other_chunk.chunk_id = "c2".into();
    assert!(unproven(proven(
        Some("conversation"),
        None,
        &[other_chunk],
        std::slice::from_ref(&e)
    )));
    // the author alone proves nothing, and contradicting the event proves less
    assert!(unproven(proven(
        Some("conversation"),
        Some(Speaker::User),
        &[],
        &[]
    )));
    assert!(unproven(proven(
        Some("conversation"),
        Some(Speaker::Assistant),
        std::slice::from_ref(&s),
        std::slice::from_ref(&e)
    )));
    assert!(unproven(proven(
        Some("conversation"),
        Some(Speaker::ToolResult),
        &[s],
        &[e]
    )));
}

#[test]
fn stored_classification_cuts_only_what_a_proven_owner_could_have_emitted() {
    let evidence = |content: &'static str, channel: &str, source: Option<&'static str>| {
        let e = [event("e1", "conv", channel)];
        let s = [span("c1", "e1", content)];
        classify_stored_chunk(StoredChunkEvidence {
            chunk_id: "c1",
            conversation_id: "conv",
            source,
            content,
            aggregate_author: None,
            spans: &s,
            events: &e,
        })
    };
    assert_eq!(
        evidence(DEGRADED, "user_message", Some("conversation")),
        Some(ContaminationReason::DegradedStatus)
    );
    assert_eq!(
        evidence(DEGRADED, "assistant_message", Some("conversation")),
        Some(ContaminationReason::DegradedStatus)
    );
    // proven, but the text is a mention
    assert_eq!(
        evidence(
            "see `CSR engine ready (degraded mode).`",
            "user_message",
            Some("conversation")
        ),
        None
    );
    // eligible text, unproven source
    assert_eq!(evidence(DEGRADED, "user_message", None), None);
    assert_eq!(evidence(DEGRADED, "user_message", Some("plan")), None);
    // a tool result is never counted, even holding every marker
    assert_eq!(
        evidence(RECAP, "tool_result:Bash", Some("conversation")),
        None
    );
    // the same text with nothing to prove it
    assert_eq!(
        classify_stored_chunk(StoredChunkEvidence {
            chunk_id: "c1",
            conversation_id: "conv",
            source: Some("conversation"),
            content: DEGRADED,
            aggregate_author: Some(Speaker::User),
            spans: &[],
            events: &[],
        }),
        None
    );
    // as-if-user reading, used for the unjudged count
    assert_eq!(
        classify_as_user_text(DEGRADED),
        Some(ContaminationReason::DegradedStatus)
    );
    assert_eq!(classify_as_user_text("ordinary prose"), None);
}

#[test]
fn could_contain_emission_is_a_sound_prefilter() {
    for case in crate::eval::contamination::cases() {
        let removes = !remove_csr_emissions(OwnedText {
            source: SourceKind::from_chunk_source(Some(&case.source)),
            channel: ContentChannel::from_name(&case.channel).unwrap(),
            text: &case.input,
        })
        .removed
        .is_empty();
        let structural = matches!(
            ContentChannel::from_name(&case.channel),
            Some(
                ContentChannel::CsrToolUse
                    | ContentChannel::CsrToolResult
                    | ContentChannel::HarnessRow
            )
        );
        if removes && !structural {
            assert!(could_contain_emission(&case.input), "{}", case.id);
        }
    }
    assert!(!could_contain_emission(
        "We fixed the stale HNSW vector by replacing it explicitly."
    ));
}

/// Component classification budget: p95 at most 100 µs on 900-byte
/// components, clean and matching alike. Release builds only.
#[cfg(not(debug_assertions))]
#[test]
fn classification_p95_stays_inside_the_budget() {
    let clean = "We fixed the stale HNSW vector by replacing it explicitly, then reran the suite. "
        .repeat(12);
    let clean = &clean[..900];
    let matching = format!(
        "{}\n\n{}\n\n{}",
        "Please look at this and tell me what you think. ".repeat(6),
        "recap [4m ago]: Fixed the importer. [[CSR:RECAP]]\nSettled: a (b).\nNext: c.",
        "CSR CONTINUUM [4m ago]: Fix\nLAST: x (outcome=success, trust=user_history)\nFull state: csr_reflect_on_past(\"conv_a\")",
    );
    for (label, text) in [("clean", clean.to_string()), ("matching", matching)] {
        let mut samples = Vec::with_capacity(10_000);
        for _ in 0..10_000 {
            let start = std::time::Instant::now();
            let reason = classify_csr_contamination(OwnedText {
                source: SourceKind::Conversation,
                channel: ContentChannel::UserText,
                text: std::hint::black_box(&text),
            });
            samples.push(start.elapsed());
            std::hint::black_box(reason);
        }
        samples.sort_unstable();
        let p95 = samples[samples.len() * 95 / 100];
        assert!(
            p95 <= std::time::Duration::from_micros(100),
            "{label} p95 {p95:?} over the 100 µs budget"
        );
    }
}
