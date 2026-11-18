//! Terminal-capture replay: text that is a screen recorder's frames, not
//! anything anyone said.
//!
//! A script that polls a terminal pane (a tmux `capture-pane` loop, a TUI
//! test harness) prints one line per frame: a frame index and timestamp, the
//! prompt line as it was typed so far, and whatever the status line showed.
//! The same words come back on every frame, each a longer prefix of the last
//! (`❯ why did we d`, `❯ why did we dr`, ...), and the spinner rows
//! (`✶ Twisting… (running UserPromptSubmit hooks… 5/6 · 0s)`) repeat between
//! them. Such text matches a typed question better than the answer does and
//! carries no answer. The rule reads the text alone.
//!
//! It fires only when the text is frames and nothing else: at least
//! [`MIN_FRAMES`] frame segments, and no other segment but a first or last one
//! cut mid-line by the chunk boundary. A test result, an error, an answer or
//! any other substantive line disqualifies it, as does a chunk that holds one
//! prompt line or one status line among real content. Search uses it to
//! re-order only; import never drops on it.

use std::sync::LazyLock;

use regex::Regex;

/// Fewest frame segments a replay holds. A handful of prompt lines in a pasted
/// shell session stays under it.
const MIN_FRAMES: usize = 6;

/// Frame index and timestamp a recorder prepends: `24 20.706 9 ` or `154 10 `.
static FRAME_INDEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+(?: \d+(?:\.\d+)?){1,2}(?: |$)").expect("frame index regex"));

/// A TUI spinner row: a spinner glyph, a gerund and an ellipsis, optionally
/// with the elapsed time and effort or hook progress in parentheses.
static SPINNER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\u{00b7}\u{2722}\u{2733}\u{2736}\u{273b}\u{273d}\u{2726}*]\s+\p{L}+\u{2026}(?:\s*\(.*\))?$")
        .expect("spinner regex")
});

/// A run of the Claude Code logo glyphs. A progress bar (`████████`) is a run
/// of the same block glyph; the logo mixes in the half and corner blocks.
static LOGO_RUN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\u{2590}\u{259b}\u{259c}\u{259d}\u{2598}\u{2588}\u{2580}\u{258c}]{3,}")
        .expect("logo regex")
});

/// The banner's model and plan line (`Opus 5.5 with medium effort · Claude Max`).
/// A banner is a recognisable structure only when this line is in the text.
static MODEL_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bwith \w+ effort \u{00b7} Claude\b").expect("model line regex"));

/// Version and working-directory lines of the banner.
static BANNER_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:Claude Code v\d|\bwith \w+ effort \u{00b7} Claude\b|~/\S*\s+\u{00b7}\s+/\w+$)")
        .expect("banner text regex")
});

/// Whether `line` is a line of the start banner: its logo glyphs (a run that
/// is not a single repeated block), its version, model or directory line.
fn is_banner_line(line: &str) -> bool {
    BANNER_TEXT.is_match(line)
        || LOGO_RUN
            .find_iter(line)
            .any(|run| run.as_str().chars().any(|c| c != '\u{2588}'))
}

/// Other TUI chrome that repeats on every frame.
static CHROME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:Showing detailed transcript\b|\? for shortcuts|\u{23f5}\u{23f5}\s)")
        .expect("chrome regex")
});

#[derive(Clone, Copy, PartialEq, Eq)]
enum Segment {
    Frame,
    Banner,
    Other,
}

/// Longest a first or last segment can be and still be the remnant of a frame
/// the chunk boundary cut.
const MAX_CUT_REMNANT_CHARS: usize = 20;

/// Whether `text` is terminal-capture frames (keystroke replay, spinner and
/// status rows, the start banner) and nothing else, so it carries no answer.
pub fn is_terminal_replay(text: &str) -> bool {
    let has_model_line = MODEL_LINE.is_match(text);
    let segments = segments(text, has_model_line);
    if segments.len() < 2 {
        return false;
    }
    let frames = segments
        .iter()
        .filter(|(kind, _)| *kind == Segment::Frame)
        .count();
    let banners = segments
        .iter()
        .filter(|(kind, _)| *kind == Segment::Banner)
        .count();
    let prompts: Vec<&str> = segments
        .iter()
        .filter_map(|(kind, rest)| {
            (*kind == Segment::Frame)
                .then(|| rest.strip_prefix('\u{276f}').map(str::trim))
                .flatten()
        })
        .collect();
    let last = segments.len() - 1;
    let only_cut_remnants = segments
        .iter()
        .enumerate()
        .filter(|(_, (kind, _))| *kind == Segment::Other)
        .all(|(at, (_, rest))| (at == 0 || at == last) && is_cut_remnant(rest, &prompts));
    only_cut_remnants && (frames + banners >= MIN_FRAMES || banners >= 2)
}

