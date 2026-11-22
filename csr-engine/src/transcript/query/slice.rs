//! The `slice` view of `csr_transcript` / `csr-engine transcript`, and the cut notes the list
//! views share with it. Lives here so `query.rs` stays under the file-size cap.

use serde_json::{json, Map, Value};

use super::{mark_mirror, resume_turns, turn_in_range, xml_escape, RenderItem, ViewData};
use crate::transcript::{Entry, ParsedTranscript, Role, TranscriptRequest, DEFAULT_LAST_N};

/// Why a result or input was cut to fewer chars than the request asked.
#[derive(Clone, Copy, PartialEq)]
enum Fit {
    /// Cut only at the `result_chars` the request gave.
    No,
    /// Cut to keep the turn inside a `budget_chars` the caller can still raise.
    Budget,
    /// Cut to keep the turn inside the largest `budget_chars` the server accepts.
    AtMax,
}

/// Smallest `budget_chars` at which a slice turn is fitted to the budget.
/// Below it the generic rule stands: a turn that does not fit is not shown,
/// and the resume hint names that turn.
const SLICE_FIT_MIN_BUDGET_CHARS: usize = 1_000;
/// What a fitted turn leaves free for the header lines and the continuation line.
const SLICE_FIT_RESERVE_CHARS: usize = 800;
/// The least a fitted result or input keeps.
const SLICE_FIT_FLOOR_CHARS: usize = 100;
/// What a hint adds on top of a turn's own chars when it names a budget: the
/// turn line, a tool result's tag, and a cut note.
const SLICE_HINT_ALLOWANCE_CHARS: usize = 300;

/// The smallest `budget_chars`, in whole thousands, at which a slice item of
/// `item_chars` is not fitted down: the item, the header and continuation
/// room the fit keeps free, and the hint allowance.
fn budget_to_hold(item_chars: usize) -> usize {
    (item_chars + SLICE_FIT_RESERVE_CHARS + SLICE_HINT_ALLOWANCE_CHARS).div_ceil(1_000) * 1_000
}

/// Chars of each tool result and tool input the request asks `slice` to show.
pub(super) fn slice_result_limit(req: &TranscriptRequest) -> usize {
    req.result_chars
        .unwrap_or(crate::transcript::DEFAULT_RESULT_CHARS)
        .clamp(1, crate::transcript::MAX_RESULT_CHARS)
}

/// The first `limit` chars of `full`, and whether anything was cut.
fn cap_chars(full: &str, limit: usize) -> (&str, bool) {
    match full.char_indices().nth(limit) {
        Some((i, _)) => (&full[..i], true),
        None => (full, false),
    }
}

/// The note that ends a cut result or input. `fitted` marks a cut made to
/// keep the turn inside `budget_chars`, where a larger `result_chars` alone
/// does not help; at the server's largest budget the note does not tell the
/// caller to raise it. A `result_chars` the note names comes with the
/// `budget_chars` that keeps the turn from being fitted down again, and is
/// left out when no budget the server accepts would hold it.
fn cut_note(total: &str, limit: usize, fitted: Fit) -> String {
    let bytes = total.len();
    match fitted {
        Fit::Budget => {
            return format!(
                "… [cut: {bytes} bytes total, to fit budget_chars; raise budget_chars, or view=\"grep\" to see more]"
            )
        }
        Fit::AtMax => {
            return format!(
                "… [cut: {bytes} bytes total, to fit budget_chars; view=\"grep\" to see more]"
            )
        }
        Fit::No => {}
    }
    let larger = total
        .chars()
        .count()
        .min(crate::transcript::MAX_RESULT_CHARS);
    if larger > limit {
        let escaped = xml_escape(cap_chars(total, larger).0).chars().count();
        let need = budget_to_hold(escaped);
        if need <= crate::transcript::DEFAULT_BUDGET_CHARS {
            return format!(
                "… [cut: {bytes} bytes total; result_chars={larger} or view=\"grep\" to see more]"
            );
        }
        if need <= crate::transcript::MAX_BUDGET_CHARS {
            return format!(
                "… [cut: {bytes} bytes total; result_chars={larger} budget_chars={need} or view=\"grep\" to see more]"
            );
        }
    }
    format!("… [cut: {bytes} bytes total; view=\"grep\" to see more]")
}

