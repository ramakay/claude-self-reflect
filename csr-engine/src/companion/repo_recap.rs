//! Read-only prior-session recall, using the same evidence composer as SessionStart.
use crate::{
    hooks::{
        recap::{self, RecapFeeds},
        session_start,
    },
    storage::Storage,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RepoRecap {
    pub session: String,
    pub at: f64,
    pub age: String,
    pub text: String,
}

pub(super) fn age_label(seconds: f64) -> String {
    if seconds < 60. {
        "just now".into()
    } else if seconds < 3600. {
        format!("{}m ago", (seconds / 60.).floor() as u64)
    } else if seconds < 86400. {
        format!("{}h ago", (seconds / 3600.).floor() as u64)
    } else {
        format!("{}d ago", (seconds / 86400.).floor() as u64)
    }
}

pub(super) fn load(storage: &Storage, project: &str, sid: &str, now: f64) -> Option<RepoRecap> {
    let registry = registry_prior(storage, project, sid, now);
    for (episode, _) in session_start::recent_project_episodes(storage, project, 50) {
        if episode.session_id == sid {
            continue;
        }
        let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(&episode.timestamp) else {
            continue;
        };
        let at = timestamp.timestamp() as f64;
        if at > now {
            continue;
        }
        if registry
            .as_ref()
            .is_some_and(|prior| prior.session != episode.session_id && prior.at > at)
        {
            return registry;
        }
        let age = age_label((now - at).max(0.));
        let (settled, still_open) = storage
            .recap_ledger_feeds(project, &episode.session_id)
            .unwrap_or_default();
        let feeds = RecapFeeds {
            settled,
            still_open,
            retired_while_away: vec![],
            corrections: vec![],
            open_proposals: storage.recap_open_proposals(project).unwrap_or(0),
            top_dream: None,
        };
        // Eligibility and recap evidence are separate: missing narrative evidence must
        // neither erase the prior session nor substitute an older session's memory.
        let text = recap::compose_recap(&episode, &feeds, &age)
            .unwrap_or_else(|| "No recap was saved for this session.".into());
        let prefix = format!("recap [{age}]: ");
        let text = text
            .strip_prefix(&prefix)
            .unwrap_or(&text)
            .replace(crate::extraction::provenance::RECAP_SENTINEL, "");
        let text = text
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        return Some(RepoRecap {
            session: episode.session_id,
            at,
            age,
            text,
        });
    }
    registry
}

fn registry_prior(storage: &Storage, project: &str, sid: &str, now: f64) -> Option<RepoRecap> {
    let rows = storage.with_connection(|con| {
        let mut statement = con.prepare("SELECT session_id,last_ts FROM session_registry WHERE project=?1 AND session_id!=?2 AND last_ts IS NOT NULL ORDER BY julianday(last_ts) DESC,session_id")?;
        let rows = statement.query_map(rusqlite::params![project,sid], |row| {
            Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }).ok()?;
    let candidates = rows.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
    let headless = storage
        .headless_conversation_ids(&candidates)
        .unwrap_or_default();
    rows.into_iter()
        .filter(|(id, _)| !headless.contains(id))
        .find_map(|(session, timestamp)| {
            let at = chrono::DateTime::parse_from_rfc3339(&timestamp)
                .map(|at| at.timestamp() as f64)
                .or_else(|_| {
                    chrono::NaiveDateTime::parse_from_str(&timestamp, "%Y-%m-%d %H:%M:%S%.f")
                        .map(|at| at.and_utc().timestamp() as f64)
                })
                .ok()?;
            if at > now {
                return None;
            }
            Some(RepoRecap {
                session,
                at,
                age: age_label((now - at).max(0.)),
                text: "No recap was saved for this session.".into(),
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn seed(storage: &Storage, sid: &str, project: &str, at: &str) {
        let episode = json!({"schema":"v2", "session_id":sid,"project":project,"timestamp":at,
            "request":"Repair the audio player","investigated":[],"completed":"", "next_steps":"Run the device tests",
            "blockers":null,"outcome":"done","error_signatures":[],"tools_used":[],"files_modified":[],
            "message_count":8,"duration_minutes":12,"recap_evidence_version":1,
            "anchor_request":"Repair the audio player","completions":[{"claim":"Fix pause handling","receipt":"abcdef1"}]});
        storage
            .insert_reflection(
                sid,
                &episode.to_string(),
                &["session_episode".into(), format!("project_{project}")],
                &[],
            )
            .unwrap();
        storage
            .with_connection(|con| {
                con.execute(
                    "UPDATE reflections SET timestamp=?1 WHERE id=?2",
                    rusqlite::params![at, sid],
                )?;
                Ok(())
            })
            .unwrap();
    }
    #[test]
    fn latest_prior_session_uses_existing_composer_and_preserves_receipts() {
        let storage = Storage::open_memory().unwrap();
        seed(&storage, "older", "anukriti", "2026-10-19T09:00:00Z");
        seed(&storage, "latest", "anukriti", "2026-10-20T09:00:00Z");
        seed(&storage, "current", "anukriti", "2026-10-20T10:00:00Z");
        seed(&storage, "unrelated", "elsewhere", "2026-10-20T10:30:00Z");
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-20T11:00:00Z")
            .unwrap()
            .timestamp() as f64;
        storage.with_connection(|con| {
            con.execute("INSERT INTO session_registry(session_id,project,last_ts) VALUES('latest','anukriti','2026-10-20T09:05:00Z')", [])?;
            con.execute("INSERT INTO chunks(id,conversation_id,project_name,timestamp,content,message_count) VALUES('settled','latest','anukriti','2026-10-20T09:00:00Z','fixture',1)", [])?;
            con.execute("INSERT INTO resolution_ledger(chunk_id,status,evidence,claim,source,created_at) VALUES('settled','resolved','commit fedcba9','Use a single audio owner','user_confirmed','2026-10-20T09:30:00Z')", [])?;
            Ok(())
        }).unwrap();
        let got = load(&storage, "anukriti", "current", now).unwrap();
        assert_eq!(got.session, "latest");
        assert_eq!(got.age, "2h ago");
        assert!(got.text.contains("Repair the audio player"), "{}", got.text);
        assert!(got.text.contains("abcdef1"), "{}", got.text);
        assert!(
            got.text.contains("Next: Run the device tests"),
            "{}",
            got.text
        );
        assert!(
            got.text.contains("Settled: Use a single audio owner") && got.text.contains("fedcba9"),
            "{}",
            got.text
        );
        assert!(!got.text.contains("[[CSR:RECAP]]"));
        assert!(load(&storage, "new-project", "new-session", now).is_none());
    }
    #[test]
    fn latest_eligible_session_with_no_recap_never_substitutes_older_memory() {
        let storage = Storage::open_memory().unwrap();
        seed(&storage, "older", "anukriti", "2026-10-19T09:00:00Z");
        seed(&storage, "latest", "anukriti", "2026-10-20T09:00:00Z");
        let (content, tags, _) = storage.get_reflection_by_id("latest").unwrap().unwrap();
        let mut episode: serde_json::Value = serde_json::from_str(&content).unwrap();
        // The episode has a substantive request and remains eligible under SessionStart,
        // but measured recap evidence explicitly has no clean anchor or completion.
        episode["anchor_request"] = serde_json::Value::Null;
        episode["completions"] = json!([]);
        storage
            .insert_reflection("latest", &episode.to_string(), &tags, &[])
            .unwrap();
        storage
            .with_connection(|con| {
                con.execute(
                    "UPDATE reflections SET timestamp='2026-10-20T09:00:00Z' WHERE id='latest'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-20T11:00:00Z")
            .unwrap()
            .timestamp() as f64;
        let got = load(&storage, "anukriti", "brand-new", now)
            .expect("prior-session metadata must suppress the new-repo pitch");
        assert_eq!(got.session, "latest");
        assert_eq!(got.at, now - 7200.);
        assert_eq!(got.age, "2h ago");
        assert_eq!(got.text, "No recap was saved for this session.");
        assert!(
            !got.text.contains("abcdef1"),
            "must not show older completion receipts"
        );
    }
    #[test]
    fn registry_only_history_is_scoped_and_skips_current_future_and_headless() {
        let storage = Storage::open_memory().unwrap();
        storage.with_connection(|con| {
            for (sid,project,at) in [
                ("older","anukriti","2026-10-19T09:00:00Z"),
                ("latest","anukriti","2026-10-20 09:00:00"),
                ("current","anukriti","2026-10-20T10:00:00Z"),
                ("unrelated","elsewhere","2026-10-20T10:30:00Z"),
                ("headless","anukriti","2026-10-20T10:45:00Z"),
                ("future","anukriti","2026-10-21T09:00:00Z"),
                ("broken","anukriti","not-a-timestamp"),
                ("only-current","no-history","2026-10-20T09:00:00Z"),
            ] {
                con.execute("INSERT INTO session_registry(session_id,project,last_ts,prompt_count) VALUES(?1,?2,?3,1)",rusqlite::params![sid,project,at])?;
            }
            con.execute("INSERT INTO conversation_entrypoint(conversation_id,entrypoint) VALUES('headless','sdk-cli')",[])?;
            Ok(())
        }).unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-20T11:00:00Z")
            .unwrap()
            .timestamp() as f64;
        seed(&storage, "older", "anukriti", "2026-10-19T09:00:00Z");
        let got = load(&storage, "anukriti", "current", now)
            .expect("newer registry history must not substitute an older recap");
        assert_eq!(got.session, "latest");
        assert_eq!(got.age, "2h ago");
        assert_eq!(got.text, "No recap was saved for this session.");
        assert!(load(&storage, "no-history", "only-current", now).is_none());
        assert!(load(&storage, "new-project", "brand-new", now).is_none());
    }
}
