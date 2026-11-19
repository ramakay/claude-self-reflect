//! How fast the daemon's ratification pass may spend the user's plan.
//!
//! Each ratified conversation is one `claude -p` call on the user's own account. Unpaced, the
//! loop took the next unratified conversation every 20 s, so a fresh install worked through its
//! whole imported history at once: 36 Haiku calls in the first 14 minutes on a 40-conversation
//! index (clean Studio install, 2026-11-19), before the user had asked anything. Now:
//!
//! - at most [`PER_HOUR`] calls in any rolling hour (`CSR_RATIFICATION_PER_HOUR`), counted from
//!   the `narrative_usage` rows every call writes, failed calls included, so a restart or a
//!   second daemon cannot reset it;
//! - only conversations active in the last [`RECENT_DAYS`] days (`CSR_RATIFICATION_DAYS`, 0 for
//!   all): older history is not ratified in the background. `csr-engine ratify` still takes any.
//!
//! `CSR_NO_RATIFICATION=1` and `CSR_NO_AI_NARRATIVES=1` still stop the pass before any of this.

use anyhow::Result;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::OptionalExtension;

use crate::storage::Storage;

/// Calls allowed in any rolling hour by default.
pub const PER_HOUR: usize = 4;
/// Conversations last active longer ago than this are left alone by default.
pub const RECENT_DAYS: i64 = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pace {
    pub per_hour: usize,
    /// 0: every conversation, however old.
    pub recent_days: i64,
}

impl Pace {
    pub fn from_env() -> Self {
        Self::from_values(
            std::env::var("CSR_RATIFICATION_PER_HOUR").ok().as_deref(),
            std::env::var("CSR_RATIFICATION_DAYS").ok().as_deref(),
        )
    }
    /// The pace the two settings ask for; a missing or unreadable one keeps its default.
    pub fn from_values(per_hour: Option<&str>, days: Option<&str>) -> Self {
        Self {
            per_hour: per_hour
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(PER_HOUR),
            recent_days: days
                .and_then(|v| v.trim().parse().ok())
                .filter(|d: &i64| *d >= 0)
                .unwrap_or(RECENT_DAYS),
        }
    }
}

/// `narrative_usage.ts` is written as `%Y-%m-%dT%H:%M:%fZ`, so the same shape compares as text.
fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Ratification calls recorded since `since`.
fn calls_since(storage: &Storage, since: DateTime<Utc>) -> Result<usize> {
    storage.with_connection(|conn| {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM narrative_usage WHERE call_site = 'ratification' AND ts >= ?1",
            [stamp(since)],
            |row| row.get(0),
        )?;
        Ok(n.max(0) as usize)
    })
}

