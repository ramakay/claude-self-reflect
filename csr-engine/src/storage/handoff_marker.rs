//! Source tag for the Lost and found handoff draft.
//!
//! The companion pane writes a draft (`companion::handoff::draft`) that the person copies or
//! enters into a fresh Claude Code session. It is a prompt the person sends, so it imports like
//! any other prompt and their real ask is kept. But most of its words are CSR's: the title and
//! lead, CSR's stored summary, the numbers, the paths, the reading rules. Without a tag, later
//! search, recaps and episodes would read those words as the person's own.
//!
//! The tag is one closing line, `↳ csr-handoff <conversation id>`, the same shape as the dream
//! card's `↳ csr-dream <id>` ([`super::dream_attribution::MARKER_PREFIX`]). The stored chunk
//! keeps the whole paste and the marker (search labels it); a derived artifact drops the
//! CSR-authored span with [`strip_handoff`]. A draft is recognised only when the marker closes
//! it, so ordinary prose that happens to use one of these headings is never touched.
//!
//! Recognition is a whitelist, not a parser. A draft is the opening line, then every line
//! `companion::handoff::draft` always writes ([`REQUIRED`]) in that order, then the last ask
//! heading, then the marker. Anything else abstains: a hand-edited draft that lost one of those
//! lines, a quoted example, a forged marker over ordinary writing, a draft inside a code fence
//! (search and derived text share one scan, `import::contamination::detect`). A draft longer than
//! [`MAX_WHOLE_DRAFT_BYTES`] is not one chunk and stays unlabelled by design; `draft()` trims its
//! own fields to fit, so only a hand-edited paste can be that long.
//!
//! What the draft is, top to bottom, and what each part is for here:
//!
//! - [`HANDOFF_OPENING`] line, lead, `### What the thread says`, summary, numbers, places, how
//!   to read it: CSR's words. The span a derived artifact drops and the search label covers.
//! - [`HANDOFF_ASK_HEADER`] and the ask under it: kept. It is the request. The last ask heading
//!   before the marker is the one that counts.
//! - the marker line: kept in the stored chunk, removed from derived text.
//! - anything the person typed before the opening or after the marker: kept.

/// The draft's first line, up to the thread title. Single source: `companion::handoff` formats
/// the draft with this exact string and the recognisers below look for it.
pub const HANDOFF_OPENING: &str = "## Pick up a quiet thread: ";
/// The heading of the ask, the one section of the draft a derived artifact keeps.
pub const HANDOFF_ASK_HEADER: &str = "### The ask";
/// The lead line's opening, the first line after the title.
pub const HANDOFF_LEAD: &str = "This thread went quiet after ";
/// Section titles (each written as `### <title>`), in the order the draft writes them.
pub const HANDOFF_THREAD_TITLE: &str = "What the thread says";
pub const HANDOFF_SUMMARY_TITLE: &str = "CSR's summary of that conversation";
pub const HANDOFF_NUMBERS_TITLE: &str = "The session in numbers";
pub const HANDOFF_PLACES_TITLE: &str = "Where CSR keeps it";
pub const HANDOFF_READ_TITLE: &str = "How to read it";

/// The lines every draft carries between its opening and its ask, in order: the lead (by its
/// opening words) and the five section headings (whole lines).
const REQUIRED: usize = 6;
fn required_matches(index: usize, line: &str) -> bool {
    let heading = |title: &str| {
        line.trim_end()
            .strip_prefix("### ")
            .is_some_and(|t| t == title)
    };
    match index {
        0 => line.starts_with(HANDOFF_LEAD),
        1 => heading(HANDOFF_THREAD_TITLE),
        2 => heading(HANDOFF_SUMMARY_TITLE),
        3 => heading(HANDOFF_NUMBERS_TITLE),
        4 => heading(HANDOFF_PLACES_TITLE),
        _ => heading(HANDOFF_READ_TITLE),
    }
}
/// The marker's fixed prefix. Shares no substring with the dream marker (`↳ csr-dream `), the
/// machine sentinels or any emission header.
pub const HANDOFF_MARKER_PREFIX: &str = "↳ csr-handoff ";
/// What the marker carries when no conversation id is on record.
pub const HANDOFF_NO_ID: &str = "unidentified";