/// The slice call a list view's hint names for a turn of `chars` chars, and
/// whether that call shows the whole turn.
struct SliceCall {
    result_chars: usize,
    /// Named only when the default budget would cut the turn again.
    budget_chars: Option<usize>,
    whole: bool,
}

fn slice_call_for(entry: &Entry, chars: usize) -> SliceCall {
    let want = chars.min(crate::transcript::MAX_RESULT_CHARS);
    // Without `result_chars` the slice shows `DEFAULT_RESULT_CHARS`.
    let need =
        |limit: usize| budget_to_hold(slice_item_chars(&slice_item(entry, limit, Fit::No), false));
    if chars <= crate::transcript::MAX_RESULT_CHARS {
        let held = need(want.max(crate::transcript::DEFAULT_RESULT_CHARS));
        if held <= crate::transcript::DEFAULT_BUDGET_CHARS {
            return SliceCall {
                result_chars: want,
                budget_chars: None,
                whole: true,
            };
        }
        if held <= crate::transcript::MAX_BUDGET_CHARS {
            return SliceCall {
                result_chars: want,
                budget_chars: Some(held),
                whole: true,
            };
        }
    }
    // No budget the server accepts holds it: name the most that one does.
    let (mut lo, mut hi) = (1usize, want);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if need(mid) <= crate::transcript::MAX_BUDGET_CHARS {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    SliceCall {
        result_chars: lo,
        budget_chars: Some(crate::transcript::MAX_BUDGET_CHARS),
        whole: false,
    }
}

/// Up to `max` chars of a list view's turn text. A cut names the slice call that shows it whole, or,
/// when no call can, how much of it the best call shows.
pub(super) fn list_preview(text: &str, max: usize, entry: Option<&Entry>) -> String {
    let (shown, cut) = cap_chars(text, max);
    if !cut {
        return text.to_string();
    }
    let bytes = text.len();
    let Some(entry) = entry else {
        return format!("{shown}… [cut: {bytes} bytes total; view=\"grep\" to see more]");
    };
    let turn = entry.turn;
    // What the slice caps is the entry's own text, which can carry more than the ask.
    let call = slice_call_for(entry, entry.text.trim().chars().count());
    let result = if call.result_chars > crate::transcript::DEFAULT_RESULT_CHARS {
        format!(" result_chars={}", call.result_chars)
    } else {
        String::new()
    };
    let budget = call
        .budget_chars
        .map(|b| format!(" budget_chars={b}"))
        .unwrap_or_default();
    if call.whole {
        format!(
            "{shown}… [cut: {bytes} bytes total; view=\"slice\" turns=\"{turn}..{turn}\"{result}{budget} shows it whole]"
        )
    } else if text_needs_parts(entry) {
        format!(
            "{shown}… [cut: {bytes} bytes total; view=\"slice\" turns=\"{turn}..{turn}\" reads it in parts, each ending with the next call; view=\"grep\" finds a word in it]"
        )
    } else {
        // The text fits one page, and the tool results are what is too big. The largest budget
        // with the whole text asked for reads all of it: the slice reads it in parts when the
        // tool results beside it take the room.
        let words = entry.text.trim().chars().count();
        let result = if words > crate::transcript::DEFAULT_RESULT_CHARS {
            format!(" result_chars={words}")
        } else {
            String::new()
        };
        format!(
            "{shown}… [cut: {bytes} bytes total; view=\"slice\" turns=\"{turn}..{turn}\"{result} budget_chars={} reads all of its text, in parts when the tool results beside it take the room; view=\"grep\" finds a word in it]",
            crate::transcript::MAX_BUDGET_CHARS
        )
    }
}

/// Escape for the text envelope, and indent the continuation lines so a
/// result cannot start a line that looks like a turn header.
fn escape_block(s: &str) -> String {
    xml_escape(s).replace('\n', "\n    ")
}

/// True when the tool call's input holds only the four lifted fields, all
/// strings. The lifted-field line then says all there is to say.
fn input_is_lifted_only(input: &Value) -> bool {
    match input {
        Value::Null => true,
        Value::Object(map) => map.iter().all(|(k, v)| {
            matches!(k.as_str(), "file_path" | "command" | "pattern" | "prompt") && v.is_string()
        }),
        _ => false,
    }
}

/// What the text view shows for one tool call: the lifted-field line when the
/// input is only those fields, else the whole input as compact JSON.
fn slice_tool_use_text(tu: &crate::transcript::ToolUse, limit: usize, fitted: Fit) -> String {
    let body = match &tu.input {
        Some(input) if !input_is_lifted_only(input) => input.to_string(),
        _ => super::raw_tool_use_fields(tu),
    };
    let (shown, cut) = cap_chars(&body, limit);
    let mut text = format!("\n  tool_use: {}", xml_escape(&tu.name));
    if !shown.is_empty() {
        text.push(' ');
        text.push_str(&escape_block(shown));
    }
    if cut {
        text.push_str(&cut_note(&body, limit, fitted));
    }
    text
}

/// What the text view shows for one tool result.
fn slice_tool_result_text(tr: &crate::transcript::ToolResult, limit: usize, fitted: Fit) -> String {
    let full = tr.full.as_deref().unwrap_or(&tr.preview);
    let (shown, cut) = cap_chars(full, limit);
    let tag = if tr.is_error { "error" } else { "ok" };
    let mut text = format!(
        "\n  tool_result: {tag} ({} bytes): {}",
        tr.byte_size,
        escape_block(shown)
    );
    if cut {
        text.push_str(&cut_note(full, limit, fitted));
    }
    text
}

/// The line that opens a slice turn, up to its words.
fn turn_head(entry: &Entry) -> String {
    let ts = entry.timestamp.as_deref().unwrap_or("-");
    format!("[turn {}] {} {ts}: ", entry.turn, entry.role)
}

/// The lines for a turn's tool calls and results, each cut to `limit` chars.
fn tool_lines(entry: &Entry, limit: usize, fitted: Fit) -> String {
    let mut text = String::new();
    for tu in &entry.tool_uses {
        text.push_str(&slice_tool_use_text(tu, limit, fitted));
    }
    for tr in &entry.tool_results {
        text.push_str(&slice_tool_result_text(tr, limit, fitted));
    }
    text
}

/// One slice turn, with its words, each tool result and each tool input cut to `limit` chars.
/// A cut says so: a reply cut at a fixed 500 chars with no note once hid the line a caller asked for.
fn slice_item(entry: &Entry, limit: usize, fitted: Fit) -> RenderItem {
    let words = entry.text.trim();
    let (shown, words_cut) = cap_chars(words, limit);
    let mut text = format!("{}{}", turn_head(entry), xml_escape(shown));
    if words_cut {
        text.push_str(&cut_note(words, limit, fitted));
    }
    text.push_str(&tool_lines(entry, limit, fitted));

    let tool_uses: Vec<Value> = entry
        .tool_uses
        .iter()
        .map(|tu| {
            let mut v = serde_json::to_value(tu).unwrap_or(Value::Null);
            if let (Some(input), Some(obj)) = (&tu.input, v.as_object_mut()) {
                let body = input.to_string();
                let (shown, cut) = cap_chars(&body, limit);
                obj.insert("input".to_string(), json!(shown));
                obj.insert("input_cut".to_string(), json!(cut));
            }
            v
        })
        .collect();
    let tool_results: Vec<Value> = entry
        .tool_results
        .iter()
        .map(|tr| {
            let mut v = serde_json::to_value(tr).unwrap_or(Value::Null);
            if let (Some(full), Some(obj)) = (&tr.full, v.as_object_mut()) {
                let (shown, cut) = cap_chars(full, limit);
                obj.insert("preview".to_string(), json!(shown));
                obj.insert("cut".to_string(), json!(cut));
            }
            v
        })
        .collect();

    RenderItem {
        turn: entry.turn,
        json: json!({
            "turn": entry.turn,
            "role": entry.role.to_string(),
            "timestamp": entry.timestamp,
            "text": entry.text,
            "tool_uses": tool_uses,
            "tool_results": tool_results,
        }),
        text,
    }
}

/// How many chars `item` takes in the form the request renders.
fn slice_item_chars(item: &RenderItem, json: bool) -> usize {
    if json {
        item.json.to_string().chars().count()
    } else {
        item.text.chars().count() + 1
    }
}

/// Hide transport echoes without renumbering source turns. Repeated genuine
/// requests after delivery remain distinct. Replay before range filtering so
/// pagination cannot bring an echo back.
pub(crate) fn queued_duplicates(parsed: &ParsedTranscript) -> std::collections::HashSet<usize> {
    let mut hidden = std::collections::HashSet::new();
    // Enqueues have no UUID; queued attachments have one. Pair only the
    // opposite representation, preserving two deliberately repeated enqueues.
    let mut pending: Vec<(String, bool, bool)> = Vec::new();
    let mut qi = 0;
    for (idx, e) in parsed.entries.iter().enumerate() {
        while qi < parsed.queue_events.len() && parsed.queue_events[qi].before_entry_index <= idx {
            let ev = &parsed.queue_events[qi];
            if ev.op == crate::transcript::QueueOpKind::Remove {
                if let Some(pos) = ev
                    .content
                    .as_ref()
                    .and_then(|text| pending.iter().position(|p| &p.0 == text))
                {
                    pending.remove(pos);
                }
            }
            qi += 1;
        }
        if e.is_compact_boundary() {
            pending.clear();
        }
        if e.role != Role::User
            || e.text.trim().is_empty()
            || !e.tool_uses.is_empty()
            || !e.tool_results.is_empty()
        {
            continue;
        }
        let text = crate::transcript::strip_queued_prefix(&e.text)
            .trim()
            .to_string();
        if e.text.starts_with("[queued] ") {
            let attachment = e.uuid.is_some();
            if let Some(p) = pending
                .iter_mut()
                .find(|p| p.0 == text && if attachment { !p.2 } else { !p.1 })
            {
                p.1 |= !attachment;
                p.2 |= attachment;
                hidden.insert(e.turn);
            } else {
                pending.push((text, !attachment, attachment));
            }
        } else if let Some(pos) = pending.iter().position(|p| p.0 == text) {
            pending.remove(pos);
            hidden.insert(e.turn);
        } else if crate::transcript::prompt_noise_class(e).is_none() {
            pending.clear();
        }
    }
    hidden
}

/// What the cut note of a turn read in parts can take, newline and counts included.
const SLICE_PART_NOTE_CHARS: usize = 200;

/// `n` with a comma between thousands, as the cut note states a turn's size.
fn with_commas(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Chars `from..to` of `s`.
fn char_range(s: &str, from: usize, to: usize) -> &str {
    let byte = |n: usize| s.char_indices().nth(n).map_or(s.len(), |(i, _)| i);
    &s[byte(from)..byte(to)]
}

/// How many chars `xml_escape` leaves of `c`.
fn escaped_width(c: char) -> usize {
    match c {
        '&' => 5,
        '<' | '>' => 4,
        _ => 1,
    }
}

/// How many chars `xml_escape` leaves of `s`.
fn escaped_chars(s: &str) -> usize {
    s.chars().map(escaped_width).sum()
}

/// True when no `budget_chars` the server accepts shows the whole text of the turn, so the
/// turn can only be read in parts. The test is [`budget_to_hold`], the same estimate the hints
/// use to name a call that shows a turn whole, and it must stay so: a turn is read in parts
/// when no whole-read call can be named. A looser test (the exact room a page keeps,
/// a little more) would leave turns in between neither paged nor given a call that shows them
/// whole, and their last chars would be out of reach.
///
/// There is a second case. A turn whose text fits a page is still read in parts when the fit
/// would cut its words to make room for the tool lines beside them, because that cut names no
/// offset. `slice_view` decides that case, not this test.
fn text_needs_parts(entry: &Entry) -> bool {
    let line = escaped_chars(entry.text.trim()) + turn_head(entry).chars().count() + 1;
    budget_to_hold(line) > crate::transcript::MAX_BUDGET_CHARS
}

/// The slice item JSON for chars `start..end` of the turn's text. Part 0 keeps the turn's other
/// fields (`normal`); a later part carries only the turn, its role and the text.
fn json_text_part(entry: &Entry, normal: &Value, start: usize, end: usize) -> Value {
    let total = entry.text.chars().count();
    let (start, end) = (start.min(total), end.min(total));
    let mut obj = Map::new();
    if start == 0 {
        if let Some(whole) = normal.as_object() {
            obj = whole.clone();
        }
    } else {
        obj.insert("turn".to_string(), json!(entry.turn));
        obj.insert("role".to_string(), json!(entry.role.to_string()));
    }
    obj.insert(
        "text".to_string(),
        json!(char_range(&entry.text, start, end)),
    );
    obj.insert("text_offset".to_string(), json!(start));
    obj.insert("text_chars_total".to_string(), json!(total));
    if end < total {
        obj.insert("text_cut".to_string(), json!(true));
    }
    Value::Object(obj)
}

/// The text-view item for chars `start..` of a turn read in parts: as many chars as fit under
/// `cap`, and a note that names the next call, through the request's end turn, when the rest does
/// not fit. The turn's tool calls
/// and results show with part 0 only. `None` when not even one char fits. The bool is true when
/// the part ends before the turn does.
fn text_part_item(
    entry: &Entry,
    start: usize,
    cap: usize,
    requested: usize,
    fit: Fit,
    req: &TranscriptRequest,
) -> Option<(RenderItem, bool)> {
    let total = entry.text.chars().count();
    let begin = start.min(total);
    let head = turn_head(entry);
    let (mut limit, mut kind) = (requested, Fit::No);
    let mut tools = String::new();
    loop {
        if begin == 0 {
            tools = tool_lines(entry, limit, kind);
        }
        let base = head.chars().count() + tools.chars().count() + 1 + SLICE_PART_NOTE_CHARS;
        if base <= cap || limit <= SLICE_FIT_FLOOR_CHARS {
            break;
        }
        limit = (limit / 2).max(SLICE_FIT_FLOOR_CHARS);
        kind = fit;
    }
    let room = cap.checked_sub(head.chars().count() + tools.chars().count() + 1)?;
    let rest = char_range(&entry.text, begin, total);
    let turn = entry.turn;
    let (body, ends_early) = if escaped_chars(rest) <= room {
        let note = if start > 0 && rest.is_empty() {
            format!(
                "\n[offset {start} is at or past the end of turn {turn} ({} chars): nothing left in this turn]",
                with_commas(total)
            )
        } else {
            String::new()
        };
        (format!("{}{note}", xml_escape(rest)), false)
    } else {
        let room_text = room.checked_sub(SLICE_PART_NOTE_CHARS)?;
        let mut used = 0;
        let mut take = 0;
        for c in rest.chars() {
            let width = escaped_width(c);
            if used + width > room_text {
                break;
            }
            used += width;
            take += 1;
        }
        if take == 0 {
            return None;
        }
        let note = format!(
            "\n[cut: turn {turn} has {} chars; next: turns=\"{}\" offset={}]",
            with_commas(total),
            resume_turns(turn, req),
            begin + take
        );
        (
            format!("{}{note}", xml_escape(char_range(rest, 0, take))),
            true,
        )
    };
    let item = RenderItem {
        turn,
        text: format!("{head}{body}{tools}"),
        json: Value::Null,
    };
    Some((item, ends_early))
}

/// The least `budget_chars` at which [`text_part_item`] shows a part of `entry`'s text from
/// `start`: the fit's header room, the turn line, and the room for the rest of the text, or for
/// its cut note and first char when the rest is longer.
fn part_budget(entry: &Entry, start: usize) -> usize {
    let total = entry.text.chars().count();
    let rest = char_range(&entry.text, start.min(total), total);
    let first = rest.chars().next().map_or(0, escaped_width);
    let room = escaped_chars(rest).min(SLICE_PART_NOTE_CHARS + first);
    (SLICE_FIT_RESERVE_CHARS + turn_head(entry).chars().count() + 1 + room)
        .max(SLICE_FIT_MIN_BUDGET_CHARS)
}

/// The note that stands for a first turn whose `offset` no part fits at this budget. Showing
/// the turn from its first char instead would hand the reader its start twice.
fn offset_note_item(entry: &Entry, start: usize, budget: usize) -> RenderItem {
    let turn = entry.turn;
    RenderItem {
        turn,
        text: format!(
            "[offset {start} not applied: budget_chars {budget} is too small to read turn {turn} in parts; use budget_chars={} or more]",
            part_budget(entry, start)
        ),
        json: Value::Null,
    }
}

/// The turns a slice request selects, in order.
fn selected_entries<'a>(parsed: &'a ParsedTranscript, req: &TranscriptRequest) -> Vec<&'a Entry> {
    let duplicates = queued_duplicates(parsed);
    let filtered: Vec<&Entry> = parsed
        .entries
        .iter()
        .filter(|e| req.role.matches_role(e.role))
        .collect();

    let selected: Vec<&Entry> = if let Some((start, end)) = req.turns {
        filtered
            .into_iter()
            .filter(|e| turn_in_range(e.turn, Some((start, end))))
            .collect()
    } else {
        let n = req.last.unwrap_or(DEFAULT_LAST_N);
        let skip = filtered.len().saturating_sub(n);
        filtered.into_iter().skip(skip).collect()
    };
    selected
        .into_iter()
        // `last` addresses original turns, like FullScope::Last in the parser.
        // Dedupe after that selection so we never pull in an older turn whose
        // full tool payloads were intentionally released during parsing.
        .filter(|e| req.include_system || !duplicates.contains(&e.turn))
        .collect()
}

