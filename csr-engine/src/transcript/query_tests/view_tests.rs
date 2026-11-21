use super::*;

#[test]
fn queue_operation_enqueue_becomes_a_prompt_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_auxiliary_kinds_fixture(dir.path());
    let out = transcript::run(
        dir.path(),
        &base_req(ViewKind::Prompts, &path.to_string_lossy()),
    );
    assert!(
        out.contains("afplay when muxed, also pick a female voice"),
        "queued instruction text must reach the prompts view, got: {out}"
    );
}

#[test]
fn duplicate_queue_operation_and_attachment_representation_is_deduped_not_doubled() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_auxiliary_kinds_fixture(dir.path());
    let parsed = transcript::parse_transcript(&path).unwrap();
    let occurrences = parsed
        .entries
        .iter()
        .filter(|e| e.text.contains("afplay when muxed"))
        .count();
    assert_eq!(
        occurrences, 1,
        "the same (timestamp, text) enqueue event recorded twice on disk \
             (queue-operation + attachment.queued_command) must appear as ONE entry"
    );
}

#[test]
fn queue_operation_remove_does_not_consume_a_turn() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_auxiliary_kinds_fixture(dir.path());
    let parsed = transcript::parse_transcript(&path).unwrap();
    // Only 3 substantive entries expected: user prompt (turn1),
    // deduped enqueue (turn2), file-history-delta (turn3), away_summary
    // (turn4) = 4 total; `remove` contributes nothing.
    assert_eq!(parsed.entries.len(), 4);
}

#[test]
fn file_history_delta_feeds_the_files_view() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_auxiliary_kinds_fixture(dir.path());
    let out = transcript::run(
        dir.path(),
        &base_req(ViewKind::Files, &path.to_string_lossy()),
    );
    assert!(
        out.contains("/repo/output/make-spot.py"),
        "file-history-delta's trackingPath must reach the files view, got: {out}"
    );
}

#[test]
fn system_away_summary_content_is_preserved_not_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_auxiliary_kinds_fixture(dir.path());
    let parsed = transcript::parse_transcript(&path).unwrap();
    let summary = parsed
        .entries
        .iter()
        .find(|e| e.text.contains("[system:away_summary]"))
        .expect("away_summary entry must exist");
    assert!(
        summary.text.contains("Goal is shipping the radio spot"),
        "away_summary's `content` must be preserved, not just the bare subtype label, got: {}",
        summary.text
    );
}

#[test]
fn metadata_kinds_are_counted_separately_from_true_unrecognized_drift() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_auxiliary_kinds_fixture(dir.path());
    let parsed = transcript::parse_transcript(&path).unwrap();
    // Metadata: deduped queued_command attachment, queue-operation
    // remove, file-history-snapshot, last-prompt = 4.
    assert_eq!(parsed.metadata_entries, 4);
    // Unrecognized: only the genuinely malformed line = 1.
    assert_eq!(parsed.unrecognized_entries, 1);
}

#[test]
fn later_harness_kinds_are_metadata_and_unknown_kinds_stay_drift() {
    let raw = [
        r#"{"type":"atis-latch","atis":"","sessionId":"s1"}"#,
        r#"{"type":"cost-state","totalCostUSD":0.5,"sessionId":"s1"}"#,
        r#"{"type":"session-owner","accountUuid":"a","sessionId":"s1"}"#,
        r#"{"type":"tools-used","tools":["Read"],"sessionId":"s1"}"#,
        r#"{"type":"custom-title","customTitle":"made up","sessionId":"s1"}"#,
        r#"{"type":"api-request","id":"r1","sessionId":"s1"}"#,
        r#"{"type":"api-request-shape","shapeHash":"h","sessionId":"s1"}"#,
        // Holds message content: counted on its own, never an entry, never drift.
        r#"{"type":"api-request-blob","hash":"h","message":{"role":"user","content":[]}}"#,
        r#"{"type":"never-seen-kind","sessionId":"s1"}"#,
    ]
    .join("\n");
    let parsed = transcript::parse_transcript_fragment(&raw);
    assert_eq!(parsed.metadata_entries, 7);
    assert_eq!(parsed.unrecognized_entries, 1);
    assert_eq!(parsed.request_blobs, 1);
    assert!(parsed.entries.is_empty());
}

// ─── parse_transcript ───

