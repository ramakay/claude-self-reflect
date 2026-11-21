//! `replaced_turns`: the turn range a compact's summary replaced.

use super::*;
use std::io::Write;

fn user(n: usize) -> String {
    format!(
        r#"{{"type":"user","timestamp":"2026-11-01T10:00:{n:02}Z","uuid":"u{n}","message":{{"role":"user","content":"question number {n}"}}}}"#
    )
}

fn assistant(n: usize) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"2026-11-01T10:00:{n:02}Z","uuid":"a{n}","message":{{"role":"assistant","content":[{{"type":"text","text":"answer number {n}"}}]}}}}"#
    )
}

fn boundary(n: usize) -> String {
    format!(
        r#"{{"type":"system","subtype":"compact_boundary","timestamp":"2026-11-01T10:00:{n:02}Z","uuid":"b{n}","content":"Conversation compacted"}}"#
    )
}

/// The user row Claude Code writes after a compact boundary.
fn summary(n: usize) -> String {
    format!(
        r#"{{"type":"user","timestamp":"2026-11-01T10:00:{n:02}Z","uuid":"s{n}","isCompactSummary":true,"message":{{"role":"user","content":"This session is being continued from a previous conversation. Summary number {n}."}}}}"#
    )
}

/// A harness attachment row. It is metadata and takes no turn number.
fn attachment() -> String {
    r#"{"type":"attachment","timestamp":"2026-11-01T10:00:00Z","attachment":{"type":"hook_success","hookName":"SessionStart"}}"#
        .to_string()
}

fn noise() -> String {
    r#"{"type":"queue-operation","operation":"dequeue","timestamp":"2026-11-01T10:00:00Z"}"#
        .to_string()
}

fn write(dir: &Path, name: &str, lines: &[String]) -> std::path::PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    for line in lines {
        writeln!(f, "{line}").unwrap();
    }
    path
}

/// The turns a plain `slice` of `range` shows, read from its text output.
fn sliced_turns(path: &Path, range: (usize, usize)) -> Vec<usize> {
    let req = TranscriptRequest {
        session: path.to_string_lossy().into_owned(),
        view: ViewKind::Slice,
        turns: Some((range.0, Some(range.1))),
        ..TranscriptRequest::default()
    };
    run(path.parent().unwrap(), &req)
        .lines()
        .filter_map(|l| l.strip_prefix("[turn "))
        .filter_map(|l| l.split(']').next()?.parse().ok())
        .collect()
}

#[test]
fn no_boundary_covers_the_whole_file() {
    let dir = tempfile::tempdir().unwrap();
    let lines = vec![user(1), noise(), assistant(2), user(3), assistant(4)];
    let path = write(dir.path(), "a.jsonl", &lines);
    assert_eq!(replaced_turns(&path), Some((1, 4)));
}

#[test]
fn a_boundary_with_answers_after_it_starts_the_range_after_it() {
    let dir = tempfile::tempdir().unwrap();
    // turns: 1 user, 2 assistant, 3 boundary, 4 user, 5 assistant, 6 user
    let lines = vec![
        user(1),
        assistant(2),
        boundary(3),
        user(4),
        assistant(5),
        user(6),
    ];
    let path = write(dir.path(), "b.jsonl", &lines);
    assert_eq!(replaced_turns(&path), Some((4, 6)));
}

#[test]
fn a_boundary_with_no_assistant_after_it_is_this_compact() {
    let dir = tempfile::tempdir().unwrap();
    // turns: 1 user, 2 assistant, 3 user, 4 boundary, 5 summary user
    let lines = vec![user(1), assistant(2), user(3), boundary(4), summary(5)];
    let path = write(dir.path(), "c.jsonl", &lines);
    assert_eq!(replaced_turns(&path), Some((1, 3)));
}

#[test]
fn harness_rows_after_the_boundary_do_not_make_it_old() {
    let dir = tempfile::tempdir().unwrap();
    let local_command = r#"{"type":"user","uuid":"lc","isMeta":true,"message":{"role":"user","content":"<local-command-caveat>Caveat: made-up note.</local-command-caveat>"}}"#;
    let system_row = r#"{"type":"system","subtype":"stop_hook_summary","uuid":"sh"}"#;
    // 1 user, 2 assistant, 3 user, 4 boundary, 5 summary, 6 caveat, 7 system row
    let lines = vec![
        user(1),
        assistant(2),
        user(3),
        boundary(4),
        attachment(),
        summary(5),
        noise(),
        local_command.to_string(),
        system_row.to_string(),
    ];
    let path = write(dir.path(), "c2.jsonl", &lines);
    assert_eq!(replaced_turns(&path), Some((1, 3)));
}