/// Longest id read from a marker. Conversation ids are UUIDs; the bound keeps a pasted wall of
/// characters from reading as one.
const MAX_ID_LEN: usize = 80;

fn is_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// The single closing line of a draft: the conversation id and nothing else (no title, no
/// paths). It travels into whatever the draft is pasted into, so it must be safe to leak.
pub fn marker_line(conversation_id: Option<&str>) -> String {
    let id: String = conversation_id
        .unwrap_or("")
        .chars()
        .filter(|c| is_id_char(*c))
        .take(MAX_ID_LEN)
        .collect();
    let id = if id.is_empty() {
        HANDOFF_NO_ID.to_owned()
    } else {
        id
    };
    format!("{HANDOFF_MARKER_PREFIX}{id}")
}

/// Bytes of the marker (prefix and id) at the start of `line`, `None` when the line is not a
/// marker. An id of no characters is not a marker.
fn marker_len(line: &str) -> Option<usize> {
    let rest = line.strip_prefix(HANDOFF_MARKER_PREFIX)?;
    let id = rest
        .chars()
        .take(MAX_ID_LEN)
        .take_while(|c| is_id_char(*c))
        .map(char::len_utf8)
        .sum::<usize>();
    (id > 0).then_some(HANDOFF_MARKER_PREFIX.len() + id)
}

/// Where a draft that opens at some line ends, counted in lines after the opening one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Extent {
    /// Lines from the opening line (offset 0) to the last ask heading before the marker. CSR's
    /// span is the lines `[0, span_end)`.
    pub span_end: usize,
    /// The marker line's offset from the opening line.
    pub marker: usize,
}

/// Whether `line` opens a draft (column zero, so a quoted or indented mention never does).
pub(crate) fn opens_draft(line: &str) -> bool {
    line.starts_with(HANDOFF_OPENING)
}

/// The extent of the draft whose opening line has just been seen, from the lines after it. The
/// lines `draft()` always writes must all be there in order, then an ask heading, then the
/// marker; anything else abstains. The last ask heading before the marker ends CSR's span, so a
/// quoted earlier heading cannot end it early. The caller has already ruled out an opening inside
/// a code fence.
pub(crate) fn extent<'a>(after_opening: impl Iterator<Item = &'a str>) -> Option<Extent> {
    let mut next = 0;
    let mut ask = None;
    for (i, line) in after_opening.enumerate() {
        if opens_draft(line) {
            return None;
        }
        if marker_len(line).is_some() {
            return match (next == REQUIRED, ask) {
                (true, Some(span_end)) => Some(Extent {
                    span_end,
                    marker: i + 1,
                }),
                _ => None,
            };
        }
        if next < REQUIRED {
            if required_matches(next, line) {
                next += 1;
            }
        } else if line.trim_end() == HANDOFF_ASK_HEADER {
            ask = Some(i + 1);
        }
    }
    None
}

/// Largest draft kept whole by the chunker. `companion::handoff::draft` trims its own fields to
/// fit; a bigger stretch between an opening and a marker is a hand-edited paste, chunked like
/// any other text and left unlabelled by design.
pub const MAX_WHOLE_DRAFT_BYTES: usize = 12_000;

/// Byte offsets of the marker line of the draft that opens at `start` in `text`.
struct MarkerAt {
    line_start: usize,
    token_end: usize,
    line_end: usize,
}

