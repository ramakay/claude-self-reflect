use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{bail, Result};
use chrono::NaiveDate;
use serde::Serialize;

use crate::dream::backfill::intent_channel::{containment, content_tokens};
use crate::extraction::provenance::LESSONS_SENTINEL;
use crate::storage::Storage;
use crate::transcript::intent_events::{IntentEvent, IntentEventKind};

#[derive(Debug, Clone, Serialize)]
pub struct LessonReceipt {
    pub session_id: String,
    pub turn: u32,
    pub transcript_path: PathBuf,
    pub byte_start: usize,
    pub byte_end: usize,
    pub ts: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LessonGroup {
    pub quote: String,
    pub events: usize,
    pub distinct_sessions: usize,
    pub last_date: String,
    pub receipt: String,
    pub score: f64,
    pub members: Vec<LessonReceipt>,
}

fn event_date(event: &IntentEvent) -> &str {
    event.ts.get(..10).unwrap_or(event.ts.as_str())
}

fn session8(session_id: &str) -> String {
    session_id.chars().take(8).collect()
}

fn project_matches(requested: &str, stored: &str) -> bool {
    let requested = requested.trim().to_ascii_lowercase();
    let stored = stored.trim().to_ascii_lowercase();
    stored == requested || stored.starts_with(&format!("{requested}-"))
}

fn find(parent: &mut [usize], index: usize) -> usize {
    if parent[index] != index {
        parent[index] = find(parent, parent[index]);
    }
    parent[index]
}

fn union(parent: &mut [usize], left: usize, right: usize) {
    let left_root = find(parent, left);
    let right_root = find(parent, right);
    if left_root != right_root {
        parent[right_root] = left_root;
    }
}

pub fn group_events(mut events: Vec<IntentEvent>, min_sessions: usize) -> Vec<LessonGroup> {
    events.retain(|event| {
        matches!(
            event.kind,
            IntentEventKind::Correction | IntentEventKind::Redirect
        )
    });
    events.sort_by(|left, right| {
        left.ts
            .cmp(&right.ts)
            .then_with(|| left.session_id.cmp(&right.session_id))
            .then_with(|| left.turn.cmp(&right.turn))
    });
    let tokens: Vec<_> = events
        .iter()
        .map(|event| content_tokens(&event.quote))
        .collect();
    let mut parent: Vec<usize> = (0..events.len()).collect();
    for left in 0..events.len() {
        for right in left + 1..events.len() {
            if containment(&tokens[left], &tokens[right])
                .max(containment(&tokens[right], &tokens[left]))
                >= 0.5
            {
                union(&mut parent, left, right);
            }
        }
    }

    let reference_date = events
        .iter()
        .filter_map(|event| NaiveDate::parse_from_str(event_date(event), "%Y-%m-%d").ok())
        .max();
    let mut roots = std::collections::BTreeMap::<usize, Vec<usize>>::new();
    for index in 0..events.len() {
        let root = find(&mut parent, index);
        roots.entry(root).or_default().push(index);
    }

    let mut groups = Vec::new();
    for indices in roots.into_values() {
        let sessions: BTreeSet<&str> = indices
            .iter()
            .map(|index| events[*index].session_id.as_str())
            .collect();
        if sessions.len() < min_sessions {
            continue;
        }
        let shortest = indices
            .iter()
            .map(|index| &events[*index])
            .min_by(|left, right| {
                left.quote
                    .chars()
                    .count()
                    .cmp(&right.quote.chars().count())
                    .then_with(|| left.quote.cmp(&right.quote))
            })
            .expect("non-empty connected component");
        let latest = indices
            .iter()
            .map(|index| &events[*index])
            .max_by(|left, right| {
                left.ts
                    .cmp(&right.ts)
                    .then_with(|| left.session_id.cmp(&right.session_id))
                    .then_with(|| left.turn.cmp(&right.turn))
            })
            .expect("non-empty connected component");
        let last_date = event_date(latest).to_string();
        let days = reference_date
            .zip(NaiveDate::parse_from_str(&last_date, "%Y-%m-%d").ok())
            .map_or(0, |(reference, latest)| {
                (reference - latest).num_days().max(0)
            });
        let distinct_sessions = sessions.len();
        groups.push(LessonGroup {
            quote: shortest.quote.trim().to_string(),
            events: indices.len(),
            distinct_sessions,
            last_date,
            receipt: format!("{}:{}", session8(&latest.session_id), latest.turn),
            score: distinct_sessions as f64 / (1.0 + days as f64),
            members: indices
                .iter()
                .map(|index| {
                    let event = &events[*index];
                    LessonReceipt {
                        session_id: event.session_id.clone(),
                        turn: event.turn,
                        transcript_path: event.transcript_path.clone(),
                        byte_start: event.byte_start,
                        byte_end: event.byte_end,
                        ts: event.ts.clone(),
                    }
                })
                .collect(),
        });
    }
    groups.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.distinct_sessions.cmp(&left.distinct_sessions))
            .then_with(|| right.last_date.cmp(&left.last_date))
            .then_with(|| left.quote.cmp(&right.quote))
    });
    groups
}