#[test]
fn golden_fixture_parses_expected_turns_and_drift_count() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_golden_fixture(dir.path());
    let parsed = transcript::parse_transcript(&path).unwrap();

    assert_eq!(parsed.entries.len(), 9, "9 recognized entries (turns)");
    // The content-less `queue-operation` (no `content` field — pure
    // bookkeeping) and the recognized `last-prompt` metadata line are
    // now `metadata_entries`, NOT `unrecognized_entries` (finding 1:
    // metadata-only kinds must be counted separately from genuine
    // schema drift). Only the malformed JSON line is truly unrecognized.
    assert_eq!(parsed.metadata_entries, 2);
    assert_eq!(parsed.unrecognized_entries, 1);
    assert_eq!(parsed.entries[0].role, Role::User);
    assert_eq!(parsed.entries[0].turn, 1);
    assert_eq!(parsed.entries.last().unwrap().turn, 9);
}

#[test]
fn hostile_text_is_preserved_verbatim_not_executed_or_stripped() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_golden_fixture(dir.path());
    let parsed = transcript::parse_transcript(&path).unwrap();
    assert!(parsed.entries[0]
        .text
        .contains("<script>alert('x')</script>"));
    let turn7 = parsed.entries.iter().find(|e| e.turn == 7).unwrap();
    assert!(turn7.text.contains("DROP TABLE users"));
}

// ─── stats view ───

#[test]
fn stats_view_text_reports_exact_counts() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_golden_fixture(dir.path());
    let out = transcript::run(
        dir.path(),
        &base_req(ViewKind::Stats, &path.to_string_lossy()),
    );
    assert!(out.contains("<lines total=\"12\" metadata=\"2\" unrecognized=\"1\"/>"));
    assert!(out.contains("<turns total=\"9\" user=\"4\" assistant=\"4\" system=\"1\"/>"));
    assert!(out.contains("<tool_errors count=\"1\"/>"));
    assert!(
        out.contains("<note>unrecognized_entries: 1 (schema drift? check aux_schema_miss)</note>")
    );
}

#[test]
fn stats_view_json_round_trips_and_matches_fixture_facts() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_golden_fixture(dir.path());
    let mut req = base_req(ViewKind::Stats, &path.to_string_lossy());
    req.json = true;
    let out = transcript::run(dir.path(), &req);
    let v: Value = serde_json::from_str(&out).expect("stats --json must be valid JSON");
    assert_eq!(v["turn_count"], json!(9));
    assert_eq!(v["metadata_entries"], json!(2));
    assert_eq!(v["unrecognized_entries"], json!(1));
    assert_eq!(v["tool_errors"], json!(1));
    assert_eq!(v["user_turns"], json!(4));
    assert_eq!(v["assistant_turns"], json!(4));
    assert_eq!(v["system_turns"], json!(1));
    assert_eq!(v["tool_calls"]["Read"], json!(1));
    assert_eq!(v["tool_calls"]["Edit"], json!(1));
    assert_eq!(v["tool_calls"]["Bash"], json!(1));
    assert_eq!(v["tool_calls"]["Grep"], json!(1));
}

/// (i) default (human-only) view shows exactly the 5 genuine human
/// prompts. (ii) header counts are exact per class.
#[test]
fn prompts_view_default_hides_noise_and_reports_exact_hidden_counts() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_prompt_noise_fixture(dir.path());
    let mut req = base_req(ViewKind::Prompts, &path.to_string_lossy());
    req.json = true;
    let out = transcript::run(dir.path(), &req);
    let v: Value = serde_json::from_str(&out).unwrap();

    let texts: Vec<String> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        texts,
        vec![
            "do the first real thing",
            "legacy human turn, no origin field",
            "[queued] go build the widget",
            "[queued] ship the release notes",
            "/codex:rescue fix the bug",
        ],
        "got: {out}"
    );

    let hidden = &v["hidden"];
    assert_eq!(hidden["task_notification"], json!(2));
    assert_eq!(hidden["queued_duplicate"], json!(1));
    assert_eq!(hidden["local_command"], json!(2));
    assert_eq!(hidden["slash_command_no_args"], json!(1));
    assert_eq!(hidden["compaction_summary"], json!(1));
    assert_eq!(hidden["interrupt_marker"], json!(1));
    assert_eq!(hidden["cross_session_message"], json!(1));
    assert_eq!(hidden["skill_body"], json!(1));
    assert_eq!(hidden["meta"], json!(1));
    assert_eq!(hidden.get("other_origin"), None);
    assert_eq!(v["include_system"], json!(false));
}

