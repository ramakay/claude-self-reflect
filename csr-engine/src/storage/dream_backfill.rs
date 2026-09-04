//! Dream backfill — Stage 0 (`.plans/dream-backfill-design.md` §3 "Stage 0",
//! with the D3/D4 round-2 deltas from §8 folded in, since that section
//! overrides §3-6 wherever they conflict).
//!
//! Materializes `episode_index`, a refreshable flat projection over the
//! `reflections` rows that carry schema-v2 episode JSON
//! (`hooks::stop::Episode`). `hooks::stop::store_episode` remains the writer
//! of record for episode content; this module never touches `reflections`,
//! it only reads it and rebuilds a query-shaped cache so the (future)
//! pair-generator / rank / adjudicate / verify stages can scan one flat
//! table instead of re-parsing every reflection's JSON on every pass.
//!
//! [`refresh_episode_index`] is the entry point later stages should call: it
//! runs [`materialize_episode_index`] (base columns, `INSERT OR REPLACE`
//! keyed by `episode_id` — full-row idempotent refresh) and then
//! [`fill_aliveness`] (D3 aliveness columns, always recomputed fresh against
//! current git state) in sequence, so a caller never observes a
//! partially-stale row. The two are also exposed separately for narrower
//! callers and for testing.
//!
//! `episode.prev_episode_id` (see `hooks::stop::pick_prev_episode`) holds
//! another episode's `session_id`, **not** a `reflections.id` /
//! `episode_index.episode_id` — despite the field's name, for rows the
//! current hooks write. Imported/historical corpora (and any row written
//! before this convention was settled) can instead carry the descendant's
//! `episode_id` directly in this field. [`prev_chain_pairs`] joins on
//! EITHER domain (`session_id` OR `episode_id`) for exactly that reason; see
//! its own doc comment.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// A fully materialized `episode_index` row.
#[derive(Debug, Clone, PartialEq)]
pub struct EpisodeIndexRow {
    pub episode_id: String,
    pub session_id: String,
    pub project: String,
    pub ts: String,
    pub outcome: String,
    pub request: String,
    pub completed: String,
    pub next_steps: Option<String>,
    pub blockers: Option<String>,
    pub todo_count: i64,
    pub files_json: String,
    pub anchors_json: String,
    pub prev_episode_id: Option<String>,
}

/// Subset of `hooks::stop::Episode` this module actually reads. A local,
/// module-private struct rather than importing `hooks::stop::Episode`
/// directly — same convention as `storage::dream_items::EpisodeRecord` /
/// `storage::dream_report`'s `EpisodeJson` (each reader takes only the
/// fields it needs, `#[serde(default)]` so a v1-shaped or partially-absent
/// field never fails deserialization).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EpisodeRecord {
    session_id: String,
    project: String,
    timestamp: String,
    request: String,
    completed: String,
    next_steps: Option<String>,
    blockers: Option<String>,
    outcome: String,
    files_modified: Vec<String>,
    todos: Vec<EpisodeTodo>,
    prev_episode_id: Option<String>,
    anchors: Vec<EpisodeAnchor>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EpisodeTodo {
    #[allow(dead_code)] // kept for shape-fidelity with the source JSON; not read yet
    content: String,
    status: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct EpisodeAnchor {
    file: String,
    node_kind: String,
    name: String,
    body_hash: String,
}

/// Outcome of a [`materialize_episode_index`] run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MaterializeStats {
    /// Rows matching the schema-v2 predicate that were considered.
    pub scanned: usize,
    /// Rows successfully upserted into `episode_index`.
    pub upserted: usize,
    /// Rows that matched the predicate but failed to deserialize as episode
    /// JSON (a corrupt or unexpectedly-shaped row) — skipped, not fatal.
    pub skipped_invalid: usize,
}

/// Outcome of a [`fill_aliveness`] run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AlivenessStats {
    pub episodes_scanned: usize,
    /// Episodes whose aliveness resolved to `(None, None)` — no touched file
    /// resolved to a locally-present git repo.
    pub episodes_unresolved: usize,
}

/// Full Stage 0 refresh: base columns, then aliveness. See the module doc
/// for why these always run together.
pub fn refresh_episode_index(conn: &Connection) -> Result<(MaterializeStats, AlivenessStats)> {
    refresh_episode_index_at(conn, chrono::Utc::now().timestamp())
}

/// Injectable-clock variant of [`refresh_episode_index`] (round-4 F-R1).
pub fn refresh_episode_index_at(
    conn: &Connection,
    now_unix: i64,
) -> Result<(MaterializeStats, AlivenessStats)> {
    let materialize = materialize_episode_index(conn)?;
    let aliveness = fill_aliveness_at(conn, now_unix)?;
    Ok((materialize, aliveness))
}

/// Count of non-completed todos in an episode — the "unfinished work" signal
/// Stage 1's seed condition (design §3 Stage 1: `outcome ∈ (partial, failed)
/// OR todo_count > 0`) reads. A todo the episode itself marked `completed`
/// is not open work; `hooks::stop::extract_episode` already drops
/// `status == "deleted"` items before persisting, so every row reaching here
/// is a live todo in some non-completed state.
fn open_todo_count(todos: &[EpisodeTodo]) -> i64 {
    todos.iter().filter(|t| t.status != "completed").count() as i64
}