fn marker_at(text: &str, start: usize) -> Option<MarkerAt> {
    let mut lines = text[start..].split_inclusive('\n');
    lines.next()?;
    let e = extent(lines.clone())?;
    let mut offset = start;
    for (i, line) in text[start..].split_inclusive('\n').enumerate() {
        if i == e.marker {
            return Some(MarkerAt {
                line_start: offset,
                token_end: offset + marker_len(line)?,
                line_end: offset + line.len(),
            });
        }
        offset += line.len();
    }
    None
}

fn draft_hits(text: &str) -> Vec<crate::import::contamination::Hit> {
    crate::import::contamination::detect_within(text)
        .into_iter()
        .filter(|hit| hit.reason == crate::import::contamination::ContaminationReason::HandoffDraft)
        .collect()
}

/// The byte range of each marker-closed draft in `text`, from the start of its opening line to
/// the end of its marker (the id, not the rest of that line). The importer cuts its chunks at
/// these edges so a draft is one chunk: the search label needs the opening and the marker in the
/// same chunk to prove a draft, and a draft is longer than the chunk budget. The drafts are the
/// ones the search label finds (one scan), and none over [`MAX_WHOLE_DRAFT_BYTES`].
pub(crate) fn draft_ranges(text: &str) -> Vec<(usize, usize)> {
    if !text.contains(HANDOFF_MARKER_PREFIX) {
        return Vec::new();
    }
    draft_hits(text)
        .iter()
        .filter_map(|hit| {
            let m = marker_at(text, hit.start_byte)?;
            (m.token_end - hit.start_byte <= MAX_WHOLE_DRAFT_BYTES)
                .then_some((hit.start_byte, m.token_end))
        })
        .collect()
}