/// (iii) `include_system=true` reproduces the pre-change output: every
/// non-empty user entry, unfiltered, no hidden map populated, and (review
/// finding, high) the RAW `e.text` — no `command_display_override`
/// rewrite. The `/codex:rescue` fixture line in particular must keep its
/// raw `<command-name>...</command-args>` text, not `/codex:rescue fix
/// the bug`.
#[test]
fn prompts_view_include_system_reproduces_every_user_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_prompt_noise_fixture(dir.path());
    let mut req = base_req(ViewKind::Prompts, &path.to_string_lossy());
    req.json = true;
    req.include_system = true;
    let out = transcript::run(dir.path(), &req);
    let v: Value = serde_json::from_str(&out).unwrap();

    let texts: Vec<String> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        texts,
        vec![
            "do the first real thing",
            "legacy human turn, no origin field",
            "[queued] <task-notification>background task finished</task-notification>",
            "<task-notification>background task finished</task-notification>",
            "[queued] go build the widget",
            "go build the widget",
            "[queued] ship the release notes",
            "Base directory for this skill: /Users/x/.claude/skills/foo",
            "Another Claude session sent a message: <cross-session-message from=\"other\">check on the build</cross-session-message>",
            "<local-command-caveat>Caveat: the messages below were generated by the user while running local commands.</local-command-caveat>",
            "<command-name>/compact</command-name>\n<command-message>compact</command-message>\n<command-args></command-args>",
            "<local-command-stdout>Compacted conversation summary.</local-command-stdout>",
            "<command-name>/codex:rescue</command-name>\n<command-message>codex:rescue</command-message>\n<command-args>fix the bug</command-args>",
            "This session is being continued from a previous conversation that ran out of context.",
            "[Request interrupted by user]",
            "[Image: source: clipboard.png]",
        ],
        "include_system=true must reproduce raw e.text unrewritten, got: {out}"
    );
    assert_eq!(v["include_system"], json!(true));
    assert_eq!(v["hidden"], json!({}));

    let text_out = transcript::run(
        dir.path(),
        &TranscriptRequest {
            include_system: true,
            ..base_req(ViewKind::Prompts, &path.to_string_lossy())
        },
    );
    assert!(!text_out.contains("hidden:"));
}

/// (medium review finding) a human prompt typed twice with NEITHER copy
/// queued must not be deduped — dedupe only pairs a queued copy with its
/// delivered twin.
#[test]
fn prompts_view_repeated_non_queued_human_prompt_both_survive() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("repeated_human.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(
        f,
        r#"{{"type":"user","timestamp":"2026-09-22T00:15:00Z","origin":{{"kind":"human"}},"message":{{"role":"user","content":"proceed"}}}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"user","timestamp":"2026-09-22T05:15:00Z","origin":{{"kind":"human"}},"message":{{"role":"user","content":"proceed"}}}}"#
    )
    .unwrap();
    drop(f);

    let mut req = base_req(ViewKind::Prompts, &path.to_string_lossy());
    req.json = true;
    let out = transcript::run(dir.path(), &req);
    let v: Value = serde_json::from_str(&out).unwrap();
    let texts: Vec<String> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        texts,
        vec!["proceed", "proceed"],
        "two non-queued identical human prompts must both survive, got: {out}"
    );
    assert_eq!(v["hidden"].get("queued_duplicate"), None, "got: {out}");
}

/// (low review finding) a queued slash command with args keeps its
/// `[queued] ` marker in default mode, same as every other queued human
/// prompt — it must not silently lose the marker just because
/// `command_display_override` rewrote the underlying text.
#[test]
fn prompts_view_queued_command_with_args_keeps_queued_marker() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("queued_command.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(
        f,
        r#"{{"type":"queue-operation","operation":"enqueue","timestamp":"2026-09-22T00:20:00Z","content":"<command-name>/codex:rescue</command-name>\n<command-args>fix the bug</command-args>"}}"#
    )
    .unwrap();
    drop(f);

    let mut req = base_req(ViewKind::Prompts, &path.to_string_lossy());
    req.json = true;
    let out = transcript::run(dir.path(), &req);
    let v: Value = serde_json::from_str(&out).unwrap();
    let texts: Vec<String> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        texts,
        vec!["[queued] /codex:rescue fix the bug"],
        "queued command-with-args must keep its [queued] marker, got: {out}"
    );
}