/// Materialize `episode_index` base columns from `reflections` rows carrying
/// schema-v2 episode JSON. `INSERT OR REPLACE` keyed by `episode_id` — a
/// full-row replace, so a re-run is idempotent and, on a row whose episode
/// content changed since the last refresh, reflects the new content exactly
/// (never a stale merge of old and new fields). Aliveness columns are left
/// at their table default (`NULL`) by this pass; call [`fill_aliveness`]
/// (or [`refresh_episode_index`]) afterward to populate them.
///
/// The `json_valid(content) AND json_extract(content, '$.schema') = 'v2'`
/// predicate is the same one already used by `dream::threads`,
/// `journal::composer`, `storage::dream_items` and `storage::dream_clusters`
/// to select episode reflections — reused here rather than invented fresh.
pub fn materialize_episode_index(conn: &Connection) -> Result<MaterializeStats> {
    super::artifact_provenance::atomic_write(conn, materialize_episode_index_inner)
}

pub(crate) fn projection_matches_source(conn: &Connection, id: &str) -> Result<bool> {
    let source: Option<(String, String)> = conn
        .query_row(
            "SELECT content,timestamp FROM reflections WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((content, ts)) = source else {
        return Ok(false);
    };
    let Ok(record) = serde_json::from_str::<EpisodeRecord>(&content) else {
        return Ok(false);
    };
    let expected = EpisodeIndexRow {
        episode_id: id.into(),
        session_id: record.session_id,
        project: record.project.clone(),
        ts: if record.timestamp.trim().is_empty() {
            ts
        } else {
            record.timestamp
        },
        outcome: record.outcome,
        request: record.request,
        completed: record.completed,
        next_steps: record.next_steps.filter(|s| !s.trim().is_empty()),
        blockers: record.blockers.filter(|s| !s.trim().is_empty()),
        todo_count: open_todo_count(&record.todos),
        files_json: serde_json::to_string(&record.files_modified)?,
        anchors_json: serde_json::to_string(&record.anchors)?,
        prev_episode_id: record.prev_episode_id.filter(|s| !s.trim().is_empty()),
    };
    Ok(load_project_rows(conn, &record.project)?
        .iter()
        .any(|row| row == &expected))
}

fn materialize_episode_index_inner(conn: &Connection) -> Result<MaterializeStats> {
    let rows: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, content, timestamp FROM reflections
             WHERE json_valid(content)
               AND json_extract(content, '$.schema') = 'v2'",
        )?;
        let collected = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    };

    let mut stats = MaterializeStats {
        scanned: rows.len(),
        upserted: 0,
        skipped_invalid: 0,
    };

    let mut upsert = conn.prepare(
        "INSERT OR REPLACE INTO episode_index (
            episode_id, session_id, project, ts, outcome, request, completed,
            next_steps, blockers, todo_count, files_json, anchors_json,
            prev_episode_id, refreshed_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, datetime('now'))",
    )?;

    for (id, content, reflection_ts) in rows {
        let Ok(record) = serde_json::from_str::<EpisodeRecord>(&content) else {
            stats.skipped_invalid += 1;
            continue;
        };

        // `ts` resolution mirrors `storage::dream_items::EpisodeRow::origin_ts`:
        // the episode's own `timestamp` field when non-blank, else the
        // reflection row's own `timestamp` (import/store time).
        let ts = if record.timestamp.trim().is_empty() {
            reflection_ts
        } else {
            record.timestamp.clone()
        };

        let files_json = serde_json::to_string(&record.files_modified)?;
        let anchors_json = serde_json::to_string(&record.anchors)?;
        let todo_count = open_todo_count(&record.todos);
        let prev_episode_id = record
            .prev_episode_id
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .cloned();
        let next_steps = record
            .next_steps
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .cloned();
        let blockers = record
            .blockers
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .cloned();

        upsert.execute(params![
            id,
            record.session_id,
            record.project,
            ts,
            record.outcome,
            record.request,
            record.completed,
            next_steps,
            blockers,
            todo_count,
            files_json,
            anchors_json,
            prev_episode_id,
        ])?;
        let parent = super::artifact_provenance::artifact_input(
            conn,
            super::artifact_provenance::ArtifactKind::Reflection,
            &id,
        )?;
        super::artifact_provenance::record_stored_inputs(
            conn,
            super::artifact_provenance::ArtifactKind::EpisodeIndex,
            &id,
            &super::artifact_provenance::InputEnvelope::new(vec![parent]),
        )?;
        stats.upserted += 1;
    }

    Ok(stats)
}

/// A `git -C <repo_root>` command with any ambient `GIT_*` environment
/// stripped — same rationale and pattern as `extraction::repo_root::
/// git_toplevel` / `import::backfill::git_at`, duplicated locally per this
/// codebase's convention of keeping each storage submodule's small git
/// helpers dependency-free of its siblings.
fn git_at(repo_root: &str) -> Command {
    let mut cmd = Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    cmd.arg("-C").arg(repo_root);
    cmd
}

