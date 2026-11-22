//! A turn whose text is longer than the reply cap, read through `csr_transcript` paging.
//! Before the fix a JSON page held 0 items and `resume_turns` named the same turn again.

use super::*;
use crate::transcript::MAX_BUDGET_CHARS;

const BIG_CHARS: usize = 100_000;

/// 100,000 chars with multi-byte chars and chars the text view escapes, so cut points land
/// inside them. Starts and ends with a visible char.
fn big_text() -> String {
    let text = format!("START {}{}", "é€😀<&>\"q\\\n".repeat(9_999), " END");
    assert_eq!(text.chars().count(), BIG_CHARS);
    text
}

/// Turn 1 is short, turn 2 is [`big_text`], turn 3 is short. Returns the text the parser gives turn 2.
fn write_oversize_fixture(dir: &std::path::Path) -> (std::path::PathBuf, String) {
    let path = dir.join("oversize.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    let mut put = |v: Value| writeln!(f, "{v}").unwrap();
    put(json!({"type":"user","message":{"role":"user","content":"first turn"}}));
    put(json!({"type":"user","message":{"role":"user","content":big_text()}}));
    put(json!({"type":"user","message":{"role":"user","content":"last turn"}}));
    drop(f);
    let parsed = transcript::parse_transcript(&path).unwrap();
    let original = parsed.entries[1].text.clone();
    assert_eq!(parsed.entries[1].turn, 2);
    assert!(
        original.chars().count() >= BIG_CHARS,
        "the parser kept the text"
    );
    (path, original)
}

fn page_req(
    path: &std::path::Path,
    json: bool,
    budget: usize,
    call: (usize, usize),
) -> TranscriptRequest {
    let mut req = base_req(ViewKind::Slice, &path.to_string_lossy());
    req.turns = Some((call.0, None));
    req.json = json;
    req.budget_chars = budget;
    req.offset = call.1;
    req
}

/// `(turn, offset)` is strictly after `start` in reading order.
fn assert_forward(start: (usize, usize), resume: (usize, usize)) {
    assert!(
        resume > start,
        "a page must resume strictly after where it started"
    );
}

/// Read turns 1.. by following each page's own resume hint. Returns the text of turn 2 joined
/// from its parts, and how many times each turn was shown.
fn walk_json(dir: &std::path::Path, path: &std::path::Path, budget: usize) -> (String, [usize; 4]) {
    let (mut call, mut joined, mut shown) = ((1, 0), String::new(), [0; 4]);
    for _ in 0..200 {
        let out = transcript::run(dir, &page_req(path, true, budget, call));
        assert!(out.chars().count() <= budget + 220, "page over budget");
        let v: Value = serde_json::from_str(&out).expect("a cut page is still valid JSON");
        let items = v["items"].as_array().unwrap();
        assert!(!items.is_empty(), "a page must hold at least one item");
        for item in items {
            let turn = item["turn"].as_u64().unwrap() as usize;
            if turn == 2 {
                assert_eq!(
                    item["text_offset"].as_u64().unwrap() as usize,
                    joined.chars().count(),
                    "each part starts where the last one ended"
                );
                assert_eq!(item["text_chars_total"], json!(BIG_CHARS));
                joined.push_str(item["text"].as_str().unwrap());
            }
            if item.get("text_offset").is_none_or(|o| o == 0) {
                shown[turn] += 1;
            }
        }
        if v["truncated"] != json!(true) {
            return (joined, shown);
        }
        let turn: usize = v["resume_turns"]
            .as_str()
            .unwrap()
            .trim_end_matches("..")
            .parse()
            .unwrap();
        let offset = v
            .get("resume_offset")
            .map_or(0, |o| o.as_u64().unwrap() as usize);
        assert_forward(call, (turn, offset));
        call = (turn, offset);
    }
    panic!("still paging after 200 pages");
}

/// The text a text page shows for `turn`, un-escaped, and the offset its cut note names.
fn text_part(page: &str, turn: usize) -> Option<(String, Option<usize>)> {
    let at = page.find(&format!("\n[turn {turn}] "))? + 1;
    let after = &page[at..];
    let body = &after[after.find(": ")? + 2..];
    let (raw, next) = if let Some(c) = body.find("\n[cut: turn ") {
        let note = &body[c + 1..];
        let note = &note[..note.find(']').unwrap()];
        assert!(
            note.contains(&format!("next: turns=\"{turn}..\" offset=")),
            "the cut note names the next call"
        );
        (&body[..c], Some(hint_number(note, "offset").unwrap()))
    } else if let Some(n) = body.find("\n[turn ") {
        (&body[..n], None)
    } else {
        (body.strip_suffix('\n').unwrap_or(body), None)
    };
    let text = raw
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    Some((text, next))
}

/// [`walk_json`] in text mode: the next call is the one a cut note names, or the page's
/// `--turns N.. to continue` line.
fn walk_text(dir: &std::path::Path, path: &std::path::Path, budget: usize) -> (String, [usize; 4]) {
    let (mut call, mut joined, mut shown) = ((1, 0), String::new(), [0; 4]);
    for _ in 0..200 {
        let out = transcript::run(dir, &page_req(path, false, budget, call));
        assert!(out.chars().count() <= budget + 220, "page over budget");
        for turn in [1, 3] {
            shown[turn] += out.matches(&format!("\n[turn {turn}] ")).count();
        }
        let mut next = None;
        if let Some((part, offset)) = text_part(&out, 2) {
            if call.0 == 2 {
                assert_eq!(
                    call.1,
                    joined.chars().count(),
                    "each part starts where the last ended"
                );
            }
            joined.push_str(&part);
            shown[2] += usize::from(call.1 == 0);
            next = offset.map(|o| (2, o));
        }
        if next.is_none() {
            if let Some(line) = out.lines().find(|l| l.contains(".. to continue")) {
                let turn = line
                    .split("--turns ")
                    .nth(1)
                    .unwrap()
                    .split("..")
                    .next()
                    .unwrap();
                next = Some((turn.parse().unwrap(), 0));
            }
        }
        let Some(next) = next else {
            return (joined, shown);
        };
        assert_forward(call, next);
        call = next;
    }
    panic!("still paging after 200 pages");
}

#[test]
fn json_pages_walk_an_oversize_turn_to_the_end_and_join_to_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let (path, original) = write_oversize_fixture(dir.path());
    for budget in [MAX_BUDGET_CHARS, 30_000, 5_000] {
        let (joined, shown) = walk_json(dir.path(), &path, budget);
        assert_eq!(joined.chars().count(), BIG_CHARS);
        assert!(joined == original, "the parts do not join to the turn");
        assert_eq!(shown, [0, 1, 1, 1], "every turn is shown once");
    }
}

