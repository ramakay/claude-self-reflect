//! The size cap of the SessionStart block.
//!
//! The whole emitted block (pointer, manifest, briefing, recap, learnt line,
//! `Open` block, episode index, footer) stays at or under
//! [`START_BLOCK_MAX_BYTES`] bytes. The episode index gives way first, one
//! whole line at a time from the oldest episode; its header goes with its last
//! line. Then the `Open` block, the learnt line and the briefing give way (the
//! briefing is clipped to what is left, its header kept); the recap itself is
//! never trimmed. No new line type is ever added, so what is shown is still
//! inside the grammar `provenance::is_csr_emission` matches.

use super::session_start::{
    format_briefing, format_briefing_written, format_episode_index, DEEPER_CONTEXT_FOOTER,
    MEMORY_MANIFEST,
};
use super::stop::Episode;

/// Most bytes the whole SessionStart block may take (bytes, not characters:
/// the dashes in the framing are three bytes each).
pub(crate) const START_BLOCK_MAX_BYTES: usize = 3000;

/// Bytes the handler adds around the body: the compact pointer line, the
/// manifest, the footer and the newline `println!` ends with.
fn tail_bytes(pointer: Option<&str>) -> usize {
    pointer.map_or(0, |pointer| pointer.len() + 1)
        + MEMORY_MANIFEST.len()
        + DEEPER_CONTEXT_FOOTER.len()
        + 1
}

/// Fewest bytes of briefing text worth printing: a shorter clip gives way
/// entirely.
const MIN_BRIEFING_BODY_BYTES: usize = 200;

/// A cached briefing held back until the rest of the body is known, so it can
/// be clipped (or dropped) to keep the block under the cap.
pub(crate) struct PendingBriefing {
    pub trust: crate::provenance::TrustTier,
    pub text: String,
    /// When it was written, when that is known (the header names it).
    pub written_ago: Option<String>,
}

impl PendingBriefing {
    /// The labelled block for `body`: the header line is always the one the
    /// grammar matches, whatever part of the text is kept.
    fn render(&self, body: &str) -> String {
        match &self.written_ago {
            Some(ago) => format_briefing_written(self.trust, body, ago),
            None => format_briefing(self.trust, body),
        }
    }

    /// The block with the whole text.
    pub(crate) fn render_whole(&self) -> String {
        self.render(&self.text)
    }
}

/// What the body budget leaves in: the briefing block (clipped, or `None` when
/// even a clip would not be worth printing) and whether the learnt line stays.
pub(crate) struct BodyFit {
    pub briefing: Option<String>,
    pub keep_learnt: bool,
}

/// `text` within `room` bytes: whole, else the whole lines that fit, else cut
/// at a word boundary and ended in `…`. `None` when what fits is under
/// [`MIN_BRIEFING_BODY_BYTES`].
fn clip_text(text: &str, room: usize) -> Option<String> {
    let text = text.trim_end();
    if text.len() <= room {
        return Some(text.to_string());
    }
    let mut lines_end = 0;
    for (at, _) in text.match_indices('\n') {
        if at > room {
            break;
        }
        lines_end = at;
    }
    let by_lines = text[..lines_end].trim_end();
    if by_lines.len() >= MIN_BRIEFING_BODY_BYTES {
        return Some(by_lines.to_string());
    }
    let mut limit = room.checked_sub('\u{2026}'.len_utf8())?;
    while !text.is_char_boundary(limit) {
        limit -= 1;
    }
    let at = text[..limit].rfind(char::is_whitespace)?;
    let cut = text[..at].trim_end();
    (cut.len() >= MIN_BRIEFING_BODY_BYTES).then(|| format!("{cut}\u{2026}"))
}

