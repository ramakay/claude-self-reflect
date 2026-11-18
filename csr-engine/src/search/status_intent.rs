//! Status-intent queries ("current state of X", "where does X stand", "what is
//! left") ask for the newest report on a subject. This module holds the narrow
//! phrase whitelist that recognises them, the subject terms they carry, and the
//! pure near-tie reorder that lets the newest matching report win a tie. It
//! abstains whenever it is unsure: a query that is not on the whitelist, a
//! candidate without a score, a timestamp or a subject match, or a gap larger
//! than the near-tie epsilon all leave the order exactly as it was.

use std::collections::HashMap;

/// Phrases that mark a status question. Matched on the lower-cased query with
/// single spaces. Deliberately short: an unlisted phrasing is not a status
/// query.
const STATUS_PHRASES: &[&str] = &[
    "current state of",
    "what is left",
    "what's left",
    "what is still left",
    "what's still left",
    "what is remaining",
    "what remains",
    "where do we stand",
    "where are we on",
    "where are we with",
];

/// Words that carry the question, not the subject.
const QUESTION_WORDS: &[&str] = &[
    "a",
    "an",
    "and",
    "are",
    "current",
    "do",
    "does",
    "for",
    "in",
    "is",
    "it",
    "left",
    "of",
    "on",
    "or",
    "remaining",
    "remains",
    "state",
    "stand",
    "still",
    "status",
    "the",
    "to",
    "we",
    "what",
    "whats",
    "where",
    "with",
];

fn normalized(query: &str) -> String {
    query
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// True when `q` is a status question: a whitelisted phrase, "state of X" at the
/// start of the query, or "where does X stand". Anything else is not.
pub fn is_status_query(q: &str) -> bool {
    let q = normalized(q);
    if q.is_empty() {
        return false;
    }
    if STATUS_PHRASES.iter().any(|phrase| q.contains(phrase)) {
        return true;
    }
    if q.starts_with("state of ") && !q.starts_with("state of the art") {
        return true;
    }
    if let Some(at) = q.find("where does ") {
        return q[at..].contains(" stand");
    }
    false
}

/// The subject terms of a status query, as the stems a chunk must contain: the
/// query's words without question words, at least three letters, cut to six
/// characters so "hardening" matches "harden". Empty when fewer than two
/// remain, which makes the caller abstain.
pub fn subject_stems(q: &str) -> Vec<String> {
    let mut stems: Vec<String> = Vec::new();
    for word in normalized(q).split(|c: char| !c.is_alphanumeric()) {
        if word.chars().count() < 3 || QUESTION_WORDS.contains(&word) {
            continue;
        }
        let stem: String = word.chars().take(6).collect();
        if !stems.contains(&stem) {
            stems.push(stem);
        }
    }
    if stems.len() < 2 {
        return Vec::new();
    }
    stems
}

/// True when `content` carries at least half of the subject stems (and at
/// least two). An empty stem list never matches.
pub fn matches_subject(content: &str, stems: &[String]) -> bool {
    if stems.is_empty() {
        return false;
    }
    let lower = content.to_lowercase();
    let hits = stems.iter().filter(|s| lower.contains(s.as_str())).count();
    hits >= 2 && hits * 2 >= stems.len()
}

/// One candidate of the top page, as the reorder reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusCandidate {
    /// Final ranker score (cross-encoder logit); `None` abstains.
    pub score: Option<f32>,
    /// Session time in epoch seconds; `None` abstains.
    pub when: Option<i64>,
    /// Matches the query subject and is a report (assistant or user content,
    /// not tool output or boilerplate).
    pub eligible: bool,
}