/// (iv) surviving items' turn numbers must equal their turn numbers
/// under `include_system=true` — filtering never renumbers turns, and
/// (inbox-B3 review #2 finding 1c/2c) a surviving turn's default-view
/// text must actually derive from THAT SAME turn's raw text, not from
/// an already-emitted item that merely absorbed this turn's number
/// during dedupe. A mutation that reattaches an emitted queued item's
/// turn number to its consumed twin (`it.turn = e.turn`) passes a
/// plain set-membership check but fails this one — verified manually
/// (see git history / PR description) and restored.
#[test]
fn prompts_view_turn_numbers_stable_across_include_system() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_prompt_noise_fixture(dir.path());

    let mut full_req = base_req(ViewKind::Prompts, &path.to_string_lossy());
    full_req.json = true;
    full_req.include_system = true;
    let full_out = transcript::run(dir.path(), &full_req);
    let full_v: Value = serde_json::from_str(&full_out).unwrap();
    let full_by_turn: HashMap<u64, String> = full_v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["turn"].as_u64().unwrap(),
                i["text"].as_str().unwrap().to_string(),
            )
        })
        .collect();

    let mut default_req = base_req(ViewKind::Prompts, &path.to_string_lossy());
    default_req.json = true;
    let default_out = transcript::run(dir.path(), &default_req);
    let default_v: Value = serde_json::from_str(&default_out).unwrap();
    let default_items = default_v["items"].as_array().unwrap();

    // Pin the fixture's exact surviving turn list — a regression that
    // hides/reveals a different noise class shifts this list, which
    // would otherwise silently defeat the per-turn checks below.
    let turns: Vec<u64> = default_items
        .iter()
        .map(|i| i["turn"].as_u64().unwrap())
        .collect();
    assert_eq!(
        turns,
        vec![1, 2, 5, 7, 13],
        "surviving turn list changed, got: {default_out}"
    );

    const QUEUED_PREFIX: &str = "[queued] ";
    for item in default_items {
        let turn = item["turn"].as_u64().unwrap();
        let default_text = item["text"].as_str().unwrap();
        let full_text = full_by_turn.get(&turn).unwrap_or_else(|| {
            panic!(
                "turn {turn} in default view must also appear under include_system=true, got: {default_out}"
            )
        });
        // Reproduce render_prompts' own display_text derivation from
        // this turn's raw include_system text, then require it match
        // what the default view actually rendered for the SAME turn —
        // this is turn-to-text, not just turn membership.
        let stripped = crate::transcript::strip_queued_prefix(full_text);
        let expected = match command_display_override(stripped) {
            Some(over) if full_text.starts_with(QUEUED_PREFIX) => {
                format!("{QUEUED_PREFIX}{over}")
            }
            Some(over) => over,
            None => full_text.clone(),
        };
        assert_eq!(
            default_text, expected,
            "turn {turn}: default-view text does not derive from this turn's own include_system text, got: {default_out}"
        );
    }
}

/// (v) the other views' JSON output on this fixture carries no new keys
/// — the new `Entry` fields (`origin_kind`/`is_meta`/`is_compact_summary`)
/// must not leak into any view that doesn't serialize `Entry` directly.
#[test]
fn other_views_json_unaffected_by_new_entry_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_prompt_noise_fixture(dir.path());
    for view in [
        ViewKind::Slice,
        ViewKind::Grep,
        ViewKind::Tools,
        ViewKind::Files,
        ViewKind::Errors,
        ViewKind::Stats,
    ] {
        let mut req = base_req(view, &path.to_string_lossy());
        req.json = true;
        if view == ViewKind::Grep {
            req.grep = Some("thing".to_string());
        }
        let out = transcript::run(dir.path(), &req);
        assert!(
            !out.contains("origin_kind")
                && !out.contains("is_meta")
                && !out.contains("is_compact_summary"),
            "new Entry field leaked into {view:?} JSON: {out}"
        );
    }
}

/// (vi) `--turns` range narrowing changes the hidden counts accordingly
/// — counts must reflect only the requested range, not the whole file.
#[test]
fn prompts_view_turns_range_narrows_hidden_counts() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_prompt_noise_fixture(dir.path());

    // Turns 1..4 cover only the typed human + legacy human + the
    // task-notification enqueue/delivered pair (turns 1-4 in file
    // order) — no local_command/compaction_summary/etc noise yet.
    let mut req = base_req(ViewKind::Prompts, &path.to_string_lossy());
    req.json = true;
    req.turns = Some((1, Some(4)));
    let out = transcript::run(dir.path(), &req);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["hidden"]["task_notification"], json!(2));
    assert_eq!(v["hidden"].get("local_command"), None);
    assert_eq!(v["hidden"].get("compaction_summary"), None);
    assert_eq!(v["items"].as_array().unwrap().len(), 2, "got: {out}");
}

// ─── gap-G6: transcript prompts residuals (inbox-B3 review #2) ───

