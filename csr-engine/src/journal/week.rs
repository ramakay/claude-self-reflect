//! This week's dreams: curated from already-gated night-pass output.
//!
//! A week-dream is not a session log row. It is one incomplete item the
//! night already dreamed, still open, with either a gated how or a matched
//! night-thread hypothesis. Hypothesis is the stored night-pass sentence
//! when one exists, but never the first one lying around — every thread
//! in the item's origin session is scored against the item's own text by
//! distinct-token overlap (alphanumeric words longer than 3 chars,
//! lowercased) and assigned greedily, highest score first (score `>= 2`
//! required, ties broken by older thread id then item id). Each thread
//! lands on at most one item, so two items from the same session never
//! wear the same hypothesis sentence. Unmatched items get no hypothesis —
//! abstention, not smear. (`dream_threads.files_json` is almost always
//! empty in the live corpus, so file overlap is not usable; text-token
//! overlap is.) How is composed only from stored plan steps —
//! `"{action} {basename} ⌗{citation}"`, each segment present only when
//! the step actually carries it — never from the item's own title. A
//! candidate with no plan, or a plan whose steps all had blank actions,
//! gets an empty how; it still survives if a hypothesis matched. A bare
//! title with neither survives — it is a log row, not a dream. Selection
//! also caps the home at one card per origin session, so a single loud
//! session cannot fill every slot.
//!
//! GET `/` never invokes a model. `claude -p` ranking is a later writer.

use std::collections::HashSet;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};

use super::composer::{self, StoredPlan};
use crate::dream::threads::DreamThread;
use crate::storage::dream_clusters::OpenItem;

/// Hard cap on the home. Spare by construction.
pub const MAX_WEEK_DREAMS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeekDream {
    pub title: String,
    pub hypothesis: Option<String>,
    pub how: Vec<String>,
    pub project: String,
    pub item_id: String,
    pub kind_label: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    item: OpenItem,
    how: Vec<String>,
    hypothesis: Option<String>,
    has_plan: bool,
}

/// True when `raw` falls inside the rolling week ending at `now`.
///
/// A rolling window, not the ISO calendar week: the calendar week empties
/// the home at the UTC week rollover (observed live: Sunday evening local
/// = Monday UTC dropped every open item), and an unfinished item does not
/// stop being this week's business at midnight. Unparseable → out (the
/// window is evidence-gated, never guessed). A small forward tolerance
/// absorbs clock skew without admitting future-dated rows.
pub fn within_week(raw: &str, now: DateTime<Utc>) -> bool {
    let Some(ts) = parse_ts(raw) else {
        return false;
    };
    let age = now.signed_duration_since(ts);
    age <= Duration::days(7) && age >= Duration::hours(-1)
}

fn parse_ts(raw: &str) -> Option<DateTime<Utc>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S") {
        return Some(Utc.from_utc_datetime(&naive));
    }
    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        return Some(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?));
    }
    None
}

/// Rank and cap candidates from the rolling week ending at `now`.
///
/// After ranking, slots fill one card per origin session — a candidate
/// whose session is already represented is skipped, never bumping an
/// earlier pick. A loud session with many open items still only ever
/// contributes its single best-ranked one.
pub(crate) fn select_week_dreams(mut cands: Vec<Candidate>, now: DateTime<Utc>) -> Vec<WeekDream> {
    cands.retain(|c| {
        c.item.completed.is_none()
            && within_week(&c.item.origin_ts, now)
            && (!c.how.is_empty() || c.hypothesis.is_some())
    });
    cands.sort_by(|a, b| {
        b.has_plan
            .cmp(&a.has_plan)
            .then_with(|| (b.item.kind == "blocker").cmp(&(a.item.kind == "blocker")))
            .then_with(|| b.how.len().cmp(&a.how.len()))
            .then_with(|| b.item.origin_ts.cmp(&a.item.origin_ts))
            .then_with(|| a.item.id.cmp(&b.item.id))
    });

    let mut seen_sessions: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(MAX_WEEK_DREAMS);
    for c in cands.into_iter() {
        if out.len() >= MAX_WEEK_DREAMS {
            break;
        }
        if !seen_sessions.insert(c.item.origin_session.clone()) {
            continue;
        }
        out.push(WeekDream {
            title: c.item.item,
            hypothesis: c.hypothesis,
            how: c.how,
            project: c.item.project,
            item_id: c.item.id,
            kind_label: if c.has_plan {
                "natural direction"
            } else {
                "unfinished"
            },
        });
    }
    out
}