/// Is `rel` present in the repo's HEAD tree? `Some(false)` is a real,
/// resolved answer ("checked, and it is not there" — a deleted file);
/// `None` means the question could not be asked at all (no `git` binary, no
/// HEAD commit, spawn failure) and must propagate as "unresolvable", never
/// collapse into `false`.
fn git_present_at_head(repo_root: &str, rel: &str) -> Option<bool> {
    let spec = format!("HEAD:{rel}");
    let output = git_at(repo_root)
        .arg("cat-file")
        .arg("-e")
        .arg(&spec)
        .output()
        .ok()?;
    Some(output.status.success())
}

/// Unix seconds of the most recent commit's **committer** date touching
/// `rel` (D6: committer date, never author date) — `git log -1
/// --format=%ct -- <rel>`, the exact command the design (§3 Stage 0 / §8
/// D3) specifies. `None` when `git` cannot answer or `rel` has no history
/// (never committed, or the repo has no commits at all).
fn git_last_committer_ts(repo_root: &str, rel: &str) -> Option<i64> {
    let output = git_at(repo_root)
        .arg("log")
        .arg("-1")
        .arg("--format=%ct")
        .arg("--")
        .arg(rel)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<i64>().ok()
}

/// Per-file aliveness: `(present_at_head, days_since_last_touch)`. `None`
/// for both when `file`'s directory does not resolve to a locally-present
/// git repo (`extraction::repo_root::repo_root_for_file`) or when `file`
/// does not sit under that repo's root — absence of evidence, not a claim
/// of deadness.
fn file_aliveness(file: &str, now_unix: i64) -> (Option<bool>, Option<i64>) {
    let Some(repo_root) = crate::extraction::repo_root::repo_root_for_file(file) else {
        return (None, None);
    };
    let Ok(rel) = Path::new(file).strip_prefix(&repo_root) else {
        return (None, None);
    };
    let rel = rel.to_string_lossy();
    if rel.is_empty() {
        return (None, None);
    }

    let present = git_present_at_head(&repo_root, &rel);
    let touched_at = git_last_committer_ts(&repo_root, &rel);
    let days_since = touched_at.map(|ts| (now_unix.saturating_sub(ts)).max(0) / 86_400);
    (present, days_since)
}

/// Aggregate per-file aliveness into one episode-level reading.
///
/// `present_at_head`: `true` if ANY touched file is still live at HEAD (one
/// surviving instance of the pattern is enough to call the episode's claim
/// "still present"), `false` only when every file that resolved an answer
/// resolved to "gone", and `None` when nothing resolved at all.
///
/// `days_since_last_touch`: the MINIMUM across files — the freshest touch is
/// the strongest "this is still alive" signal, which is what the D1 now-hook
/// gate and the D3 alive-if-touched-<45d rule both want.
fn aggregate_aliveness(per_file: &[(Option<bool>, Option<i64>)]) -> (Option<bool>, Option<i64>) {
    let mut any_present = false;
    let mut any_known_presence = false;
    let mut min_days: Option<i64> = None;

    for (present, days) in per_file {
        if let Some(p) = present {
            any_known_presence = true;
            any_present |= *p;
        }
        if let Some(d) = days {
            min_days = Some(min_days.map_or(*d, |m| m.min(*d)));
        }
    }

    let present_at_head = any_known_presence.then_some(any_present);
    (present_at_head, min_days)
}

/// Fill `episode_index.present_at_head` / `days_since_last_touch` (D3) for
/// every row currently in the table, from CURRENT git state — always
/// recomputed, never merged with a prior value, so a repeat call reflects
/// commits made since the last one. Shells out to git per distinct file
/// touched across the whole table, cached once per absolute file path for
/// the duration of this call (an absolute path already disambiguates
/// project, so no separate project key is needed in the cache).
pub fn fill_aliveness(conn: &Connection) -> Result<AlivenessStats> {
    fill_aliveness_at(conn, chrono::Utc::now().timestamp())
}

/// Injectable-clock variant (round-4 review F-R1: `days_since_last_touch`
/// flows into obsolescence classification, so Stage 0 must honor the same
/// pinned `now` the later stages already accept).
pub fn fill_aliveness_at(conn: &Connection, now_unix: i64) -> Result<AlivenessStats> {
    let rows: Vec<(String, String)> = {
        let mut stmt = conn.prepare("SELECT episode_id, files_json FROM episode_index")?;
        let collected = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        collected
    };

    let mut cache: HashMap<String, (Option<bool>, Option<i64>)> = HashMap::new();
    let mut stats = AlivenessStats {
        episodes_scanned: rows.len(),
        episodes_unresolved: 0,
    };

    let mut update = conn.prepare(
        "UPDATE episode_index SET present_at_head = ?1, days_since_last_touch = ?2
         WHERE episode_id = ?3",
    )?;

    for (episode_id, files_json) in rows {
        let files: Vec<String> = serde_json::from_str(&files_json).unwrap_or_default();
        let per_file: Vec<(Option<bool>, Option<i64>)> = files
            .iter()
            .map(|f| {
                *cache
                    .entry(f.clone())
                    .or_insert_with(|| file_aliveness(f, now_unix))
            })
            .collect();

        let (present_at_head, days_since_last_touch) = aggregate_aliveness(&per_file);
        if present_at_head.is_none() && days_since_last_touch.is_none() {
            stats.episodes_unresolved += 1;
        }

        update.execute(params![
            present_at_head.map(|b| b as i64),
            days_since_last_touch,
            episode_id,
        ])?;
    }

    Ok(stats)
}

