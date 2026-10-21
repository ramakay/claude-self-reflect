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
    #[serde(default)]
    pub history: Vec<RepoSessionSummary>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RepoSessionSummary {
    pub session: String,
    pub at: f64,
    pub age: String,
    pub intent: String,
    pub outcome: String,
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

const UNAVAILABLE: &str = "No recap was saved for this session.";

struct Prior {
    session: String,
    at: f64,
    prompt: Option<String>,
}

pub(super) fn load(storage: &Storage, project: &str, sid: &str, now: f64) -> Option<RepoRecap> {
    let episodes = session_start::recent_project_episodes(storage, project, 50);
    let mut priors = registry_priors(storage, project, sid, now);
    for (episode, _) in &episodes {
        if episode.session_id == sid || episode.project != project {
            continue;
        }
        // An invalid/future registry identity cannot be resurrected by an older
        // episode timestamp. Registry project identity is authoritative too.
        let registry_identity = storage
            .with_connection(|con| {
                use rusqlite::OptionalExtension;
                Ok(con
                    .query_row(
                        "SELECT project,last_ts FROM session_registry WHERE session_id=?1",
                        [&episode.session_id],
                        |row| {
                            Ok((
                                row.get::<_, Option<String>>(0)?,
                                row.get::<_, Option<String>>(1)?,
                            ))
                        },
                    )
                    .optional()?)
            })
            .ok()
            .flatten();
        if registry_identity.is_some_and(|(p, ts)| {
            p.as_deref() != Some(project)
                || ts
                    .as_deref()
                    .is_some_and(|ts| parse_timestamp(ts).is_none_or(|at| at > now))
        }) {
            continue;
        }
        let Some(at) = parse_timestamp(&episode.timestamp).filter(|at| *at <= now) else {
            continue;
        };
        if !priors
            .iter()
            .any(|prior| prior.session == episode.session_id)
        {
            priors.push(Prior {
                session: episode.session_id.clone(),
                at,
                prompt: None,
            });
        }
    }
    priors.sort_by(|a, b| {
        b.at.total_cmp(&a.at)
            .then_with(|| a.session.cmp(&b.session))
    });
    // Bound cold-path ledger hydration after human/identity/time filtering.
    priors.truncate(50);
    let mut entries = priors.into_iter().map(|prior| {
        // Match by identity, never by the position of an episode in the recent list.
        let episode = episodes
            .iter()
            .find(|(ep, _)| {
                ep.session_id == prior.session
                    && ep.project == project
                    && parse_timestamp(&ep.timestamp).is_some_and(|at| at <= now)
            })
            .map(|(ep, _)| ep);
        session_recap(storage, project, prior, episode, now)
    });
    let (primary, text) = entries.next()?;
    // A session CSR knows nothing about is noise in the list, not history.
    let history = entries
        .map(|(summary, _)| summary)
        .filter(|summary| summary.intent != UNAVAILABLE)
        .collect();
    Some(RepoRecap {
        session: primary.session,
        at: primary.at,
        age: primary.age,
        text,
        history,
    })
}

fn parse_timestamp(timestamp: &str) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map(|at| at.timestamp() as f64)
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%d %H:%M:%S%.f")
                .map(|at| at.and_utc().timestamp() as f64)
        })
        .ok()
}

fn registry_priors(storage: &Storage, project: &str, sid: &str, now: f64) -> Vec<Prior> {
    let rows = storage.with_connection(|con| {
        let mut statement = con.prepare("SELECT session_id,last_ts,first_prompt FROM session_registry WHERE project=?1 AND session_id!=?2 AND last_ts IS NOT NULL ORDER BY julianday(last_ts) DESC,session_id")?;
        let rows = statement.query_map(rusqlite::params![project,sid], |row| {
            Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?))
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }).unwrap_or_default();
    let ids = rows.iter().map(|(id, _, _)| id.clone()).collect::<Vec<_>>();
    let headless = storage.headless_conversation_ids(&ids).unwrap_or_default();
    rows.into_iter()
        .filter(|(id, _, _)| !headless.contains(id))
        .filter_map(|(session, ts, prompt)| {
            let at = parse_timestamp(&ts).filter(|at| *at <= now)?;
            Some(Prior {
                session,
                at,
                prompt,
            })
        })
        .collect()
}