/// The body budget. `used` is the body printed so far, without the briefing
/// (which comes first in the block). The recap is never trimmed; the learnt
/// line and the briefing give way to it, the briefing first (clipped) and the
/// learnt line when a clipped briefing still would not fit.
pub(crate) fn fit_body(
    used: usize,
    pointer: Option<&str>,
    learnt: Option<&str>,
    briefing: Option<&PendingBriefing>,
) -> BodyFit {
    let avail = START_BLOCK_MAX_BYTES.saturating_sub(used + tail_bytes(pointer));
    let learnt_len = learnt.map_or(0, str::len);
    let Some(pending) = briefing else {
        return BodyFit {
            briefing: None,
            keep_learnt: learnt_len <= avail,
        };
    };
    let overhead = pending.render("").len();
    let clipped = |room: usize| {
        let body = clip_text(&pending.text, room.checked_sub(overhead)?)?;
        Some(pending.render(&body))
    };
    if learnt_len <= avail {
        if let Some(block) = clipped(avail - learnt_len) {
            return BodyFit {
                briefing: Some(block),
                keep_learnt: true,
            };
        }
    }
    let block = clipped(avail);
    BodyFit {
        keep_learnt: block.is_none() && learnt_len <= avail,
        briefing: block,
    }
}

/// True when a body of `used` bytes fits the cap with no episode index at all.
pub(crate) fn fits_without_index(used: usize, pointer: Option<&str>) -> bool {
    used + tail_bytes(pointer) <= START_BLOCK_MAX_BYTES
}

/// The longest prefix of `index` (header line first, then one line per
/// episode, each ending in a newline) that takes at most `room` bytes and keeps
/// at least one episode line; `""` when not even one fits.
pub(crate) fn fit_index(index: &str, room: usize) -> &str {
    if index.len() <= room {
        return index;
    }
    let header_end = index.find('\n').map_or(0, |at| at + 1);
    let mut kept = 0;
    for (at, _) in index.match_indices('\n') {
        let end = at + 1;
        if end > room {
            break;
        }
        kept = end;
    }
    if kept > header_end {
        &index[..kept]
    } else {
        ""
    }
}