/// A copy of each entry whose text holds a secret, with the text redacted; `None` for the rest.
/// The slice cuts a turn's text at char offsets, and a secret cut in two parts matches no
/// pattern in either, so the scrub runs before the cut, and every offset and total counts the
/// redacted text. The reply is scrubbed again at the end.
fn redacted_copies(entries: &[&Entry]) -> Vec<Option<Entry>> {
    entries
        .iter()
        .map(|entry| match crate::redact::redact_secrets(&entry.text) {
            std::borrow::Cow::Owned(text) => {
                let mut copy = (*entry).clone();
                copy.text = text;
                Some(copy)
            }
            std::borrow::Cow::Borrowed(_) => None,
        })
        .collect()
}

/// The view of `entries`. The request's `offset` starts the first turn's text part-way in.
fn slice_view(entries: &[&Entry], req: &TranscriptRequest) -> ViewData {
    // A turn with many large results can pass the whole budget. It would then
    // be shown as nothing, and the resume hint would name the same turn, so a
    // caller on the default budget would loop. Halve the cut until the turn
    // fits; the cut note says why. A budget too small to carry any turn keeps
    // the generic rule.
    let requested = slice_result_limit(req);
    let fit_cap = (req.budget_chars >= SLICE_FIT_MIN_BUDGET_CHARS)
        .then(|| req.budget_chars - SLICE_FIT_RESERVE_CHARS);
    let fit = if req.budget_chars >= crate::transcript::MAX_BUDGET_CHARS {
        Fit::AtMax
    } else {
        Fit::Budget
    };

    let mut items: Vec<RenderItem> = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        let start = if i == 0 { req.offset } else { 0 };
        if req.json && start > 0 {
            let total = entry.text.chars().count();
            items.push(RenderItem {
                turn: entry.turn,
                text: String::new(),
                json: json_text_part(entry, &Value::Null, start, total),
            });
            continue;
        }
        if let (false, Some(cap)) = (req.json, fit_cap) {
            if start > 0 || text_needs_parts(entry) {
                if let Some((item, ends_early)) =
                    text_part_item(entry, start, cap, requested, fit, req)
                {
                    items.push(item);
                    if ends_early {
                        // The note names the next call; nothing may follow the cut.
                        break;
                    }
                    continue;
                }
            }
        }
        if !req.json && start > 0 {
            // No part fits: the note stands for the turn, and nothing follows it.
            items.push(offset_note_item(entry, start, req.budget_chars));
            break;
        }
        // In JSON the turn's own text is whole in the item, so a long text must not shrink the
        // results beside it.
        let text_chars = if req.json {
            entry.text.chars().count()
        } else {
            0
        };
        let text_share = fit_cap
            .filter(|cap| text_chars > *cap)
            .map_or(0, |_| text_chars);
        let mut limit = requested;
        let mut item = slice_item(entry, limit, Fit::No);
        if let Some(cap) = fit_cap {
            while slice_item_chars(&item, req.json).saturating_sub(text_share) > cap
                && limit > SLICE_FIT_FLOOR_CHARS
            {
                limit = (limit / 2).max(SLICE_FIT_FLOOR_CHARS);
                item = slice_item(entry, limit, fit);
            }
        }
        if let (false, Some(cap)) = (req.json, fit_cap) {
            // The fit cut the words to make room for the tool lines beside them. That cut has no
            // offset, so the rest of the words would be out of reach: read the turn in parts.
            if limit < requested && entry.text.trim().chars().count() > limit {
                if let Some((part, ends_early)) = text_part_item(entry, 0, cap, requested, fit, req)
                {
                    items.push(part);
                    if ends_early {
                        break;
                    }
                    continue;
                }
            }
        }
        items.push(item);
    }

    ViewData {
        view: "slice",
        extra_json: vec![],
        header_lines: vec!["<transcript_slice>".to_string()],
        items,
        empty_message: "(no turns in the requested range)".to_string(),
    }
}

