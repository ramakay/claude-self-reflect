//! What `/csr dreams` says about the dreamer's pacing: `dreamsText` (register.tsx 848-852) over
//! `paceText` (pace.ts), read from the session's `pace` and `chapter` rows.
//!
//! A small copy, here because the command needs only the reading and the words: the pace and
//! dreamer packages own the numbers, their writes, and `newChapter`. A chapter that is missing or
//! older than two hours is read as a fresh one and not stored, which says what `newChapter` would.

use crate::companion::relay::dispatch::Ctx;
use serde_json::Value;

const FIRST_EVERY: f64 = 6.;
const PER_CHAPTER: f64 = 5.;
const CHAPTER_MS: f64 = 2. * 60. * 60. * 1000.;
const SHARE: f64 = 0.1;

struct Pace {
    claude: f64,
    dreamer: f64,
    looks: f64,
    dreams: f64,
}

struct Chapter {
    steps: f64,
    due: f64,
    shown: f64,
    adopted: f64,
}

fn number(row: &Value, key: &str, otherwise: f64) -> f64 {
    row[key].as_f64().unwrap_or(otherwise)
}

/// The session's pace row; a fresh pace when there is none or it is another session's.
fn pace_of(cx: &Ctx) -> Pace {
    let row = cx.store.get(&cx.sid, "pace").ok().flatten();
    let row = row.filter(|row| row["session"] == cx.sid.as_str());
    let row = row.unwrap_or(Value::Null);
    Pace {
        claude: number(&row, "claude", 0.),
        dreamer: number(&row, "dreamer", 0.),
        looks: number(&row, "looks", 0.),
        dreams: number(&row, "dreams", 0.),
    }
}

fn chapter_of(cx: &Ctx) -> Chapter {
    let row = cx.store.get(&cx.sid, "chapter").ok().flatten();
    let now = cx.clock.now();
    let row = row
        .filter(|row| row["session"] == cx.sid.as_str())
        .filter(|row| now - number(row, "started", f64::NEG_INFINITY) < CHAPTER_MS)
        .unwrap_or(Value::Null);
    Chapter {
        steps: number(&row, "steps", 0.),
        due: number(&row, "due", FIRST_EVERY),
        shown: number(&row, "shown", 0.),
        adopted: number(&row, "adopted", 0.),
    }
}

fn allowance(chapter: &Chapter) -> f64 {
    PER_CHAPTER + chapter.adopted
}

fn dreams_left(chapter: &Chapter) -> f64 {
    (allowance(chapter) - chapter.shown).max(0.)
}

/// Tokens the dreamer has spent past its share: 0 while it is within it.
fn owed(pace: &Pace) -> f64 {
    (pace.dreamer - SHARE * pace.claude).max(0.)
}

fn plural(n: f64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1. { one } else { many })
}

pub fn pace_text(pace: Option<&PaceView>, chapter: Option<&ChapterView>, on: bool) -> String {
    if !on {
        return "Dreams are off. Turn them on in /config: csr-companion, Dreams.".into();
    }
    let now = match chapter {
        Some(ChapterView(c)) if dreams_left(c) > 0. => format!(
            "{} of {} dreams left in this chapter; the next look in about {} of Claude's steps, \
            if CSR has something new.",
            dreams_left(c),
            allowance(c),
            (c.due - c.steps).max(0.)
        ),
        Some(_) => "No dreams left in this chapter; it starts over at the next compaction.".into(),
        None => format!("{PER_CHAPTER} dreams to a chapter, which starts over at each compaction."),
    };
    let Some(PaceView(pace)) = pace.filter(|p| p.0.looks != 0.) else {
        return format!(
            "Dreams are on. {now} The dreamer looks only when CSR has something new about the \
            work in hand, and checks before each look that its spend stays within 1 token for \
            every 10 Claude spends. No looks yet this session."
        );
    };
    let k = crate::companion::app::state::k;
    let share = if pace.claude != 0. {
        (pace.dreamer / pace.claude * 100.).round()
    } else {
        0.
    };
    let over = owed(pace);
    let debt = if over > 0. {
        format!(
            " A look ran over its share by {}; the next waits until Claude's spending has \
            caught up.",
            k(over)
        )
    } else {
        String::new()
    };
    format!(
        "Dreams are on. {now} This session: {}, {}, {} tokens ({share}% of Claude's {}).{debt}",
        plural(pace.looks, "look", "looks"),
        plural(pace.dreams, "dream", "dreams"),
        k(pace.dreamer),
        k(pace.claude)
    )
}

pub struct PaceView(Pace);
pub struct ChapterView(Chapter);

/// `dreamsText`: the pace in words, or that dreams are off.
pub fn dreams_text(cx: &Ctx, on: bool) -> String {
    if !on {
        return pace_text(None, None, false);
    }
    let (pace, chapter) = (PaceView(pace_of(cx)), ChapterView(chapter_of(cx)));
    pace_text(Some(&pace), Some(&chapter), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pace(claude: f64, dreamer: f64, looks: f64, dreams: f64) -> PaceView {
        PaceView(Pace {
            claude,
            dreamer,
            looks,
            dreams,
        })
    }

    fn chapter(steps: f64, due: f64, shown: f64, adopted: f64) -> ChapterView {
        ChapterView(Chapter {
            steps,
            due,
            shown,
            adopted,
        })
    }

    #[test]
    fn off_says_where_to_turn_dreams_on() {
        assert_eq!(
            pace_text(None, None, false),
            "Dreams are off. Turn them on in /config: csr-companion, Dreams."
        );
    }

    #[test]
    fn on_with_no_looks_says_the_chapter_and_that_none_ran() {
        let said = pace_text(
            Some(&pace(0., 0., 0., 0.)),
            Some(&chapter(0., 6., 0., 0.)),
            true,
        );
        assert!(said.starts_with(
            "Dreams are on. 5 of 5 dreams left in this chapter; the next look in about 6 of \
            Claude's steps, if CSR has something new."
        ));
        assert!(said.ends_with("No looks yet this session."));
        let none = pace_text(None, None, true);
        assert!(none.contains("5 dreams to a chapter, which starts over at each compaction."));
    }

    #[test]
    fn on_after_looks_counts_them_and_names_a_debt() {
        let said = pace_text(
            Some(&pace(100_000., 30_000., 1., 2.)),
            Some(&chapter(10., 20., 5., 0.)),
            true,
        );
        assert!(
            said.contains("No dreams left in this chapter; it starts over at the next compaction.")
        );
        assert!(said.contains("This session: 1 look, 2 dreams, 30k tokens (30% of Claude's"));
        assert!(said.contains("100k)."));
        assert!(said.contains("A look ran over its share by 20k;"));
    }
}