#[test]
fn a_human_prompt_after_the_summary_makes_the_boundary_old() {
    let dir = tempfile::tempdir().unwrap();
    // 1 user, 2 assistant, 3 previous boundary, 4 summary, 5 large user prompt.
    // The compact has not written its boundary yet, so the range is 4..5.
    let lines = vec![user(1), assistant(2), boundary(3), summary(4), user(5)];
    let path = write(dir.path(), "c3.jsonl", &lines);
    assert_eq!(replaced_turns(&path), Some((4, 5)));
}

/// A prompt typed in Remote Control (origin `bridge`).
fn remote_prompt(n: usize) -> String {
    format!(
        r#"{{"type":"user","timestamp":"2026-11-01T10:00:{n:02}Z","uuid":"r{n}","origin":{{"kind":"bridge"}},"message":{{"role":"user","content":"phone prompt number {n}"}}}}"#
    )
}

#[test]
fn a_remote_control_prompt_after_the_summary_makes_the_boundary_old() {
    let dir = tempfile::tempdir().unwrap();
    // 1 user, 2 assistant, 3 boundary, 4 summary, 5 prompt from the phone
    let lines = vec![
        user(1),
        assistant(2),
        boundary(3),
        summary(4),
        remote_prompt(5),
    ];
    let path = write(dir.path(), "c5.jsonl", &lines);
    assert_eq!(replaced_turns(&path), Some((4, 5)));
}

#[test]
fn a_queued_human_prompt_after_the_boundary_makes_it_old() {
    let dir = tempfile::tempdir().unwrap();
    let queued = r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-11-01T10:00:09Z","content":"please look at the second file"}"#;
    // 1 user, 2 assistant, 3 boundary, 4 summary, 5 queued prompt
    let lines = vec![
        user(1),
        assistant(2),
        boundary(3),
        summary(4),
        queued.to_string(),
    ];
    let path = write(dir.path(), "c4.jsonl", &lines);
    assert_eq!(replaced_turns(&path), Some((4, 5)));
}

#[test]
fn two_boundaries_use_the_one_before_when_the_newest_has_no_answer() {
    let dir = tempfile::tempdir().unwrap();
    // 1 user, 2 assistant, 3 boundary, 4 user, 5 assistant, 6 user, 7 boundary
    let lines = vec![
        user(1),
        assistant(2),
        boundary(3),
        user(4),
        assistant(5),
        user(6),
        boundary(7),
    ];
    let path = write(dir.path(), "d.jsonl", &lines);
    assert_eq!(replaced_turns(&path), Some((4, 6)));
}

#[test]
fn two_boundaries_with_answers_after_the_newest_start_after_the_newest() {
    let dir = tempfile::tempdir().unwrap();
    // 1 user, 2 boundary, 3 user, 4 assistant, 5 boundary, 6 user, 7 assistant, 8 user
    let lines = vec![
        user(1),
        boundary(2),
        user(3),
        assistant(4),
        boundary(5),
        user(6),
        assistant(7),
        user(8),
    ];
    let path = write(dir.path(), "e.jsonl", &lines);
    assert_eq!(replaced_turns(&path), Some((6, 8)));
}

#[test]
fn a_lone_boundary_with_nothing_before_it_has_no_range() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "f.jsonl", &[boundary(1), summary(2)]);
    assert_eq!(replaced_turns(&path), None);
}

#[test]
fn a_boundary_right_after_a_boundary_has_no_range() {
    let dir = tempfile::tempdir().unwrap();
    // 1 user, 2 assistant, 3 boundary, 4 boundary: the newest replaced nothing.
    let lines = vec![user(1), assistant(2), boundary(3), boundary(4)];
    let path = write(dir.path(), "g.jsonl", &lines);
    assert_eq!(replaced_turns(&path), None);
}

#[test]
fn empty_missing_and_unrecognized_files_give_none() {
    let dir = tempfile::tempdir().unwrap();
    let empty = write(dir.path(), "empty.jsonl", &[]);
    assert_eq!(replaced_turns(&empty), None);
    assert_eq!(replaced_turns(&dir.path().join("missing.jsonl")), None);
    let only_noise = write(
        dir.path(),
        "noise.jsonl",
        &[noise(), "{not json".to_string()],
    );
    assert_eq!(replaced_turns(&only_noise), None);
}