#[test]
fn the_first_json_part_says_it_is_cut_and_where_the_rest_starts() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = write_oversize_fixture(dir.path());
    let out = transcript::run(dir.path(), &page_req(&path, true, MAX_BUDGET_CHARS, (2, 0)));
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    let item = &v["items"][0];
    let n = item["text"].as_str().unwrap().chars().count();
    assert!(n > 20_000, "a part holds most of the reply");
    assert_eq!(item["text_cut"], json!(true));
    assert_eq!(item["text_offset"], json!(0));
    assert_eq!(item["text_chars_total"], json!(BIG_CHARS));
    assert_eq!(v["truncated"], json!(true));
    assert_eq!(v["resume_turns"], json!("2.."));
    assert_eq!(v["resume_offset"], json!(n));
    // The turn's other fields come with part 0 only.
    assert!(item.get("tool_uses").is_some());
    let next = transcript::run(dir.path(), &page_req(&path, true, MAX_BUDGET_CHARS, (2, n)));
    let next: Value = serde_json::from_str(&next).unwrap();
    assert!(next["items"][0].get("tool_uses").is_none());
    assert!(next["items"][0].get("timestamp").is_none());
}

#[test]
fn text_pages_walk_an_oversize_turn_by_their_cut_notes_and_join_to_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let (path, original) = write_oversize_fixture(dir.path());
    for budget in [MAX_BUDGET_CHARS, 30_000, 5_000] {
        let (joined, shown) = walk_text(dir.path(), &path, budget);
        assert!(joined == original, "the parts do not join to the turn");
        assert_eq!(shown, [0, 1, 1, 1], "every turn is shown once");
    }
}

#[test]
fn a_text_cut_note_names_the_exact_next_call_and_never_says_raise_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = write_oversize_fixture(dir.path());
    let out = transcript::run(
        dir.path(),
        &page_req(&path, false, MAX_BUDGET_CHARS, (2, 0)),
    );
    let note = out
        .lines()
        .find(|l| l.starts_with("[cut: "))
        .expect("a cut note");
    let offset = hint_number(note, "offset").unwrap();
    assert_eq!(
        note,
        format!("[cut: turn 2 has 100,000 chars; next: turns=\"2..\" offset={offset}]")
    );
    assert!(
        !out.contains("raise budget_chars"),
        "40,000 is the most the tool takes"
    );
    assert!(!out.contains("more turns"), "nothing follows a cut turn");
}

