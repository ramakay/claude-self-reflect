//! Lost and found retires a thread the session finished.
//!
//! A thread goes quiet when a compaction summary stops listing it. That happens to finished work
//! and to forgotten work alike, and word overlap cannot tell them apart: over 87 real quiet threads
//! it hid live work ("prod OTA" on "A prod OTA is NOT approved"). So the dreamer's fork, which has
//! the summary in its context, names the threads that finished and quotes the words that show it,
//! and this code checks the quote. A quote that is not in a summary newer than the thread is
//! dropped, whatever the model says. A thread waiting on the person is never asked about.
//!
//! A retired thread moves to "Guessed done" with its quote; nothing is deleted. It comes back when
//! a later summary lists it again (`lost::lost_found`).
use super::{
    lost::{self, Thread},
    paths::Paths,
    session::Session,
    threads,
};
use serde_json::{json, Value};

/// How many threads a look asks about, and how many answers it takes.
pub const MOST: usize = 12;
/// How much of a thread's words the fork reads.
const TEXT_MOST: usize = 300;
/// A shorter quote can be found anywhere; a longer one is a pasted section.
const QUOTE_LEAST: usize = 12;
const QUOTE_MOST: usize = 200;

/// A thread a look may ask about: gone quiet here or carried in from the session before, asking
/// the person nothing, not set aside.
fn askable(t: &Thread) -> bool {
    matches!(t.state.as_str(), "lost" | "carried") && !t.waiting && t.hidden.is_none()
}

/// (key, words) of the threads to ask the fork about; none means the prompt does not change.
pub fn candidates(paths: &Paths, sid: &str) -> Vec<(String, String)> {
    if sid.is_empty() {
        return vec![];
    }
    lost::lost_json(paths, sid)
        .into_iter()
        .filter(askable)
        .take(MOST)
        .map(|t| (t.key, t.text.chars().take(TEXT_MOST).collect()))
        .collect()
}

/// The reply's `threads_done:` line: well-formed answers only.
pub fn parse(reply: &str) -> Value {
    let raw = reply
        .lines()
        .find_map(|line| line.trim().strip_prefix("threads_done:"));
    let items: Vec<Value> = raw
        .and_then(|raw| serde_json::from_str::<Value>(raw.trim()).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|item| {
            let key = item["key"].as_str()?;
            let quote = item["quote"].as_str()?.trim();
            let size = quote.chars().count();
            ((QUOTE_LEAST..=QUOTE_MOST).contains(&size) && !key.is_empty())
                .then(|| json!({"key": key, "quote": quote}))
        })
        .take(MOST)
        .collect();
    Value::Array(items)
}