/// `text` with every marker-closed handoff draft reduced to the person's part: CSR's span (the
/// opening line through the line before the last `### The ask`) is dropped, the ask is kept, and
/// the marker is cut from its line. Whatever the person typed around the draft is untouched. A
/// cheap no-op when `text` carries no marker. The drafts are the ones search labels (one scan).
///
/// For derived artifacts only: a stored chunk keeps the draft as it was pasted (search labels
/// it), so this is never applied to conversation text.
pub fn strip_handoff(text: &str) -> String {
    if !text.contains(HANDOFF_MARKER_PREFIX) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for hit in draft_hits(text) {
        if hit.start_byte < cursor {
            continue;
        }
        let Some(m) = marker_at(text, hit.start_byte) else {
            continue;
        };
        out.push_str(&text[cursor..hit.start_byte]);
        // the ask, up to the marker line
        out.push_str(&text[hit.end_byte..m.line_start]);
        // the marker leaves its line; anything the person typed after it stays
        let rest = &text[m.token_end..m.line_end];
        if !rest.trim().is_empty() {
            out.push_str(rest.trim_start_matches([' ', '\t']));
        }
        cursor = m.line_end;
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A draft with exactly the lines `draft()` always writes.
    fn draft(id: &str) -> String {
        format!(
            "{HANDOFF_OPENING}Retry the upload\n\n{HANDOFF_LEAD}2026-10-31 14:02 in conversation `x`. \
             Everything below is copied from CSR's stored rows.\n\n\
             ### {HANDOFF_THREAD_TITLE}\n\nRetry the upload\n\n\
             ### {HANDOFF_SUMMARY_TITLE}\n\nAsked: Fix the upload\n\n\
             ### {HANDOFF_NUMBERS_TITLE}\n\n- 412 turns\n\n\
             ### {HANDOFF_PLACES_TITLE}\n\n- /Users/x/.claude/projects/p/s.jsonl\n\n\
             ### {HANDOFF_READ_TITLE}\n\nCSR's rule: read it with the tool.\n\n\
             {HANDOFF_ASK_HEADER}\n\nRevisit this thread as a side task.\n{}\n",
            marker_line(Some(id))
        )
    }

    const ASK_ONLY: &str = "### The ask\n\nRevisit this thread as a side task.\n";

    #[test]
    fn the_ask_header_constant_is_the_ask_title_under_three_hashes() {
        assert_eq!(HANDOFF_ASK_HEADER, "### The ask");
    }

    #[test]
    fn the_marker_is_the_id_alone_and_a_missing_id_gets_the_fixed_placeholder() {
        assert_eq!(
            marker_line(Some("3002ab28-7ba1-4c61-9d2e-0a1b2c3d4e5f")),
            "↳ csr-handoff 3002ab28-7ba1-4c61-9d2e-0a1b2c3d4e5f"
        );
        assert_eq!(marker_line(None), "↳ csr-handoff unidentified");
        assert_eq!(marker_line(Some("")), "↳ csr-handoff unidentified");
        // nothing but id characters ever reaches the line
        assert_eq!(marker_line(Some("a b/c\nd`e")), "↳ csr-handoff abcde");
    }

    #[test]
    fn the_handoff_marker_is_not_a_dream_marker() {
        let line = marker_line(Some("deadbeefdeadbeef"));
        assert!(crate::storage::dream_attribution::scan_markers(&line).is_empty());
        assert!(!line.contains(crate::storage::dream_attribution::MARKER_PREFIX));
    }

    #[test]
    fn a_closed_draft_keeps_its_ask_and_drops_csrs_words() {
        let text = format!(
            "fix the header too\n\n{}also fix the footer\n",
            draft("abc-123")
        );
        assert_eq!(
            strip_handoff(&text),
            format!("fix the header too\n\n{ASK_ONLY}also fix the footer\n")
        );
    }

    /// Review finding 1: a draft quoted in a code fence is the person's example.
    #[test]
    fn a_draft_inside_a_code_fence_is_the_persons_and_comes_back_whole() {
        let input = "```text\n## Pick up a quiet thread: Example\nMy own notes\n### The ask\nship it\n↳ csr-handoff abc\n```\n";
        assert_eq!(strip_handoff(input), input);
        assert!(draft_ranges(input).is_empty());
        // a whole real draft inside a fence is a mention too, for search and for derived text
        let fenced = format!("```\n{}```\n", draft("abc"));
        assert_eq!(strip_handoff(&fenced), fenced);
        assert!(draft_ranges(&fenced).is_empty());
        assert!(crate::import::contamination::detect(&fenced).is_empty());
    }

    /// Review finding 2: any marker over ordinary writing used to close a "draft".
    #[test]
    fn a_forged_marker_over_ordinary_writing_comes_back_whole_and_unlabeled() {
        let input = "## Pick up a quiet thread: Release plan\nMy own decision: ship Friday.\n↳ csr-handoff forged\n";
        assert_eq!(strip_handoff(input), input);
        assert!(draft_ranges(input).is_empty());
        assert!(crate::import::contamination::detect(input).is_empty());
    }

    /// Review finding 3, Codex's literal input has none of the lines `draft()` always writes, so
    /// it abstains and the person's words come back whole.
    #[test]
    fn a_quoted_ask_heading_in_a_hand_made_text_abstains() {
        let input = "## Pick up a quiet thread: Example\n```markdown\n### The ask\n```\nCSR-only summary\n### The ask\nship it\n↳ csr-handoff abc\n";
        assert_eq!(strip_handoff(input), input);
    }

    /// Review finding 3 on a real draft: an earlier quoted ask heading inside the draft does not
    /// end CSR's span; the last one before the marker does.
    #[test]
    fn the_last_ask_heading_before_the_marker_ends_csrs_span() {
        let text = draft("abc").replace(
            "CSR's rule: read it with the tool.\n",
            "CSR's rule: read it with the tool.\n```markdown\n### The ask\n```\nCSR-only text\n### The ask\nquoted\n",
        );
        assert_eq!(strip_handoff(&text), ASK_ONLY);
        let hits = crate::import::contamination::detect(&text);
        assert_eq!(hits.len(), 1);
        assert!(text[hits[0].start_byte..hits[0].end_byte].contains("CSR-only text"));
    }

    #[test]
    fn a_draft_missing_one_of_the_lines_it_always_writes_abstains() {
        for gone in [
            format!("### {HANDOFF_THREAD_TITLE}\n"),
            format!("### {HANDOFF_SUMMARY_TITLE}\n"),
            format!("### {HANDOFF_NUMBERS_TITLE}\n"),
            format!("### {HANDOFF_PLACES_TITLE}\n"),
            format!("### {HANDOFF_READ_TITLE}\n"),
            HANDOFF_LEAD.to_owned(),
            format!("{HANDOFF_ASK_HEADER}\n"),
        ] {
            let text = draft("abc").replace(&gone, "");
            assert_eq!(strip_handoff(&text), text, "without {gone:?}");
            assert!(draft_ranges(&text).is_empty(), "without {gone:?}");
        }
        // and the lines out of order
        let swapped = draft("abc")
            .replace(&format!("### {HANDOFF_NUMBERS_TITLE}"), "### TMP")
            .replace(
                &format!("### {HANDOFF_PLACES_TITLE}"),
                &format!("### {HANDOFF_NUMBERS_TITLE}"),
            )
            .replace("### TMP", &format!("### {HANDOFF_PLACES_TITLE}"));
        assert_eq!(strip_handoff(&swapped), swapped);
    }

    #[test]
    fn an_unclosed_draft_or_a_lone_heading_is_left_alone() {
        let unclosed = draft("abc").replace("↳ csr-handoff abc\n", "");
        assert_eq!(strip_handoff(&unclosed), unclosed);
        let prose = "### CSR's summary of that conversation\nmine\n";
        assert_eq!(strip_handoff(prose), prose);
        let bare = format!("hello\n{}\n", marker_line(Some("abc")));
        assert_eq!(strip_handoff(&bare), bare);
    }

    #[test]
    fn a_fresh_opening_before_the_marker_means_the_first_is_not_closed() {
        let text = format!("{HANDOFF_OPENING}a\n\nsummary\n{}", draft("abc"));
        let kept = strip_handoff(&text);
        // only the second, closed draft is cut; the first stays whole
        assert!(kept.starts_with(&format!("{HANDOFF_OPENING}a\n\nsummary\n")));
        assert!(!kept.contains("went quiet"));
    }

    #[test]
    fn text_typed_on_the_marker_line_survives() {
        let text =
            draft("abc").replace("↳ csr-handoff abc\n", "↳ csr-handoff abc and the footer\n");
        assert_eq!(strip_handoff(&text), format!("{ASK_ONLY}and the footer\n"));
    }

    #[test]
    fn the_draft_range_runs_from_its_opening_to_the_end_of_its_marker() {
        let text = format!("typed first\n\n{}also the footer\n", draft("abc-123"));
        let ranges = draft_ranges(&text);
        assert_eq!(ranges.len(), 1);
        let (from, to) = ranges[0];
        assert!(text[from..].starts_with(HANDOFF_OPENING));
        assert!(text[..to].ends_with("↳ csr-handoff abc-123"));
        assert_eq!(&text[to..], "\nalso the footer\n");
    }

    #[test]
    fn a_hand_edited_paste_over_the_cap_is_not_one_chunk() {
        let long = draft("abc").replace(
            "Asked: Fix the upload",
            &format!("Asked: {}", "padding ".repeat(1600)),
        );
        assert!(long.len() > MAX_WHOLE_DRAFT_BYTES);
        assert!(draft_ranges(&long).is_empty());
        // derived text is still stripped: the cap is about chunking only
        assert_eq!(strip_handoff(&long), ASK_ONLY);
    }

    #[test]
    fn a_crlf_paste_is_recognised() {
        let text = draft("abc").replace('\n', "\r\n");
        let kept = strip_handoff(&text);
        assert!(!kept.contains("went quiet"), "{kept:?}");
        assert!(kept.contains("Revisit this thread"), "{kept:?}");
    }
}