#[test]
fn offset_past_the_end_of_a_turn_gives_an_empty_remainder_then_the_next_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = write_oversize_fixture(dir.path());
    let json_out = transcript::run(
        dir.path(),
        &page_req(&path, true, MAX_BUDGET_CHARS, (2, BIG_CHARS + 50)),
    );
    let v: Value = serde_json::from_str(&json_out).unwrap();
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["turn"], json!(2));
    assert_eq!(items[0]["text"], json!(""));
    assert_eq!(
        items[0]["text_offset"],
        json!(BIG_CHARS),
        "clamped to the end"
    );
    assert_eq!(items[1]["turn"], json!(3));
    assert_eq!(v["truncated"], json!(false));

    let text_out = transcript::run(
        dir.path(),
        &page_req(&path, false, MAX_BUDGET_CHARS, (2, BIG_CHARS)),
    );
    assert!(
        text_out.contains("[offset 100000 is at or past the end of turn 2 (100,000 chars)"),
        "the page says the offset is past the end"
    );
    assert!(text_out.contains("[turn 3] "), "the next turn follows");
}

#[test]
fn offset_on_a_short_first_turn_starts_its_text_part_way_and_ignores_the_later_turns() {
    let dir = tempfile::tempdir().unwrap();
    let (path, original) = write_oversize_fixture(dir.path());
    // Turn 1 is "first turn": 10 chars.
    let run_json = |offset| {
        let out = transcript::run(
            dir.path(),
            &page_req(&path, true, MAX_BUDGET_CHARS, (1, offset)),
        );
        serde_json::from_str::<Value>(&out).unwrap()
    };
    let v = run_json(4);
    assert_eq!(v["items"][0]["text"], json!("t turn"));
    assert_eq!(v["items"][0]["text_offset"], json!(4));
    assert!(v["items"][0].get("text_cut").is_none());
    // The big turn after it does not fit beside it, so the page resumes at its own char 0.
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    assert_eq!(v["resume_turns"], json!("2.."));
    assert!(v.get("resume_offset").is_none());

    let past = run_json(999);
    assert_eq!(past["items"][0]["text"], json!(""));
    assert_eq!(past["items"][0]["text_offset"], json!(10));
    assert_eq!(past["resume_turns"], json!("2.."));
    // The offset belongs to the first turn only: turn 2 still starts at its char 0.
    let big = transcript::run(dir.path(), &page_req(&path, true, MAX_BUDGET_CHARS, (2, 0)));
    let big: Value = serde_json::from_str(&big).unwrap();
    assert!(original.starts_with(big["items"][0]["text"].as_str().unwrap()));

    let text = transcript::run(
        dir.path(),
        &page_req(&path, false, MAX_BUDGET_CHARS, (1, 4)),
    );
    assert!(
        text.contains("[turn 1] user -: t turn\n"),
        "the text starts at the offset"
    );
    let text = transcript::run(
        dir.path(),
        &page_req(&path, false, MAX_BUDGET_CHARS, (1, 999)),
    );
    assert!(
        text.contains("nothing left in this turn]"),
        "an offset past the end says so"
    );
}

#[test]
fn an_offset_of_zero_changes_nothing_on_a_session_with_no_oversize_turn() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_golden_fixture(dir.path());
    for json_form in [false, true] {
        let mut plain = base_req(ViewKind::Slice, &path.to_string_lossy());
        plain.turns = Some((1, Some(9)));
        plain.json = json_form;
        let mut zero = plain.clone();
        zero.offset = 0;
        assert_eq!(
            transcript::run(dir.path(), &plain),
            transcript::run(dir.path(), &zero)
        );
        assert!(!transcript::run(dir.path(), &plain).contains("text_offset"));
    }
}

#[test]
fn a_fitted_cut_note_stops_saying_raise_budget_at_the_server_maximum() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_huge_turn_fixture(dir.path());
    let mut req = slice_req(&path);
    req.turns = Some((2, None));
    req.budget_chars = MAX_BUDGET_CHARS;
    let out = transcript::run(dir.path(), &req);
    assert!(
        out.contains("to fit budget_chars; view=\"grep\" to see more]"),
        "the fitted note names grep"
    );
    assert!(!out.contains("raise budget_chars"));
    // Below the maximum the advice can still be followed, so it stays.
    req.budget_chars = 30_000;
    assert!(transcript::run(dir.path(), &req).contains("raise budget_chars"));
}

