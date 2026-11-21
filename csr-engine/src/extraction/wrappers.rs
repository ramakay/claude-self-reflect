//! Harness wrapper tags in stored words, for display.
//!
//! Claude Code wraps a pasted block in `<pasted_content id="818d">…</pasted_content>` and injects
//! `<system-reminder>`, `<command-*>` and similar blocks into user turns. The episode `request` is
//! cut at 200 bytes, so a stored one often has an open tag and no close tag. Anything that shows
//! stored words to a person goes through [`without_harness_tags`] so no wrapper tag reaches them.
//!
//! Read-side only: nothing here changes what is stored, so no byte offset or chunk moves. It is a
//! separate function from `companion::send::own_words`, whose callers and tests are unchanged.

use regex::Regex;
use std::sync::LazyLock;

const NAMES: &str = r"pasted_content|system-reminder|command-[\w-]+|local-command-[\w-]+|persisted-output|cross-session-message|task-notification|user-prompt-submit-hook";

static OPEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"<({NAMES})\b[^>]*(?:>|$)")).unwrap());
static ANY_CLOSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"</(?:{NAMES})\b[^>]*(?:>|$)")).unwrap());
/// The end of a stored text cut inside a closing tag: `</past`.
static CUT_CLOSE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"</[a-z_-]*$").unwrap());

/// `text` without wrapper tags. Text with no tag comes back unchanged, byte for byte. Otherwise:
/// the words typed outside any wrapper block, when there are any; else, when the whole text was a
/// paste, the pasted words without their tags. Other wrapper blocks are dropped with their
/// contents. A block with no close tag runs to the end of the text.
///
/// A person can type a tag's name while talking about it. A tag right after a backtick is such a
/// mention, and so is a bare tag with no close, no attributes and words before it on its line: the
/// harness writes its unclosed blocks at the start of a line or with an id. Mentions stay as typed.
pub fn without_harness_tags(text: &str) -> String {
    if !text.contains('<') || (!OPEN.is_match(text) && !ANY_CLOSE.is_match(text)) {
        return text.to_owned();
    }
    let mut typed = String::new();
    let mut pasted: Vec<&str> = Vec::new();
    let mut at = 0;
    let mut found = false;
    for cap in OPEN.captures_iter(text) {
        let open = cap.get(0).unwrap();
        if open.start() < at || after_backtick(text, open.start()) {
            continue;
        }
        let name = &cap[1];
        let close = Regex::new(&format!(r"</{}\b[^>]*(?:>|$)", regex::escape(name))).unwrap();
        let closed = close.find(&text[open.end()..]);
        let bare = open.as_str() == format!("<{name}>");
        let line_start = text[..open.start()]
            .rsplit('\n')
            .next()
            .is_none_or(|before| before.trim().is_empty());
        if closed.is_none() && bare && !line_start {
            continue;
        }
        found = true;
        typed.push_str(&text[at..open.start()]);
        typed.push(' ');
        let (inner_end, next) = match closed {
            Some(c) => (open.end() + c.start(), open.end() + c.end()),
            None => (text.len(), text.len()),
        };
        if name == "pasted_content" {
            pasted.push(&text[open.end()..inner_end]);
        }
        at = next;
    }
    typed.push_str(&text[at..]);
    found |= ANY_CLOSE
        .find_iter(&typed)
        .any(|c| !after_backtick(&typed, c.start()));
    if !found {
        return text.to_owned();
    }
    let typed = fold(&ANY_CLOSE.replace_all(&typed, |c: &regex::Captures<'_>| {
        let at = c.get(0).unwrap();
        if after_backtick(&typed, at.start()) {
            at.as_str().to_owned()
        } else {
            " ".to_owned()
        }
    }));
    if !typed.is_empty() {
        return typed;
    }
    let pasted: Vec<String> = pasted
        .into_iter()
        .map(without_harness_tags)
        .filter(|p| !p.trim().is_empty())
        .collect();
    fold(&pasted.join(" "))
}