/// The episode index for `episodes` (the earlier threads, newest first) within
/// what is left of the cap after `used` bytes of body, with the number of
/// episodes it names.
pub(crate) fn episode_index_within(
    episodes: &[(Episode, String)],
    pointer: Option<&str>,
    used: usize,
) -> (String, usize) {
    let room = START_BLOCK_MAX_BYTES.saturating_sub(used + tail_bytes(pointer));
    let full = format_episode_index(episodes);
    let kept = fit_index(&full, room);
    let named = kept.lines().count().saturating_sub(1);
    (kept.to_string(), named)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::session_start::EPISODE_INDEX_HEADER;

    fn index_of(lines: usize, width: usize) -> String {
        let mut out = String::from(EPISODE_INDEX_HEADER);
        for n in 0..lines {
            out.push_str(&format!("- {n}{}\n", "x".repeat(width)));
        }
        out
    }

    #[test]
    fn an_index_that_fits_is_returned_whole() {
        let index = index_of(3, 50);
        assert_eq!(fit_index(&index, index.len()), index);
        assert_eq!(fit_index(&index, index.len() + 500), index);
    }

    #[test]
    fn the_oldest_lines_go_first_and_only_whole_lines_stay() {
        let index = index_of(4, 100);
        let line = index.len() - EPISODE_INDEX_HEADER.len();
        let line = line / 4;
        let room = EPISODE_INDEX_HEADER.len() + 2 * line + line / 2;
        let kept = fit_index(&index, room);
        assert!(kept.len() <= room, "{} > {room}", kept.len());
        assert!(kept.starts_with(EPISODE_INDEX_HEADER));
        assert_eq!(kept.lines().count(), 3, "header and two lines: {kept}");
        assert!(kept.ends_with('\n'));
        assert!(kept.contains("- 0x") && kept.contains("- 1x") && !kept.contains("- 2x"));
    }

    #[test]
    fn a_header_with_no_line_is_not_printed() {
        let index = index_of(2, 100);
        let room = EPISODE_INDEX_HEADER.len() + 50;
        assert_eq!(fit_index(&index, room), "");
        assert_eq!(fit_index(&index, 0), "");
    }

    #[test]
    fn the_tail_counts_pointer_manifest_footer_and_the_final_newline() {
        let bare = tail_bytes(None);
        assert_eq!(
            bare,
            MEMORY_MANIFEST.len() + DEEPER_CONTEXT_FOOTER.len() + 1
        );
        assert_eq!(tail_bytes(Some("abc")), bare + 4);
        assert!(fits_without_index(START_BLOCK_MAX_BYTES - bare, None));
        assert!(!fits_without_index(START_BLOCK_MAX_BYTES - bare + 1, None));
    }

    fn pending(words: usize, written_ago: Option<&str>) -> PendingBriefing {
        let line = "The team shipped the parser and then fixed the cache bug.\n";
        PendingBriefing {
            trust: crate::provenance::TrustTier::External,
            text: line.repeat(words),
            written_ago: written_ago.map(str::to_string),
        }
    }

    #[test]
    fn a_briefing_that_fits_is_printed_whole() {
        let briefing = pending(2, None);
        let fit = fit_body(500, None, None, Some(&briefing));
        let block = fit.briefing.expect("kept");
        assert!(block.starts_with("[briefing: model-derived, trust=external]\n"));
        assert!(block.contains(briefing.text.trim_end()));
    }

    #[test]
    fn a_long_briefing_is_clipped_to_the_room_and_keeps_its_header() {
        let briefing = pending(200, Some("2m ago"));
        let used = 1200;
        let fit = fit_body(used, Some("pointer"), None, Some(&briefing));
        let block = fit.briefing.expect("clipped, not dropped");
        assert!(block.starts_with(
            "[briefing: model-derived, trust=external, written 2m ago, ages below as of then]\n"
        ));
        let total = used + block.len() + tail_bytes(Some("pointer"));
        assert!(total <= START_BLOCK_MAX_BYTES, "{total}");
        assert!(block.len() < briefing.render_whole().len());
        assert!(block.ends_with("\n\n"));
        assert!(crate::import::contamination::has_csr_text(&block));
    }

    #[test]
    fn one_unbroken_line_is_cut_at_a_word_and_ends_in_an_ellipsis() {
        let mut briefing = pending(1, None);
        briefing.text = "word ".repeat(2000);
        let fit = fit_body(800, None, None, Some(&briefing));
        let block = fit.briefing.expect("clipped");
        assert!(block.contains("word\u{2026}\n\n"), "{block}");
        assert!(800 + block.len() + tail_bytes(None) <= START_BLOCK_MAX_BYTES);
    }

    #[test]
    fn the_learnt_line_gives_way_before_the_briefing_is_dropped() {
        let briefing = pending(200, None);
        let avail = START_BLOCK_MAX_BYTES - tail_bytes(None);
        // Leaves room for a briefing worth printing (about 240 bytes of text
        // past its 44-byte header) only if the learnt line's 120 bytes go.
        let used = avail - 284;
        let learnt = "x".repeat(120);
        let fit = fit_body(used, None, Some(&learnt), Some(&briefing));
        assert!(!fit.keep_learnt);
        let block = fit.briefing.expect("briefing kept in its place");
        assert!(used + block.len() + tail_bytes(None) <= START_BLOCK_MAX_BYTES);
    }

    #[test]
    fn no_room_drops_the_briefing_and_the_learnt_line_but_never_the_recap() {
        let briefing = pending(200, None);
        let used = START_BLOCK_MAX_BYTES; // the recap alone fills the cap
        let learnt = "x".repeat(120);
        let fit = fit_body(used, None, Some(&learnt), Some(&briefing));
        assert!(fit.briefing.is_none());
        assert!(!fit.keep_learnt);
        let fit = fit_body(0, None, Some(&learnt), None);
        assert!(fit.keep_learnt, "a learnt line that fits stays");
    }

    #[test]
    fn no_room_after_the_body_names_no_episode() {
        let (index, named) = episode_index_within(&[], None, 0);
        assert_eq!((index.as_str(), named), ("", 0));
        let (index, named) = episode_index_within(&[], None, START_BLOCK_MAX_BYTES * 2);
        assert_eq!((index.as_str(), named), ("", 0));
    }
}
