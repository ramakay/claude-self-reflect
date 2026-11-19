//! The text pane's dashboard as cards: what a terminal that draws no pictures shows (macOS
//! Terminal.app, also under herdr, or `CSR_COMPANION_PIXELS=0`). The rows are the instrument's text
//! rows. Laid out from the top, each card's title is bold and a blank row parts it from the card
//! above while there is room; a full pane loses no row to the parting. A popup or a page keeps its
//! place right above the keys.
use super::instrument::{CONFUSED_TITLE, LOST_TITLE, SHOULD_BE_TITLE};
use std::collections::BTreeMap;

const BOLD_ON: &str = "\x1b[1m";
/// Normal intensity, not a full reset: the row keeps its ground and ink.
const BOLD_OFF: &str = "\x1b[22m";

/// Card titles that fill their rows. A narrow pane wraps one over several rows.
fn titles() -> [&'static str; 6] {
    [
        LOST_TITLE,
        CONFUSED_TITLE,
        SHOULD_BE_TITLE,
        crate::companion::same_page::TITLE,
        "Earlier here",
        "Try a question",
    ]
}
/// Rows that open a card by their first words; the words after them are the card's own.
const LEADS: [&str; 2] = ["Last time here · ", "Story · "];
/// The first row of the figure's facts, the dashboard's last card. It has no title.
const FIGURE: [&str; 3] = ["CTX ", "7D ", "CSR cost:"];

/// The rows a title that starts at `at` spans, or None when no title starts there.
fn title_rows(rows: &[String], at: usize) -> Option<usize> {
    let first = rows[at].trim();
    if first.is_empty() {
        return None;
    }
    if LEADS.iter().any(|lead| rows[at].starts_with(lead)) {
        return Some(1);
    }
    titles().into_iter().find_map(|title| {
        let mut joined = String::new();
        for (n, row) in rows[at..].iter().enumerate() {
            if !joined.is_empty() {
                joined.push(' ');
            }
            joined.push_str(row.trim());
            if joined == title {
                return Some(n + 1);
            }
            if !title.starts_with(&joined) {
                return None;
            }
        }
        None
    })
}

/// Lay `out` out in the `room` rows above the keys. The dashboard reads as cards from the top
/// row; anything else (a popup, a page) sits right above the keys as it always has. Returns the
/// row each old row moved to.
pub fn place(out: &mut Vec<String>, room: usize, dashboard: bool) -> Vec<usize> {
    let mut spare = room.saturating_sub(out.len());
    if !dashboard {
        let moved = (spare..spare + out.len()).collect();
        out.splice(0..0, std::iter::repeat_n(String::new(), spare));
        return moved;
    }
    let mut laid = Vec::with_capacity(room);
    let mut moved = Vec::with_capacity(out.len());
    let mut bold = 0;
    let mut figure = false;
    for (at, row) in out.iter().enumerate() {
        let title = title_rows(out, at).filter(|_| bold == 0);
        let facts = !figure && FIGURE.iter().any(|lead| row.starts_with(lead));
        figure |= facts;
        if (title.is_some() || facts) && at > 0 && spare > 0 {
            laid.push(String::new());
            spare -= 1;
        }
        bold = bold.max(title.unwrap_or(0));
        moved.push(laid.len());
        if bold > 0 {
            laid.push(format!("{BOLD_ON}{row}{BOLD_OFF}"));
            bold -= 1;
        } else {
            laid.push(row.clone());
        }
    }
    *out = laid;
    moved
}

/// Rows keyed as they were drawn before `place`, keyed where they are now. A row past the laid
/// out ones (none should be) moves as the last one did.
pub fn remap<T>(rows: BTreeMap<usize, T>, moved: &[usize]) -> BTreeMap<usize, T> {
    let past = moved.last().map_or(0, |last| last + 1 - moved.len());
    rows.into_iter()
        .map(|(row, value)| (moved.get(row).copied().unwrap_or(row + past), value))
        .collect()
}