/// A first or last segment the chunk boundary cut: part of a prompt line the
/// frames repeat, or a few characters of a frame index.
fn is_cut_remnant(rest: &str, prompts: &[&str]) -> bool {
    rest.chars().count() <= MAX_CUT_REMNANT_CHARS
        && rest
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ' ')
        || prompts.iter().any(|prompt| prompt.contains(rest))
}

/// Classify every `" | "`-joined segment of every line.
fn segments(text: &str, has_model_line: bool) -> Vec<(Segment, String)> {
    let mut parsed: Vec<(bool, String)> = Vec::new();
    for line in text.lines() {
        for piece in line.split(" | ") {
            // A line cut at a separator leaves a dangling `|`.
            let piece = piece.trim().trim_end_matches('|').trim();
            if piece.is_empty() {
                continue;
            }
            match FRAME_INDEX.find(piece) {
                Some(index) => parsed.push((true, piece[index.end()..].trim().to_string())),
                None => parsed.push((false, piece.to_string())),
            }
        }
    }
    let prompt_text: Vec<Option<&str>> = parsed
        .iter()
        .map(|(_, rest)| rest.strip_prefix('\u{276f}').map(str::trim))
        .collect();
    parsed
        .iter()
        .enumerate()
        .map(|(at, (indexed, rest))| {
            let kind = if rest.is_empty()
                || rest == "\u{276f}"
                || SPINNER.is_match(rest)
                || CHROME.is_match(rest)
            {
                Segment::Frame
            } else if has_model_line && is_banner_line(rest) {
                Segment::Banner
            } else if prompt_text[at].is_some() && (*indexed || grows(&prompt_text, at)) {
                Segment::Frame
            } else {
                Segment::Other
            };
            (kind, rest.clone())
        })
        .collect()
}