#[test]
fn the_mcp_tool_passes_offset_through_and_keeps_its_reply_under_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let (path, original) = write_oversize_fixture(dir.path());
    let call = |offset| {
        crate::mcp::tools::transcript_with_db(
            dir.path(),
            None,
            None,
            &path.to_string_lossy(),
            "slice",
            None,
            None,
            Some("2.."),
            None,
            None,
            None,
            true,
            Some(200_000),
            false,
            false,
            None,
            offset,
        )
        .unwrap()
    };
    let v: Value = serde_json::from_str(&call(Some(10))).unwrap();
    let item = &v["items"][0];
    assert_eq!(item["text_offset"], json!(10));
    assert!(item["text"]
        .as_str()
        .unwrap()
        .starts_with(&original.chars().skip(10).take(20).collect::<String>()));
    assert!(call(Some(10)).chars().count() <= MAX_BUDGET_CHARS + 220);
    assert_eq!(call(None), call(Some(0)));
}

/// The golden fixture's turns 1..9 as slice pages, written down from the build before paging
/// (`6c339727`): a session with no oversize turn pages byte for byte as it did.
fn golden_slice(json_form: bool, budget: usize) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = write_golden_fixture(dir.path());
    let mut req = base_req(ViewKind::Slice, &path.to_string_lossy());
    req.turns = Some((1, Some(9)));
    req.json = json_form;
    req.budget_chars = budget;
    transcript::run(dir.path(), &req)
}

#[test]
fn text_pages_of_a_session_with_no_oversize_turn_are_byte_identical_to_before() {
    let whole = r#"<transcript_slice>
[turn 1] user 2026-08-10T10:00:00Z: Fix the &lt;script&gt;alert('x')&lt;/script&gt; injection bug in auth.rs
[turn 2] assistant 2026-08-10T10:00:02Z: Reading the file first.
  tool_use: Read file_path=/repo/src/auth.rs
[turn 3] user 2026-08-10T10:00:03Z:<SP>
  tool_result: ok (13 bytes): fn login() {}
[turn 4] assistant 2026-08-10T10:00:04Z:<SP>
  tool_use: Edit file_path=/repo/src/auth.rs command=n/a
[turn 5] user 2026-08-10T10:00:05Z:<SP>
  tool_result: error (28 bytes): old_string not found in file
[turn 6] assistant 2026-08-10T10:00:06Z:<SP>
  tool_use: Bash command=cargo test
  tool_use: Grep pattern=fn login
[turn 7] user 2026-08-10T10:00:07Z: Also: `; DROP TABLE users; --` should be treated as inert data, not a command.
  tool_result: ok (26 bytes): test result: ok. 12 passed
[turn 8] system 2026-08-10T10:00:08Z: [system:stop_hook_summary]
[turn 9] assistant 2026-08-10T10:00:09Z: Done: tests pass, auth.rs untouched by the failed edit.
"#
    .replace("<SP>", " ");
    assert_eq!(golden_slice(false, 30_000), whole);
    let cut = r#"<transcript_slice>
[turn 1] user 2026-08-10T10:00:00Z: Fix the &lt;script&gt;alert('x')&lt;/script&gt; injection bug in auth.rs
[turn 2] assistant 2026-08-10T10:00:02Z: Reading the file first.
  tool_use: Read file_path=/repo/src/auth.rs
... 7 more turns; --turns 3.. to continue
"#;
    assert_eq!(golden_slice(false, 250), cut);
}

#[test]
fn json_pages_of_a_session_with_no_oversize_turn_are_byte_identical_to_before() {
    let cut = r#"{
  "items": [
    {
      "role": "user",
      "text": "Fix the <script>alert('x')</script> injection bug in auth.rs",
      "timestamp": "2026-08-10T10:00:00Z",
      "tool_results": [],
      "tool_uses": [],
      "turn": 1
    }
  ],
  "remaining": 8,
  "resume_turns": "2..",
  "truncated": true,
  "view": "slice"
}"#;
    assert_eq!(golden_slice(true, 250), cut);
    let whole = golden_slice(true, 30_000);
    assert_eq!(whole.chars().count(), 3498);
    assert!(whole.ends_with("\n  \"truncated\": false,\n  \"view\": \"slice\"\n}"));
    assert!(!whole.contains("text_offset") && !whole.contains("resume_offset"));
}