fn after_backtick(text: &str, at: usize) -> bool {
    at > 0 && text.as_bytes()[at - 1] == b'`'
}

fn fold(text: &str) -> String {
    let whole = text.split_whitespace().collect::<Vec<_>>().join(" ");
    CUT_CLOSE.replace(&whole, "").trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::without_harness_tags as strip;

    #[test]
    fn a_tag_named_in_typed_words_stays_as_typed() {
        for text in [
            "Fix `<system-reminder>` parsing in src/main.rs.",
            "why does <system-reminder> leak here",
            "the `</pasted_content>` line   is wrong",
            "strip `<pasted_content id=\"x\">` too",
        ] {
            assert_eq!(strip(text), text);
        }
    }

    #[test]
    fn an_unclosed_block_the_harness_wrote_still_goes() {
        assert_eq!(strip("do it\n<system-reminder>injected and cut"), "do it");
        assert_eq!(strip("do it\n  <system-reminder>injected and cut"), "do it");
        assert_eq!(strip("do it <pasted_content id=\"x\">cut"), "do it");
    }

    #[test]
    fn a_text_cut_inside_a_closing_tag_shows_no_markup() {
        for text in [
            "<pasted_content id=\"x\">Fix the retry</pasted_content",
            "<pasted_content id=\"x\">Fix the retry</pasted_con",
            "<pasted_content id=\"x\">Fix the retry</",
        ] {
            assert_eq!(strip(text), "Fix the retry");
        }
    }

    #[test]
    fn typed_words_outside_a_closed_block_win() {
        assert_eq!(
            strip("check this <pasted_content id=\"3b47\">\nother agent's words\n</pasted_content> and fix it"),
            "check this and fix it"
        );
    }

    #[test]
    fn a_message_that_was_all_paste_keeps_the_inner_words() {
        assert_eq!(
            strip("<pasted_content id=\"818d\">\nspawn a worker to fix it\n</pasted_content>"),
            "spawn a worker to fix it"
        );
    }

    #[test]
    fn a_cut_off_paste_keeps_the_inner_words() {
        assert_eq!(
            strip("<pasted_content id=\"818d\">\nspawn a worker to fix the feature earlier h"),
            "spawn a worker to fix the feature earlier h"
        );
        assert_eq!(
            strip("typed first <pasted_content id=\"818d\">\nspawn a wor"),
            "typed first"
        );
        assert_eq!(strip("<pasted_content id=\"81"), "");
    }

    #[test]
    fn the_close_tag_may_carry_the_id() {
        assert_eq!(
            strip("<pasted_content id=\"818d\">body words</pasted_content id=\"818d\">"),
            "body words"
        );
        assert_eq!(
            strip("mine <pasted_content id=\"x\">p</pasted_content id=\"x\"> more"),
            "mine more"
        );
    }

    #[test]
    fn other_wrappers_go_with_their_contents() {
        assert_eq!(
            strip("<system-reminder>injected</system-reminder>do it"),
            "do it"
        );
        assert_eq!(strip("<local-command-caveat>Caveat: generated by"), "");
        assert_eq!(strip("<persisted-output>Output too large"), "");
        assert_eq!(
            strip("Another <cross-session-message from=\"x\">hi</cross-session-message> ok"),
            "Another ok"
        );
        assert_eq!(
            strip("<pasted_content>a <system-reminder>b</system-reminder> c</pasted_content>"),
            "a c"
        );
    }

    #[test]
    fn text_without_a_wrapper_tag_is_untouched() {
        for plain in [
            "Fix the upload",
            "  spaced   out\n\nlines  ",
            "use Vec<String> and a < b > c",
            "<div>html is not a harness wrapper</div>",
            "",
        ] {
            assert_eq!(strip(plain), plain);
        }
    }
}