fn clean_prompt(prompt: &str) -> Option<String> {
    if crate::import::contamination::has_csr_text(prompt) {
        return None;
    }
    crate::extraction::provenance::extractable(prompt).and_then(|p| recap::clean_intent(&p))
}

fn fact_intent(fact: &recap::SettledFact) -> Option<String> {
    let claim = fact.claim.split_whitespace().collect::<Vec<_>>().join(" ");
    (!fact.receipt.trim().is_empty()
        && claim.chars().any(char::is_alphanumeric)
        && !crate::import::contamination::has_csr_text(&claim))
    .then_some(claim)
}

fn clean_next(text: &str) -> Option<String> {
    let mut text = text.trim_start();
    let lower = text.to_ascii_lowercase();
    for prefix in ["next steps:", "next step:", "next:"] {
        if lower.starts_with(prefix) {
            text = text[prefix.len()..].trim_start();
            break;
        }
    }
    clean_prompt(text).filter(|text| {
        !matches!(
            text.trim()
                .trim_matches(|c: char| c.is_ascii_punctuation())
                .to_ascii_lowercase()
                .as_str(),
            "none" | "nothing" | "n/a" | "no next step" | "no next steps"
        )
    })
}

fn empty_episode(project: &str, prior: &Prior) -> crate::hooks::stop::Episode {
    // Identity-only shell: no request, conclusion, or outcome is invented.
    serde_json::from_value(serde_json::json!({
        "schema":"session_episode/v2", "session_id":prior.session,"project":project,
        "timestamp":"", "request":"", "investigated":[],"completed":"",
        "next_steps":null,"blockers":null,"outcome":"unknown","error_signatures":[],
        "tools_used":[],"files_modified":[],"message_count":0,"duration_minutes":0,
        "recap_evidence_version":1
    }))
    .expect("identity-only episode schema")
}

const SESSION_OPEN_SQL: &str = "SELECT claim,evidence,status,source FROM (
    SELECT COALESCE(r.claim,'') AS claim,r.evidence AS evidence,r.status AS status,
           r.source AS source,r.id AS rid
    FROM chunks c INDEXED BY idx_chunks_conversation
    JOIN resolution_ledger r ON r.chunk_id=c.id
    WHERE c.conversation_id=?2 AND c.project_name=?1
      AND r.status IN ('still_open','regressed') AND r.source='user_confirmed'
      AND NOT EXISTS (SELECT 1 FROM resolution_ledger latest WHERE latest.chunk_id=r.chunk_id
          AND latest.source='user_confirmed' AND latest.id>r.id)
    UNION
    SELECT COALESCE(r.claim,''),r.evidence,r.status,r.source,r.id
    FROM chunk_provenance p INDEXED BY idx_chunk_provenance_source_conv
    JOIN chunks c ON c.id=p.chunk_id
    JOIN resolution_ledger r ON r.chunk_id=c.id
    WHERE p.source_conv_id=?2 AND c.project_name=?1
      AND r.status IN ('still_open','regressed') AND r.source='user_confirmed'
      AND NOT EXISTS (SELECT 1 FROM resolution_ledger latest WHERE latest.chunk_id=r.chunk_id
          AND latest.source='user_confirmed' AND latest.id>r.id)
) ORDER BY rid DESC LIMIT 5";