/// (finding 1a) a queued prompt whose text was withdrawn via a
/// `queue-operation remove` must NOT stay armed to swallow a later,
/// unrelated delivered prompt that happens to share its exact wording
/// — even when the two are dozens of turns apart.
#[test]
fn prompts_view_removed_queued_text_does_not_hide_a_later_unrelated_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("removed_then_repeated.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();

    writeln!(
        f,
        r#"{{"type":"user","timestamp":"2026-09-22T00:00:00Z","origin":{{"kind":"human"}},"message":{{"role":"user","content":"start"}}}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"queue-operation","operation":"enqueue","timestamp":"2026-09-22T00:00:01Z","content":"yes"}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"queue-operation","operation":"remove","timestamp":"2026-09-22T00:00:02Z","content":"yes"}}"#
    )
    .unwrap();
    // 50 unrelated turns between the withdrawal and the later real
    // "yes" — task-notifications specifically (noise-classified, never
    // disarm anything on their own) so this test isolates the
    // `remove`-disarms-its-own-content mechanism from the separate
    // "a genuine non-matching human turn disarms the rest" rule (which
    // would otherwise also mask this bug after the first ordinary
    // human turn).
    for n in 0..50 {
        writeln!(
            f,
            r#"{{"type":"assistant","timestamp":"2026-09-22T00:01:{n:02}Z","message":{{"role":"assistant","content":[{{"type":"text","text":"working on it {n}"}}]}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"user","timestamp":"2026-09-22T00:02:{n:02}Z","origin":{{"kind":"task-notification"}},"message":{{"role":"user","content":"<task-notification>bg task {n} finished</task-notification>"}}}}"#
        )
        .unwrap();
    }
    writeln!(
        f,
        r#"{{"type":"user","timestamp":"2026-09-22T00:03:00Z","origin":{{"kind":"human"}},"message":{{"role":"user","content":"yes"}}}}"#
    )
    .unwrap();
    drop(f);

    let mut req = base_req(ViewKind::Prompts, &path.to_string_lossy());
    req.json = true;
    let out = transcript::run(dir.path(), &req);
    let v: Value = serde_json::from_str(&out).unwrap();
    let texts: Vec<String> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        texts.iter().filter(|t| *t == "yes").count(),
        1,
        "the real, later 'yes' must be shown once — the withdrawn queued copy must not hide it, got: {out}"
    );
    assert_eq!(v["hidden"].get("queued_duplicate"), None, "got: {out}");
}

/// (finding 1a / real-corpus fix) two queued prompts enqueued back to
/// back must EACH dedupe against their own delivered twin even when
/// the twins arrive interleaved with an unrelated real prompt in
/// between and out of enqueue order at the text level — mirrors the
/// live 585b0176 turn 2384/2385 shape.
#[test]
fn prompts_view_two_queued_fifo_prompts_both_dedupe_across_an_intervening_match() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fifo_two_queued.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();

    writeln!(
        f,
        r#"{{"type":"queue-operation","operation":"enqueue","timestamp":"2026-09-22T00:00:00Z","content":"do the first thing"}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"queue-operation","operation":"enqueue","timestamp":"2026-09-22T00:00:01Z","content":"do the second thing"}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"assistant","timestamp":"2026-09-22T00:00:02Z","message":{{"role":"assistant","content":[{{"type":"text","text":"ack"}}]}}}}"#
    )
    .unwrap();
    // the FIRST queued item's twin arrives first — must consume only
    // that one, leaving the second still armed.
    writeln!(
        f,
        r#"{{"type":"user","timestamp":"2026-09-22T00:00:03Z","origin":{{"kind":"human"}},"message":{{"role":"user","content":"do the first thing"}}}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"assistant","timestamp":"2026-09-22T00:00:04Z","message":{{"role":"assistant","content":[{{"type":"text","text":"ack again"}}]}}}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"user","timestamp":"2026-09-22T00:00:05Z","origin":{{"kind":"human"}},"message":{{"role":"user","content":"do the second thing"}}}}"#
    )
    .unwrap();
    drop(f);

    let mut req = base_req(ViewKind::Prompts, &path.to_string_lossy());
    req.json = true;
    let out = transcript::run(dir.path(), &req);
    let v: Value = serde_json::from_str(&out).unwrap();
    let texts: Vec<String> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        texts,
        vec![
            "[queued] do the first thing",
            "[queued] do the second thing"
        ],
        "both queued prompts must dedupe against their own twin, got: {out}"
    );
    assert_eq!(v["hidden"]["queued_duplicate"], json!(2), "got: {out}");
}