/// Reorder `order` among its first `limit` ids: the eligible candidates whose
/// score is within `epsilon` of the best eligible score trade the slots they
/// already hold, newest session first. Every other id keeps its place. A group
/// of fewer than two, or no change in who is newest, returns `order` as it came.
pub fn reorder_near_ties(
    order: &[String],
    candidates: &HashMap<String, StatusCandidate>,
    limit: usize,
    epsilon: f32,
) -> Vec<String> {
    let top = limit.min(order.len());
    let usable = |id: &String| {
        candidates
            .get(id)
            .and_then(|c| match (c.eligible, c.score, c.when) {
                (true, Some(score), Some(when)) if score.is_finite() => Some((score, when)),
                _ => None,
            })
    };
    let eligible: Vec<(usize, f32, i64)> = order[..top]
        .iter()
        .enumerate()
        .filter_map(|(slot, id)| usable(id).map(|(score, when)| (slot, score, when)))
        .collect();
    let Some(best) = eligible.iter().map(|row| row.1).reduce(f32::max) else {
        return order.to_vec();
    };
    let group: Vec<(usize, f32, i64)> = eligible
        .into_iter()
        .filter(|row| row.1 + epsilon >= best)
        .collect();
    if group.len() < 2 {
        return order.to_vec();
    }
    let slots: Vec<usize> = group.iter().map(|row| row.0).collect();
    let mut newest_first = group;
    newest_first.sort_by_key(|row| std::cmp::Reverse(row.2));
    let mut out = order.to_vec();
    for (slot, member) in slots.iter().zip(newest_first) {
        out[*slot] = order[member.0].clone();
    }
    out
}

/// Tag spans and row openings the harness writes into the user channel. The
/// importer drops such rows whole when the transcript marks them
/// (`is_harness_written_row` in `import/mod.rs`); a row stored before that, or
/// a chunk cut from the middle of one, still carries the markers. Kept in step
/// with `HARNESS_WRAPPER_RE` and `HARNESS_ROW_PREFIXES` there.
const HARNESS_TAGS: &[&str] = &[
    "<task-notification>",
    "</task-notification>",
    "<local-command-caveat>",
    "</local-command-caveat>",
    "<local-command-stdout>",
    "</local-command-stdout>",
];
const HARNESS_OPENINGS: &[&str] = &[
    "[SYSTEM NOTIFICATION",
    "Caveat: The messages below were generated by the user while running local commands",
    "This session is being continued from a previous conversation",
];

/// True when `content` is harness boilerplate: it carries a harness tag or
/// opens with a harness row opening (after the keyword-lane marker).
pub fn is_harness_text(content: &str) -> bool {
    let text = content.strip_prefix("[keyword] ").unwrap_or(content);
    HARNESS_TAGS.iter().any(|tag| text.contains(tag))
        || HARNESS_OPENINGS
            .iter()
            .any(|opening| text.trim_start().starts_with(opening))
}

/// The fewest stems a chunk must carry to match the subject, as
/// [`matches_subject`] reads it. Zero stems give `usize::MAX` (never matches).
pub fn min_subject_hits(stems: &[String]) -> usize {
    if stems.is_empty() {
        return usize::MAX;
    }
    stems.len().div_ceil(2).max(2)
}

/// One chunk of a session as the status pool reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub session: String,
    pub content: String,
    /// Session time in epoch seconds; `None` abstains from representing.
    pub when: Option<i64>,
    /// Tie-break inside one timestamp: the later chunk of the transcript.
    pub seq: usize,
    /// The stored author is the user or the assistant, and the text is not an
    /// echo of the query (the caller's reading of the stored metadata).
    pub report: bool,
}

/// True when the row may stand for its session in a status answer: a stored
/// user or assistant chunk that is not an echo of the query, carries the
/// subject, and is not harness boilerplate.
pub fn is_eligible_report(row: &SessionRow, stems: &[String]) -> bool {
    row.report && !is_harness_text(&row.content) && matches_subject(&row.content, stems)
}

/// The sessions, at most `cap`, whose eligible rows match the subject. Rows are
/// in first-stage order (best first); sessions come out in the order their
/// first eligible row appears.
pub fn matched_sessions(rows: &[SessionRow], stems: &[String], cap: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for row in rows {
        if out.len() >= cap {
            break;
        }
        if is_eligible_report(row, stems) && !out.contains(&row.session) {
            out.push(row.session.clone());
        }
    }
    out
}