/// TODO(dream-backfill build stage 2+): near-duplicate merge (design §3
/// Stage 0) — `cos(vec) > 0.95` within the same project between adjacent
/// sessions (a resume) should collapse to the latest episode, recording the
/// merged ids. Deferred: needs episode vectors ([`episode_vectors`]) paired
/// with a defined "adjacent session" ordering that no caller exercises yet.
/// Returns its input unchanged until implemented — never silently drops or
/// reorders rows.
pub fn merge_near_duplicates(rows: Vec<EpisodeIndexRow>) -> Vec<EpisodeIndexRow> {
    rows
}

/// Transitive closure of the `prev_episode_id` chain, scoped to one project
/// (design §8 D4: "prev-chain is a gate, not a bonus" — chain-reachable
/// seeds are marked picked up BEFORE scoring, no similarity bonus).
///
/// **Domain note:** despite its name, `episode_index.prev_episode_id` holds
/// another episode's `session_id` (see `hooks::stop::pick_prev_episode`),
/// not a `reflections.id` / `episode_index.episode_id`. Each session
/// produces at most one live episode row (`hooks::stop::store_episode`
/// deletes any prior episode for the same session before inserting), so
/// `session_id` is a valid join key back to exactly one `episode_id` — the
/// recursive term below joins `e.prev_episode_id = c.session_id`
/// deliberately, not `= c.episode_id`.
///
/// Returns `(root_episode_id, descendant_episode_id)` pairs: `descendant`
/// continues, directly or transitively, from `root`'s session. Reflexive
/// pairs (`root_episode_id == episode_id`) are excluded — a genuine cycle in
/// malformed `prev_episode_id` data walks back to its own root, but that
/// fact carries no scoring signal, only the real descendants do. A depth cap
/// (`MAX_CHAIN_DEPTH`) bounds the walk — real chains are a handful of
/// resumes long, and the cap exists purely so a malformed cyclic
/// `prev_episode_id` (corrupt data, not a shape the hook itself can
/// currently produce) terminates the query instead of growing it without
/// bound.
///
/// **Dual-domain join (P1):** the recursive term matches `e.prev_episode_id`
/// against EITHER `c.session_id` (the convention `hooks::stop::
/// pick_prev_episode` writes) OR `c.episode_id` (a domain some
/// imported/historical rows carry instead — see the module doc). Matching
/// only `session_id` silently produces an empty chain for the latter shape,
/// which both loses a real continuation AND lets `unfinished::scan_unfinished`
/// wrongly treat the seed as "never picked up". `DISTINCT` plus
/// `MAX_CHAIN_DEPTH` already bound the OR-join's extra row multiplication,
/// and [`storage::witness_verdicts`] fitting (via `fit_tau_over_corpus`,
/// which also calls this function) inherits the fix for free.
const MAX_CHAIN_DEPTH: i64 = 200;