/// Compose one how line from a stored plan step, or `None` when the step's
/// action is blank after trim — a step with nothing imperative to say is
/// skipped, not rendered as an empty bullet.
///
/// Format: `"{action} {basename of first file} ⌗{citation}"`. The file
/// segment is omitted when `files` is empty; the receipt segment is
/// omitted when `citation` is blank. Plain `format!` only — no model text.
fn compose_how_line(step: &composer::PlanStep) -> Option<String> {
    let action = step.action.trim();
    if action.is_empty() {
        return None;
    }
    let mut line = action.to_string();
    if let Some(first) = step.files.first() {
        let basename = std::path::Path::new(first)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| first.clone());
        if !basename.is_empty() {
            line.push(' ');
            line.push_str(&basename);
        }
    }
    let citation = step.citation.trim();
    if !citation.is_empty() {
        line.push_str(" ⌗");
        line.push_str(citation);
    }
    Some(line)
}

/// Distinct lowercase alphanumeric tokens of length > 3, split on any
/// non-alphanumeric character. A matching heuristic only — not a search
/// index, not stemmed, not stopword-filtered.
fn tokens(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 3)
        .map(|w| w.to_lowercase())
        .collect()
}

/// Assign each item in one origin session at most one night-thread
/// hypothesis, by token overlap between the item's own text and each
/// thread's `thread` sentence. Returns, per input item (same order,
/// same length), the index into `threads` it was matched to.
///
/// Every (item, thread) pair scores `|tokens(item) ∩ tokens(thread)|`; a
/// pair is eligible only at `score >= 2`. Eligible pairs are sorted by
/// score descending, ties broken by older thread id first (ascending —
/// `DreamThread.id` is an autoincrement) then item id (ascending), and
/// assigned greedily: a pair fires only when both sides are still free.
/// A thread already spent on one item cannot also hypothesize a second —
/// this is what keeps two items from wearing the same sentence. Items
/// with no eligible pair get `None`; the caller drops that clause rather
/// than guessing.
fn assign_hypotheses(items: &[(&str, &str)], threads: &[DreamThread]) -> Vec<Option<usize>> {
    let item_tokens: Vec<HashSet<String>> = items.iter().map(|(_, text)| tokens(text)).collect();
    let thread_tokens: Vec<HashSet<String>> = threads.iter().map(|t| tokens(&t.thread)).collect();

    let mut pairs: Vec<(usize, usize, usize)> = Vec::new();
    for (ii, itoks) in item_tokens.iter().enumerate() {
        for (ti, ttoks) in thread_tokens.iter().enumerate() {
            let score = itoks.intersection(ttoks).count();
            if score >= 2 {
                pairs.push((ii, ti, score));
            }
        }
    }
    pairs.sort_by(|a, b| {
        b.2.cmp(&a.2) // score descending
            .then_with(|| threads[a.1].id.cmp(&threads[b.1].id)) // older thread id first
            .then_with(|| items[a.0].0.cmp(items[b.0].0)) // item id
    });

    let mut item_taken = vec![false; items.len()];
    let mut thread_taken = vec![false; threads.len()];
    let mut assigned: Vec<Option<usize>> = vec![None; items.len()];
    for (ii, ti, _score) in pairs {
        if item_taken[ii] || thread_taken[ti] {
            continue;
        }
        item_taken[ii] = true;
        thread_taken[ti] = true;
        assigned[ii] = Some(ti);
    }
    assigned
}