/// (finding 1d) a page fetched with `--turns N..` must not re-show a
/// delivered twin whose queued copy landed on an earlier page.
#[test]
fn prompts_view_range_resume_has_no_duplicate_across_pages() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("range_resume.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();

    writeln!(
        f,
        r#"{{"type":"user","timestamp":"2026-09-22T00:00:00Z","origin":{{"kind":"human"}},"message":{{"role":"user","content":"start"}}}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"queue-operation","operation":"enqueue","timestamp":"2026-09-22T00:00:01Z","content":"go build it"}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"assistant","timestamp":"2026-09-22T00:00:02Z","message":{{"role":"assistant","content":[{{"type":"text","text":"ok"}}]}}}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"user","timestamp":"2026-09-22T00:00:03Z","origin":{{"kind":"human"}},"message":{{"role":"user","content":"go build it"}}}}"#
    )
    .unwrap();
    drop(f);

    // Page 1: turns 1..2 — shows the queued placeholder.
    let mut page1 = base_req(ViewKind::Prompts, &path.to_string_lossy());
    page1.json = true;
    page1.turns = Some((1, Some(2)));
    let out1 = transcript::run(dir.path(), &page1);
    let v1: Value = serde_json::from_str(&out1).unwrap();

    // Page 2: turns 3.. — must recognize turn 4 as the queued item's
    // twin (pre-scanned from turns 1-2) and hide it as a duplicate,
    // not show it as a fresh prompt.
    let mut page2 = base_req(ViewKind::Prompts, &path.to_string_lossy());
    page2.json = true;
    page2.turns = Some((3, None));
    let out2 = transcript::run(dir.path(), &page2);
    let v2: Value = serde_json::from_str(&out2).unwrap();

    assert_eq!(
        v2["hidden"]["queued_duplicate"],
        json!(1),
        "page 2 must recognize turn 4 as the page-1 queued item's twin, got: {out2}"
    );

    let mut all_texts: Vec<String> = v1["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(v2["items"].as_array().unwrap())
        .map(|i| crate::transcript::strip_queued_prefix(i["text"].as_str().unwrap()).to_string())
        .collect();
    all_texts.sort();
    let mut deduped = all_texts.clone();
    deduped.dedup();
    assert_eq!(
        all_texts, deduped,
        "no prompt text should appear on both pages, got page1={out1} page2={out2}"
    );
}

/// (finding 1b) a bare slash command with no arguments (e.g. `/compact`
/// typed directly, not via the `<command-name>` echo shape) must be
/// hidden as `slash_command_no_args`, not shown as a genuine human
/// prompt.
#[test]
fn prompts_view_bare_slash_command_is_hidden_as_slash_command_no_args() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bare_slash.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(
        f,
        r#"{{"type":"user","timestamp":"2026-09-22T00:00:00Z","origin":{{"kind":"human"}},"message":{{"role":"user","content":"/compact"}}}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"user","timestamp":"2026-09-22T00:00:01Z","origin":{{"kind":"human"}},"message":{{"role":"user","content":"/compact  "}}}}"#
    )
    .unwrap();
    drop(f);

    let mut req = base_req(ViewKind::Prompts, &path.to_string_lossy());
    req.json = true;
    let out = transcript::run(dir.path(), &req);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["items"].as_array().unwrap().len(), 0, "got: {out}");
    assert_eq!(v["hidden"]["slash_command_no_args"], json!(2), "got: {out}");
}

#[test]
fn prompts_view_returns_only_user_text_with_turn_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_golden_fixture(dir.path());
    let out = transcript::run(
        dir.path(),
        &base_req(ViewKind::Prompts, &path.to_string_lossy()),
    );
    assert!(out.contains("[turn 1]"));
    assert!(out.contains("Fix the"));
    // turn 3, 5 are tool_result-only user entries — must NOT appear as prompts
    assert!(!out.contains("[turn 3]"));
    assert!(!out.contains("[turn 5]"));
    // turn 7 has genuine user text alongside a tool_result — must appear
    assert!(out.contains("[turn 7]"));
}