/// The `slice` page. In JSON, a first item that does not fit the budget comes back as the
/// largest part of its text that does, so a page always moves the reader forward.
pub(super) fn render_slice(parsed: &ParsedTranscript, req: &TranscriptRequest) -> String {
    let selected = selected_entries(parsed, req);
    let copies = redacted_copies(&selected);
    let entries: Vec<&Entry> = selected
        .iter()
        .zip(&copies)
        .map(|(entry, copy)| copy.as_ref().unwrap_or(*entry))
        .collect();
    let mut view = mark_mirror(parsed, slice_view(&entries, req));
    if !req.json {
        return view.render(req);
    }
    let (page, shown) = view.render_json_counted(req);
    match entries.first() {
        Some(entry) if shown == 0 => split_first_item(&mut view, entry, req).unwrap_or(page),
        _ => page,
    }
}

/// The page whose first item holds the most chars of `entry`'s text that fit, then the rest of
/// that turn as the item the resume fields name. `None` when no part of the text moves the reader on:
/// the text is used up, or what the turn carries besides its text is itself too big.
fn split_first_item(view: &mut ViewData, entry: &Entry, req: &TranscriptRequest) -> Option<String> {
    let total = entry.text.chars().count();
    let start = req.offset;
    if start >= total {
        return None;
    }
    let normal = view.items[0].json.clone();
    let mut page_of = |n: usize| -> (String, usize) {
        view.items[0].json = json_text_part(entry, &normal, start, start + n);
        let rest = RenderItem {
            turn: entry.turn,
            text: String::new(),
            json: json_text_part(entry, &normal, start + n, total),
        };
        view.items.insert(1, rest);
        let page = view.render_json_counted(req);
        view.items.remove(1);
        page
    };
    // `n` chars always leave the rest of the text out, so the page moves on by `n`.
    let (mut lo, mut hi) = (1, total - start - 1);
    if hi == 0 || page_of(1).1 == 0 {
        return None;
    }
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if page_of(mid).1 > 0 {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Some(page_of(lo).0)
}