pub fn format_text(groups: &[LessonGroup]) -> String {
    let mut output = format!("{LESSONS_SENTINEL}\n");
    for group in groups {
        output.push_str(&format!(
            "- {} (seen {}× across {} sessions, last {}; {})\n",
            group.quote, group.events, group.distinct_sessions, group.last_date, group.receipt
        ));
    }
    output
}

pub fn handle(
    storage: &Storage,
    project: &str,
    since: Option<&str>,
    min_sessions: usize,
    json: bool,
) -> Result<String> {
    if project.trim().is_empty() {
        bail!("--project must not be empty");
    }
    if min_sessions == 0 {
        bail!("--min-sessions must be at least 1");
    }
    if let Some(value) = since {
        NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map_err(|_| anyhow::anyhow!("--since must use YYYY-MM-DD"))?;
    }
    let events = storage
        .list_intent_events(None, since)?
        .into_iter()
        .filter(|event| project_matches(project, &event.project))
        .collect();
    let groups = group_events(events, min_sessions);
    if json {
        Ok(serde_json::to_string_pretty(&groups)?)
    } else {
        Ok(format_text(&groups))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::transcript::intent_events::{IntentEvent, IntentEventKind};

    fn event(session: &str, turn: u32, quote: &str, ts: &str) -> IntentEvent {
        IntentEvent {
            session_id: session.into(),
            project: "p".into(),
            turn,
            kind: IntentEventKind::Correction,
            quote: quote.into(),
            transcript_path: PathBuf::from(format!("/tmp/{session}.jsonl")),
            byte_start: turn as usize * 10,
            byte_end: turn as usize * 10 + quote.len(),
            prior_claim: String::new(),
            symbol: None,
            file: None,
            classifier_hash: "v1".into(),
            detector: None,
            classifier_score: None,
            marker: Some("no".into()),
            ts: ts.into(),
        }
    }

    #[test]
    fn groups_by_transitive_symmetric_containment_and_distinct_sessions() {
        let events = vec![
            event(
                "aaaaaaaa-1",
                1,
                "never bypass the verification checks",
                "2026-08-01T00:00:00Z",
            ),
            event(
                "bbbbbbbb-2",
                2,
                "bypass verification checks",
                "2026-08-02T00:00:00Z",
            ),
            event(
                "cccccccc-3",
                3,
                "verification checks must run",
                "2026-08-03T00:00:00Z",
            ),
            event(
                "aaaaaaaa-1",
                4,
                "use a completely different workflow",
                "2026-08-04T00:00:00Z",
            ),
        ];

        let groups = super::group_events(events, 2);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].events, 3);
        assert_eq!(groups[0].distinct_sessions, 3);
        assert_eq!(groups[0].quote, "bypass verification checks");
        assert_eq!(groups[0].last_date, "2026-08-03");
        assert_eq!(groups[0].receipt, "cccccccc:3");
        assert_eq!(groups[0].members.len(), 3);
    }

    #[test]
    fn text_output_has_sentinel_and_exact_candidate_line() {
        let groups = super::group_events(
            vec![
                event(
                    "aaaaaaaa-1",
                    1,
                    "never bypass verification checks",
                    "2026-08-01T00:00:00Z",
                ),
                event(
                    "bbbbbbbb-2",
                    7,
                    "bypass verification checks",
                    "2026-08-03T00:00:00Z",
                ),
            ],
            2,
        );
        assert_eq!(
            super::format_text(&groups),
            "[[CSR:LESSONS]]\n- bypass verification checks (seen 2× across 2 sessions, last 2026-08-03; bbbbbbbb:7)\n"
        );
        let json = serde_json::to_value(&groups).unwrap();
        assert_eq!(
            json[0]["members"][0]["transcript_path"],
            "/tmp/aaaaaaaa-1.jsonl"
        );
        assert_eq!(json[0]["members"][1]["byte_start"], 70);
    }

    #[test]
    fn project_scope_includes_case_insensitive_hyphenated_descendants_only() {
        assert!(super::project_matches("anukriti", "anukriti"));
        assert!(super::project_matches("anukriti", "Anukriti-Campaigns"));
        assert!(super::project_matches(
            "anukriti",
            "anukriti-meta-campaigns"
        ));
        assert!(!super::project_matches("anukriti", "anukriti2"));
        assert!(!super::project_matches("anukriti", "other-anukriti"));
    }
}
