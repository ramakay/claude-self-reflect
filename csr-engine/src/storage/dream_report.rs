//! Read-only projection for the session-led dream journal.

use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::transcript::instrumentation::{ErrorEvent, SessionInstrumentation, SteerEvent};

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

/// F4: which resolution-order step (plan §3.3a) produced `StorySession::
/// instrumentation`, and — for the cache step — the filesystem stat it was
/// computed against. `load_story_sessions` is a DB-only loader (module doc:
/// "read here without a freshness check — that requires a filesystem stat
/// this DB-only loader deliberately does not perform"); `dream::report`'s
/// report-time backfill is the layer that CAN stat a file, and it needs this
/// tag to know which cards it is still responsible for freshness-checking.
/// Episode-sourced instrumentation is the Stop hook's own tool-verified scan
/// of the transcript at Stop time — authoritative, never re-validated.
/// Cache-sourced instrumentation is a `session_instrumentation` row that may
/// have been computed against a transcript that has since grown, shrunk, or
/// moved — untrusted for display until the backfill confirms the stat still
/// matches.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum InstrumentationSource {
    #[default]
    None,
    Episode,
    Cache {
        transcript_size: u64,
        transcript_mtime: i64,
    },
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
    /// Journal v2 instrumentation feeds (plan §3.2/§3.3/§4.2). Fields that
    /// were already on disk in the v2 episode JSON but previously discarded
    /// by `EpisodeJson`'s `#[serde(default)]`, plus the resolved
    /// tool-verified instrumentation.
    pub files_modified: Vec<String>,
    pub completed: Option<String>,
    pub next_steps: Option<String>,
    pub blockers: Option<String>,
    /// Substring-matched, deduped signature strings (the pre-existing,
    /// over/under-counting fallback `Episode::error_signatures` — see
    /// `transcript::instrumentation` module doc). Kept raw so the renderer
    /// decides, per session, whether it is eligible as the degraded
    /// fallback (only when non-empty — an episode that never carried this
    /// field at all defaults to the same empty `Vec` as one that genuinely
    /// found zero matches, so an empty list here is never distinguishable
    /// from "not collected" and must never render `~0 errors`).
    pub error_signatures: Vec<String>,
    /// Resolved instrumentation, in the first two steps of the plan's
    /// four-step resolution order (§3.3a): episode field `Some(_)` wins;
    /// else a cached `session_instrumentation` row (read here without a
    /// freshness check — that requires a filesystem stat this DB-only
    /// loader deliberately does not perform; `dream::report`'s backfill
    /// re-validates freshness for the bounded set of rich sessions it
    /// actually renders). `None` means "never measured" and must never be
    /// confused with `Some(0)` ("measured, zero found").
    pub instrumentation: Option<SessionInstrumentation>,
    /// F4: provenance of `instrumentation`, `None` (unset) whenever
    /// `instrumentation` is `None`. See [`InstrumentationSource`].
    pub instrumentation_source: InstrumentationSource,
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
    files_modified: Vec<String>,
    completed: Option<String>,
    next_steps: Option<String>,
    blockers: Option<String>,
    error_signatures: Vec<String>,
    instrumentation: Option<SessionInstrumentation>,
    instrumentation_source: InstrumentationSource,
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
    // Journal v2 (plan §3.2/§4.2): fields the stored v2 episode JSON has
    // always carried but this projection previously dropped on the floor
    // via `#[serde(default)]`. No storage change — these bytes are already
    // on disk for every episode written since v2.
    files_modified: Vec<String>,
    error_signatures: Vec<String>,
    completed: String,
    next_steps: Option<String>,
    blockers: Option<String>,
    error_count: Option<u32>,
    top_errors: Vec<ErrorEvent>,
    steer_count: Option<u32>,
    steers: Vec<SteerEvent>,
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
                builder.files_modified = episode
                    .files_modified
                    .into_iter()
                    .filter(|file| !file.trim().is_empty())
                    .collect();
                builder.error_signatures = episode.error_signatures;
                builder.completed = nonblank(episode.completed);
                builder.next_steps = episode.next_steps.and_then(nonblank);
                builder.blockers = episode.blockers.and_then(nonblank);
                // Resolution order step 1 (plan §3.3a): the episode's own
                // tool-verified count, when the Stop hook's forward-path
                // scan ran, wins outright. `error_count`/`steer_count` are
                // always written together (one `scan_instrumentation` call
                // in `hooks::stop`), so gating construction on
                // `error_count.is_some()` is sufficient for both.
                if let Some(error_count) = episode.error_count {
                    builder.instrumentation = Some(SessionInstrumentation {
                        error_count,
                        top_errors: episode.top_errors,
                        steer_count: episode.steer_count.unwrap_or(0),
                        steers: episode.steers,
                        // Not persisted at the episode layer (plan §4 point
                        // 1 lists only error_count/top_errors/steer_count/
                        // steers on `Episode`) — only the cache table and a
                        // live scan track turn_count, reserved for a later
                        // baseline-relative feature (plan §9 Q6).
                        turn_count: 0,
                    });
                    // F4: episode-sourced instrumentation is the Stop hook's
                    // own tool-verified scan — authoritative, never
                    // re-validated or rescanned by the report-time backfill.
                    builder.instrumentation_source = InstrumentationSource::Episode;
                }
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

    // Resolution order step 2 (plan §3.3a): a cached `session_instrumentation`
    // row, for sessions the episode branch above left unset. Run this LAST,
    // after every other pass has finished creating `sessions` entries
    // (registry/episode/story/anchors/attribution) — a cache row must be
    // able to attach to a session first surfaced only via an artifact or
    // attribution row, not just one seen by the registry/episode passes.
    // Read without a freshness (size/mtime) check — that requires a
    // filesystem stat this DB-only loader does not perform;
    // `dream::report`'s report-time backfill re-validates freshness for the
    // bounded set of rich sessions it actually scans. `get_mut` only
    // touches sessions that already have an entry — a cache row for a
    // session with zero other signal must not fabricate one here, which
    // would wrongly promote it out of "omitted" on cache evidence alone.
    {
        let mut stmt = conn.prepare(
            "SELECT session_id, transcript_size, transcript_mtime, error_count, steer_count,
                    turn_count, errors_json, steers_json
             FROM session_instrumentation",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, u32>(3)?,
                row.get::<_, u32>(4)?,
                row.get::<_, u32>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;
        for row in rows {
            let (
                session_id,
                transcript_size,
                transcript_mtime,
                error_count,
                steer_count,
                turn_count,
                errors_json,
                steers_json,
            ) = row?;
            if let Some(builder) = sessions.get_mut(&session_id) {
                if builder.instrumentation.is_none() {
                    let top_errors: Vec<ErrorEvent> =
                        serde_json::from_str(&errors_json).unwrap_or_default();
                    let steers: Vec<SteerEvent> =
                        serde_json::from_str(&steers_json).unwrap_or_default();
                    builder.instrumentation = Some(SessionInstrumentation {
                        error_count,
                        top_errors,
                        steer_count,
                        steers,
                        turn_count,
                    });
                    // F4: cache-sourced — untrusted for display until the
                    // report-time backfill confirms `transcript_size`/
                    // `transcript_mtime` still match the file on disk.
                    builder.instrumentation_source = InstrumentationSource::Cache {
                        transcript_size: transcript_size.max(0) as u64,
                        transcript_mtime,
                    };
                }
            }
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
                    files_modified: builder.files_modified,
                    completed: builder.completed,
                    next_steps: builder.next_steps,
                    blockers: builder.blockers,
                    error_signatures: builder.error_signatures,
                    instrumentation: builder.instrumentation,
                    instrumentation_source: builder.instrumentation_source,
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

// --- SINCE THEN payback panel (journal v2 Phase 5) --------------------------

/// One symbol row for a session's SINCE THEN panel — "this happened": the
/// session touched `symbol` in `file`. `anchor_only` marks the fallback
/// path (see [`load_since_then_symbols`]); `touched_at` is the row's
/// recency sort key (an ISO-ish timestamp, sourced consistently within one
/// call — either every row's `observed_ts`/`last_seen` or every row's
/// `episode_anchors.created_at`, never a mix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SinceThenSymbol {
    pub file: String,
    pub symbol: String,
    pub anchor_only: bool,
    pub touched_at: String,
}

/// The symbols a session "happened" to touch, for the SINCE THEN panel
/// (journal v2 Phase 5 spec). Primary source: `code_node_attribution` rows
/// this session currently owns on the trusted `'transcript'` channel (one
/// row per node — the session that last observed it, same channel
/// `load_story_sessions`'s file-level artifact join already trusts). Only
/// when that yields nothing does this fall back to `episode_anchors` for
/// the session, labeled `anchor_only` — the plan's honest "anchor-only"
/// disclosure for symbols with attribution-grade provenance never derived.
pub(crate) fn load_since_then_symbols(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<SinceThenSymbol>> {
    let mut out = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT cn.file, cn.name, COALESCE(a.observed_ts, cn.last_seen, '')
             FROM code_node_attribution a
             JOIN code_nodes cn ON cn.id = a.node_id
             WHERE a.channel = 'transcript' AND a.source_id = ?1 AND cn.name != ''
             ORDER BY cn.file, cn.name",
        )?;
        let rows = stmt.query_map([session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (file, symbol, touched_at) = row?;
            out.push(SinceThenSymbol {
                file,
                symbol,
                anchor_only: false,
                touched_at,
            });
        }
    }
    if out.is_empty() {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT file, name, created_at
             FROM episode_anchors
             WHERE session_id = ?1
             ORDER BY file, name",
        )?;
        let rows = stmt.query_map([session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (file, symbol, touched_at) = row?;
            out.push(SinceThenSymbol {
                file,
                symbol,
                anchor_only: true,
                touched_at,
            });
        }
    }
    Ok(out)
}

// --- Report-time instrumentation backfill cache (plan §3.3a step 3) -------
//
// These two functions are the only place `session_instrumentation` is read
// or written outside `load_story_sessions`'s naive (freshness-unaware) join
// above. Callers that need freshness validation — the report-time backfill
// in `dream::report`, which is the only caller with a live transcript
// `(size, mtime)` pair to validate against — go through
// `read_instrumentation_cache` instead of the bulk join.

/// Read a cached row IFF it matches the given `(transcript_size,
/// transcript_mtime)` — a stale row (the transcript grew, shrank, or was
/// touched since the row was written) is treated as absent, not returned,
/// so the caller knows to rescan.
pub(crate) fn read_instrumentation_cache(
    conn: &Connection,
    session_id: &str,
    transcript_size: u64,
    transcript_mtime: i64,
) -> Result<Option<SessionInstrumentation>> {
    let row = conn
        .query_row(
            "SELECT transcript_size, transcript_mtime, error_count, steer_count,
                    turn_count, errors_json, steers_json
             FROM session_instrumentation WHERE session_id = ?1",
            [session_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, u32>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((size, mtime, error_count, steer_count, turn_count, errors_json, steers_json)) = row
    else {
        return Ok(None);
    };
    if size as u64 != transcript_size || mtime != transcript_mtime {
        return Ok(None);
    }
    Ok(Some(SessionInstrumentation {
        error_count,
        top_errors: serde_json::from_str(&errors_json).unwrap_or_default(),
        steer_count,
        steers: serde_json::from_str(&steers_json).unwrap_or_default(),
        turn_count,
    }))
}

/// Upsert a freshly-scanned instrumentation result, keyed by `session_id`,
/// stamped with the transcript's `(size, mtime)` it was computed from.
pub(crate) fn write_instrumentation_cache(
    conn: &Connection,
    session_id: &str,
    transcript_size: u64,
    transcript_mtime: i64,
    instrumentation: &SessionInstrumentation,
) -> Result<()> {
    let errors_json = serde_json::to_string(&instrumentation.top_errors)?;
    let steers_json = serde_json::to_string(&instrumentation.steers)?;
    conn.execute(
        "INSERT INTO session_instrumentation
            (session_id, transcript_size, transcript_mtime, error_count,
             steer_count, turn_count, errors_json, steers_json, computed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         ON CONFLICT(session_id) DO UPDATE SET
            transcript_size = excluded.transcript_size,
            transcript_mtime = excluded.transcript_mtime,
            error_count = excluded.error_count,
            steer_count = excluded.steer_count,
            turn_count = excluded.turn_count,
            errors_json = excluded.errors_json,
            steers_json = excluded.steers_json,
            computed_at = excluded.computed_at",
        rusqlite::params![
            session_id,
            transcript_size as i64,
            transcript_mtime,
            instrumentation.error_count,
            instrumentation.steer_count,
            instrumentation.turn_count,
            errors_json,
            steers_json,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{load_story_sessions, read_instrumentation_cache, write_instrumentation_cache};
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

    // ---- §7.2 pinned Phase 2 tests ---------------------------------------

    /// Pinned test 12: three sessions, one per resolution branch. The third
    /// (no episode instrumentation, no cache row) must come back `None` —
    /// never `Some(0)`, which would be indistinguishable from a real
    /// measured zero.
    #[test]
    fn instrumentation_resolution_prefers_episode_then_cache_then_none() {
        let conn = memory_db();
        conn.execute_batch(
            r#"
            INSERT INTO session_registry
                (session_id, project, first_prompt, first_ts, last_ts, prompt_count)
            VALUES
                ('from-episode', 'proj', 'ask', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 1),
                ('from-cache', 'proj', 'ask', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 1),
                ('from-neither', 'proj', 'ask', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 1);
            INSERT INTO reflections (id, content, tags, timestamp) VALUES
                ('episode-from-episode',
                 '{"schema":"v2","session_id":"from-episode","project":"proj","timestamp":"2026-01-01T00:00:00Z","request":"r","outcome":"done","investigated":[],"todos":[],"error_count":3,"top_errors":[{"turn":2,"tool":"Bash","preview":"boom"}],"steer_count":1,"steers":[{"turn":3,"text":"no, hindi"}]}',
                 '["session_episode","schema_v2","conv_from-episode"]',
                 '2026-01-01T00:00:00Z');
            INSERT INTO session_instrumentation
                (session_id, transcript_size, transcript_mtime, error_count, steer_count,
                 turn_count, errors_json, steers_json)
            VALUES ('from-cache', 100, 200, 5, 2, 10, '[]', '[]');
            "#,
        )
        .unwrap();

        let sessions = load_story_sessions(&conn).unwrap();
        let from_episode = sessions
            .iter()
            .find(|s| s.session_id == "from-episode")
            .unwrap();
        let from_cache = sessions
            .iter()
            .find(|s| s.session_id == "from-cache")
            .unwrap();
        let from_neither = sessions
            .iter()
            .find(|s| s.session_id == "from-neither")
            .unwrap();

        assert_eq!(
            from_episode.instrumentation.as_ref().map(|i| i.error_count),
            Some(3)
        );
        assert_eq!(
            from_cache.instrumentation.as_ref().map(|i| i.error_count),
            Some(5)
        );
        assert_eq!(
            from_neither.instrumentation, None,
            "no episode instrumentation and no cache row must resolve to None, not Some(0)"
        );
    }

    /// Pinned test 13.
    #[test]
    fn cache_row_is_reused_when_size_and_mtime_match_and_invalidated_when_they_dont() {
        let conn = memory_db();
        let inst = crate::transcript::instrumentation::SessionInstrumentation {
            error_count: 4,
            top_errors: vec![],
            steer_count: 2,
            steers: vec![],
            turn_count: 12,
        };
        write_instrumentation_cache(&conn, "sess-a", 1_000, 500, &inst).unwrap();

        let hit = read_instrumentation_cache(&conn, "sess-a", 1_000, 500)
            .unwrap()
            .expect("matching size/mtime must be reused");
        assert_eq!(hit.error_count, 4);
        assert_eq!(hit.steer_count, 2);

        let stale_size = read_instrumentation_cache(&conn, "sess-a", 1_001, 500).unwrap();
        assert!(
            stale_size.is_none(),
            "a size mismatch must invalidate the cached row"
        );
        let stale_mtime = read_instrumentation_cache(&conn, "sess-a", 1_000, 501).unwrap();
        assert!(
            stale_mtime.is_none(),
            "an mtime mismatch must invalidate the cached row"
        );

        // Rewriting with the new size/mtime makes the row fresh again.
        write_instrumentation_cache(&conn, "sess-a", 1_001, 501, &inst).unwrap();
        let refreshed = read_instrumentation_cache(&conn, "sess-a", 1_001, 501).unwrap();
        assert!(refreshed.is_some());
    }

    /// Pinned test 14.
    #[test]
    fn episode_json_now_reads_files_modified_and_error_signatures() {
        let conn = memory_db();
        conn.execute_batch(
            r#"
            INSERT INTO reflections (id, content, tags, timestamp) VALUES
                ('episode-fm',
                 '{"schema":"v2","session_id":"fm-session","project":"proj","timestamp":"2026-01-05T00:00:00Z","request":"r","outcome":"done","investigated":[],"todos":[],"files_modified":["/repo/src/a.rs","/repo/src/b.rs"],"error_signatures":["Error: boom","panic: x"],"completed":"shipped it","next_steps":"deploy","blockers":"none"}',
                 '["session_episode","schema_v2","conv_fm-session"]',
                 '2026-01-05T00:00:00Z');
            "#,
        )
        .unwrap();

        let sessions = load_story_sessions(&conn).unwrap();
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.files_modified, ["/repo/src/a.rs", "/repo/src/b.rs"]);
        assert_eq!(session.error_signatures, ["Error: boom", "panic: x"]);
        assert_eq!(session.completed.as_deref(), Some("shipped it"));
        assert_eq!(session.next_steps.as_deref(), Some("deploy"));
        assert_eq!(session.blockers.as_deref(), Some("none"));
    }

    // ---- SINCE THEN symbol loading (journal v2 Phase 5) --------------------

    #[test]
    fn since_then_symbols_prefer_attribution_over_anchors() {
        let conn = memory_db();
        conn.execute_batch(
            r#"
            INSERT INTO code_nodes (id, project, file, kind, name, first_conv_id, last_conv_id)
            VALUES ('node-a', 'proj', '/repo/src/lib.rs', 'function', 'attributed_fn', '', '');
            INSERT INTO code_node_attribution (node_id, channel, source_id, observed_ts, evidence)
            VALUES ('node-a', 'transcript', 'sess-1', '2026-01-05T00:00:00Z', 'coedit_event');
            INSERT INTO episode_anchors
                (session_id, project, file, node_kind, name, body_hash, created_at)
            VALUES ('sess-1', 'proj', '/repo/src/other.rs', 'function_item',
                    'anchor_fn', 'hash', '2026-01-05 00:00:00');
            "#,
        )
        .unwrap();

        let symbols = super::load_since_then_symbols(&conn, "sess-1").unwrap();
        assert_eq!(symbols.len(), 1, "attribution present -> anchors ignored");
        assert_eq!(symbols[0].symbol, "attributed_fn");
        assert!(!symbols[0].anchor_only);
    }

    #[test]
    fn since_then_symbols_fall_back_to_anchors_labeled_anchor_only() {
        let conn = memory_db();
        conn.execute_batch(
            r#"
            INSERT INTO episode_anchors
                (session_id, project, file, node_kind, name, body_hash, created_at)
            VALUES ('sess-2', 'proj', '/repo/src/other.rs', 'function_item',
                    'anchor_fn', 'hash', '2026-01-05 00:00:00');
            "#,
        )
        .unwrap();

        let symbols = super::load_since_then_symbols(&conn, "sess-2").unwrap();
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].symbol, "anchor_fn");
        assert!(symbols[0].anchor_only);
    }

    #[test]
    fn since_then_symbols_empty_for_session_with_neither() {
        let conn = memory_db();
        assert!(super::load_since_then_symbols(&conn, "sess-3")
            .unwrap()
            .is_empty());
    }
}