#[test]
fn a_user_text_that_looks_like_a_boundary_label_is_not_one() {
    let dir = tempfile::tempdir().unwrap();
    let fake = r#"{"type":"user","uuid":"x","message":{"role":"user","content":"[system:compact_boundary]"}}"#;
    let lines = vec![user(1), fake.to_string(), assistant(3)];
    let path = write(dir.path(), "h.jsonl", &lines);
    assert_eq!(replaced_turns(&path), Some((1, 3)));
}

#[test]
fn a_mirror_copy_gives_the_same_range_as_the_plain_file() {
    let dir = tempfile::tempdir().unwrap();
    let lines = vec![
        user(1),
        assistant(2),
        boundary(3),
        user(4),
        assistant(5),
        user(6),
        boundary(7),
    ];
    let plain = write(dir.path(), "m.jsonl", &lines);
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let mirror = dir.path().join("m.jsonl.zst");
    std::fs::write(
        &mirror,
        zstd::stream::encode_all(body.as_bytes(), 3).unwrap(),
    )
    .unwrap();
    assert_eq!(replaced_turns(&mirror), Some((4, 6)));
    assert_eq!(replaced_turns(&mirror), replaced_turns(&plain));
}

#[test]
fn a_torn_mirror_copy_gives_none() {
    let dir = tempfile::tempdir().unwrap();
    let body: String = (1..=400).map(|n| format!("{}\n", user(n % 60))).collect();
    let mut bytes = zstd::stream::encode_all(body.as_bytes(), 3).unwrap();
    bytes.truncate(bytes.len() - 8);
    let mirror = dir.path().join("t.jsonl.zst");
    std::fs::write(&mirror, bytes).unwrap();
    assert_eq!(replaced_turns(&mirror), None);
}

#[test]
fn slice_over_the_returned_range_shows_exactly_those_entries() {
    let dir = tempfile::tempdir().unwrap();
    // Noise lines in between take no turn number.
    let lines = vec![
        user(1),
        noise(),
        assistant(2),
        boundary(3),
        user(4),
        noise(),
        assistant(5),
        user(6),
        assistant(7),
        boundary(8),
    ];
    let path = write(dir.path(), "s.jsonl", &lines);
    let range = replaced_turns(&path).expect("range");
    assert_eq!(range, (4, 7));
    assert_eq!(sliced_turns(&path, range), vec![4, 5, 6, 7]);

    let plain = write(dir.path(), "p.jsonl", &[user(1), assistant(2), user(3)]);
    let range = replaced_turns(&plain).expect("range");
    assert_eq!(range, (1, 3));
    assert_eq!(sliced_turns(&plain, range), vec![1, 2, 3]);
}

#[test]
fn the_pointer_line_is_the_exact_text_the_hook_prints() {
    let sid = "3002ab28-7ba1-4c61-9d2e-0a1b2c3d4e5f";
    assert_eq!(
        compact_pointer_line(sid, Some((12, 340))),
        "Before this compact: turns 12..340 of this session. The summary replaced them. Read them with csr_transcript(session=\"3002ab28-7ba1-4c61-9d2e-0a1b2c3d4e5f\", view=\"slice\", turns=\"12..340\"). view=\"yourasks\" lists what the person asked. view=\"grep\" finds a word. This works after Claude Code deletes the JSONL."
    );
    assert_eq!(
        compact_pointer_line(sid, None),
        "Before this compact: this session's earlier turns. The summary replaced them. Read them with csr_transcript(session=\"3002ab28-7ba1-4c61-9d2e-0a1b2c3d4e5f\", view=\"stats\"), then view=\"slice\" with turns. view=\"yourasks\" lists what the person asked. view=\"grep\" finds a word. This works after Claude Code deletes the JSONL."
    );
}