/// Whether the prompt segment at `at` is a prefix of the nearest prompt segment
/// before or after it (or equal to it): the same line typed out over frames.
fn grows(prompts: &[Option<&str>], at: usize) -> bool {
    let Some(this) = prompts[at] else {
        return false;
    };
    let related = |other: Option<&str>| {
        other.is_some_and(|other| {
            !this.is_empty() && (other.starts_with(this) || this.starts_with(other))
        })
    };
    let before = prompts[..at].iter().rev().find_map(|prompt| *prompt);
    let after = prompts[at + 1..].iter().find_map(|prompt| *prompt);
    related(before) || related(after)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The five live chunks of one fork's pane recording that outranked the
    /// answer to "why did we drop Docker and Qdrant" (agent-aa8201b27121421bd).
    const KEYSTROKES: &str = r#"y did we drop Docker and Qdrant
24 20.706 9 ❯ why did we drop Docker and Qdrant f
25 20.87 9 ❯ why did we drop Docker and Qdrant for
26 21.034 9 ❯ why did we drop Docker and Qdrant for a
27 21.2 9 ❯ why did we drop Docker and Qdrant for a s
28 21.367 9 ❯ why did we drop Docker and Qdrant for a sin
29 21.532 9 ❯ why did we drop Docker and Qdrant for a singl
30 21.697 9 ❯ why did we drop Docker and Qdrant for a single
31 21.863 9 ❯ why did we drop Docker and Qdrant for a single Ru
32 22.028 9 ❯ why did we drop Docker and Qdrant for a single Rust
33 22.197 9 ❯ why did we drop Docker and Qdrant for a single Rust b
34 22.363 9 ❯ why did we drop Docker and Qdrant for a single Rust bin
35 22.528 9 ❯ why did we drop Docker and Qdrant for a single Rust binar
36 22.693 9 ❯ why did we drop Docker and Qdrant for a single Rust binary
37 23.53 11 ❯ why did we d"#;
    const ACTIONING: &str = r#"154 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | · Actioning… (3s · thinking with medium effort) | ❯
55 27.318 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✢ Actioning… (3s · thinking with medium effort) | ❯
56 27.485 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✶ Actioning… (4s · thinking with medium effort) |"#;
    const TWISTING_RUN: &str = r#"rop Docker and Qdrant for a single Rust binary | ✶ Twisting… (running UserPromptSubmit hooks… 5/6 · 0s) | ❯
38 23.696 11 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✻ Twisting… (running UserPromptSubmit hooks… 5/6 · 0s) | ❯
39 23.86 11 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✽ Twisting… | ❯
40 24.023 11 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✽ Twisting… | ❯
41 24.364 11 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✶ Twisting… | ❯
42 24.529 11 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✳ Twisting… | ❯
43 24.694 11 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✢ Twisting… | ❯
44 24.86 11 ❯ why did we drop Docker and Qdrant for a single Rust binary | · Twisting… | ❯
45 25.191 10 ❯ why did we drop Docker"#;
    const TWISTING_TAIL: &str = r#"and Qdrant for a single Rust binary | · Twisting… | ❯
46 25.356 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✳ Twisting… | ❯
47 25.519 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✶ Twisting… | ❯
48 25.684 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✻ Twisting… | ❯
49 25.843 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✽ Twisting… | ❯
50 26.328 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✶ Twisting… | ❯
51 26.491 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✳ Twisting… | ❯
52 26.654 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | ✢ Twisting… | ❯
53 26.82 10 ❯ why did we drop Docker and Qdrant for a single Rust binary | · Actioning… (3s · thinking with medium effort) | ❯
54 27."#;
    /// The start banner, a prompt and the reply: a screen with real content.
    const SCREEN_WITH_ANSWER: &str = r#"▝▜██████▀  Opus 5.5 with medium effort · Claude Max
 ▝▝   ▝▝   ~/projects/claude-self-reflect · /rc
❯ why did we drop Docker and Qdrant for a single Rust binary?
⏺ Checking past sessions for the actual reasoning.
  Called claude-self-reflect
⏺ Installing it was too painful, and the scale never needed a database server. A Phase 0 spike then showed an embedded
  index was faster anyway.
  - Install friction: v7 needed Docker Desktop, a Qdrant container, a Python watcher and a venv. One past session was a
    146-iteration loop just fighting the npm + Docker packaging. v8 is one 44MB binary installed with curl | sh.
  - Fragility: if Docker wasn't running, memory went dead with no error. Qdrant also ran out of memory once because it
    had no memory limit (the fix was mem_limit: 2g, and that incident is now a test fixture). Commit fa332fe had to add
    Qd"#;
    const BANNER_ONLY: &str = "\u{2590}\u{259b}\u{2588}\u{2588}\u{2588}\u{259c}\u{258c}   Claude Code v2.1.296\n\u{259d}\u{259c}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259b}\u{2598}  Opus 5.5 with xhigh effort \u{00b7} Claude Max\n \u{2598}\u{2598} \u{259d}\u{259d}    ~/projects/claude-self-reflect \u{00b7} /rc\n\n";

    #[test]
    fn keystroke_replay_is_replay() {
        assert!(is_terminal_replay(KEYSTROKES));
    }

    #[test]
    fn spinner_and_status_frames_are_replay() {
        assert!(is_terminal_replay(ACTIONING));
        assert!(is_terminal_replay(TWISTING_RUN));
        assert!(is_terminal_replay(TWISTING_TAIL));
    }

    #[test]
    fn a_banner_block_alone_is_replay() {
        assert!(is_terminal_replay(BANNER_ONLY));
    }

    #[test]
    fn a_screen_that_holds_the_reply_is_not_replay() {
        assert!(!is_terminal_replay(SCREEN_WITH_ANSWER));
    }

    #[test]
    fn one_prompt_line_among_real_content_is_not_replay() {
        let text = "\u{276f} why did we drop Docker and Qdrant for a single Rust binary?\n\nInstalling it was too painful, and the scale never needed a database server. A Phase 0 spike then showed an embedded index was faster anyway.\n\nInstall friction: v7 needed Docker Desktop, a Qdrant container, a Python watcher and a venv.";
        assert!(!is_terminal_replay(text));
    }

    #[test]
    fn one_status_line_among_real_content_is_not_replay() {
        let text = "The fix landed in the stop hook.\n\u{2736} Twisting\u{2026} (running UserPromptSubmit hooks\u{2026} 5/6 \u{00b7} 0s)\nIt now exits 0 on a lock timeout and logs the skip, so Claude Code never blocks on it.\nThe hooks integration suite covers the lock path.";
        assert!(!is_terminal_replay(text));
    }

    #[test]
    fn a_few_shell_prompts_with_output_are_not_replay() {
        let text = "\u{276f} git status\nOn branch main\nnothing to commit, working tree clean\n\u{276f} git log -1\ncommit abc123\n\u{276f} cargo test\nrunning 4 tests\ntest result: ok. 4 passed";
        assert!(!is_terminal_replay(text));
    }

    /// Finding 1: block glyphs alone are not a banner.
    #[test]
    fn progress_bars_are_not_a_banner() {
        assert!(!is_terminal_replay(
            "download \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588} 100%\nchecksum \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588} verified"
        ));
    }

    /// Finding 2: a real result among the status rows keeps the text in play.
    #[test]
    fn a_test_result_among_spinner_rows_is_not_replay() {
        let text =
            "* Testing\u{2026}\n".repeat(6) + "test result: FAILED. import_roundtrip panicked";
        assert!(!is_terminal_replay(&text));
        let spinners_only = "* Testing\u{2026}\n".repeat(6);
        assert!(is_terminal_replay(&spinners_only));
    }

    #[test]
    fn plain_prose_and_empty_text_are_not_replay() {
        assert!(!is_terminal_replay(""));
        assert!(!is_terminal_replay("\u{276f}"));
        assert!(!is_terminal_replay("why did we drop Docker and Qdrant?"));
    }
}