/// Load this week's dreams from stored rows. No model.
pub fn load_week_dreams(
    conn: &rusqlite::Connection,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<WeekDream>> {
    let open = crate::storage::dream_clusters::load_open_items(conn)?;

    // Build every candidate's how-line first (hypothesis assigned below,
    // per origin session, once threads for that session are in hand).
    let mut cands = Vec::new();
    for item in open {
        if item.completed.is_some() {
            continue;
        }
        let plan: Option<StoredPlan> = composer::load_plan(conn, &item.id)?;
        let plan_exists = plan.is_some();
        let how: Vec<String> = plan
            .into_iter()
            .flat_map(|p| p.steps.into_iter())
            .filter_map(|s| compose_how_line(&s))
            .collect();
        let has_plan = plan_exists && !how.is_empty();
        cands.push(Candidate {
            item,
            how,
            hypothesis: None,
            has_plan,
        });
    }

    // Group candidate indices by origin session, then resolve hypotheses
    // one session at a time — `threads_for_session` runs once per distinct
    // session, not once per item.
    let mut by_session: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (idx, c) in cands.iter().enumerate() {
        by_session
            .entry(c.item.origin_session.clone())
            .or_default()
            .push(idx);
    }
    for (session_id, indices) in by_session {
        let threads = composer::threads_for_session(conn, &session_id)?;
        if threads.is_empty() {
            continue;
        }
        let session_items: Vec<(&str, &str)> = indices
            .iter()
            .map(|&i| (cands[i].item.id.as_str(), cands[i].item.item.as_str()))
            .collect();
        let assigned = assign_hypotheses(&session_items, &threads);
        for (slot, &idx) in indices.iter().enumerate() {
            if let Some(ti) = assigned[slot] {
                cands[idx].hypothesis = Some(threads[ti].thread.clone());
            }
        }
    }

    Ok(select_week_dreams(cands, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::dream_clusters::OpenItem;

    fn item(id: &str, kind: &str, ts: &str, title: &str) -> OpenItem {
        OpenItem {
            id: id.into(),
            project: "csr".into(),
            item: title.into(),
            kind: kind.into(),
            origin_session: format!("sess-{id}"),
            origin_ts: ts.into(),
            origin_date: ts[..10].into(),
            completed: None,
            examined: true,
        }
    }

    fn cand(
        id: &str,
        kind: &str,
        ts: &str,
        title: &str,
        has_plan: bool,
        how_n: usize,
    ) -> Candidate {
        Candidate {
            item: item(id, kind, ts, title),
            how: (0..how_n).map(|i| format!("step {i}")).collect(),
            hypothesis: Some(format!("hyp {id}")),
            has_plan,
        }
    }

    fn at(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .expect("test timestamp")
            .with_timezone(&Utc)
    }

    #[test]
    fn within_week_is_a_rolling_window_not_a_calendar_week() {
        let now = at("2026-08-16T18:00:00Z");
        assert!(within_week("2026-08-12T12:00:00Z", now));
        assert!(within_week("2026-08-09T19:00:00Z", now));
        assert!(!within_week("2026-08-09T10:00:00Z", now), "8d ago is out");
        assert!(!within_week("2026-08-20T00:00:00Z", now), "future is out");
        assert!(!within_week("", now));
    }

    #[test]
    fn the_utc_week_rollover_does_not_empty_the_home() {
        // Observed live 2026-08-16: Sunday evening local is Monday 03:31 UTC.
        // ISO-week matching dropped every open item at that boundary.
        let now = at("2026-08-17T03:31:00Z");
        assert!(within_week("2026-08-15T21:46:13Z", now));
        assert!(within_week("2026-08-13T23:05:57.942739+00:00", now));
    }

    #[test]
    fn select_keeps_this_week_only_and_prefers_plans() {
        let now = DateTime::parse_from_rfc3339("2026-08-16T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let last_week = cand("old", "todo", "2026-08-09T10:00:00Z", "old", true, 3);
        let thin = cand("thin", "todo", "2026-08-15T10:00:00Z", "thin", false, 1);
        let planned = cand("plan", "todo", "2026-08-14T10:00:00Z", "planned", true, 2);
        let blocker = cand("blk", "blocker", "2026-08-13T10:00:00Z", "block", false, 1);
        let got = select_week_dreams(vec![last_week, thin, planned, blocker], now);
        assert_eq!(
            got.iter().map(|d| d.item_id.as_str()).collect::<Vec<_>>(),
            vec!["plan", "blk", "thin"]
        );
        assert_eq!(got[0].kind_label, "natural direction");
        assert_eq!(got[1].kind_label, "unfinished");
    }

    #[test]
    fn select_drops_completed_and_bare_titles_with_no_how_or_hypothesis() {
        let now = DateTime::parse_from_rfc3339("2026-08-16T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut done = cand("done", "todo", "2026-08-15T10:00:00Z", "done", true, 2);
        done.item.completed = Some(crate::storage::dream_clusters::CompletionReceipt {
            session_id: "later".into(),
            completed_at: "2026-08-16T00:00:00Z".into(),
            completed_date: "2026-08-16".into(),
        });
        let bare = Candidate {
            item: item("bare", "todo", "2026-08-15T10:00:00Z", "bare"),
            how: vec![],
            hypothesis: None,
            has_plan: false,
        };
        let got = select_week_dreams(vec![done, bare], now);
        assert!(got.is_empty());
    }

    #[test]
    fn a_no_plan_candidate_with_a_hypothesis_survives_with_empty_how() {
        let now = DateTime::parse_from_rfc3339("2026-08-16T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let hyp_only = Candidate {
            item: item("hyp", "todo", "2026-08-15T10:00:00Z", "hyp only"),
            how: vec![],
            hypothesis: Some("matched night thread".into()),
            has_plan: false,
        };
        let got = select_week_dreams(vec![hyp_only], now);
        assert_eq!(got.len(), 1);
        assert!(got[0].how.is_empty());
        assert_eq!(got[0].kind_label, "unfinished");
    }

    #[test]
    fn a_no_plan_candidate_with_no_hypothesis_drops() {
        let now = DateTime::parse_from_rfc3339("2026-08-16T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let neither = Candidate {
            item: item("neither", "todo", "2026-08-15T10:00:00Z", "neither"),
            how: vec![],
            hypothesis: None,
            has_plan: false,
        };
        let got = select_week_dreams(vec![neither], now);
        assert!(got.is_empty());
    }

    #[test]
    fn compose_how_line_carries_file_basename_and_receipt_oid() {
        let step = composer::PlanStep {
            action: "review".into(),
            files: vec!["src/journal/week.rs".into()],
            citation: "abc123".into(),
        };
        assert_eq!(
            compose_how_line(&step),
            Some("review week.rs ⌗abc123".into())
        );
    }

    #[test]
    fn compose_how_line_omits_file_segment_when_files_is_empty() {
        let step = composer::PlanStep {
            action: "resolve the blocker".into(),
            files: vec![],
            citation: "def456".into(),
        };
        assert_eq!(
            compose_how_line(&step),
            Some("resolve the blocker ⌗def456".into())
        );
    }

    #[test]
    fn compose_how_line_omits_receipt_segment_when_citation_is_blank() {
        let step = composer::PlanStep {
            action: "review".into(),
            files: vec!["src/journal/week.rs".into()],
            citation: "  ".into(),
        };
        assert_eq!(compose_how_line(&step), Some("review week.rs".into()));
    }

    #[test]
    fn compose_how_line_skips_steps_with_blank_action() {
        let step = composer::PlanStep {
            action: "   ".into(),
            files: vec!["src/journal/week.rs".into()],
            citation: "abc123".into(),
        };
        assert_eq!(compose_how_line(&step), None);
    }

    fn thread(id: i64, session_id: &str, text: &str) -> DreamThread {
        DreamThread {
            id,
            episode_hash: format!("hash-{id}"),
            session_id: session_id.into(),
            project: "csr".into(),
            thread: text.into(),
            evidence_quote: String::new(),
            files: vec![],
            receipt_tier: crate::dream::threads::ReceiptTier::Verdict,
            receipts: vec![],
            model: "test".into(),
            created_at: "2026-08-15T00:00:00Z".into(),
        }
    }

    #[test]
    fn assign_hypotheses_only_matches_the_item_with_real_overlap() {
        let items = [
            ("a", "fix the release gate for journal composer"),
            ("b", "add tests for dashboard rendering"),
        ];
        let threads = [thread(1, "sess-1", "release gate journal composer stuck")];
        let assigned = assign_hypotheses(&items, &threads);
        assert_eq!(assigned, vec![Some(0), None]);
    }

    #[test]
    fn assign_hypotheses_gives_a_contested_thread_to_the_higher_score_only() {
        let items = [
            ("weak", "journal composer touched"),
            ("strong", "journal composer release gate rewrite"),
        ];
        let threads = [thread(
            1,
            "sess-1",
            "journal composer release gate rewrite done",
        )];
        let assigned = assign_hypotheses(&items, &threads);
        // "strong" scores higher overlap than "weak" against the same thread.
        assert_eq!(assigned, vec![None, Some(0)]);
    }

    #[test]
    fn assign_hypotheses_tie_breaks_deterministically_and_reruns_identically() {
        // Both items score exactly 2 against the same thread; the thread id
        // is identical for both pairs, so the tie-break falls to item id
        // ascending — "aaa" wins over "bbb".
        let items = [
            ("bbb", "release gate broken"),
            ("aaa", "release gate stuck"),
        ];
        let threads = [thread(1, "sess-1", "release gate needs work")];
        let first = assign_hypotheses(&items, &threads);
        let second = assign_hypotheses(&items, &threads);
        assert_eq!(first, second);
        // index 1 is "aaa" — it must be the one that wins the tie.
        assert_eq!(first, vec![None, Some(0)]);
    }

    #[test]
    fn assign_hypotheses_requires_score_at_least_two() {
        let items = [("only", "release notes")];
        // Shares only "release" (one token) with the item text.
        let threads = [thread(1, "sess-1", "release cut yesterday afternoon")];
        let assigned = assign_hypotheses(&items, &threads);
        assert_eq!(assigned, vec![None]);
    }

    #[test]
    fn select_never_returns_two_cards_from_the_same_origin_session() {
        let now = DateTime::parse_from_rfc3339("2026-08-16T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut first = cand("first", "todo", "2026-08-15T12:00:00Z", "first", true, 3);
        first.item.origin_session = "sess-shared".into();
        let mut second = cand("second", "todo", "2026-08-15T11:00:00Z", "second", true, 2);
        second.item.origin_session = "sess-shared".into();
        let third = cand("third", "todo", "2026-08-15T10:00:00Z", "third", true, 1);
        let got = select_week_dreams(vec![first, second, third], now);
        assert_eq!(got.len(), 2, "sess-shared contributes only one card");
        assert_eq!(
            got.iter().map(|d| d.item_id.as_str()).collect::<Vec<_>>(),
            vec!["first", "third"],
            "higher-ranked candidate wins the shared session, third survives from its own"
        );
    }
}