/// Fixture sets that cover boundaries, harness rows and queued duplicates.
fn scan_fixtures() -> Vec<Vec<String>> {
    let queued_op = r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-11-01T10:00:09Z","content":"look at the second file"}"#;
    let queued_att = r#"{"type":"attachment","timestamp":"2026-11-01T10:00:09Z","attachment":{"type":"queued_command","prompt":"look at the second file","humanTurn":true}}"#;
    let delta = r#"{"type":"file-history-delta","trackingPath":"/made/up/file.rs","timestamp":"2026-11-01T10:00:10Z"}"#;
    vec![
        vec![],
        vec![user(1), noise(), assistant(2), user(3)],
        vec![user(1), assistant(2), boundary(3), user(4), assistant(5)],
        vec![user(1), assistant(2), boundary(3), summary(4), user(5)],
        vec![
            user(1),
            assistant(2),
            boundary(3),
            summary(4),
            remote_prompt(5),
        ],
        vec![
            user(1),
            assistant(2),
            user(3),
            boundary(4),
            attachment(),
            summary(5),
        ],
        vec![boundary(1), boundary(2), boundary(3)],
        vec![
            user(1),
            queued_op.to_string(),
            queued_att.to_string(),
            delta.to_string(),
            assistant(2),
            boundary(3),
            summary(4),
            "{not json".to_string(),
            r#"{"type":"mystery"}"#.to_string(),
        ],
    ]
}

#[test]
fn the_range_scan_numbers_equal_a_full_parse() {
    let dir = tempfile::tempdir().unwrap();
    for (i, lines) in scan_fixtures().iter().enumerate() {
        let path = write(dir.path(), &format!("scan{i}.jsonl"), lines);
        let parsed = parse_transcript(&path).unwrap();
        let scan = scan_range(&path).expect("scan");
        assert_eq!(scan, RangeScan::of_entries(&parsed.entries), "fixture {i}");
        // Numbers read straight off the parsed entries, not through the scan.
        let boundaries: Vec<usize> = parsed
            .entries
            .iter()
            .filter(|e| e.is_compact_boundary())
            .map(|e| e.turn)
            .collect();
        assert_eq!(scan.entries, parsed.entries.len(), "fixture {i}");
        assert_eq!(
            scan.last_boundary,
            boundaries.last().copied(),
            "fixture {i}"
        );
        let before_last = boundaries.len().checked_sub(2).map(|n| boundaries[n]);
        assert_eq!(scan.previous_boundary, before_last, "fixture {i}");
        let after = scan.last_boundary.unwrap_or(usize::MAX);
        let live = parsed
            .entries
            .iter()
            .any(|e| e.turn > after && (e.role == Role::Assistant || is_human_prompt(e)));
        assert_eq!(scan.live_after_last_boundary, live, "fixture {i}");

        // The mirror copy scans to the same numbers.
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let mirror = dir.path().join(format!("scan{i}.jsonl.zst"));
        std::fs::write(
            &mirror,
            zstd::stream::encode_all(body.as_bytes(), 3).unwrap(),
        )
        .unwrap();
        assert_eq!(scan_range(&mirror), Some(scan), "fixture {i} mirror");
    }
}

fn tool_row(n: usize) -> (String, String) {
    let call = format!(
        r#"{{"type":"assistant","timestamp":"2026-11-01T10:01:{n:02}Z","uuid":"ta{n}","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t{n}","name":"Bash","input":{{"command":"echo {n}","extra":"input number {n}"}}}}]}}}}"#
    );
    let big = "made-up output line. ".repeat(60);
    let result = format!(
        r#"{{"type":"user","timestamp":"2026-11-01T10:01:{n:02}Z","uuid":"tr{n}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t{n}","content":"result {n} {big}"}}]}}}}"#
    );
    (call, result)
}

fn slice_fixture(dir: &Path) -> std::path::PathBuf {
    let mut lines = vec![user(1)];
    for n in 2..=7 {
        let (call, result) = tool_row(n);
        lines.push(call);
        lines.push(result);
    }
    write(dir, "slice.jsonl", &lines)
}

/// Turns whose tool payloads are still whole.
fn full_turns(parsed: &ParsedTranscript) -> Vec<usize> {
    parsed
        .entries
        .iter()
        .filter(|e| {
            e.tool_uses.iter().any(|t| t.input.is_some())
                || e.tool_results.iter().any(|r| r.full.is_some())
        })
        .map(|e| e.turn)
        .collect()
}

fn slice_req(path: &Path) -> TranscriptRequest {
    TranscriptRequest {
        session: path.to_string_lossy().into_owned(),
        view: ViewKind::Slice,
        ..TranscriptRequest::default()
    }
}