/// When the conversation's newest chunk was written, when its timestamps can be read.
fn last_active(storage: &Storage, conv_id: &str) -> Result<Option<DateTime<Utc>>> {
    let newest: Option<String> = storage.with_connection(|conn| {
        Ok(conn
            .query_row(
                "SELECT MAX(timestamp) FROM chunks WHERE conversation_id = ?1",
                [conv_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten())
    })?;
    Ok(newest
        .and_then(|ts| DateTime::parse_from_rfc3339(&ts).ok())
        .map(|ts| ts.with_timezone(&Utc)))
}

/// Whether one more call may go now: the hour's budget has room, and the conversation was active
/// within the window. A conversation whose time cannot be read counts as recent.
pub fn may_call(
    pace: Pace,
    used_this_hour: usize,
    last_active: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    let recent = pace.recent_days == 0
        || last_active.is_none_or(|at| at >= now - Duration::days(pace.recent_days));
    used_this_hour < pace.per_hour && recent
}

/// The conversation the daemon ratifies next, or none while the hour's budget is spent or the
/// newest unratified conversation is older than the window. Candidates come newest first, so an
/// old one at the head means every other one is older still.
pub fn next_due_at(
    storage: &Storage,
    pace: Pace,
    now: DateTime<Utc>,
) -> Result<Vec<(String, String)>> {
    let used = calls_since(storage, now - Duration::hours(1))?;
    if used >= pace.per_hour {
        return Ok(vec![]);
    }
    let mut due = storage.get_unenriched_conversations("ratification", 1)?;
    if let Some((conv_id, _)) = due.first() {
        if !may_call(pace, used, last_active(storage, conv_id)?, now) {
            due.clear();
        }
    }
    Ok(due)
}

/// [`next_due_at`] now, at the pace the environment sets.
pub fn next_due(storage: &Storage) -> Result<Vec<(String, String)>> {
    next_due_at(storage, Pace::from_env(), Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::NarrativeUsageRow;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn the_settings_read_numbers_and_keep_the_defaults_otherwise() {
        let default = Pace {
            per_hour: PER_HOUR,
            recent_days: RECENT_DAYS,
        };
        assert_eq!(Pace::from_values(None, None), default);
        assert_eq!(Pace::from_values(Some("x"), Some("-3")), default);
        assert_eq!(
            Pace::from_values(Some(" 12 "), Some("0")),
            Pace {
                per_hour: 12,
                recent_days: 0
            }
        );
        assert_eq!(Pace::from_values(Some("0"), None).per_hour, 0);
    }

    #[test]
    fn a_call_waits_for_room_in_the_hour_and_old_work_is_left_alone() {
        let now = at("2026-11-19T18:00:00Z");
        let pace = Pace::from_values(None, None);
        let yesterday = Some(at("2026-11-18T18:00:00Z"));
        assert!(may_call(pace, 0, yesterday, now));
        assert!(may_call(pace, PER_HOUR - 1, yesterday, now));
        assert!(!may_call(pace, PER_HOUR, yesterday, now));
        // Eight days old: outside the week.
        assert!(!may_call(pace, 0, Some(at("2026-11-11T17:59:00Z")), now));
        assert!(may_call(pace, 0, Some(at("2026-11-12T18:00:00Z")), now));
        // A time that cannot be read is not a reason to skip it.
        assert!(may_call(pace, 0, None, now));
        let all = Pace {
            recent_days: 0,
            ..pace
        };
        assert!(may_call(all, 0, Some(at("2020-01-01T00:00:00Z")), now));
        let none = Pace {
            per_hour: 0,
            ..pace
        };
        assert!(!may_call(none, 0, yesterday, now));
    }

    fn call(storage: &Storage) {
        storage
            .record_narrative_usage(&NarrativeUsageRow {
                call_site: "ratification".into(),
                model: "claude-haiku-5-5".into(),
                input_tokens: 2,
                output_tokens: 210,
                cache_read_tokens: 3144,
                cache_creation_tokens: 2276,
                duration_ms: 0,
                success: true,
            })
            .unwrap();
    }

    /// An imported, unratified conversation whose newest chunk was written at `at`.
    fn imported(storage: &Storage, id: &str, at: DateTime<Utc>) {
        let chunk = crate::import::ConversationChunk {
            id: format!("{id}-0"),
            conversation_id: id.into(),
            project_name: "project".into(),
            timestamp: stamp(at),
            content: "work".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::User,
            seq: 0,
            is_sidechain: false,
        };
        storage.insert_chunk(&chunk, &[0.1; 4]).unwrap();
        storage
            .upsert_import_state_explicit(&format!("/p/{id}.jsonl"), id, 1, "x")
            .unwrap();
    }

    /// The burst a clean install made: forty conversations waiting, the loop asking every 20 s.
    /// Paced, the first hour makes `PER_HOUR` calls and then nothing until an hour has passed,
    /// and a conversation older than the window is never handed out.
    #[test]
    fn a_fresh_install_with_a_backlog_makes_at_most_the_hours_budget_of_calls() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("csr-engine.db")).unwrap();
        let pace = Pace::from_values(None, None);
        let now = Utc::now();
        for n in 0..40 {
            imported(
                &storage,
                &format!("conv-{n:02}"),
                now - Duration::minutes(n),
            );
        }
        // Other callers' rows do not spend ratification's budget.
        storage
            .with_connection(|c| {
                c.execute(
                    "INSERT INTO narrative_usage (call_site, model) VALUES ('briefing', 'haiku')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        // 45 loop passes, one every 20 s: fifteen minutes of the old burst. Each conversation the
        // pass hands out is called on (and ratified), as the daemon loop does.
        let mut made = vec![];
        for _ in 0..45 {
            for (conv, _) in next_due_at(&storage, pace, now).unwrap() {
                call(&storage);
                storage
                    .mark_enrichment_completed(&conv, "ratification", &conv)
                    .unwrap();
                made.push(conv);
            }
        }
        assert_eq!(made, ["conv-00", "conv-01", "conv-02", "conv-03"]);
        assert_eq!(made.len(), PER_HOUR);
        // An hour on, the calls have aged out of the window and the next one is due.
        let later = now + Duration::minutes(61);
        assert_eq!(
            calls_since(&storage, later - Duration::hours(1)).unwrap(),
            0
        );
        assert_eq!(next_due_at(&storage, pace, later).unwrap()[0].0, "conv-04");
    }

    #[test]
    fn a_conversation_older_than_the_window_waits_for_a_manual_ratify() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("csr-engine.db")).unwrap();
        let now = Utc::now();
        imported(&storage, "old", now - Duration::days(8));
        let pace = Pace::from_values(None, None);
        assert!(next_due_at(&storage, pace, now).unwrap().is_empty());
        let all = Pace {
            recent_days: 0,
            ..pace
        };
        assert_eq!(next_due_at(&storage, all, now).unwrap()[0].0, "old");
        let none = Pace { per_hour: 0, ..all };
        assert!(next_due_at(&storage, none, now).unwrap().is_empty());
    }
}