fn session_open_feeds(storage: &Storage, project: &str, sid: &str) -> Vec<recap::SettledFact> {
    // SessionStart's open feed is project-wide. Historical cards must instead
    // credit only this transcript and its recorded sidechains.
    storage
        .with_connection(|con| {
            let mut statement = con.prepare(SESSION_OPEN_SQL)?;
            let rows = statement
                .query_map(rusqlite::params![project, sid], |row| {
                    Ok(recap::SettledFact {
                        claim: row.get(0)?,
                        receipt: row.get(1)?,
                        status: row.get(2)?,
                        source: row.get(3)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap_or_default()
}

fn session_recap(
    storage: &Storage,
    project: &str,
    prior: Prior,
    episode: Option<&crate::hooks::stop::Episode>,
    now: f64,
) -> (RepoSessionSummary, String) {
    let age = age_label((now - prior.at).max(0.));
    let mut ep = episode
        .cloned()
        .unwrap_or_else(|| empty_episode(project, &prior));
    let (settled, project_open) = storage
        .recap_ledger_feeds(project, &prior.session)
        .unwrap_or_default();
    // Empty is conclusive; a nonempty capped project feed cannot tell whether
    // this session has additional facts outside those five, so always scope it.
    let still_open = if project_open.is_empty() {
        vec![]
    } else {
        session_open_feeds(storage, project, &prior.session)
    };
    let feeds = RecapFeeds {
        settled,
        still_open,
        retired_while_away: vec![],
        corrections: vec![],
        open_proposals: 0,
        top_dream: None,
    };
    // A legacy request is the raw FIRST prompt, not an evidenced session intent.
    // Only a measured anchor may lead the recap while other evidence exists.
    let anchor = ep
        .recap_evidence_version
        .and_then(|_| recap::recap_intent(&ep));
    let displayed = recap::displayed_receipts(&ep.completions);
    let completed = !displayed.is_empty();
    let settled_intent = feeds.settled.iter().find(|f| fact_intent(f).is_some());
    let open_intent = feeds.still_open.iter().find(|f| fact_intent(f).is_some());
    let done = completed || settled_intent.is_some();
    let next = ep.next_steps.as_deref().and_then(clean_next);
    let blocker = ep.blockers.as_deref().and_then(clean_prompt);
    let open = open_intent.is_some()
        || next.is_some()
        || blocker.is_some()
        || ep
            .todos
            .iter()
            .any(|todo| todo.status != "completed" && clean_next(&todo.content).is_some());
    let fallback = (!done && !open)
        .then(|| {
            anchor
                .clone()
                .or_else(|| prior.prompt.as_deref().and_then(clean_prompt))
                .or_else(|| episode.and_then(|ep| clean_prompt(&ep.request)))
        })
        .flatten()
        .map(|p| format!("you asked: {p}"));
    let intent = fallback
        .clone()
        .or_else(|| anchor.clone())
        .or_else(|| displayed.first().map(|(_, text)| text.clone()))
        .or_else(|| settled_intent.and_then(fact_intent))
        .or_else(|| open_intent.and_then(fact_intent))
        .or_else(|| next.clone())
        .or_else(|| blocker.clone())
        .unwrap_or_else(|| UNAVAILABLE.into());
    ep.recap_evidence_version = Some(1);
    ep.anchor_request = fallback.or(anchor);
    // The composer deliberately requires an intent/completion before its ledger
    // clauses. An internal scaffold permits ledger-only sessions without
    // misrepresenting a resolved claim as a task completion. It never appears
    // in the returned text; the composer still gates and budgets every fact.
    let scaffold = ep.anchor_request.is_none() && !completed && (done || open);
    if scaffold {
        ep.anchor_request = Some("Session evidence".into());
    }
    let text = recap::compose_recap(&ep, &feeds, &age)
        .map(|text| {
            let prefix = format!("recap [{age}]: ");
            let body = text.strip_prefix(&prefix).unwrap_or(&text);
            let body = if scaffold {
                body.strip_prefix("Session evidence.").unwrap_or(body)
            } else {
                body
            };
            body.replace(crate::extraction::provenance::RECAP_SENTINEL, "")
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| UNAVAILABLE.into());
    (
        RepoSessionSummary {
            session: prior.session,
            at: prior.at,
            age,
            intent,
            outcome: if done {
                "done"
            } else if open {
                "open"
            } else {
                "unknown"
            }
            .into(),
        },
        text,
    )
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
        assert_eq!(got.age, "1h ago");
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
    fn latest_session_with_only_next_steps_never_substitutes_older_memory() {
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
        assert!(
            got.text.contains("Next: Run the device tests"),
            "{}",
            got.text
        );
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
    #[test]
    fn legacy_prompt_is_fallback_only_and_always_labeled() {
        let storage = Storage::open_memory().unwrap();
        seed(&storage, "legacy", "repo", "2026-10-20T09:00:00Z");
        let (content, tags, _) = storage.get_reflection_by_id("legacy").unwrap().unwrap();
        let mut ep: serde_json::Value = serde_json::from_str(&content).unwrap();
        ep["recap_evidence_version"] = serde_json::Value::Null;
        ep["anchor_request"] = serde_json::Value::Null;
        ep["next_steps"] = serde_json::Value::Null;
        storage
            .insert_reflection("legacy", &ep.to_string(), &tags, &[])
            .unwrap();
        let now = parse_timestamp("2026-10-20T11:00:00Z").unwrap();
        let recap = load(&storage, "repo", "new", now).unwrap();
        assert!(recap.text.contains("abcdef1"));
        assert!(
            !recap.text.contains("Repair the audio player"),
            "{}",
            recap.text
        );
        ep["completions"] = serde_json::json!([]);
        storage
            .insert_reflection("legacy", &ep.to_string(), &tags, &[])
            .unwrap();
        let recap = load(&storage, "repo", "new", now).unwrap();
        assert!(
            recap.text.starts_with("you asked: Repair the audio player"),
            "{}",
            recap.text
        );
    }

    #[test]
    fn registry_latest_uses_its_own_ledger_and_history_is_ordered() {
        let storage = Storage::open_memory().unwrap();
        seed(&storage, "older", "repo", "2026-10-19T09:00:00Z");
        seed(&storage, "middle", "repo", "2026-10-20T08:00:00Z");
        storage.with_connection(|con| {
            con.execute("INSERT INTO session_registry(session_id,project,last_ts,first_prompt) VALUES('latest','repo','2026-10-20T10:00:00Z','This raw prompt must be suppressed')", [])?;
            con.execute("INSERT INTO chunks(id,conversation_id,project_name,timestamp,content,message_count) VALUES('ledger','latest','repo','2026-10-20T10:00:00Z','fixture',1)", [])?;
            con.execute("INSERT INTO resolution_ledger(chunk_id,status,evidence,claim,source,created_at) VALUES('ledger','resolved','commit fedcba9','Use a single audio owner','user_confirmed','2026-10-20T10:00:00Z')", [])?;
            Ok(())
        }).unwrap();
        let now = parse_timestamp("2026-10-20T11:00:00Z").unwrap();
        let recap = load(&storage, "repo", "new", now).unwrap();
        assert_eq!(recap.session, "latest");
        assert!(
            recap
                .text
                .contains("Settled: Use a single audio owner (fedcba9)"),
            "{}",
            recap.text
        );
        assert!(!recap.text.contains("raw prompt") && !recap.text.contains("abcdef1"));
        assert_eq!(
            recap
                .history
                .iter()
                .map(|r| r.session.as_str())
                .collect::<Vec<_>>(),
            vec!["middle", "older"]
        );
        assert_eq!(recap.history[0].age, "3h ago");
        assert_eq!(recap.history[0].intent, "Repair the audio player");
        assert_eq!(recap.history[0].outcome, "done");
    }

    #[test]
    fn registry_only_open_receipt_and_prompt_history_are_honest() {
        let storage = Storage::open_memory().unwrap();
        storage.with_connection(|con| {
            con.execute("INSERT INTO session_registry(session_id,project,last_ts,first_prompt) VALUES('latest','repo','2026-10-20T10:00:00Z','Old prompt'),('older','repo','2026-10-20T09:00:00Z','Check the device audio')", [])?;
            con.execute("INSERT INTO chunks(id,conversation_id,project_name,timestamp,content,message_count) VALUES('open','latest','repo','2026-10-20T10:00:00Z','fixture',1)", [])?;
            con.execute("INSERT INTO resolution_ledger(chunk_id,status,evidence,claim,source,created_at) VALUES('open','still_open','commit fedcba9','Verify pause on device','user_confirmed','2026-10-20T10:00:00Z')", [])?;
            Ok(())
        }).unwrap();
        let recap = load(
            &storage,
            "repo",
            "new",
            parse_timestamp("2026-10-20T11:00:00Z").unwrap(),
        )
        .unwrap();
        assert!(
            recap
                .text
                .contains("Now: Verify pause on device (commit fedcba9)"),
            "{}",
            recap.text
        );
        assert!(!recap.text.contains("Old prompt"));
        assert_eq!(recap.history[0].intent, "you asked: Check the device audio");
        assert_eq!(recap.history[0].outcome, "unknown");
        let legacy: RepoRecap = serde_json::from_str(
            r#"{"session":"old","at":0.0,"age":"1d ago","text":"Saved recap"}"#,
        )
        .unwrap();
        assert!(legacy.history.is_empty());
    }
    #[test]
    fn registry_identity_does_not_hydrate_future_or_invalid_episode_content() {
        for timestamp in ["2026-10-21T09:00:00Z", "invalid-timestamp"] {
            let storage = Storage::open_memory().unwrap();
            seed(&storage, "prior", "repo", timestamp);
            storage.with_connection(|con| {
                con.execute("INSERT INTO session_registry(session_id,project,last_ts,first_prompt) VALUES('prior','repo','2026-10-20T09:00:00Z','Check the device audio')", [])?;
                Ok(())
            }).unwrap();
            let recap = load(
                &storage,
                "repo",
                "new",
                parse_timestamp("2026-10-20T11:00:00Z").unwrap(),
            )
            .unwrap();
            assert_eq!(recap.session, "prior");
            assert!(
                recap.text.starts_with("you asked: Check the device audio"),
                "{}",
                recap.text
            );
            assert!(
                !recap.text.contains("abcdef1") && !recap.text.contains("Repair the audio player")
            );
        }
    }

    #[test]
    fn prefixed_no_next_evidence_keeps_labeled_prompt_and_unknown_outcome() {
        for next in [
            "Next: none",
            "Next steps: nothing",
            "NEXT STEP: n/a",
            "Next: no next steps.",
        ] {
            let storage = Storage::open_memory().unwrap();
            let prior = Prior {
                session: "prior".into(),
                at: 0.,
                prompt: Some("Check the device audio".into()),
            };
            let mut episode = empty_episode("repo", &prior);
            episode.next_steps = Some(next.into());
            let (summary, text) = session_recap(&storage, "repo", prior, Some(&episode), 3600.);
            assert!(
                text.starts_with("you asked: Check the device audio"),
                "{next}: {text}"
            );
            assert_eq!(summary.intent, "you asked: Check the device audio");
            assert_eq!(summary.outcome, "unknown");
        }
        assert_eq!(
            clean_next("Next steps: Run the device tests"),
            Some("Run the device tests".into())
        );
    }

    #[test]
    fn hydration_is_bounded_after_excluding_nonhuman_and_future_sessions() {
        let storage = Storage::open_memory().unwrap();
        storage.with_connection(|con| {
            for n in 0..65 {
                let at = chrono::DateTime::from_timestamp(1_792_492_800 - n * 60,0).unwrap().to_rfc3339();
                con.execute("INSERT INTO session_registry(session_id,project,last_ts,first_prompt) VALUES(?1,'repo',?2,'Check device audio')",rusqlite::params![format!("human-{n:02}"),at])?;
            }
            con.execute("INSERT INTO session_registry(session_id,project,last_ts) VALUES('headless','repo','2026-10-20T10:59:00Z'),('future','repo','2099-10-20T10:59:00Z')", [])?;
            con.execute("INSERT INTO conversation_entrypoint(conversation_id,entrypoint) VALUES('headless','sdk-cli')", [])?;
            Ok(())
        }).unwrap();
        let recap = load(
            &storage,
            "repo",
            "new",
            parse_timestamp("2026-10-20T11:00:00Z").unwrap(),
        )
        .unwrap();
        assert_eq!(recap.session, "human-00");
        assert_eq!(recap.history.len(), 49);
        assert_eq!(recap.history.last().unwrap().session, "human-49");
    }
    #[test]
    fn measured_anchor_without_other_evidence_is_labeled_in_primary_and_history() {
        let storage = Storage::open_memory().unwrap();
        for (sid, at) in [
            ("latest", "2026-10-20T10:00:00Z"),
            ("older", "2026-10-20T09:00:00Z"),
        ] {
            seed(&storage, sid, "repo", at);
            let (content, tags, _) = storage.get_reflection_by_id(sid).unwrap().unwrap();
            let mut episode: serde_json::Value = serde_json::from_str(&content).unwrap();
            episode["request"] = json!("A different first request");
            episode["anchor_request"] = json!(format!("Check device audio for {sid}"));
            episode["completions"] = json!([]);
            episode["next_steps"] = serde_json::Value::Null;
            storage
                .insert_reflection(sid, &episode.to_string(), &tags, &[])
                .unwrap();
            storage.with_connection(|con| {
                con.execute("UPDATE reflections SET timestamp=?1 WHERE id=?2",rusqlite::params![at,sid])?;
                con.execute("INSERT INTO session_registry(session_id,project,last_ts,first_prompt) VALUES(?1,'repo',?2,'A different registry prompt')",rusqlite::params![sid,at])?;
                Ok(())
            }).unwrap();
        }
        let recap = load(
            &storage,
            "repo",
            "new",
            parse_timestamp("2026-10-20T11:00:00Z").unwrap(),
        )
        .unwrap();
        assert!(
            recap
                .text
                .starts_with("you asked: Check device audio for latest"),
            "{}",
            recap.text
        );
        assert!(!recap.text.contains("different"));
        assert_eq!(
            recap.history[0].intent,
            "you asked: Check device audio for older"
        );
        assert_eq!(recap.history[0].outcome, "unknown");
    }
    #[test]
    fn scoped_open_union_preserves_main_sidechain_receipts_and_latest_verdicts() {
        let storage = Storage::open_memory().unwrap();
        storage.with_connection(|con| {
            for (id,session,project) in [("main","prior","repo"),("side","child","repo"),("settled","prior","repo"),("other","other","repo"),("foreign","prior","elsewhere")] {
                con.execute("INSERT INTO chunks(id,conversation_id,project_name,timestamp,content,message_count) VALUES(?1,?2,?3,'2026-10-20T09:00:00Z','fixture',1)",rusqlite::params![id,session,project])?;
                con.execute("INSERT INTO resolution_ledger(chunk_id,status,evidence,claim,source) VALUES(?1,'still_open',?2,?3,'user_confirmed')",rusqlite::params![id,format!("full original receipt for {id}"),format!("Verify {id} device playback")])?;
            }
            // A main chunk with recorded provenance qualifies in both arms but
            // must appear once; a sidechain is credited to its parent session.
            for id in ["main","side"] {
                con.execute("INSERT INTO chunk_provenance(chunk_id,author,source_conv_id) VALUES(?1,'assistant','prior')",[id])?;
            }
            con.execute("INSERT INTO resolution_ledger(chunk_id,status,evidence,claim,source) VALUES('settled','resolved','commit fedcba9','Verify settled device playback','user_confirmed')", [])?;
            con.execute("INSERT INTO resolution_ledger(chunk_id,status,evidence,claim,source) VALUES('side','resolved','agent assertion','Verify side device playback','agent')", [])?;
            Ok(())
        }).unwrap();
        let facts = session_open_feeds(&storage, "repo", "prior");
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].claim, "Verify side device playback");
        assert_eq!(facts[1].claim, "Verify main device playback");
        assert_eq!(facts[0].receipt, "full original receipt for side");
        assert_eq!(facts[1].receipt, "full original receipt for main");
    }

    #[test]
    fn scoped_open_query_uses_conversation_and_provenance_indexes() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|con| {
                let plan = con
                    .prepare(&format!("EXPLAIN QUERY PLAN {SESSION_OPEN_SQL}"))?
                    .query_map(rusqlite::params!["repo", "prior"], |row| {
                        row.get::<_, String>(3)
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?
                    .join("\n");
                assert!(plan.contains("idx_chunks_conversation"), "{plan}");
                assert!(plan.contains("idx_chunk_provenance_source_conv"), "{plan}");
                assert!(
                    !plan
                        .lines()
                        .any(|line| line == "SCAN c" || line == "SCAN p"),
                    "{plan}"
                );
                Ok(())
            })
            .unwrap();
    }
}