#[test]
fn a_last_n_slice_keeps_full_payloads_only_for_the_last_n_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = slice_fixture(dir.path());
    let all = parse_transcript(&path).unwrap().entries.len();
    let mut req = slice_req(&path);
    req.last = Some(2);
    let parsed = parse_transcript_with(&path, ParseOpts::for_request(&req)).unwrap();
    assert_eq!(full_turns(&parsed), vec![all - 1, all]);
    // The cut entries keep preview and size.
    let early = parsed.entries.iter().find(|e| e.turn == 3).unwrap();
    assert!(early.tool_results[0].byte_size > early.tool_results[0].preview.len());

    // Role filter: the window counts matching entries only.
    req.role = RoleFilter::Assistant;
    let parsed = parse_transcript_with(&path, ParseOpts::for_request(&req)).unwrap();
    assert_eq!(full_turns(&parsed), vec![all - 3, all - 1]);
    req.last = Some(0);
    let parsed = parse_transcript_with(&path, ParseOpts::for_request(&req)).unwrap();
    assert!(full_turns(&parsed).is_empty());
}

#[test]
fn a_turn_range_slice_keeps_full_payloads_only_in_range() {
    let dir = tempfile::tempdir().unwrap();
    let path = slice_fixture(dir.path());
    let mut req = slice_req(&path);
    req.turns = Some((4, Some(6)));
    let parsed = parse_transcript_with(&path, ParseOpts::for_request(&req)).unwrap();
    assert_eq!(full_turns(&parsed), vec![4, 5, 6]);
    req.turns = Some((11, None));
    let parsed = parse_transcript_with(&path, ParseOpts::for_request(&req)).unwrap();
    assert_eq!(full_turns(&parsed), vec![11, 12, 13]);
}

#[test]
fn the_other_views_and_grep_keep_what_they_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = slice_fixture(dir.path());
    let mut req = slice_req(&path);
    req.view = ViewKind::Grep;
    req.last = Some(1);
    let parsed = parse_transcript_with(&path, ParseOpts::for_request(&req)).unwrap();
    assert_eq!(full_turns(&parsed).len(), 12);
    req.view = ViewKind::Stats;
    let parsed = parse_transcript_with(&path, ParseOpts::for_request(&req)).unwrap();
    assert!(full_turns(&parsed).is_empty());
}

#[test]
fn slice_output_is_the_same_with_the_trimmed_parse() {
    let dir = tempfile::tempdir().unwrap();
    let path = slice_fixture(dir.path());
    let reference = |req: &TranscriptRequest| {
        let parsed = parse_transcript_with(&path, ParseOpts::for_view(ViewKind::Slice)).unwrap();
        query::render_view(&parsed, &path, "p", req)
    };
    for json in [false, true] {
        for (turns, last, role) in [
            (None, Some(1), RoleFilter::All),
            (None, Some(3), RoleFilter::Assistant),
            (None, None, RoleFilter::All),
            (Some((3, Some(5))), None, RoleFilter::All),
            (Some((9, None)), None, RoleFilter::User),
        ] {
            let mut req = slice_req(&path);
            req.json = json;
            req.turns = turns;
            req.last = last;
            req.role = role;
            req.result_chars = Some(300);
            assert_eq!(
                run(dir.path(), &req),
                reference(&req),
                "{turns:?} {last:?} {role:?}"
            );
        }
    }
}

/// Timing for the SessionStart hook's parse of a large transcript. Run on the
/// Studio with `--ignored --nocapture`; it prints the time for about 100 MiB.
#[test]
#[ignore]
fn perf_smoke_100_mib_replaced_turns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.jsonl");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    let filler = "word ".repeat(150);
    let mut i = 0usize;
    let mut bytes = 0usize;
    while bytes < 100 * 1024 * 1024 {
        let line = if i.is_multiple_of(2) {
            format!(
                r#"{{"type":"user","timestamp":"2026-11-01T00:00:00Z","uuid":"u{i}","message":{{"role":"user","content":"message {i} {filler}"}}}}"#
            )
        } else {
            format!(
                r#"{{"type":"assistant","timestamp":"2026-11-01T00:00:00Z","uuid":"a{i}","message":{{"role":"assistant","content":[{{"type":"text","text":"reply {i} {filler}"}},{{"type":"tool_use","id":"t{i}","name":"Bash","input":{{"command":"echo {i}"}}}}]}}}}"#
            )
        };
        bytes += line.len() + 1;
        writeln!(f, "{line}").unwrap();
        i += 1;
    }
    drop(f);
    let size = std::fs::metadata(&path).unwrap().len();
    let began = std::time::Instant::now();
    let range = replaced_turns(&path);
    let took = began.elapsed();
    println!(
        "TIMING replaced_turns {:.1} MiB {} entries: {took:?}",
        size as f64 / (1024. * 1024.),
        i
    );
    assert_eq!(range, Some((1, i)));
}
