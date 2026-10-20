//! The main page's "How Claude read you" section: where Claude read a short word of yours one
//! way, so you can check it. pane.py: own_readings / how_claude_read (the reader), reading_items.
use super::{
    coach::Reading,
    flight::{counted, py_float},
    paths::Paths,
    render::text::one_line,
    session::{self, Session},
};
use chrono::{Local, TimeZone};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Seconds a session's lineage is kept before it is looked up again.
const LINEAGE_KEPT: f64 = 10.;

/// The readers of the session and the sessions it continues, each remembering where its last
/// read stopped, so a frame reads only what was written since (the Python pane's `_READINGS`).
#[derive(Default)]
pub struct Readers {
    sessions: BTreeMap<String, Session>,
    lineage: Option<(String, f64, Vec<String>)>,
}
impl Readers {
    /// This session's newest readings; while it has none, the nearest session it continues that
    /// has some.
    pub fn how_claude_read(
        &mut self,
        paths: &Paths,
        sid: &str,
        most: usize,
        now: f64,
    ) -> Vec<Reading> {
        if sid.is_empty() {
            return vec![];
        }
        let fresh = self
            .lineage
            .as_ref()
            .is_some_and(|(kept, at, _)| kept == sid && (0. ..LINEAGE_KEPT).contains(&(now - at)));
        if !fresh {
            self.lineage = Some((sid.to_owned(), now, session::lineage(paths, sid)));
        }
        let ids = self
            .lineage
            .as_ref()
            .map(|l| l.2.clone())
            .unwrap_or_default();
        for id in ids {
            let reader = self
                .sessions
                .entry(id.clone())
                .or_insert_with(|| Session::with_readings(&id));
            reader.read_transcript(paths);
            if !reader.readings.is_empty() {
                return reader.readings.iter().rev().take(most).cloned().collect();
            }
        }
        vec![]
    }
}
fn short_date(at: f64) -> String {
    Local
        .timestamp_opt(at as i64, 0)
        .single()
        .map(|t| t.format("%m-%d").to_string())
        .unwrap_or_default()
}
/// The "How Claude read you" rows: “dknr” → read as “done” where it parsed, else the marked line
/// trimmed. A click opens the whole line; a second puts the check in Claude's prompt.
fn reading_items(readings: &[Reading]) -> Vec<Value> {
    readings
        .iter()
        .map(|r| {
            let (title, send) = if !r.words.is_empty() && !r.read.is_empty() {
                (
                    format!("“{}” → read as “{}”", r.words, r.read),
                    format!("Earlier you read my “{}” as “{}”.", r.words, r.read),
                )
            } else if !r.words.is_empty() {
                (
                    format!("“{}” → read as unclear", r.words),
                    format!("Earlier you read my “{}” as unclear.", r.words),
                )
            } else {
                (
                    r.line.clone(),
                    format!(
                        "Earlier you read me this way: “{}”.",
                        r.line.trim_end_matches('.')
                    ),
                )
            };
            let where_ = if r.source == "compaction" {
                "in a compaction summary"
            } else {
                "in a reply"
            };
            let named = if r.words.is_empty() {
                &r.line
            } else {
                &r.words
            };
            let meta = [
                if r.when != 0. {
                    short_date(r.when)
                } else {
                    String::new()
                },
                where_.to_owned(),
            ]
            .into_iter()
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
            json!({
                "key": format!(
                    "read:{}:{}",
                    py_float(r.when),
                    named.chars().take(24).collect::<String>()
                ),
                "title": title,
                "lines": 2,
                "meta": meta,
                "detail": [r.line],
                "send": one_line(&format!("{send} Check that reading before relying on it."), 700),
            })
        })
        .collect()
}
/// The section: None-valued `at` while no reading has a time.
pub fn coach_section(readings: &[Reading]) -> Value {
    let at = readings
        .iter()
        .map(|r| r.when)
        .filter(|w| *w != 0.)
        .fold(None, |m: Option<f64>, w| Some(m.map_or(w, |m| m.max(w))));
    json!({
        "name": "coach",
        "title": "How Claude read you",
        "right": "",
        "items": reading_items(readings),
        "empty": "",
        "at": at,
        "brief": counted(readings.len() as u64, "reading"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(words: &str, read: &str, line: &str, source: &str) -> Reading {
        Reading {
            words: words.into(),
            read: read.into(),
            line: line.into(),
            when: 1_792_319_865.,
            source: source.into(),
        }
    }
    #[test]
    fn test_a_parsed_reading_names_both_words_and_a_bare_line_is_its_own_title() {
        let rows = reading_items(&[
            reading(
                "dknr",
                "done",
                "Your word “dknr” — interpreted as: done.",
                "compaction",
            ),
            reading("zz", "", "“zz” (unclear)", "reply"),
            reading("", "", "reading this as a request to stop.", "reply"),
        ]);
        assert_eq!(rows[0]["title"], "“dknr” → read as “done”");
        assert_eq!(rows[0]["key"], "read:1792319865.0:dknr");
        assert_eq!(rows[0]["meta"], "10-18 · in a compaction summary");
        assert_eq!(
            rows[0]["send"],
            "Earlier you read my “dknr” as “done”. Check that reading before relying on it."
        );
        assert_eq!(rows[1]["title"], "“zz” → read as unclear");
        assert_eq!(rows[1]["meta"], "10-18 · in a reply");
        assert_eq!(rows[2]["title"], "reading this as a request to stop.");
        assert_eq!(
            rows[2]["send"],
            "Earlier you read me this way: “reading this as a request to stop”. \
             Check that reading before relying on it."
        );
    }
    #[test]
    fn test_no_readings_is_an_empty_section_with_no_time() {
        let section = coach_section(&[]);
        assert!(section["items"].as_array().unwrap().is_empty());
        assert!(section["at"].is_null());
        assert_eq!(section["brief"], "");
    }
}