#[test]
fn yourasks_cuts_csr_own_lines_pasted_among_a_persons_words() {
    for line in own_lines() {
        let raw = format!("Please explain this:\n{line}\n<system-reminder>x</system-reminder>");
        let ask = ask_of(&raw).expect("the person's words stay");
        assert_eq!(ask.text, "Please explain this:", "{line}");
        // Words after the line stay too, and the other order works.
        let raw = format!("{line}\nWhy did it stop?\n<system-reminder>x</system-reminder>");
        assert_eq!(ask_of(&raw).unwrap().text, "Why did it stop?", "{line}");
        // CRLF line ends.
        let raw = format!("Look:\r\n{line}\r\n<system-reminder>x</system-reminder>");
        assert_eq!(ask_of(&raw).unwrap().text.trim(), "Look:", "{line}");
        // The line alone, or with only wrapper tags, is CSR's send.
        assert_eq!(ask_of(&line).err(), Some("csr_sent"), "{line}");
        let raw = format!("{line}\n<system-reminder>x</system-reminder>");
        assert_eq!(ask_of(&raw).err(), Some("csr_sent"), "{line}");
    }
    // A line with a person's word in front of it is not CSR's line.
    let line = &own_lines()[0];
    let spoken = format!("My note: {line}\n<system-reminder>x</system-reminder>");
    assert!(ask_of(&spoken).unwrap().text.starts_with("My note:"));
    // Text with none of CSR's lines is read as before.
    assert_eq!(
        ask_of("Fix it.\n<system-reminder>x</system-reminder>")
            .unwrap()
            .text,
        "Fix it."
    );
    assert_eq!(
        ask_of("<system-reminder>x</system-reminder>").err(),
        Some("harness_only")
    );
}

#[test]
fn yourasks_keeps_the_humans_words_and_hides_the_rest_by_class() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_asks_fixture(dir.path());
    let v = asks_json(dir.path(), &path, ViewKind::YourAsks);
    assert_eq!(v["view"], "yourasks");
    let items = ask_items(&v);
    let texts: Vec<(u64, &str, bool, bool)> = items
        .iter()
        .map(|(t, s, q, p)| (*t, s.as_str(), *q, *p))
        .collect();
    assert_eq!(
        texts,
        vec![
            // A bare acknowledgement is still the human's word.
            (1, "ok go", false, false),
            // An all-paste message keeps the pasted words.
            (2, "spawn a helper to tidy the widget", false, true),
            // Typed words plus a paste: the typed words only.
            (3, "check this and fix it", false, true),
            // Talk about CSR is not CSR's send.
            (
                6,
                "why did CSR compacted at show twice in the pane?",
                false,
                false
            ),
            // The queued prompt stays, once, marked; its delivered twin is a duplicate.
            (7, "also rename the helper", true, false),
            // Remote Control counts.
            (10, "from my phone: run it again", false, false),
            // A handoff the person added words to keeps those words.
            (11, "and check the hub first", false, false),
        ]
    );
    assert_eq!(
        v["hidden"],
        json!({
            "queued_duplicate": 1,
            "interrupt_marker": 1,
            "task_notification": 1,
            "harness_only": 1,
            "csr_sent": 1,
        })
    );
}

#[test]
fn yourasks_text_form_carries_turn_timestamp_queued_marker_and_hidden_header() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_asks_fixture(dir.path());
    let out = transcript::run(
        dir.path(),
        &base_req(ViewKind::YourAsks, &path.to_string_lossy()),
    );
    assert!(out.starts_with("<transcript_yourasks>\n"), "got: {out}");
    assert!(
        out.contains("hidden: task_notification=1 queued_duplicate=1 interrupt_marker=1 harness_only=1 csr_sent=1\n"),
        "got: {out}"
    );
    assert!(!out.contains("include_system"), "got: {out}");
    assert!(
        out.contains("[turn 1] 2026-08-10T10:00:01Z: ok go\n"),
        "got: {out}"
    );
    assert!(
        out.contains("[turn 7] 2026-08-10T10:00:07Z: [queued] also rename the helper\n"),
        "got: {out}"
    );
    assert!(!out.contains("a made-up reminder body"), "got: {out}");
    assert!(!out.contains("Lost and found"), "got: {out}");
}

#[test]
fn yourasks_turn_numbers_match_the_prompts_view() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_asks_fixture(dir.path());
    let asks: Vec<u64> = ask_items(&asks_json(dir.path(), &path, ViewKind::YourAsks))
        .iter()
        .map(|i| i.0)
        .collect();
    let prompts: Vec<u64> = asks_json(dir.path(), &path, ViewKind::Prompts)["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["turn"].as_u64().unwrap())
        .collect();
    // `prompts` shows the reminder (4) and the handoff (5) and hides the
    // Remote Control prompt (10) as other_origin; `yourasks` differs only
    // by those three.
    assert_eq!(prompts, vec![1, 2, 3, 4, 5, 6, 7, 11]);
    assert_eq!(asks, vec![1, 2, 3, 6, 7, 10, 11]);
    for turn in &asks {
        if *turn != 10 {
            assert!(prompts.contains(turn), "turn {turn} must match prompts");
        }
    }
}