/// Words alone: a model drops a summary's bold and backticks when it copies them.
fn plain(text: &str) -> String {
    text.replace(['*', '`'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Why an answer did not retire its thread.
pub const NOT_ASKABLE: &str = "not askable";
pub const QUOTE_NOT_NEWER: &str = "quote not in a newer summary";
/// How many failed answers a look's record keeps, and how much of each quote.
const REJECTED_KEPT: usize = 3;
const REJECTED_QUOTE_CHARS: usize = 120;

/// Each well-formed answer judged: those whose thread may retire and whose quote is in a summary
/// newer than the thread, (the session the thread went quiet in, key, the quote); and the rest,
/// as {key, quote, why}, in the reply's order.
pub fn judged(
    sid: &str,
    threads: &[Thread],
    summaries: &[(f64, String)],
    answers: &Value,
) -> (Vec<(String, String, String)>, Vec<Value>) {
    let summaries: Vec<(f64, String)> = summaries
        .iter()
        .map(|(at, text)| (*at, plain(text)))
        .collect();
    let empty = vec![];
    let (mut done, mut rejected) = (vec![], vec![]);
    for item in answers.as_array().unwrap_or(&empty) {
        let (Some(key), Some(quote)) = (item["key"].as_str(), item["quote"].as_str()) else {
            continue;
        };
        let refuse = |why: &str| json!({"key": key, "quote": quote, "why": why});
        let Some(thread) = threads.iter().find(|t| t.key == key && askable(t)) else {
            rejected.push(refuse(NOT_ASKABLE));
            continue;
        };
        let words = plain(quote);
        if summaries
            .iter()
            .any(|(at, text)| *at > thread.last_seen && text.contains(&words))
        {
            let origin = thread.origin_sid.as_deref().unwrap_or(sid);
            done.push((origin.to_owned(), key.to_owned(), quote.to_owned()));
        } else {
            rejected.push(refuse(QUOTE_NOT_NEWER));
        }
    }
    (done, rejected)
}

/// The answers whose thread may retire and whose quote is in a summary newer than the thread:
/// (the session the thread went quiet in, key, the quote).
pub fn checked(
    sid: &str,
    threads: &[Thread],
    summaries: &[(f64, String)],
    answers: &Value,
) -> Vec<(String, String, String)> {
    judged(sid, threads, summaries, answers).0
}

/// What a look's answers came to.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Retired {
    /// How many threads were written down as done.
    pub retired: usize,
    /// Up to three answers that were not, as {key, quote (first 120 chars), why}.
    pub rejected: Vec<Value>,
}

/// Write down the threads the fork showed finished, and say what became of every answer.
pub fn retire(paths: &Paths, sid: &str, answers: &Value, now: f64) -> Retired {
    if sid.is_empty() || answers.as_array().is_none_or(Vec::is_empty) {
        return Retired::default();
    }
    let mut session = Session::new(sid);
    session.read_transcript(paths);
    let (done, rejected) = judged(
        sid,
        &lost::lost_json(paths, sid),
        &lost::pairs(&session.data["summaries"]),
        answers,
    );
    for (origin, key, quote) in &done {
        let more = json!({"quote": quote, "sid": origin, "seen_in": sid});
        threads::keep_lost(paths, key, "done", now, None, more);
    }
    Retired {
        retired: done.len(),
        rejected: kept_rejected(rejected),
    }
}

/// The first few failed answers, each with the start of its quote.
fn kept_rejected(rejected: Vec<Value>) -> Vec<Value> {
    rejected
        .into_iter()
        .take(REJECTED_KEPT)
        .map(|mut item| {
            let quote: String = item["quote"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(REJECTED_QUOTE_CHARS)
                .collect();
            item["quote"] = json!(quote);
            item
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(key: &str, state: &str, waiting: bool, last_seen: f64) -> Thread {
        Thread {
            key: key.into(),
            state: state.into(),
            waiting,
            last_seen,
            text: format!("thread {key}"),
            ..Thread::default()
        }
    }

    #[test]
    fn a_quote_must_be_in_a_summary_newer_than_the_thread() {
        let threads = [
            thread("a", "lost", false, 100.),
            thread("b", "lost", false, 300.),
        ];
        let summaries = [(
            200.,
            "Problem Solving: **Phone bench** 0.372 s/MP, `passed`.".to_owned(),
        )];
        let answers = json!([
            {"key": "a", "quote": "Phone bench 0.372 s/MP, passed."},
            {"key": "b", "quote": "Phone bench 0.372 s/MP, passed."},
            {"key": "a", "quote": "words that the summary never said"},
        ]);
        let done = checked("s", &threads, &summaries, &answers);
        assert_eq!(
            done.len(),
            1,
            "b is newer than the summary; the third quote is made up"
        );
        assert_eq!(done[0].1, "a");
    }

    #[test]
    fn a_thread_waiting_on_the_person_or_still_open_never_retires() {
        let threads = [
            thread("w", "lost", true, 100.),
            thread("o", "open", false, 100.),
            thread("h", "handed", false, 100.),
            thread("u", "lost", false, 100.),
        ];
        let summaries = [(
            200.,
            "Everything here is finished and installed.".to_owned(),
        )];
        let quote = "Everything here is finished";
        let answers = json!([
            {"key": "w", "quote": quote}, {"key": "o", "quote": quote},
            {"key": "h", "quote": quote}, {"key": "gone", "quote": quote},
        ]);
        assert!(checked("s", &threads, &summaries, &answers).is_empty());
        let answers = json!([{"key": "u", "quote": quote}]);
        assert_eq!(checked("s", &threads, &summaries, &answers).len(), 1);
        // A thread carried in is written under the session it went quiet in.
        let mut carried = thread("c", "carried", false, 100.);
        carried.origin_sid = Some("before".into());
        let answers = json!([{"key": "c", "quote": quote}]);
        let done = checked("s", &[carried], &summaries, &answers);
        assert_eq!(done[0].0, "before");
    }

    #[test]
    fn every_answer_is_judged_and_a_failed_one_says_why() {
        let threads = [
            thread("a", "lost", false, 100.),
            thread("b", "lost", false, 100.),
            thread("w", "lost", true, 100.),
        ];
        let summaries = [(200., "Phone bench 0.372 s/MP, passed.".to_owned())];
        let long = "x".repeat(300);
        let answers = json!([
            {"key": "a", "quote": "Phone bench 0.372 s/MP, passed."},
            {"key": "b", "quote": long},
            {"key": "w", "quote": "Phone bench 0.372 s/MP, passed."},
            {"key": "gone", "quote": "Phone bench 0.372 s/MP, passed."},
        ]);
        let (done, rejected) = judged("s", &threads, &summaries, &answers);
        assert_eq!(done.len(), 1);
        assert_eq!(done, checked("s", &threads, &summaries, &answers));
        let why: Vec<(&str, &str)> = rejected
            .iter()
            .map(|r| (r["key"].as_str().unwrap(), r["why"].as_str().unwrap()))
            .collect();
        assert_eq!(
            why,
            [
                ("b", QUOTE_NOT_NEWER),
                ("w", NOT_ASKABLE),
                ("gone", NOT_ASKABLE)
            ]
        );
    }

    #[test]
    fn a_look_keeps_three_failed_answers_and_the_start_of_each_quote() {
        let five: Vec<Value> = (0..5)
            .map(|i| json!({"key": format!("k{i}"), "quote": "é".repeat(300), "why": NOT_ASKABLE}))
            .collect();
        let kept = kept_rejected(five);
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[2]["key"], "k2");
        assert_eq!(kept[0]["quote"].as_str().unwrap().chars().count(), 120);
        assert_eq!(kept[0]["why"], NOT_ASKABLE);
    }

    #[test]
    fn the_reply_line_keeps_well_formed_answers_and_a_broken_one_is_nothing() {
        let reply = "dream: none\nthreads_done: [{\"key\":\"a\",\"quote\":\"Committed as 0fb9972.\"},\
                     {\"key\":\"b\",\"quote\":\"short\"},{\"quote\":\"no key here at all\"}]\nsame_page: {}";
        let parsed = parse(reply);
        assert_eq!(parsed.as_array().unwrap().len(), 1);
        assert_eq!(parsed[0]["key"], "a");
        assert_eq!(parse("threads_done: [{broken"), json!([]));
        assert_eq!(parse("dream: none"), json!([]));
    }
}
