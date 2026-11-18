use std::collections::{BTreeSet, HashSet};
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

const MIN_CONTENT_TOKENS: usize = 3;
const MIN_GROUP_COMMON_TOKENS: usize = 2;

fn directly_matches(left: &HashSet<String>, right: &HashSet<String>) -> bool {
    containment(left, right).max(containment(right, left)) >= 0.5
}

fn common_core_with(group: &[usize], candidate: usize, tokens: &[HashSet<String>]) -> usize {
    let mut common = tokens[candidate].clone();
    for member in group {
        common.retain(|token| tokens[*member].contains(token));
    }
    common.len()
}

pub fn group_events(mut events: Vec<IntentEvent>, min_sessions: usize) -> Vec<LessonGroup> {
    events.retain(|event| {
        matches!(
            event.kind,
            IntentEventKind::Correction | IntentEventKind::Redirect
        ) && content_tokens(&event.quote).len() >= MIN_CONTENT_TOKENS
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
    let mut groups_by_index: Vec<Vec<usize>> = Vec::new();
    for candidate in 0..events.len() {
        if let Some(group) = groups_by_index.iter_mut().find(|group| {
            group
                .iter()
                .all(|member| directly_matches(&tokens[*member], &tokens[candidate]))
                && common_core_with(group, candidate, &tokens) >= MIN_GROUP_COMMON_TOKENS
        }) {
            group.push(candidate);
        } else {
            groups_by_index.push(vec![candidate]);
        }
    }

    let reference_date = events
        .iter()
        .filter_map(|event| NaiveDate::parse_from_str(event_date(event), "%Y-%m-%d").ok())
        .max();
    let mut groups = Vec::new();
    for indices in groups_by_index {
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
    let events = storage.list_intent_events(Some(project.trim()), since)?;
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
    fn tiny_member_cannot_create_a_lesson_group() {
        let groups = super::group_events(
            vec![
                event("aaaaaaaa-1", 1, "no thats you", "2026-08-23T00:00:00Z"),
                event(
                    "bbbbbbbb-2",
                    2,
                    "no thats not good, we need something that is repeatable",
                    "2026-08-24T00:00:00Z",
                ),
            ],
            2,
        );

        assert!(groups.is_empty(), "tiny bridge produced {groups:?}");
    }

    #[test]
    fn grouping_does_not_chain_through_a_single_link_bridge() {
        let groups = super::group_events(
            vec![
                event("aaaaaaaa-1", 1, "alpha beta gamma", "2026-08-01T00:00:00Z"),
                event(
                    "bbbbbbbb-2",
                    2,
                    "alpha beta gamma delta epsilon zeta",
                    "2026-08-02T00:00:00Z",
                ),
                event(
                    "cccccccc-3",
                    3,
                    "delta epsilon zeta",
                    "2026-08-03T00:00:00Z",
                ),
            ],
            2,
        );

        assert_eq!(groups.len(), 1, "unexpected groups: {groups:?}");
        assert_eq!(groups[0].events, 2, "bridge merged all members");
    }

    #[test]
    fn emitted_group_keeps_a_two_token_common_core() {
        let groups = super::group_events(
            vec![
                event("aaaaaaaa-1", 1, "alpha beta gamma", "2026-08-01T00:00:00Z"),
                event("bbbbbbbb-2", 2, "alpha beta delta", "2026-08-02T00:00:00Z"),
                event("cccccccc-3", 3, "alpha gamma delta", "2026-08-03T00:00:00Z"),
            ],
            2,
        );

        assert_eq!(groups.len(), 1, "unexpected groups: {groups:?}");
        assert_eq!(groups[0].events, 2, "one-token group core was accepted");
    }

    #[test]
    fn exact_project_scope_does_not_include_hyphenated_siblings() {
        let storage = crate::storage::Storage::open_memory().unwrap();
        let mut exact = event(
            "aaaaaaaa-1",
            1,
            "never bypass verification checks",
            "2026-08-01T00:00:00Z",
        );
        exact.project = "anukriti".into();
        let mut sibling = event(
            "bbbbbbbb-2",
            2,
            "bypass verification checks",
            "2026-08-02T00:00:00Z",
        );
        sibling.project = "Anukriti-Campaigns".into();
        storage.insert_intent_events(&[exact, sibling]).unwrap();

        assert_eq!(
            super::handle(&storage, "anukriti", None, 2, true).unwrap(),
            "[]"
        );
    }

    #[test]
    fn three_session_paraphrases_emit_one_shortest_candidate() {
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
                    2,
                    "do not bypass verification checks ever",
                    "2026-08-02T00:00:00Z",
                ),
                event(
                    "cccccccc-3",
                    3,
                    "bypass verification checks",
                    "2026-08-03T00:00:00Z",
                ),
            ],
            2,
        );

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].quote, "bypass verification checks");
        assert_eq!(groups[0].events, 3);
        assert_eq!(groups[0].distinct_sessions, 3);
        assert!(super::format_text(&groups)
            .contains("- bypass verification checks (seen 3× across 3 sessions, last 2026-08-03;"));
    }
}