pub fn prev_chain_pairs(conn: &Connection, project: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE chain(root_episode_id, episode_id, session_id, prev_episode_id, depth) AS (
            SELECT episode_id, episode_id, session_id, prev_episode_id, 0
            FROM episode_index
            WHERE project = ?1
            UNION ALL
            SELECT c.root_episode_id, e.episode_id, e.session_id, e.prev_episode_id, c.depth + 1
            FROM episode_index e
            JOIN chain c
              ON e.prev_episode_id = c.session_id
              OR e.prev_episode_id = c.episode_id
            WHERE e.project = ?1 AND c.depth < ?2
        )
        SELECT DISTINCT root_episode_id, episode_id FROM chain
        WHERE depth > 0 AND root_episode_id != episode_id",
    )?;
    let rows = stmt.query_map(params![project, MAX_CHAIN_DEPTH], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// `episode_index.episode_id -> vector` for every currently-materialized
/// episode. Vectors are a straight 1:1 join to `reflection_embeddings`
/// (`reflection_embeddings.reflection_id` is that table's PRIMARY KEY and
/// `episode_id IS reflections.id`) — no pooling stage exists or is needed
/// (design §8, "REJECTED — F9 vector-count claim"). Decoding reuses
/// `storage::queries::load_all_reflection_vectors`'s existing little-endian
/// f32 blob decode rather than re-implementing it here.
pub fn episode_vectors(conn: &Connection) -> Result<Vec<(String, Vec<f32>)>> {
    let episode_ids: HashSet<String> = {
        let mut stmt = conn.prepare("SELECT episode_id FROM episode_index")?;
        let collected = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        collected
    };
    let all = super::queries::load_all_reflection_vectors(conn)?;
    Ok(all
        .into_iter()
        .filter(|(id, _)| episode_ids.contains(id))
        .collect())
}

/// Read back every materialized `episode_index` row for `project`, ordered
/// by `ts`. Used by `projection_matches_source` and tests — later pipeline
/// stages are expected to query narrower slices (`WHERE outcome IN (...)`,
/// etc.) directly.
pub(crate) fn load_project_rows(conn: &Connection, project: &str) -> Result<Vec<EpisodeIndexRow>> {
    let mut stmt = conn.prepare(
        "SELECT episode_id, session_id, project, ts, outcome, request, completed,
                next_steps, blockers, todo_count, files_json, anchors_json, prev_episode_id
         FROM episode_index WHERE project = ?1 ORDER BY ts ASC",
    )?;
    let rows = stmt.query_map(params![project], |row| {
        Ok(EpisodeIndexRow {
            episode_id: row.get(0)?,
            session_id: row.get(1)?,
            project: row.get(2)?,
            ts: row.get(3)?,
            outcome: row.get(4)?,
            request: row.get(5)?,
            completed: row.get(6)?,
            next_steps: row.get(7)?,
            blockers: row.get(8)?,
            todo_count: row.get(9)?,
            files_json: row.get(10)?,
            anchors_json: row.get(11)?,
            prev_episode_id: row.get(12)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        conn
    }

    fn insert_reflection(conn: &Connection, id: &str, json: &str, timestamp: &str) {
        conn.execute(
            "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, '[]', ?3)",
            params![id, json, timestamp],
        )
        .unwrap();
    }

    fn v2_episode(
        session_id: &str,
        project: &str,
        ts: &str,
        outcome: &str,
        todos_json: &str,
        files_json: &str,
        prev: Option<&str>,
    ) -> String {
        let prev_field = match prev {
            Some(p) => format!("\"{p}\""),
            None => "null".to_string(),
        };
        format!(
            r#"{{
                "schema": "v2",
                "session_id": "{session_id}",
                "project": "{project}",
                "timestamp": "{ts}",
                "request": "do the thing",
                "investigated": [],
                "completed": "did some of the thing",
                "next_steps": "finish the thing",
                "blockers": null,
                "outcome": "{outcome}",
                "error_signatures": [],
                "tools_used": [],
                "files_modified": {files_json},
                "message_count": 5,
                "duration_minutes": 3,
                "todos": {todos_json},
                "approved_plan": null,
                "prev_episode_id": {prev_field},
                "anchors": [
                    {{"file": "a.rs", "node_kind": "function", "name": "f", "body_hash": "abc123"}}
                ]
            }}"#
        )
    }

    #[test]
    fn materializes_only_valid_schema_v2_rows() {
        let conn = open();
        insert_reflection(
            &conn,
            "ep-1",
            &v2_episode(
                "sess-1",
                "proj",
                "2026-08-20T00:00:00+00:00",
                "partial",
                "[]",
                "[]",
                None,
            ),
            "2026-08-20T00:00:01+00:00",
        );
        // v1 episode — must be excluded.
        insert_reflection(
            &conn,
            "ep-v1",
            r#"{"schema":"v1","session_id":"sess-old","project":"proj","timestamp":"2026-01-01T00:00:00Z","outcome":"completed"}"#,
            "2026-01-01T00:00:01Z",
        );
        // A bare `{"schema":"v2"}` object still matches the predicate (same
        // one `dream::threads`/`journal::composer`/`dream_items` already use
        // to select episode rows) and deserializes via `#[serde(default)]`
        // into an all-defaults episode — scanned and upserted, just under
        // its own blank `project`/`session_id`, never surfaced under "proj".
        insert_reflection(
            &conn,
            "note-1",
            r#"{"schema":"v2"}"#,
            "2026-08-20T00:00:01+00:00",
        );
        // Malformed JSON fails `json_valid` — excluded before it is ever
        // deserialized, and never counted as `skipped_invalid` (that counter
        // is for rows the predicate matched but `serde_json` still rejected).
        insert_reflection(&conn, "broken", "{not json", "2026-08-20T00:00:01+00:00");

        let stats = materialize_episode_index(&conn).unwrap();
        assert_eq!(
            stats.scanned, 2,
            "ep-1 plus the bare schema-v2 object; not the v1 row or the malformed one"
        );
        assert_eq!(stats.upserted, 2);
        assert_eq!(stats.skipped_invalid, 0);

        let rows = load_project_rows(&conn, "proj").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].episode_id, "ep-1");
        assert_eq!(rows[0].session_id, "sess-1");
        assert_eq!(rows[0].outcome, "partial");

        // The bare-object episode landed under project = "" (its own default).
        let blank_project_rows = load_project_rows(&conn, "").unwrap();
        assert_eq!(blank_project_rows.len(), 1);
        assert_eq!(blank_project_rows[0].episode_id, "note-1");
    }

    #[test]
    fn todo_count_excludes_completed_items() {
        let conn = open();
        let todos = r#"[
            {"content": "a", "status": "completed"},
            {"content": "b", "status": "pending"},
            {"content": "c", "status": "in_progress"}
        ]"#;
        insert_reflection(
            &conn,
            "ep-1",
            &v2_episode(
                "sess-1",
                "proj",
                "2026-08-20T00:00:00+00:00",
                "partial",
                todos,
                "[]",
                None,
            ),
            "2026-08-20T00:00:01+00:00",
        );
        materialize_episode_index(&conn).unwrap();
        let rows = load_project_rows(&conn, "proj").unwrap();
        assert_eq!(rows[0].todo_count, 2);
    }

    #[test]
    fn files_and_anchors_round_trip_as_json_arrays() {
        let conn = open();
        insert_reflection(
            &conn,
            "ep-1",
            &v2_episode(
                "sess-1",
                "proj",
                "2026-08-20T00:00:00+00:00",
                "completed",
                "[]",
                r#"["/repo/a.rs", "/repo/b.rs"]"#,
                None,
            ),
            "2026-08-20T00:00:01+00:00",
        );
        materialize_episode_index(&conn).unwrap();
        let rows = load_project_rows(&conn, "proj").unwrap();
        let files: Vec<String> = serde_json::from_str(&rows[0].files_json).unwrap();
        assert_eq!(
            files,
            vec!["/repo/a.rs".to_string(), "/repo/b.rs".to_string()]
        );

        let anchors: serde_json::Value = serde_json::from_str(&rows[0].anchors_json).unwrap();
        assert_eq!(anchors[0]["name"], "f");
        assert_eq!(anchors[0]["body_hash"], "abc123");
    }

    #[test]
    fn ts_falls_back_to_reflection_timestamp_when_episode_timestamp_is_blank() {
        let conn = open();
        let json = v2_episode("sess-1", "proj", "", "completed", "[]", "[]", None);
        insert_reflection(&conn, "ep-1", &json, "2026-08-20T09:00:00+00:00");
        materialize_episode_index(&conn).unwrap();
        let rows = load_project_rows(&conn, "proj").unwrap();
        assert_eq!(rows[0].ts, "2026-08-20T09:00:00+00:00");
    }

    #[test]
    fn refresh_is_idempotent_and_fully_replaces_base_columns() {
        let conn = open();
        insert_reflection(
            &conn,
            "ep-1",
            &v2_episode(
                "sess-1",
                "proj",
                "2026-08-20T00:00:00+00:00",
                "partial",
                "[]",
                "[]",
                None,
            ),
            "2026-08-20T00:00:01+00:00",
        );
        materialize_episode_index(&conn).unwrap();
        // Same session_id -> `hooks::stop::store_episode` semantics delete
        // the old row and insert a new one under a fresh id; simulate that
        // by removing the old reflection and inserting the replacement.
        conn.execute("DELETE FROM reflections WHERE id = 'ep-1'", [])
            .unwrap();
        insert_reflection(
            &conn,
            "ep-1",
            &v2_episode(
                "sess-1",
                "proj",
                "2026-08-21T00:00:00+00:00",
                "completed",
                "[]",
                "[]",
                None,
            ),
            "2026-08-21T00:00:01+00:00",
        );
        let stats = materialize_episode_index(&conn).unwrap();
        assert_eq!(stats.upserted, 1);

        let rows = load_project_rows(&conn, "proj").unwrap();
        assert_eq!(
            rows.len(),
            1,
            "INSERT OR REPLACE must not duplicate the row"
        );
        assert_eq!(rows[0].outcome, "completed");
    }

    #[test]
    fn aliveness_is_null_when_no_local_repo_resolves() {
        let conn = open();
        insert_reflection(
            &conn,
            "ep-1",
            &v2_episode(
                "sess-1",
                "proj",
                "2026-08-20T00:00:00+00:00",
                "partial",
                "[]",
                r#"["/definitely/not/a/repo/anywhere/on/disk/xyz.rs"]"#,
                None,
            ),
            "2026-08-20T00:00:01+00:00",
        );
        materialize_episode_index(&conn).unwrap();
        let stats = fill_aliveness(&conn).unwrap();
        assert_eq!(stats.episodes_unresolved, 1);

        let present: Option<i64> = conn
            .query_row(
                "SELECT present_at_head FROM episode_index WHERE episode_id = 'ep-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let days: Option<i64> = conn
            .query_row(
                "SELECT days_since_last_touch FROM episode_index WHERE episode_id = 'ep-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(present, None);
        assert_eq!(days, None);
    }

    #[test]
    fn aliveness_reflects_real_git_history_for_a_temp_repo() {
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&repo).unwrap();

        let git_init = |args: &[&str]| {
            let mut cmd = Command::new("git");
            for (k, _) in std::env::vars_os() {
                if k.to_string_lossy().starts_with("GIT_") {
                    cmd.env_remove(&k);
                }
            }
            cmd.arg("-C").arg(&repo).args(args).status()
        };

        if git_init(&["init", "-q"])
            .map(|s| !s.success())
            .unwrap_or(true)
        {
            return; // git unavailable in this environment — fail-soft skip
        }
        let _ = git_init(&["config", "user.email", "test@example.com"]);
        let _ = git_init(&["config", "user.name", "Test"]);

        // `git rev-parse --show-toplevel` reports the symlink-resolved path
        // (macOS temp dirs live under a `/var` -> `/private/var` symlink), so
        // the file paths handed to `file_aliveness` must be canonicalized
        // too — otherwise `strip_prefix` never matches, exactly the
        // "unresolvable" case `aliveness_is_null_when_no_local_repo_resolves`
        // covers deliberately. Same fix `extraction::repo_root`'s own test
        // applies.
        let live_file = fs::canonicalize(&repo).unwrap().join("live.rs");
        fs::write(&live_file, "fn live() {}\n").unwrap();
        let dead_file = fs::canonicalize(&repo).unwrap().join("dead.rs");
        fs::write(&dead_file, "fn dead() {}\n").unwrap();
        if git_init(&["add", "-A"])
            .map(|s| !s.success())
            .unwrap_or(true)
        {
            return;
        }
        if git_init(&["commit", "-q", "-m", "add both"])
            .map(|s| !s.success())
            .unwrap_or(true)
        {
            return;
        }
        // Delete dead.rs and commit again, so it has history but is absent at HEAD.
        fs::remove_file(&dead_file).unwrap();
        let _ = git_init(&["add", "-A"]);
        if git_init(&["commit", "-q", "-m", "remove dead"])
            .map(|s| !s.success())
            .unwrap_or(true)
        {
            return;
        }

        let conn = open();
        let files_json = serde_json::to_string(&[
            live_file.to_string_lossy().to_string(),
            dead_file.to_string_lossy().to_string(),
        ])
        .unwrap();
        insert_reflection(
            &conn,
            "ep-1",
            &v2_episode(
                "sess-1",
                "proj",
                "2026-08-20T00:00:00+00:00",
                "partial",
                "[]",
                &files_json,
                None,
            ),
            "2026-08-20T00:00:01+00:00",
        );
        materialize_episode_index(&conn).unwrap();
        fill_aliveness(&conn).unwrap();

        let present: Option<i64> = conn
            .query_row(
                "SELECT present_at_head FROM episode_index WHERE episode_id = 'ep-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // live.rs is still at HEAD, so the episode-level aggregate is "present".
        assert_eq!(present, Some(1));

        let days: Option<i64> = conn
            .query_row(
                "SELECT days_since_last_touch FROM episode_index WHERE episode_id = 'ep-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            days,
            Some(0),
            "both commits just ran, so the freshest touch is today"
        );
    }

    #[test]
    fn aggregate_aliveness_prefers_any_present_and_minimum_days() {
        let per_file = vec![(Some(false), Some(90)), (Some(true), Some(3)), (None, None)];
        let (present, days) = aggregate_aliveness(&per_file);
        assert_eq!(present, Some(true));
        assert_eq!(days, Some(3));

        let all_absent = vec![(Some(false), Some(90)), (Some(false), Some(120))];
        let (present, days) = aggregate_aliveness(&all_absent);
        assert_eq!(present, Some(false));
        assert_eq!(days, Some(90));

        let all_unknown = vec![(None, None), (None, None)];
        let (present, days) = aggregate_aliveness(&all_unknown);
        assert_eq!(present, None);
        assert_eq!(days, None);

        let empty: Vec<(Option<bool>, Option<i64>)> = vec![];
        let (present, days) = aggregate_aliveness(&empty);
        assert_eq!(present, None);
        assert_eq!(days, None);
    }

    #[test]
    fn prev_chain_pairs_walks_multi_hop_session_resumes() {
        let conn = open();
        // sess-1 (root) -> sess-2 (resumes sess-1) -> sess-3 (resumes sess-2).
        insert_reflection(
            &conn,
            "ep-1",
            &v2_episode(
                "sess-1",
                "proj",
                "2026-08-18T00:00:00+00:00",
                "partial",
                "[]",
                "[]",
                None,
            ),
            "2026-08-18T00:00:01+00:00",
        );
        insert_reflection(
            &conn,
            "ep-2",
            &v2_episode(
                "sess-2",
                "proj",
                "2026-08-19T00:00:00+00:00",
                "partial",
                "[]",
                "[]",
                Some("sess-1"),
            ),
            "2026-08-19T00:00:01+00:00",
        );
        insert_reflection(
            &conn,
            "ep-3",
            &v2_episode(
                "sess-3",
                "proj",
                "2026-08-20T00:00:00+00:00",
                "completed",
                "[]",
                "[]",
                Some("sess-2"),
            ),
            "2026-08-20T00:00:01+00:00",
        );
        // Unrelated episode in the same project — must not appear in any pair.
        insert_reflection(
            &conn,
            "ep-unrelated",
            &v2_episode(
                "sess-x",
                "proj",
                "2026-08-19T12:00:00+00:00",
                "partial",
                "[]",
                "[]",
                None,
            ),
            "2026-08-19T12:00:01+00:00",
        );
        materialize_episode_index(&conn).unwrap();

        let mut pairs = prev_chain_pairs(&conn, "proj").unwrap();
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                ("ep-1".to_string(), "ep-2".to_string()),
                ("ep-1".to_string(), "ep-3".to_string()),
                ("ep-2".to_string(), "ep-3".to_string()),
            ]
        );
    }

    #[test]
    fn prev_chain_pairs_terminates_on_a_cycle() {
        let conn = open();
        // sess-a and sess-b each claim the other as prev_episode_id.
        insert_reflection(
            &conn,
            "ep-a",
            &v2_episode(
                "sess-a",
                "proj",
                "2026-08-18T00:00:00+00:00",
                "partial",
                "[]",
                "[]",
                Some("sess-b"),
            ),
            "2026-08-18T00:00:01+00:00",
        );
        insert_reflection(
            &conn,
            "ep-b",
            &v2_episode(
                "sess-b",
                "proj",
                "2026-08-19T00:00:00+00:00",
                "partial",
                "[]",
                "[]",
                Some("sess-a"),
            ),
            "2026-08-19T00:00:01+00:00",
        );
        materialize_episode_index(&conn).unwrap();

        // Must return promptly (depth-capped), not hang or error.
        let mut pairs = prev_chain_pairs(&conn, "proj").unwrap();
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                ("ep-a".to_string(), "ep-b".to_string()),
                ("ep-b".to_string(), "ep-a".to_string())
            ]
        );
    }

    #[test]
    fn prev_chain_pairs_scopes_to_the_requested_project() {
        let conn = open();
        insert_reflection(
            &conn,
            "ep-1",
            &v2_episode(
                "sess-1",
                "proj-a",
                "2026-08-18T00:00:00+00:00",
                "partial",
                "[]",
                "[]",
                None,
            ),
            "2026-08-18T00:00:01+00:00",
        );
        // Same session_id string reused in a different project — must not
        // cross-link into proj-a's chain.
        insert_reflection(
            &conn,
            "ep-2",
            &v2_episode(
                "sess-1",
                "proj-b",
                "2026-08-19T00:00:00+00:00",
                "completed",
                "[]",
                "[]",
                None,
            ),
            "2026-08-19T00:00:01+00:00",
        );
        materialize_episode_index(&conn).unwrap();

        let pairs_a = prev_chain_pairs(&conn, "proj-a").unwrap();
        let pairs_b = prev_chain_pairs(&conn, "proj-b").unwrap();
        assert!(pairs_a.is_empty());
        assert!(pairs_b.is_empty());
    }

    #[test]
    fn prev_chain_pairs_also_matches_the_episode_id_domain() {
        // Imported/historical rows can carry the descendant's `episode_id`
        // directly in `prev_episode_id` instead of the session_id the
        // current hooks write (P1) — the fixture shape is literally
        // `ep-007.prev_episode_id = 'ep-006'` (an episode_id, not a
        // session_id — no session named "ep-006" exists here at all).
        let conn = open();
        insert_reflection(
            &conn,
            "ep-006",
            &v2_episode(
                "s-04",
                "proj",
                "2026-04-02T13:00:00+00:00",
                "partial",
                "[]",
                "[]",
                None,
            ),
            "2026-04-02T13:00:01+00:00",
        );
        insert_reflection(
            &conn,
            "ep-007",
            &v2_episode(
                "s-04b",
                "proj",
                "2026-04-03T09:00:00+00:00",
                "completed",
                "[]",
                "[]",
                Some("ep-006"),
            ),
            "2026-04-03T09:00:01+00:00",
        );
        materialize_episode_index(&conn).unwrap();

        let pairs = prev_chain_pairs(&conn, "proj").unwrap();
        assert_eq!(
            pairs,
            vec![("ep-006".to_string(), "ep-007".to_string())],
            "the episode-id-shaped prev_episode_id must still close the chain"
        );
    }

    #[test]
    fn episode_vectors_returns_only_registered_episode_ids() {
        let conn = open();
        insert_reflection(
            &conn,
            "ep-1",
            &v2_episode(
                "sess-1",
                "proj",
                "2026-08-20T00:00:00+00:00",
                "partial",
                "[]",
                "[]",
                None,
            ),
            "2026-08-20T00:00:01+00:00",
        );
        materialize_episode_index(&conn).unwrap();

        conn.execute(
            "INSERT INTO reflection_embeddings (reflection_id, embedding) VALUES ('ep-1', ?1)",
            params![vec_to_bytes(&[1.0, 2.0, 3.0])],
        )
        .unwrap();
        // A non-episode reflection's vector must not leak into the join.
        insert_reflection(
            &conn,
            "note-1",
            r#"{"schema":"v2"}"#,
            "2026-08-20T00:00:01+00:00",
        );
        conn.execute(
            "INSERT INTO reflection_embeddings (reflection_id, embedding) VALUES ('note-1', ?1)",
            params![vec_to_bytes(&[9.0, 9.0, 9.0])],
        )
        .unwrap();

        let vectors = episode_vectors(&conn).unwrap();
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].0, "ep-1");
        assert_eq!(vectors[0].1, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn merge_near_duplicates_is_a_documented_no_op_for_now() {
        let rows = vec![EpisodeIndexRow {
            episode_id: "ep-1".into(),
            session_id: "sess-1".into(),
            project: "proj".into(),
            ts: "2026-08-20T00:00:00+00:00".into(),
            outcome: "partial".into(),
            request: String::new(),
            completed: String::new(),
            next_steps: None,
            blockers: None,
            todo_count: 0,
            files_json: "[]".into(),
            anchors_json: "[]".into(),
            prev_episode_id: None,
        }];
        let out = merge_near_duplicates(rows.clone());
        assert_eq!(out, rows);
    }

    fn vec_to_bytes(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }
}