#[test]
fn yourasks_ignores_include_system() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_asks_fixture(dir.path());
    let mut req = base_req(ViewKind::YourAsks, &path.to_string_lossy());
    req.json = true;
    let plain = transcript::run(dir.path(), &req);
    req.include_system = true;
    assert_eq!(transcript::run(dir.path(), &req), plain);
}

#[test]
fn yourasks_range_start_still_dedupes_a_queued_twin_from_an_earlier_page() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_asks_fixture(dir.path());
    let mut req = base_req(ViewKind::YourAsks, &path.to_string_lossy());
    req.json = true;
    req.turns = Some((8, None));
    let v: Value = serde_json::from_str(&transcript::run(dir.path(), &req)).unwrap();
    let turns: Vec<u64> = ask_items(&v).iter().map(|i| i.0).collect();
    assert_eq!(turns, vec![10, 11], "the twin at turn 8 stays hidden");
}

#[test]
fn yourasks_view_names_parse() {
    assert_eq!(ViewKind::parse("yourasks"), Some(ViewKind::YourAsks));
    assert_eq!(ViewKind::parse("your_asks"), Some(ViewKind::YourAsks));
    assert_eq!(ViewKind::YourAsks.as_str(), "yourasks");
}

#[test]
fn yourasks_view_resume_has_no_overlap_no_gap() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_pagination_fixture(dir.path());
    assert_pagination_resume_has_no_overlap_no_gap(dir.path(), &path, ViewKind::YourAsks, 200);
}

// ─── tools view ───

#[test]
fn tools_view_pairs_tool_use_with_its_later_tool_result() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_golden_fixture(dir.path());
    let out = transcript::run(
        dir.path(),
        &base_req(ViewKind::Tools, &path.to_string_lossy()),
    );
    assert!(out.contains("[turn 2] Read file_path=/repo/src/auth.rs -> ok (13 bytes)"));
    assert!(
        out.contains("[turn 4] Edit file_path=/repo/src/auth.rs command=n/a -> error (28 bytes)")
    );
    assert!(out.contains("[turn 6] Grep pattern=fn login -> pending (no matching tool_result)"));
}

#[test]
fn tools_view_tool_filter_narrows_to_named_tool_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_golden_fixture(dir.path());
    let mut req = base_req(ViewKind::Tools, &path.to_string_lossy());
    req.tool = Some("Edit".to_string());
    let out = transcript::run(dir.path(), &req);
    assert!(out.contains("Edit"));
    assert!(!out.contains("Read file_path"));
    assert!(!out.contains("Bash"));
}

// ─── files view ───

#[test]
fn files_view_aggregates_distinct_paths_with_turn_span() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_golden_fixture(dir.path());
    let out = transcript::run(
        dir.path(),
        &base_req(ViewKind::Files, &path.to_string_lossy()),
    );
    assert!(out.contains("/repo/src/auth.rs: 2 touches (turns 2-4)"));
}

// ─── errors view ───

#[test]
fn errors_view_lists_only_error_tool_results_with_owning_tool_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_golden_fixture(dir.path());
    let out = transcript::run(
        dir.path(),
        &base_req(ViewKind::Errors, &path.to_string_lossy()),
    );
    assert!(out.contains("[turn 5] Edit failed: old_string not found in file"));
    assert!(!out.contains("Read failed"));
    assert!(!out.contains("Bash failed"));
}

// ─── slice + grep composition ───

#[test]
fn grep_returns_turn_then_slice_of_that_turn_matches() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_golden_fixture(dir.path());

    let mut grep_req = base_req(ViewKind::Grep, &path.to_string_lossy());
    grep_req.grep = Some("DROP TABLE".to_string());
    let grep_out = transcript::run(dir.path(), &grep_req);
    assert!(grep_out.contains("[turn 7]"));

    let mut slice_req = base_req(ViewKind::Slice, &path.to_string_lossy());
    slice_req.turns = Some((7, Some(7)));
    let slice_out = transcript::run(dir.path(), &slice_req);
    assert!(slice_out.contains("[turn 7]"));
    assert!(slice_out.contains("DROP TABLE"));
    assert!(!slice_out.contains("[turn 6]"));
    assert!(!slice_out.contains("[turn 8]"));
}

#[test]
fn grep_invalid_regex_reports_honest_error_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_golden_fixture(dir.path());
    let mut req = base_req(ViewKind::Grep, &path.to_string_lossy());
    req.grep = Some("(unclosed".to_string());
    let out = transcript::run(dir.path(), &req);
    assert!(out.contains("invalid --grep regex"));
}
