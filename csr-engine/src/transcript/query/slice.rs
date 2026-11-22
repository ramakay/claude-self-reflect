//! The `slice` view of `csr_transcript` / `csr-engine transcript`, and the cut notes the list
//! views share with it. Lives here so `query.rs` stays under the file-size cap.

use serde_json::{json, Value};

use super::{turn_in_range, xml_escape, RenderItem, ViewData};
use crate::transcript::{Entry, ParsedTranscript, Role, TranscriptRequest, DEFAULT_LAST_N};

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
/// does not help. A `result_chars` the note names comes with the
/// `budget_chars` that keeps the turn from being fitted down again, and is
/// left out when no budget the server accepts would hold it.
fn cut_note(total: &str, limit: usize, fitted: bool) -> String {
    let bytes = total.len();
    if fitted {
        return format!(
            "… [cut: {bytes} bytes total, to fit budget_chars; raise budget_chars, or view=\"grep\" to see more]"
        );
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
        |limit: usize| budget_to_hold(slice_item_chars(&slice_item(entry, limit, false), false));
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
    } else {
        format!(
            "{shown}… [cut: {bytes} bytes total; view=\"slice\" turns=\"{turn}..{turn}\"{result}{budget} shows its first {} chars; view=\"grep\" finds the rest]",
            call.result_chars
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
fn slice_tool_use_text(tu: &crate::transcript::ToolUse, limit: usize, fitted: bool) -> String {
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
fn slice_tool_result_text(
    tr: &crate::transcript::ToolResult,
    limit: usize,
    fitted: bool,
) -> String {
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

/// One slice turn, with its words, each tool result and each tool input cut to `limit` chars.
/// A cut says so: a reply cut at a fixed 500 chars with no note once hid the line a caller asked for.
fn slice_item(entry: &Entry, limit: usize, fitted: bool) -> RenderItem {
    let ts = entry.timestamp.as_deref().unwrap_or("-");
    let words = entry.text.trim();
    let (shown, words_cut) = cap_chars(words, limit);
    let mut text = format!(
        "[turn {}] {} {ts}: {}",
        entry.turn,
        entry.role,
        xml_escape(shown)
    );
    if words_cut {
        text.push_str(&cut_note(words, limit, fitted));
    }
    for tu in &entry.tool_uses {
        text.push_str(&slice_tool_use_text(tu, limit, fitted));
    }
    for tr in &entry.tool_results {
        text.push_str(&slice_tool_result_text(tr, limit, fitted));
    }

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

pub(super) fn render_slice(parsed: &ParsedTranscript, req: &TranscriptRequest) -> ViewData {
    let duplicates = queued_duplicates(parsed);
    let filtered: Vec<&Entry> = parsed
        .entries
        .iter()
        .filter(|e| req.role.matches_role(e.role))
        .collect();

    let selected: Vec<&&Entry> = if let Some((start, end)) = req.turns {
        filtered
            .iter()
            .filter(|e| turn_in_range(e.turn, Some((start, end))))
            .collect()
    } else {
        let n = req.last.unwrap_or(DEFAULT_LAST_N);
        let skip = filtered.len().saturating_sub(n);
        filtered.iter().skip(skip).collect()
    };

    // A turn with many large results can pass the whole budget. It would then
    // be shown as nothing, and the resume hint would name the same turn, so a
    // caller on the default budget would loop. Halve the cut until the turn
    // fits; the cut note says why. A budget too small to carry any turn keeps
    // the generic rule.
    let requested = slice_result_limit(req);
    let fit_cap = (req.budget_chars >= SLICE_FIT_MIN_BUDGET_CHARS)
        .then(|| req.budget_chars - SLICE_FIT_RESERVE_CHARS);

    let items: Vec<RenderItem> = selected
        .into_iter()
        // `last` addresses original turns, like FullScope::Last in the parser.
        // Dedupe after that selection so we never pull in an older turn whose
        // full tool payloads were intentionally released during parsing.
        .filter(|e| req.include_system || !duplicates.contains(&e.turn))
        .map(|entry| {
            let mut limit = requested;
            let mut item = slice_item(entry, limit, false);
            if let Some(cap) = fit_cap {
                while slice_item_chars(&item, req.json) > cap && limit > SLICE_FIT_FLOOR_CHARS {
                    limit = (limit / 2).max(SLICE_FIT_FLOOR_CHARS);
                    item = slice_item(entry, limit, true);
                }
            }
            item
        })
        .collect();

    ViewData {
        view: "slice",
        extra_json: vec![],
        header_lines: vec!["<transcript_slice>".to_string()],
        items,
        empty_message: "(no turns in the requested range)".to_string(),
    }
}
