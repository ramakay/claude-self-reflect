//! This week's dreams: curated from already-gated night-pass output.
//!
//! A week-dream is not a session log row. It is one incomplete item the
//! night already dreamed, still open, with a gated how. Hypothesis is the
//! stored night-pass sentence when one exists (one free sentence). How is
//! verified plan steps, else the item text as TOUCH NEXT.
//!
//! GET `/` never invokes a model. `claude -p` ranking is a later writer.

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, TimeZone, Utc};

use super::composer::{self, StoredPlan};
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

/// `(iso_year, iso_week)` for a stored timestamp, or `None` if unparseable.
pub fn week_key(raw: &str) -> Option<(i32, u32)> {
    let dt = parse_ts(raw)?;
    let week = dt.iso_week();
    Some((week.year(), week.week()))
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

/// Rank and cap candidates that already belong to `now`'s ISO week.
pub(crate) fn select_week_dreams(mut cands: Vec<Candidate>, now: DateTime<Utc>) -> Vec<WeekDream> {
    let now_week = {
        let w = now.iso_week();
        (w.year(), w.week())
    };
    cands.retain(|c| {
        c.item.completed.is_none()
            && week_key(&c.item.origin_ts) == Some(now_week)
            && !c.how.is_empty()
    });
    cands.sort_by(|a, b| {
        b.has_plan
            .cmp(&a.has_plan)
            .then_with(|| (b.item.kind == "blocker").cmp(&(a.item.kind == "blocker")))
            .then_with(|| b.how.len().cmp(&a.how.len()))
            .then_with(|| b.item.origin_ts.cmp(&a.item.origin_ts))
            .then_with(|| a.item.id.cmp(&b.item.id))
    });
    cands.truncate(MAX_WEEK_DREAMS);
    cands
        .into_iter()
        .map(|c| WeekDream {
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
        })
        .collect()
}

/// Load this week's dreams from stored rows. No model.
pub fn load_week_dreams(
    conn: &rusqlite::Connection,
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<WeekDream>> {
    let open = crate::storage::dream_clusters::load_open_items(conn)?;
    let mut cands = Vec::new();
    for item in open {
        if item.completed.is_some() {
            continue;
        }
        let plan: Option<StoredPlan> = composer::load_plan(conn, &item.id)?;
        let (how, has_plan) = match plan {
            Some(plan) if !plan.steps.is_empty() => {
                (plan.steps.into_iter().map(|s| s.action).collect(), true)
            }
            _ => (vec![item.item.clone()], false),
        };
        let threads = composer::threads_for_session(conn, &item.origin_session)?;
        let hypothesis = threads
            .into_iter()
            .map(|t| t.thread)
            .find(|t| !t.trim().is_empty());
        cands.push(Candidate {
            item,
            how,
            hypothesis,
            has_plan,
        });
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

    #[test]
    fn week_key_reads_iso_week() {
        assert_eq!(week_key("2026-08-12T12:00:00Z"), Some((2026, 33)));
        assert_eq!(week_key("2026-08-16T23:00:00Z"), Some((2026, 33)));
        assert_eq!(week_key("2026-08-09T23:00:00Z"), Some((2026, 32)));
        assert_eq!(week_key(""), None);
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
    fn select_drops_empty_how_and_completed() {
        let now = DateTime::parse_from_rfc3339("2026-08-16T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut done = cand("done", "todo", "2026-08-15T10:00:00Z", "done", true, 2);
        done.item.completed = Some(crate::storage::dream_clusters::CompletionReceipt {
            session_id: "later".into(),
            completed_at: "2026-08-16T00:00:00Z".into(),
            completed_date: "2026-08-16".into(),
        });
        let empty_how = Candidate {
            item: item("empty", "todo", "2026-08-15T10:00:00Z", "empty"),
            how: vec![],
            hypothesis: Some("h".into()),
            has_plan: false,
        };
        let got = select_week_dreams(vec![done, empty_how], now);
        assert!(got.is_empty());
    }
}