/// The chunk that represents `session` in a status answer: its newest
/// eligible report row (latest time, then latest `seq`, then id). Rows of other
/// sessions are ignored; a row without a time never represents. `None` when the
/// session has no eligible row.
pub fn newest_eligible(rows: &[SessionRow], session: &str, stems: &[String]) -> Option<String> {
    rows.iter()
        .filter(|row| row.session == session && is_eligible_report(row, stems))
        .filter_map(|row| row.when.map(|when| (when, row.seq, &row.id)))
        .max_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)).then_with(|| b.2.cmp(a.2)))
        .map(|(_, _, id)| id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(score: f32, when: i64, eligible: bool) -> StatusCandidate {
        StatusCandidate {
            score: Some(score),
            when: Some(when),
            eligible,
        }
    }

    fn ids(order: &[&str]) -> Vec<String> {
        order.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn f1b_status_detects_whitelist_phrases() {
        for q in [
            "what is the current state of the Vidhi Android maestro hardening workflow and what is left",
            "Current state of the release gate",
            "where does the dream backfill stand",
            "where does X stand?",
            "what's left on the migration",
            "state of the codemap daemon",
            "where do we stand on the fork PRs",
        ] {
            assert!(is_status_query(q), "should be a status query: {q}");
        }
        for q in [
            "why do hooks use catch-all wrappers",
            "state of the art embedding models",
            "left join on chunk_provenance",
            "where does the config file live",
            "",
            "   ",
        ] {
            assert!(!is_status_query(q), "should not be a status query: {q}");
        }
        let stems = subject_stems(
            "what is the current state of the Vidhi Android maestro hardening workflow and what is left",
        );
        assert_eq!(stems, ["vidhi", "androi", "maestr", "harden", "workfl"]);
        assert!(matches_subject("The Vidhi Maestro workflow state", &stems));
        assert!(!matches_subject("a maestro flow", &stems));
        assert!(subject_stems("what is left").is_empty());
    }

    #[test]
    fn f1b_status_non_status_query_unchanged() {
        // The reorder itself never looks at the query; the gate is
        // `is_status_query`. A page with no eligible candidate, or none with a
        // score, comes back exactly as it went in.
        let order = ids(&["a", "b", "c"]);
        let mut candidates = HashMap::new();
        candidates.insert("a".to_string(), cand(5.0, 10, false));
        candidates.insert("b".to_string(), cand(5.0, 20, false));
        assert_eq!(reorder_near_ties(&order, &candidates, 3, 0.25), order);
        assert_eq!(reorder_near_ties(&order, &HashMap::new(), 3, 0.25), order);
        assert!(!is_status_query("why do hooks use catch-all wrappers"));
    }

    #[test]
    fn f1b_status_latest_session_report_wins_near_tie() {
        let order = ids(&["old", "tool", "new", "far"]);
        let mut candidates = HashMap::new();
        candidates.insert("old".to_string(), cand(10.0, 100, true));
        candidates.insert("tool".to_string(), cand(9.9, 900, false));
        candidates.insert("new".to_string(), cand(9.9, 300, true));
        candidates.insert("far".to_string(), cand(5.0, 999, true));
        let out = reorder_near_ties(&order, &candidates, 4, 0.25);
        // `old` and `new` trade slots; the tool row and the far row stay.
        assert_eq!(out, ids(&["new", "tool", "old", "far"]));
        // Only the top `limit` is touched.
        assert_eq!(reorder_near_ties(&order, &candidates, 2, 0.25), order);
    }

    #[test]
    fn f1b_status_does_not_override_clear_winner() {
        let order = ids(&["best", "newer"]);
        let mut candidates = HashMap::new();
        candidates.insert("best".to_string(), cand(10.0, 100, true));
        candidates.insert("newer".to_string(), cand(9.5, 900, true));
        assert_eq!(reorder_near_ties(&order, &candidates, 2, 0.25), order);
        // A candidate with no timestamp abstains.
        candidates.insert(
            "newer".to_string(),
            StatusCandidate {
                score: Some(10.0),
                when: None,
                eligible: true,
            },
        );
        assert_eq!(reorder_near_ties(&order, &candidates, 2, 0.25), order);
    }

    fn row(id: &str, session: &str, text: &str, when: i64, seq: usize, report: bool) -> SessionRow {
        SessionRow {
            id: id.into(),
            session: session.into(),
            content: text.into(),
            when: Some(when),
            seq,
            report,
        }
    }

    fn vidhi_stems() -> Vec<String> {
        subject_stems(
            "what is the current state of the Vidhi Android maestro hardening workflow and what is left",
        )
    }

    #[test]
    fn f3b_status_newest_eligible_represents_the_session() {
        let stems = vidhi_stems();
        let report = "Vidhi Android maestro run: green";
        let rows = vec![
            row("old", "s1", report, 100, 1, true),
            row("new", "s1", report, 300, 2, true),
            row("newer-off-topic", "s1", "radio station notes", 400, 3, true),
            row("other-session", "s2", report, 900, 1, true),
        ];
        assert_eq!(newest_eligible(&rows, "s1", &stems).as_deref(), Some("new"));
        assert_eq!(newest_eligible(&rows, "s3", &stems), None);
        // Same instant: the later chunk of the transcript wins.
        let tied = vec![
            row("a", "s1", report, 300, 5, true),
            row("b", "s1", report, 300, 6, true),
        ];
        assert_eq!(newest_eligible(&tied, "s1", &stems).as_deref(), Some("b"));
    }

    #[test]
    fn f3b_status_harness_and_tool_rows_never_represent() {
        let stems = vidhi_stems();
        // The tail of a task notification: stored as a user chunk, carries the
        // subject, but the harness wrote it.
        let tail = "sses on vidhi-qa-clock). Android: PASS maestro\n</result>\n<usage>x</usage>\n</task-notification>";
        assert!(is_harness_text(tail));
        assert!(is_harness_text("[keyword] <task-notification> vidhi"));
        assert!(is_harness_text(
            "This session is being continued from a previous conversation vidhi"
        ));
        assert!(!is_harness_text("Prod is live. Vidhi maestro Android"));
        let rows = vec![
            row(
                "report",
                "s1",
                "Prod is live. Vidhi maestro Android",
                100,
                1,
                true,
            ),
            row("harness", "s1", tail, 900, 9, true),
            // Tool output: the caller marks it as not a report.
            row("tool", "s1", "vidhi maestro android log", 800, 8, false),
        ];
        assert_eq!(
            newest_eligible(&rows, "s1", &stems).as_deref(),
            Some("report")
        );
        assert_eq!(
            matched_sessions(&rows[1..], &stems, 3),
            Vec::<String>::new()
        );
        let only_harness = vec![rows[1].clone(), rows[2].clone()];
        assert_eq!(newest_eligible(&only_harness, "s1", &stems), None);
    }

    #[test]
    fn f3b_status_matched_sessions_caps_and_orders() {
        let stems = vidhi_stems();
        let text = "Vidhi Android maestro report";
        let rows = vec![
            row("a1", "sa", text, 1, 1, true),
            row("x", "sx", "radio notes", 2, 1, true),
            row("b1", "sb", text, 3, 1, true),
            row("a2", "sa", text, 4, 2, true),
            row("c1", "sc", text, 5, 1, true),
            row("d1", "sd", text, 6, 1, true),
        ];
        assert_eq!(matched_sessions(&rows, &stems, 3), ["sa", "sb", "sc"]);
        assert_eq!(matched_sessions(&rows, &stems, 1), ["sa"]);
        assert!(matched_sessions(&rows, &[], 3).is_empty());
        assert_eq!(min_subject_hits(&stems), 3);
        assert_eq!(min_subject_hits(&[]), usize::MAX);
    }
}