/// The columns a row takes on screen: its characters, not counting the escapes that colour it.
pub fn width(line: &str) -> usize {
    let mut count = 0;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &[&str]) -> Vec<String> {
        text.iter().map(|s| s.to_string()).collect()
    }
    fn plain(line: &str) -> String {
        line.replace(BOLD_ON, "").replace(BOLD_OFF, "")
    }

    #[test]
    fn the_dashboard_starts_at_the_top_with_a_blank_row_before_each_card_and_bold_titles() {
        let mut out = rows(&[
            "☾ The dreamer looks as you work.",
            "No open what-if yet.",
            "Last time here · 17m ago",
            "you asked: /exit.",
            "Earlier here",
            "21m ago · what did we work on lately?",
            "Try a question",
            "What did we decide about this repo?",
            "7D 86%» ↻Sun 06:00",
            "CTX 52k/1M",
            "CSR 5.5 pts",
        ]);
        let moved = place(&mut out, 29, true);
        assert_eq!(
            out.len(),
            15,
            "four cards after the first, a blank row each"
        );
        assert_eq!(out[0], "☾ The dreamer looks as you work.");
        assert_eq!(out[2], "");
        assert_eq!(
            out[3],
            format!("{BOLD_ON}Last time here · 17m ago{BOLD_OFF}")
        );
        assert_eq!(out[4], "you asked: /exit.");
        assert_eq!(out[6], format!("{BOLD_ON}Earlier here{BOLD_OFF}"));
        assert_eq!(out[9], format!("{BOLD_ON}Try a question{BOLD_OFF}"));
        // The figure opens one card, not one per fact, and has no title to bold.
        assert_eq!(
            &out[11..],
            ["", "7D 86%» ↻Sun 06:00", "CTX 52k/1M", "CSR 5.5 pts"]
        );
        assert_eq!(moved, [0, 1, 3, 4, 6, 7, 9, 10, 12, 13, 14]);
    }

    #[test]
    fn a_title_wrapped_by_a_narrow_pane_is_bold_on_every_row_it_spans() {
        let mut out = rows(&[
            "☾ The dreamer looks as you",
            "work.",
            "Should be a skill / memory /",
            "tool",
            "Write the release checklist",
        ]);
        place(&mut out, 29, true);
        assert_eq!(
            out[3..6],
            [
                format!("{BOLD_ON}Should be a skill / memory /{BOLD_OFF}"),
                format!("{BOLD_ON}tool{BOLD_OFF}"),
                "Write the release checklist".to_string(),
            ]
        );
        // Words that only begin like a title are no title.
        let mut out = rows(&[
            "☾ q",
            "Should be a skill / memory / tool",
            "Try a questionnaire",
        ]);
        place(&mut out, 9, true);
        assert_eq!(out.iter().filter(|r| r.contains(BOLD_ON)).count(), 1);
    }

    #[test]
    fn a_full_pane_keeps_every_row_and_parts_cards_only_while_there_is_room() {
        let full = rows(&["☾ q", "Earlier here", "a", "Try a question", "b"]);
        let mut out = full.clone();
        let moved = place(&mut out, 5, true);
        assert_eq!(out.iter().map(|r| plain(r)).collect::<Vec<_>>(), full);
        assert_eq!(moved, [0, 1, 2, 3, 4]);
        let mut out = full.clone();
        place(&mut out, 6, true);
        assert_eq!(out.len(), 6);
        assert_eq!(out[1], "", "the first card down gets the one spare row");
        assert_eq!(plain(&out[4]), "Try a question");
    }

    #[test]
    fn a_popup_or_page_still_sits_right_above_the_keys() {
        let mut out = rows(&["Lost and found", "a thread"]);
        let moved = place(&mut out, 5, false);
        assert_eq!(out, ["", "", "", "Lost and found", "a thread"]);
        assert_eq!(moved, [3, 4]);
        let acts: BTreeMap<usize, &str> = [(0, "title"), (1, "thread"), (2, "past")].into();
        let acts = remap(acts, &moved);
        assert_eq!(acts, [(3, "title"), (4, "thread"), (5, "past")].into());
    }

    #[test]
    fn width_counts_what_shows_not_the_escapes() {
        assert_eq!(width("Try a question"), 14);
        assert_eq!(width(&format!("{BOLD_ON}Try a question{BOLD_OFF}")), 14);
        assert_eq!(width("\x1b[38;2;1;2;3m7D\x1b[0m 86%"), 6);
        assert_eq!(width("☾ é"), 3);
    }
}
