//! Read-only projection for the session-led dream journal.

use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// Bound on how many sessions a single shared-identity artifact carries in
/// its `conversations` list. Prevents a hot symbol touched by hundreds of
/// sessions from forcing unbounded per-session cloning (finding 8); the
/// newest sessions by anchor timestamp are kept, oldest are dropped first.
const MAX_SHARED_ARTIFACT_SESSIONS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct StoryTodo {
    pub content: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct StoryArtifact {
    pub file: String,
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_receipt: Option<String>,
    pub conversations: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct StorySession {
    pub session_id: String,
    pub project: String,
    pub timestamp: String,
    pub request: Option<String>,
    pub outcome: Option<String>,
    pub investigated: Vec<String>,
    pub todos: Vec<StoryTodo>,
    pub narrative: Option<String>,
    pub first_prompt: Option<String>,
    pub artifacts: Vec<StoryArtifact>,
}

#[derive(Debug, Default)]
struct SessionBuilder {
    project: String,
    timestamp: String,
    sort_key: f64,
    request: Option<String>,
    outcome: Option<String>,
    episode_loaded: bool,
    investigated: Vec<String>,
    todos: Vec<StoryTodo>,
    narrative: Option<String>,
    narrative_loaded: bool,
    first_prompt: Option<String>,
    artifacts: Vec<StoryArtifact>,
}

impl SessionBuilder {
    fn touch(&mut self, project: &str, timestamp: &str, sort_key: f64) {
        if self.project.is_empty() && !project.is_empty() {
            self.project = project.to_string();
        }
        if sort_key > self.sort_key || self.timestamp.is_empty() {
            self.sort_key = sort_key;
            self.timestamp = timestamp.to_string();
        }
    }

    fn add_artifact(&mut self, artifact: StoryArtifact) {
        if !self
            .artifacts
            .iter()
            .any(|existing| existing.file == artifact.file && existing.symbol == artifact.symbol)
        {
            self.artifacts.push(artifact);
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EpisodeJson {
    session_id: String,
    project: String,
    timestamp: String,
    request: String,
    outcome: String,
    investigated: Vec<String>,
    todos: Vec<EpisodeTodoJson>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EpisodeTodoJson {
    content: String,
    status: String,
}

fn nonblank(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn reflection_tags(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

/// Load one deterministic, newest-first projection per known session.
///
/// The candidate spine is the union of registry rows, v2 episodes,
/// `session_story` reflections, episode anchors, and transcript-attributed code
/// nodes. The renderer decides which candidates have enough signal to show.
pub(crate) fn load_story_sessions(conn: &Connection) -> Result<Vec<StorySession>> {
    let mut sessions: BTreeMap<String, SessionBuilder> = BTreeMap::new();
    let mut linked_anchors = Vec::new();

    {
        let mut stmt = conn.prepare(
            "SELECT session_id, project, first_prompt,
                    COALESCE(last_ts, first_ts, ''),
                    COALESCE(julianday(last_ts), julianday(first_ts), 0.0)
             FROM session_registry",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, f64>(4)?,
            ))
        })?;
        for row in rows {
            let (session_id, project, first_prompt, timestamp, sort_key) = row?;
            if session_id.is_empty() {
                continue;
            }
            let builder = sessions.entry(session_id.clone()).or_default();
            builder.touch(&project, &timestamp, sort_key);
            builder.first_prompt = first_prompt.and_then(nonblank);
        }
    }

    {
        let mut stmt = conn.prepare(
            "SELECT content, timestamp,
                    COALESCE(julianday(json_extract(content, '$.timestamp')),
                             julianday(timestamp), 0.0)
             FROM reflections
             WHERE json_valid(content)
               AND json_extract(content, '$.schema') = 'v2'
             ORDER BY 3 DESC, rowid DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?;
        for row in rows {
            let (content, reflection_ts, sort_key) = row?;
            let episode: EpisodeJson = match serde_json::from_str::<EpisodeJson>(&content) {
                Ok(episode) if !episode.session_id.is_empty() => episode,
                _ => continue,
            };
            let timestamp = if episode.timestamp.is_empty() {
                reflection_ts
            } else {
                episode.timestamp.clone()
            };
            let builder = sessions.entry(episode.session_id).or_default();
            builder.touch(&episode.project, &timestamp, sort_key);
            if !builder.episode_loaded {
                builder.episode_loaded = true;
                builder.request = nonblank(episode.request);
                builder.outcome = nonblank(episode.outcome);
                builder.investigated = episode
                    .investigated
                    .into_iter()
                    .filter(|file| !file.trim().is_empty())
                    .collect();
                builder.todos = episode
                    .todos
                    .into_iter()
                    .filter_map(|todo| {
                        nonblank(todo.content).map(|content| StoryTodo {
                            content,
                            status: todo.status,
                        })
                    })
                    .collect();
            }
        }
    }

    {
        let mut stmt = conn.prepare(
            "SELECT content, tags, timestamp, COALESCE(julianday(timestamp), 0.0)
             FROM reflections
             WHERE tags LIKE '%session_story%'
             ORDER BY 4 DESC, rowid DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
            ))
        })?;
        for row in rows {
            let (content, raw_tags, timestamp, sort_key) = row?;
            let tags = reflection_tags(&raw_tags);
            if !tags.iter().any(|tag| tag == "session_story") {
                continue;
            }
            let Some(session_id) = tags
                .iter()
                .find_map(|tag| tag.strip_prefix("conv_"))
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            let project = tags
                .iter()
                .find_map(|tag| tag.strip_prefix("project_"))
                .unwrap_or_default();
            let builder = sessions.entry(session_id.to_string()).or_default();
            builder.touch(project, &timestamp, sort_key);
            if !builder.narrative_loaded {
                builder.narrative_loaded = true;
                builder.narrative = nonblank(content);
            }
        }
    }

    {
        let mut stmt = conn.prepare(
            "SELECT ea.session_id, ea.project, ea.file, ea.name, ea.created_at,
                    COALESCE(julianday(ea.created_at), 0.0),
                    (SELECT v.receipt_oid
                     FROM witness_verdicts v
                     JOIN witness_ledger wl ON wl.id = v.witness_id
                     WHERE v.verdict = 'superseded_by'
                       AND wl.project = ea.project
                       AND wl.file = ea.file
                       AND wl.symbol = ea.name
                     ORDER BY v.id DESC LIMIT 1)
             FROM episode_anchors ea
             ORDER BY ea.created_at DESC, ea.id DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, f64>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        for row in rows {
            let (session_id, project, file, symbol, timestamp, sort_key, receipt) = row?;
            if session_id.is_empty() {
                continue;
            }
            let builder = sessions.entry(session_id.clone()).or_default();
            builder.touch(&project, &timestamp, sort_key);
            let artifact = StoryArtifact {
                file,
                symbol: Some(symbol),
                superseded_receipt: receipt.and_then(nonblank),
                conversations: Vec::new(),
            };
            linked_anchors.push((session_id, project, timestamp, sort_key, artifact));
        }
    }

    let identities = linked_anchors
        .iter()
        .filter_map(|(_, project, _, _, artifact)| {
            artifact
                .symbol
                .as_ref()
                .map(|symbol| (project.clone(), artifact.file.clone(), symbol.clone()))
        })
        .collect::<Vec<_>>();
    let transcript_conversations =
        super::witness_verdicts::conversation_ids_for_anchors(conn, &identities)?;

    // One artifact template per identity (project, file, symbol), built from
    // whichever anchor row establishes it first. Every row sharing an
    // identity carries the same receipt (looked up by project/file/symbol
    // alone, not by session), so there is nothing session-specific to
    // preserve beyond this template.
    let mut identity_templates: BTreeMap<(String, String, String), StoryArtifact> = BTreeMap::new();
    // Each linked anchor's own (session, sort_key) — used below to rank a
    // shared identity's candidate sessions by recency without an extra
    // query. Sessions known only via `transcript_conversations` have no
    // entry here and sort last (oldest), since we have no evidence they are
    // newer than the sessions that actually own an anchor for this identity.
    let mut linked_anchor_sort_keys: BTreeMap<String, f64> = BTreeMap::new();
    for (session_id, project, _, sort_key, artifact) in &linked_anchors {
        let Some(symbol) = artifact.symbol.clone() else {
            continue;
        };
        let key = (project.clone(), artifact.file.clone(), symbol);
        identity_templates
            .entry(key)
            .or_insert_with(|| artifact.clone());
        linked_anchor_sort_keys
            .entry(session_id.clone())
            .and_modify(|existing| {
                if *sort_key > *existing {
                    *existing = *sort_key;
                }
            })
            .or_insert(*sort_key);
    }

    let mut artifact_conversations: BTreeMap<(String, String, String), Vec<String>> =
        BTreeMap::new();
    for (session_id, project, _, _, artifact) in &linked_anchors {
        let Some(symbol) = artifact.symbol.as_ref() else {
            continue;
        };
        let key = (project.clone(), artifact.file.clone(), symbol.clone());
        artifact_conversations
            .entry(key)
            .or_default()
            .push(session_id.clone());
    }
    for (key, session_ids) in transcript_conversations {
        artifact_conversations
            .entry(key)
            .or_default()
            .extend(session_ids);
    }
    for session_ids in artifact_conversations.values_mut() {
        session_ids.sort();
        session_ids.dedup();
        // Select the newest candidate sessions BEFORE the attach/clone step
        // below runs, and cap the result. Without this, an identity shared
        // by hundreds of sessions would force every one of those sessions to
        // carry a full, independently cloned copy of the others' IDs —
        // quadratic attach work and cubic string cloning for a single hot
        // symbol (finding 8).
        session_ids.sort_by(|a, b| {
            let key_a = linked_anchor_sort_keys.get(a).copied().unwrap_or(f64::MIN);
            let key_b = linked_anchor_sort_keys.get(b).copied().unwrap_or(f64::MIN);
            key_b.total_cmp(&key_a).then_with(|| a.cmp(b))
        });
        session_ids.truncate(MAX_SHARED_ARTIFACT_SESSIONS);
    }

    // Attach each identity's artifact exactly once per identity — not once
    // per anchor row that established it — so a symbol shared by many
    // sessions does bounded attachment work instead of work proportional to
    // (anchor rows for the identity) x (sessions sharing the identity)
    // (finding 8). Only the anchor's OWNING session had its timestamp
    // applied above (loop over `episode_anchors` rows); sessions that merely
    // share the identity must never borrow another session's date here —
    // each session's sort position stays derived solely from its own
    // records (finding 7).
    for (key, session_ids) in &artifact_conversations {
        let Some(template) = identity_templates.get(key) else {
            continue;
        };
        let mut artifact = template.clone();
        artifact.conversations = session_ids.clone();
        for session_id in session_ids {
            let builder = sessions.entry(session_id.clone()).or_default();
            builder.add_artifact(artifact.clone());
        }
    }

    {
        let mut stmt = conn.prepare(
            "SELECT a.source_id, cn.project, cn.file,
                    COALESCE(a.observed_ts, cn.last_seen, ''),
                    COALESCE(julianday(a.observed_ts), julianday(cn.last_seen), 0.0)
             FROM code_node_attribution a
             JOIN code_nodes cn ON cn.id = a.node_id
             WHERE a.channel = 'transcript'
             ORDER BY 5 DESC, a.node_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, f64>(4)?,
            ))
        })?;
        for row in rows {
            let (session_id, project, file, timestamp, sort_key) = row?;
            if session_id.is_empty() || file.is_empty() {
                continue;
            }
            let builder = sessions.entry(session_id.clone()).or_default();
            builder.touch(&project, &timestamp, sort_key);
            builder.add_artifact(StoryArtifact {
                file,
                symbol: None,
                superseded_receipt: None,
                conversations: vec![session_id],
            });
        }
    }

    let mut rows = sessions
        .into_iter()
        .map(|(session_id, mut builder)| {
            builder
                .artifacts
                .sort_by(|a, b| (&a.file, &a.symbol).cmp(&(&b.file, &b.symbol)));
            (
                builder.sort_key,
                StorySession {
                    session_id,
                    project: builder.project,
                    timestamp: builder.timestamp,
                    request: builder.request,
                    outcome: builder.outcome,
                    investigated: builder.investigated,
                    todos: builder.todos,
                    narrative: builder.narrative,
                    first_prompt: builder.first_prompt,
                    artifacts: builder.artifacts,
                },
            )
        })
        .collect::<Vec<_>>();
    rows.sort_by(|(left_key, left), (right_key, right)| {
        right_key
            .total_cmp(left_key)
            .then_with(|| right.session_id.cmp(&left.session_id))
    });
    Ok(rows.into_iter().map(|(_, session)| session).collect())
}

#[cfg(test)]
mod tests {
    use super::load_story_sessions;
    use crate::storage::migrations;
    use rusqlite::Connection;

    fn memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        conn
    }

    #[test]
    fn story_sessions_join_latest_episode_story_and_registry_newest_first() {
        let conn = memory_db();
        conn.execute_batch(
            r#"
            INSERT INTO session_registry
                (session_id, project, first_prompt, first_ts, last_ts, prompt_count)
            VALUES
                ('older', 'proj', 'registry older', '2026-01-01T00:00:00Z', '2026-01-01T01:00:00Z', 1),
                ('newer', 'proj', 'registry newer', '2026-01-02T00:00:00Z', '2026-01-02T01:00:00Z', 1);
            INSERT INTO reflections (id, content, tags, timestamp) VALUES
                ('episode-newer-old',
                 '{"schema":"v2","session_id":"newer","project":"proj","timestamp":"2026-01-02T00:30:00Z","request":"old request","investigated":[],"outcome":"partial","todos":[]}',
                 '["session_episode","schema_v2","conv_newer"]',
                 '2026-01-02T00:30:00Z'),
                ('episode-newer-latest',
                 '{"schema":"v2","session_id":"newer","project":"proj","timestamp":"2026-01-02T01:00:00Z","request":"latest request","investigated":["/repo/src/lib.rs","/repo/src/api.rs"],"outcome":"done","todos":[{"content":"ship it","status":"completed"}]}',
                 '["session_episode","schema_v2","conv_newer"]',
                 '2026-01-02T01:00:00Z'),
                ('story-newer-old', 'Old narrative.',
                 '["session_story","project_proj","conv_newer"]',
                 '2026-01-02T00:40:00Z'),
                ('story-newer-latest', 'Latest narrative. More detail.',
                 '["session_story","project_proj","conv_newer"]',
                 '2026-01-02T01:10:00Z');
            "#,
        )
        .unwrap();

        let sessions = load_story_sessions(&conn).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, "newer");
        assert_eq!(sessions[0].request.as_deref(), Some("latest request"));
        assert_eq!(
            sessions[0].narrative.as_deref(),
            Some("Latest narrative. More detail.")
        );
        assert_eq!(
            sessions[0].investigated,
            ["/repo/src/lib.rs", "/repo/src/api.rs"]
        );
        assert_eq!(sessions[0].todos[0].content, "ship it");
        assert_eq!(sessions[1].session_id, "older");
        assert_eq!(sessions[1].first_prompt.as_deref(), Some("registry older"));
    }

    #[test]
    fn story_sessions_join_anchor_receipts_and_attributed_files() {
        let conn = memory_db();
        conn.execute_batch(
            r#"
            INSERT INTO episode_anchors
                (session_id, project, file, node_kind, name, body_hash, created_at)
            VALUES ('artifact-session', 'proj', '/repo/src/lib.rs', 'function_item',
                    'old_symbol', 'hash', '2026-01-03 01:00:00');
            INSERT INTO witness_ledger
                (project, file, symbol, span_start, span_end, stamp, tier,
                 at_oid, source_kind, source_id)
            VALUES ('proj', '/repo/src/lib.rs', 'old_symbol', 1, 3, 'b3:old',
                    'committed', 'oldoid', 'backfill', 'oldoid');
            INSERT INTO witness_verdicts
                (witness_id, verdict, successor_witness_id, receipt_oid,
                 observed_head_oid, created_at)
            VALUES ((SELECT id FROM witness_ledger WHERE symbol = 'old_symbol'),
                    'superseded_by', NULL, '1234567890abcdef', 'head',
                    '2026-01-03 02:00:00');
            INSERT INTO code_nodes
                (id, project, file, kind, name, first_conv_id, last_conv_id)
            VALUES ('node-one', 'proj', '/repo/src/extra.rs', 'function',
                    'extra_symbol', '', '');
            INSERT INTO code_node_attribution
                (node_id, channel, source_id, observed_ts, evidence)
            VALUES ('node-one', 'transcript', 'artifact-session',
                    '2026-01-03T03:00:00Z', 'coedit_event');
            "#,
        )
        .unwrap();

        let sessions = load_story_sessions(&conn).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "artifact-session");
        assert!(sessions[0].artifacts.iter().any(|artifact| {
            artifact.symbol.as_deref() == Some("old_symbol")
                && artifact.superseded_receipt.as_deref() == Some("1234567890abcdef")
        }));
        assert!(sessions[0].artifacts.iter().any(|artifact| {
            artifact.file == "/repo/src/extra.rs" && artifact.symbol.is_none()
        }));
    }

    /// Regression for finding 7: two sessions sharing an anchor identity
    /// (same project/file/symbol) must not cross-pollute each other's
    /// timestamp/sort key. Before the fix, propagating the shared artifact
    /// re-touched every session sharing the identity with whichever anchor
    /// row happened to be processed, so the older session inherited the
    /// newer session's date and could displace genuinely newer stories in
    /// the report's newest-first ordering.
    #[test]
    fn shared_symbol_across_sessions_keeps_each_sessions_own_timestamp() {
        let conn = memory_db();
        conn.execute_batch(
            r#"
            INSERT INTO episode_anchors
                (session_id, project, file, node_kind, name, body_hash, created_at)
            VALUES
                ('session-old', 'proj', '/repo/src/lib.rs', 'function_item',
                 'shared_symbol', 'hash', '2026-01-01 00:00:00'),
                ('session-new', 'proj', '/repo/src/lib.rs', 'function_item',
                 'shared_symbol', 'hash', '2026-02-01 00:00:00');
            "#,
        )
        .unwrap();

        let sessions = load_story_sessions(&conn).unwrap();
        assert_eq!(sessions.len(), 2);

        let old = sessions
            .iter()
            .find(|s| s.session_id == "session-old")
            .expect("older session must be present");
        let new = sessions
            .iter()
            .find(|s| s.session_id == "session-new")
            .expect("newer session must be present");

        assert_eq!(
            old.timestamp, "2026-01-01 00:00:00",
            "the older session must keep deriving its own timestamp, not the newer session's"
        );
        assert_eq!(new.timestamp, "2026-02-01 00:00:00");

        // Newest-first ordering must reflect each session's own timestamp,
        // not a timestamp borrowed from a session it merely shares a symbol
        // with.
        assert_eq!(sessions[0].session_id, "session-new");
        assert_eq!(sessions[1].session_id, "session-old");
    }

    /// Regression for finding 8: an identity shared by far more sessions
    /// than the report's card cap must not force unbounded per-session
    /// cloning of the full conversation list, and the sessions that do get
    /// attached must be the newest ones by anchor timestamp — selected
    /// before the attach/clone step runs, not after.
    #[test]
    fn shared_identity_across_many_sessions_bounds_and_keeps_newest_candidates() {
        let conn = memory_db();
        let base = chrono::NaiveDate::from_ymd_opt(2020, 1, 1).unwrap();
        let mut batch = String::new();
        for i in 0..200i64 {
            let date = base + chrono::Duration::days(i);
            batch.push_str(&format!(
                "INSERT INTO episode_anchors \
                 (session_id, project, file, node_kind, name, body_hash, created_at) \
                 VALUES ('session-{i:04}', 'proj', '/repo/src/lib.rs', 'function_item', \
                 'shared_symbol', 'hash', '{date} 00:00:00');\n"
            ));
        }
        conn.execute_batch(&batch).unwrap();

        let sessions = load_story_sessions(&conn).unwrap();
        assert_eq!(
            sessions.len(),
            200,
            "every session still gets its own entry"
        );

        let newest = sessions
            .iter()
            .find(|s| s.session_id == "session-0199")
            .expect("newest session must be present");
        let artifact = newest
            .artifacts
            .iter()
            .find(|a| a.symbol.as_deref() == Some("shared_symbol"))
            .expect("newest session must carry the shared artifact");

        assert!(
            artifact.conversations.len() <= super::MAX_SHARED_ARTIFACT_SESSIONS,
            "conversation list must be capped, got {}",
            artifact.conversations.len()
        );
        assert!(
            artifact.conversations.contains(&"session-0199".to_string()),
            "the newest session must retain itself in the capped candidate set"
        );
        assert!(
            !artifact.conversations.contains(&"session-0000".to_string()),
            "the oldest session must be dropped once the shared identity exceeds the cap \
             (proves selection happens before expansion, not after)"
        );
    }
}
