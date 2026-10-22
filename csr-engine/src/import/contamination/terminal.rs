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
//! It fires only when frames dominate: at least [`MIN_FRAMES`] frame segments
//! and no more than a fifth of the segments being anything else. A chunk that
//! holds one prompt line or one status line among real content is not replay.

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

/// A line of the Claude Code start banner: the logo glyphs, the version, the
/// model and plan line, or the working-directory line.
static BANNER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:[\u{2590}\u{259b}\u{259c}\u{259d}\u{2598}\u{2588}\u{2580}\u{258c}]{3,}|Claude Code v\d|\bwith \w+ effort \u{00b7} Claude\b|~/\S*\s+\u{00b7}\s+/\w+$)",
    )
    .expect("banner regex")
});

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

/// Whether `text` is dominated by terminal-capture frames (keystroke replay,
/// spinner and status rows, the start banner) and so carries no answer.
pub fn is_terminal_replay(text: &str) -> bool {
    let segments = segments(text);
    if segments.len() < 2 {
        return false;
    }
    let frames = segments
        .iter()
        .filter(|segment| **segment == Segment::Frame)
        .count();
    let banners = segments
        .iter()
        .filter(|segment| **segment == Segment::Banner)
        .count();
    let others = segments.len() - frames - banners;
    let chrome = frames + banners;
    let dominated = others * 5 <= segments.len();
    (chrome >= MIN_FRAMES && dominated) || (banners >= 2 && others == 0)
}

/// Classify every `" | "`-joined segment of every line.
fn segments(text: &str) -> Vec<Segment> {
    let mut parsed: Vec<(bool, String)> = Vec::new();
    for line in text.lines() {
        for piece in line.split(" | ") {
            let piece = piece.trim();
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
            if rest.is_empty()
                || rest == "\u{276f}"
                || SPINNER.is_match(rest)
                || CHROME.is_match(rest)
            {
                Segment::Frame
            } else if BANNER.is_match(rest) {
                Segment::Banner
            } else if prompt_text[at].is_some() && (*indexed || grows(&prompt_text, at)) {
                Segment::Frame
            } else {
                Segment::Other
            }
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

    #[test]
    fn plain_prose_and_empty_text_are_not_replay() {
        assert!(!is_terminal_replay(""));
        assert!(!is_terminal_replay("\u{276f}"));
        assert!(!is_terminal_replay("why did we drop Docker and Qdrant?"));
    }
}
