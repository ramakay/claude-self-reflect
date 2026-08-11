//! Journal v3 "Dreams Gateway" Phase 1 — read-only storage feeds.
//!
//! Three queries feed the (future) dreams home/detail views:
//!
//! - [`load_dream_items`] — open todos/blockers the night pass actually
//!   touched, gated by ≥1 matching night-pass artifact (a `witness_verdicts`
//!   row or a `resolution_proposals` row). Two-channel grading per the plan's
//!   probe findings (`.plans/journal-v3-dreams-gateway.md`): **item-grade**
//!   when the item's own text names a symbol/file codegraph resolves,
//!   **session-grade** when only the item's origin session's touched files
//!   overlap witnessed ground. Naive substring matching was measured to
//!   produce junk ("cand", "Phase", "GOLD" matching plain English inside real
//!   symbol names) and is banned — every match here is a whole-token
//!   comparison against a structurally code-shaped token (backtick span,
//!   snake_case, multi-hump CamelCase, or a path ending in a known code
//!   extension).
//! - [`load_churn`] — measured per-file touch counts (`files_modified`
//!   occurrences across a project's recent v2 episodes), for the heat-map
//!   panel.
//! - [`load_anchor_tree`] — verdict-bearing symbols grouped by file, for the
//!   AST panel's solid-node layer.
//!
//! All three are read-only and return `Result`; on a storage error the
//! `?` propagates as `Err`, never a panic — the same contract as every other
//! `storage::*` reader (`witness_verdicts`, `dream_report`). Malformed
//! per-row JSON (a single corrupt episode) is skipped in place rather than
//! failing the whole query, so one bad row degrades that row's evidence, not
//! the caller.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Which channel qualified a [`DreamItem`] — see the module doc's two-channel
/// summary. `Ord` is derived so `ItemGrade < SessionGrade`, matching the
/// plan's required render order ("item-grade ranked above").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DreamItemGrade {
    ItemGrade,
    SessionGrade,
}

/// One piece of night-pass evidence backing a [`DreamItem`] — either a
/// grouped `witness_verdicts` row (`file`/`symbol` set, `verdict` one of the
/// `witness_verdicts.verdict` CHECK values) or an open `resolution_proposals`
/// match (`verdict = "proposal"`, `symbol: None`, `file: ""`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamEvidence {
    pub symbol: Option<String>,
    pub file: String,
    pub verdict: String,
    pub receipt_oid: Option<String>,
    pub witnessed_at: String,
}

/// One dream-gated open item: an incomplete todo or a non-trivial blocker
/// from a v2 episode, qualified by ≥1 [`DreamEvidence`] row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamItem {
    /// First 16 hex chars of `sha256(project || "\0" || normalized item
    /// text)` — stable across re-renders so deep links (`#/item/<id>`)
    /// survive a re-run of this query.
    pub id: String,
    pub project: String,
    pub item: String,
    /// `"todo"` or `"blocker"`.
    pub kind: String,
    pub origin_session: String,
    pub origin_ts: String,
    pub grade: DreamItemGrade,
    /// Deduped on `(symbol, verdict, receipt_oid)`, capped at 8, newest
    /// (`witnessed_at` desc) first.
    pub evidence: Vec<DreamEvidence>,
}

/// One file's measured touch count for the churn heat map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChurnTile {
    pub file: String,
    pub touches: u32,
}

// --- episode JSON (v2 schema) -----------------------------------------------
//
// Mirrors `dream_report.rs`'s `EpisodeJson` precedent: a local, deliberately
// narrow `#[serde(default)]` projection of the fields this module needs, so
// a missing/renamed field degrades to the empty default instead of failing
// deserialization of the whole row.

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EpisodeRecord {
    session_id: String,
    project: String,
    timestamp: String,
    todos: Vec<EpisodeTodo>,
    files_modified: Vec<String>,
    investigated: Vec<String>,
    blockers: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct EpisodeTodo {
    content: String,
    status: String,
}

struct EpisodeRow {
    record: EpisodeRecord,
    /// `record.timestamp` when non-blank, else the reflection row's own
    /// `timestamp` — same resolution order as `dream_report::load_story_sessions`.
    origin_ts: String,
    /// `julianday` of the same resolved timestamp — the comparable sort key.
    origin_ts_julian: f64,
}

/// `(!value.trim().is_empty()).then_some(value)` — same helper as
/// `dream_report.rs`'s `nonblank`, duplicated locally to keep this module
/// dependency-free of that one (both are private to their own file).
fn nonblank(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn load_v2_episodes(conn: &Connection) -> Result<Vec<EpisodeRow>> {
    let mut stmt = conn.prepare(
        "SELECT content, timestamp,
                COALESCE(julianday(json_extract(content, '$.timestamp')), julianday(timestamp), 0.0)
         FROM reflections
         WHERE json_valid(content) AND json_extract(content, '$.schema') = 'v2'
         ORDER BY rowid",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out = Vec::with_capacity(rows.len());
    for (content, reflection_ts, sort_key) in rows {
        // Fail-open: a single malformed episode row is skipped, not fatal —
        // no `store_reflection` write plausibly holds `schema=v2` without
        // being written by our own episode composer, but a corpus can carry
        // partial/legacy rows.
        let Ok(record) = serde_json::from_str::<EpisodeRecord>(&content) else {
            continue;
        };
        if record.session_id.trim().is_empty() {
            continue;
        }
        let origin_ts = if record.timestamp.trim().is_empty() {
            reflection_ts
        } else {
            record.timestamp.clone()
        };
        out.push(EpisodeRow {
            record,
            origin_ts,
            origin_ts_julian: sort_key,
        });
    }
    Ok(out)
}

// --- code-grade token extraction (channel A) --------------------------------
//
// Deliberately stricter than `search::reinstatement::query_identifier_candidates`
// (which treats any single-capitalized word like "Phase" as CamelCase-shaped
// and would reproduce the probe's banned false positives) — every predicate
// here requires internal structure a plain English word does not have.

const CODE_EXTENSIONS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "java", "rb", "c", "cc", "cpp", "h",
    "hpp", "cs", "swift", "kt", "php", "scala", "sh", "sql",
];

/// `foo_bar`, `at_least_two_parts` — at least two non-empty underscore
/// segments, no other punctuation.
fn is_snake_case_token(word: &str) -> bool {
    if !word.contains('_') {
        return false;
    }
    if !word.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return false;
    }
    word.split('_').filter(|s| !s.is_empty()).count() >= 2
}

/// At least two "humps" — a hump is the implicit first segment plus one for
/// every lowercase/digit-to-uppercase transition. A single leading capital
/// ("Phase") is exactly one hump and is deliberately rejected; `FooBar` /
/// `handleClick` (two humps each) qualify.
fn is_camel_case_token(word: &str) -> bool {
    if !word.chars().all(|c| c.is_alphanumeric()) {
        return false;
    }
    let has_upper = word.chars().any(|c| c.is_uppercase());
    let has_lower = word.chars().any(|c| c.is_lowercase());
    if !has_upper || !has_lower {
        return false;
    }
    let chars: Vec<char> = word.chars().collect();
    let mut humps = 1usize;
    for i in 1..chars.len() {
        if chars[i].is_uppercase() && !chars[i - 1].is_uppercase() {
            humps += 1;
        }
    }
    humps >= 2
}

/// A file-path-like token ending in a known code extension (`src/foo.rs`,
/// bare `foo.rs`, `module/thing.py`, ...).
fn is_code_path_token(word: &str) -> bool {
    if !word
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '/' | '.' | '-'))
    {
        return false;
    }
    let lower = word.to_lowercase();
    let Some(ext) = lower.rsplit('.').next() else {
        return false;
    };
    if ext == lower {
        return false; // no '.' present at all
    }
    CODE_EXTENSIONS.contains(&ext)
}

/// Extract code-grade tokens from free text: backtick spans verbatim, plus
/// any whitespace-delimited word that is structurally snake_case, multi-hump
/// CamelCase, or a code-extension path. Plain prose words never qualify —
/// this is the ban on the naive substring matching the probe measured
/// ("cand", "Phase", "GOLD" matching real symbol names). `pub(crate)` so
/// `dream::threads` (Journal v3 Phase 1.5) can reuse the same code-grade
/// token extractor for its receipt join.
pub(crate) fn extract_code_tokens(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();

    let mut rest = text;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else {
            break;
        };
        let inner = after[..end].trim();
        if !inner.is_empty() {
            tokens.push(inner.to_string());
        }
        rest = &after[end + 1..];
    }

    for raw in text.split(|c: char| c.is_whitespace()) {
        let trimmed = raw.trim_matches(|c: char| {
            matches!(
                c,
                '`' | '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '?' | '!'
            )
        });
        let word = trimmed.trim_end_matches('.');
        if word.is_empty() {
            continue;
        }
        if is_snake_case_token(word) || is_camel_case_token(word) || is_code_path_token(word) {
            tokens.push(word.to_string());
        }
    }

    tokens.sort();
    tokens.dedup();
    tokens
}

/// Case-insensitive whole-token match: `token` equals `row`'s symbol name,
/// equals `row`'s file basename, or (for a token that itself looks like a
/// path) `row`'s file path ends with `token`. Never a substring match.
fn token_matches_row(token: &str, row: &VerdictGroupRow) -> bool {
    if let Some(symbol) = &row.symbol {
        if symbol.eq_ignore_ascii_case(token) {
            return true;
        }
    }
    let basename = row.file.rsplit('/').next().unwrap_or(&row.file);
    if basename.eq_ignore_ascii_case(token) {
        return true;
    }
    if token.contains('/') && row.file.to_lowercase().ends_with(&token.to_lowercase()) {
        return true;
    }
    false
}

/// Whole-word case-insensitive containment: does `claim` mention `token` as
/// its own word (never as a substring of a longer word)?
fn claim_contains_token(claim: &str, token: &str) -> bool {
    claim
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | ':' | '/' | '.' | '-')))
        .any(|word| !word.is_empty() && word.eq_ignore_ascii_case(token))
}

/// Last two non-empty `/`- or `\`-separated path segments, lowercased — the
/// channel-B / anchor-tree file-identity comparator (a session's
/// `files_modified` entry and a witness's absolute repo path rarely share a
/// root, but the trailing `dir/file.ext` almost always matches). `pub(crate)`
/// so `dream::threads` (Journal v3 Phase 1.5) can reuse the same file-identity
/// comparator for its receipt join instead of duplicating it.
pub(crate) fn last_two_segments(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let n = parts.len();
    let slice = if n >= 2 { &parts[n - 2..] } else { &parts[..] };
    slice.join("/").to_lowercase()
}

fn stable_id(project: &str, item: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(project.as_bytes());
    hasher.update(b"\0");
    hasher.update(item.trim().to_lowercase().as_bytes());
    let digest = hasher.finalize();
    digest
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

// --- verdict evidence base ---------------------------------------------------

/// `pub(crate)` (fields too) so `dream::threads` (Journal v3 Phase 1.5) can
/// reuse [`verdict_rows_for_project`] for its own receipt-tier join instead
/// of re-deriving the same `witness_verdicts JOIN witness_ledger` query.
#[derive(Debug, Clone)]
pub(crate) struct VerdictGroupRow {
    pub(crate) file: String,
    pub(crate) symbol: Option<String>,
    pub(crate) verdict: String,
    pub(crate) receipt_oid: Option<String>,
    pub(crate) witnessed_at: String,
}

/// `witness_verdicts v JOIN witness_ledger l ON l.id = v.witness_id`, grouped
/// to `(file, symbol, verdict, receipt_oid, MAX(created_at))` for one
/// project — the shared evidence base channels A and B (and the anchor tree)
/// match against.
pub(crate) fn verdict_rows_for_project(
    conn: &Connection,
    project: &str,
) -> Result<Vec<VerdictGroupRow>> {
    let mut stmt = conn.prepare(
        "SELECT l.file, l.symbol, v.verdict, v.receipt_oid, MAX(v.created_at) AS witnessed_at
         FROM witness_verdicts v
         JOIN witness_ledger l ON l.id = v.witness_id
         WHERE l.project = ?1
         GROUP BY l.file, l.symbol, v.verdict, v.receipt_oid
         ORDER BY witnessed_at DESC",
    )?;
    let rows = stmt
        .query_map(params![project], |row| {
            Ok(VerdictGroupRow {
                file: row.get(0)?,
                symbol: row.get(1)?,
                verdict: row.get(2)?,
                receipt_oid: row.get(3)?,
                witnessed_at: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

struct ProposalRow {
    claim: Option<String>,
    session_id: String,
    created_at: String,
}

fn load_all_proposals(conn: &Connection) -> Result<Vec<ProposalRow>> {
    let mut stmt =
        conn.prepare("SELECT claim, session_id, created_at FROM resolution_proposals")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ProposalRow {
                claim: row.get(0)?,
                session_id: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn dedup_and_cap_evidence(evidence: &mut Vec<DreamEvidence>) {
    let mut seen: HashSet<(Option<String>, String, Option<String>)> = HashSet::new();
    evidence.retain(|e| seen.insert((e.symbol.clone(), e.verdict.clone(), e.receipt_oid.clone())));
    evidence.sort_by(|a, b| b.witnessed_at.cmp(&a.witnessed_at));
    evidence.truncate(8);
}

/// One open-item candidate before evidence gating — a pending todo or a
/// non-trivial blocker from one v2 episode.
struct Candidate {
    project: String,
    item: String,
    kind: &'static str,
    origin_session: String,
    origin_ts: String,
    origin_ts_julian: f64,
    /// This episode's `files_modified` + `investigated`, deduped — channel
    /// B's match set.
    channel_b_files: Vec<String>,
}

const MAX_DREAM_ITEMS: usize = 60;

/// Load every dream-gated open item — see the module doc for the full
/// two-channel gate, grading, dedupe, and ordering contract.
pub fn load_dream_items(conn: &Connection) -> Result<Vec<DreamItem>> {
    let episodes = load_v2_episodes(conn)?;
    if episodes.is_empty() {
        return Ok(Vec::new());
    }

    // Superseded-todo suppression: (project, normalized todo text) -> the
    // most recent origin_ts_julian at which that text was marked completed.
    let mut completed_latest: BTreeMap<(String, String), f64> = BTreeMap::new();
    for ep in &episodes {
        for todo in &ep.record.todos {
            if todo.status != "completed" {
                continue;
            }
            let Some(text) = nonblank(todo.content.clone()) else {
                continue;
            };
            let key = (ep.record.project.clone(), text.trim().to_lowercase());
            completed_latest
                .entry(key)
                .and_modify(|v| {
                    if ep.origin_ts_julian > *v {
                        *v = ep.origin_ts_julian;
                    }
                })
                .or_insert(ep.origin_ts_julian);
        }
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    for ep in &episodes {
        if ep.record.session_id.trim().is_empty() {
            continue;
        }
        let channel_b_files: Vec<String> = ep
            .record
            .files_modified
            .iter()
            .chain(ep.record.investigated.iter())
            .filter(|f| !f.trim().is_empty())
            .cloned()
            .collect();

        for todo in &ep.record.todos {
            if todo.status != "pending" {
                continue;
            }
            let Some(text) = nonblank(todo.content.clone()) else {
                continue;
            };
            let key = (ep.record.project.clone(), text.trim().to_lowercase());
            if let Some(&latest_completed) = completed_latest.get(&key) {
                if latest_completed > ep.origin_ts_julian {
                    continue; // a newer episode already completed this text — stale.
                }
            }
            candidates.push(Candidate {
                project: ep.record.project.clone(),
                item: text,
                kind: "todo",
                origin_session: ep.record.session_id.clone(),
                origin_ts: ep.origin_ts.clone(),
                origin_ts_julian: ep.origin_ts_julian,
                channel_b_files: channel_b_files.clone(),
            });
        }

        if let Some(blockers) = ep.record.blockers.clone().and_then(nonblank) {
            if blockers.trim().to_lowercase() != "none" {
                candidates.push(Candidate {
                    project: ep.record.project.clone(),
                    item: blockers,
                    kind: "blocker",
                    origin_session: ep.record.session_id.clone(),
                    origin_ts: ep.origin_ts.clone(),
                    origin_ts_julian: ep.origin_ts_julian,
                    channel_b_files,
                });
            }
        }
    }

    // Dedupe case-insensitively on (project, normalized item text); newest
    // origin wins. `dedup` iterates candidates in query order (`ORDER BY
    // rowid`), so ties (equal origin_ts_julian) deterministically keep the
    // first-seen row.
    let mut dedup: BTreeMap<(String, String), Candidate> = BTreeMap::new();
    for c in candidates {
        let key = (c.project.clone(), c.item.trim().to_lowercase());
        let replace = match dedup.get(&key) {
            Some(existing) => c.origin_ts_julian > existing.origin_ts_julian,
            None => true,
        };
        if replace {
            dedup.insert(key, c);
        }
    }

    let proposals = load_all_proposals(conn)?;
    let mut verdict_cache: HashMap<String, Vec<VerdictGroupRow>> = HashMap::new();

    let mut items: Vec<DreamItem> = Vec::new();
    for candidate in dedup.into_values() {
        let tokens = extract_code_tokens(&candidate.item);
        let verdict_rows = match verdict_cache.get(&candidate.project) {
            Some(rows) => rows,
            None => {
                let rows = verdict_rows_for_project(conn, &candidate.project)?;
                verdict_cache
                    .entry(candidate.project.clone())
                    .or_insert(rows)
            }
        };

        let mut evidence: Vec<DreamEvidence> = Vec::new();
        let mut has_channel_a = false;
        let mut has_channel_b = false;

        if !tokens.is_empty() {
            for row in verdict_rows {
                if tokens.iter().any(|t| token_matches_row(t, row)) {
                    has_channel_a = true;
                    evidence.push(DreamEvidence {
                        symbol: row.symbol.clone(),
                        file: row.file.clone(),
                        verdict: row.verdict.clone(),
                        receipt_oid: row.receipt_oid.clone(),
                        witnessed_at: row.witnessed_at.clone(),
                    });
                }
            }
        }

        if !candidate.channel_b_files.is_empty() {
            let targets: Vec<String> = candidate
                .channel_b_files
                .iter()
                .map(|f| last_two_segments(f))
                .collect();
            for row in verdict_rows {
                let row_key = last_two_segments(&row.file);
                if targets.contains(&row_key) {
                    has_channel_b = true;
                    evidence.push(DreamEvidence {
                        symbol: row.symbol.clone(),
                        file: row.file.clone(),
                        verdict: row.verdict.clone(),
                        receipt_oid: row.receipt_oid.clone(),
                        witnessed_at: row.witnessed_at.clone(),
                    });
                }
            }
        }

        // The item qualifies iff channel A or B fired. Proposal evidence is
        // additive only — it never qualifies an item on its own (plan §Data
        // Phase 1: "Item qualifies iff ≥1 [channel A/B] match").
        if !has_channel_a && !has_channel_b {
            continue;
        }

        for proposal in &proposals {
            let matched = proposal.session_id == candidate.origin_session
                || tokens
                    .iter()
                    .any(|t| claim_contains_token(proposal.claim.as_deref().unwrap_or(""), t));
            if matched {
                evidence.push(DreamEvidence {
                    symbol: None,
                    file: String::new(),
                    verdict: "proposal".to_string(),
                    receipt_oid: None,
                    witnessed_at: proposal.created_at.clone(),
                });
            }
        }

        dedup_and_cap_evidence(&mut evidence);

        let grade = if has_channel_a {
            DreamItemGrade::ItemGrade
        } else {
            DreamItemGrade::SessionGrade
        };

        items.push(DreamItem {
            id: stable_id(&candidate.project, &candidate.item),
            project: candidate.project,
            item: candidate.item,
            kind: candidate.kind.to_string(),
            origin_session: candidate.origin_session,
            origin_ts: candidate.origin_ts,
            grade,
            evidence,
        });
    }

    items.sort_by(|a, b| {
        a.grade.cmp(&b.grade).then_with(|| {
            let a_wa = a
                .evidence
                .first()
                .map(|e| e.witnessed_at.as_str())
                .unwrap_or("");
            let b_wa = b
                .evidence
                .first()
                .map(|e| e.witnessed_at.as_str())
                .unwrap_or("");
            b_wa.cmp(a_wa).then_with(|| a.item.cmp(&b.item))
        })
    });
    items.truncate(MAX_DREAM_ITEMS);

    Ok(items)
}

const MAX_CHURN_TILES: usize = 12;

/// Per-file touch counts (`files_modified` occurrences) across `project`'s v2
/// episodes within the last `window_days` days — the heat-map feed. Measured
/// or absent, never estimated: a file with zero occurrences in-window simply
/// does not appear.
pub fn load_churn(conn: &Connection, project: &str, window_days: u32) -> Result<Vec<ChurnTile>> {
    let mut stmt = conn.prepare(
        "SELECT content
         FROM reflections
         WHERE json_valid(content)
           AND json_extract(content, '$.schema') = 'v2'
           AND json_extract(content, '$.project') = ?1
           AND COALESCE(julianday(json_extract(content, '$.timestamp')), julianday(timestamp), 0.0)
               >= julianday('now') - ?2",
    )?;
    let contents: Vec<String> = stmt
        .query_map(params![project, f64::from(window_days)], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for content in contents {
        let Ok(record) = serde_json::from_str::<EpisodeRecord>(&content) else {
            continue;
        };
        for file in record.files_modified {
            if file.trim().is_empty() {
                continue;
            }
            *counts.entry(file).or_insert(0) += 1;
        }
    }

    let mut tiles: Vec<ChurnTile> = counts
        .into_iter()
        .map(|(file, touches)| ChurnTile { file, touches })
        .collect();
    tiles.sort_by(|a, b| b.touches.cmp(&a.touches).then_with(|| a.file.cmp(&b.file)));
    tiles.truncate(MAX_CHURN_TILES);
    Ok(tiles)
}

/// Verdict-bearing symbols grouped by file, for `files` (matched via
/// [`last_two_segments`]) — the AST panel's solid-node layer. Whole-file
/// witnesses (`symbol IS NULL`) are excluded — a symbol tree node needs a
/// name. Deterministic order: files ascending, then symbol ascending, then
/// verdict ascending within a symbol.
#[allow(clippy::type_complexity)]
pub fn load_anchor_tree(
    conn: &Connection,
    project: &str,
    files: &[String],
) -> Result<Vec<(String, Vec<(String, Option<String>, String)>)>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let rows = verdict_rows_for_project(conn, project)?;
    let targets: Vec<String> = files.iter().map(|f| last_two_segments(f)).collect();

    let mut grouped: BTreeMap<String, Vec<(String, Option<String>, String)>> = BTreeMap::new();
    for row in rows {
        let Some(symbol) = row.symbol.clone() else {
            continue;
        };
        let row_key = last_two_segments(&row.file);
        if targets.contains(&row_key) {
            grouped.entry(row.file.clone()).or_default().push((
                symbol,
                row.receipt_oid.clone(),
                row.verdict.clone(),
            ));
        }
    }

    let mut out: Vec<(String, Vec<(String, Option<String>, String)>)> =
        grouped.into_iter().collect();
    for (_, symbols) in out.iter_mut() {
        symbols.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.2.cmp(&b.2)));
        symbols.dedup();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::witness_ledger::{self, WitnessLedgerRow};
    use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};

    fn open() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        conn
    }

    fn insert_episode(conn: &Connection, id: &str, json: &str, timestamp: &str) {
        conn.execute(
            "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, '[]', ?3)",
            params![id, json, timestamp],
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_witness_with_verdict(
        conn: &Connection,
        project: &str,
        file: &str,
        symbol: Option<&str>,
        at_oid: &str,
        stamp: &str,
        verdict: VerdictKind,
        receipt_oid: &str,
    ) {
        witness_ledger::insert_witness(
            conn,
            &WitnessLedgerRow {
                id: 0,
                project: project.into(),
                file: file.into(),
                symbol: symbol.map(|s| s.to_string()),
                span_start: Some(1),
                span_end: Some(3),
                stamp: stamp.into(),
                tier: "committed".into(),
                at_oid: Some(at_oid.into()),
                source_kind: "backfill".into(),
                source_id: Some(at_oid.into()),
            },
        )
        .unwrap();
        let witness_id: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE project = ?1 AND file = ?2 AND stamp = ?3",
                params![project, file, stamp],
                |r| r.get(0),
            )
            .unwrap();
        witness_verdicts::insert_verdict_if_changed(
            conn,
            &WitnessVerdictRow {
                witness_id,
                verdict,
                successor_witness_id: None,
                receipt_oid: Some(receipt_oid.into()),
                observed_head_oid: receipt_oid.into(),
            },
        )
        .unwrap();
    }

    // ---- channel A: exact-token qualification ------------------------------

    #[test]
    fn channel_a_backtick_token_qualifies_item_grade() {
        let conn = open();
        insert_episode(
            &conn,
            "ep-1",
            r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"Fix `parse_config` before ship","status":"pending"}]}"#,
            "2026-01-01T00:00:00Z",
        );
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/repo/src/config.rs",
            Some("parse_config"),
            "aaa",
            "b3:1",
            VerdictKind::SupersededBy,
            "aaa",
        );

        let items = load_dream_items(&conn).unwrap();
        assert_eq!(items.len(), 1, "one qualifying item expected: {items:?}");
        assert_eq!(items[0].grade, DreamItemGrade::ItemGrade);
        assert_eq!(items[0].evidence.len(), 1);
        assert_eq!(items[0].evidence[0].symbol.as_deref(), Some("parse_config"));
    }

    // ---- channel A rejects plain-word substring ----------------------------

    #[test]
    fn channel_a_rejects_plain_word_phase_does_not_match_symbol() {
        let conn = open();
        insert_episode(
            &conn,
            "ep-1",
            r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"Phase 2 decision","status":"pending"}]}"#,
            "2026-01-01T00:00:00Z",
        );
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/repo/src/phases.rs",
            Some("Phase"),
            "aaa",
            "b3:1",
            VerdictKind::SupersededBy,
            "aaa",
        );

        let items = load_dream_items(&conn).unwrap();
        assert!(
            items.is_empty(),
            "a plain capitalized English word must never whole-token-match a symbol: {items:?}"
        );
    }

    // ---- channel B: files_modified -----------------------------------------

    #[test]
    fn channel_b_files_modified_qualifies_session_grade() {
        let conn = open();
        insert_episode(
            &conn,
            "ep-1",
            r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"clean up the leftover mess","status":"pending"}],"files_modified":["/repo/src/storage/queries.rs"]}"#,
            "2026-01-01T00:00:00Z",
        );
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/other/checkout/src/storage/queries.rs",
            Some("run_query"),
            "aaa",
            "b3:1",
            VerdictKind::SupersededBy,
            "aaa",
        );

        let items = load_dream_items(&conn).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].grade, DreamItemGrade::SessionGrade);
        assert_eq!(items[0].evidence.len(), 1);
        assert_eq!(items[0].evidence[0].symbol.as_deref(), Some("run_query"));
    }

    // ---- grade precedence: A beats B when both fire ------------------------

    #[test]
    fn grade_precedence_item_grade_wins_when_both_channels_fire() {
        let conn = open();
        insert_episode(
            &conn,
            "ep-1",
            r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"pending"}],"files_modified":["/repo/src/storage/queries.rs"]}"#,
            "2026-01-01T00:00:00Z",
        );
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/other/checkout/src/storage/queries.rs",
            Some("run_query"),
            "aaa",
            "b3:1",
            VerdictKind::SupersededBy,
            "aaa",
        );

        let items = load_dream_items(&conn).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].grade,
            DreamItemGrade::ItemGrade,
            "channel A firing must win over channel B even though both matched"
        );
    }

    // ---- blocker "none" excluded -------------------------------------------

    #[test]
    fn blocker_none_is_excluded() {
        let conn = open();
        insert_episode(
            &conn,
            "ep-1",
            r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","blockers":"None","files_modified":["/repo/src/storage/queries.rs"]}"#,
            "2026-01-01T00:00:00Z",
        );
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/other/checkout/src/storage/queries.rs",
            Some("run_query"),
            "aaa",
            "b3:1",
            VerdictKind::SupersededBy,
            "aaa",
        );

        let items = load_dream_items(&conn).unwrap();
        assert!(
            items.is_empty(),
            "a literal 'none' blocker must never become an item: {items:?}"
        );
    }

    #[test]
    fn blocker_with_content_qualifies() {
        let conn = open();
        insert_episode(
            &conn,
            "ep-1",
            r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","blockers":"waiting on `run_query` fix upstream"}"#,
            "2026-01-01T00:00:00Z",
        );
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/repo/src/queries.rs",
            Some("run_query"),
            "aaa",
            "b3:1",
            VerdictKind::SupersededBy,
            "aaa",
        );

        let items = load_dream_items(&conn).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "blocker");
    }

    // ---- completed-newer suppression ---------------------------------------

    #[test]
    fn pending_todo_superseded_by_newer_completed_episode_is_dropped() {
        let conn = open();
        insert_episode(
            &conn,
            "ep-older",
            r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"ship the thing","status":"pending"}],"files_modified":["/repo/src/storage/queries.rs"]}"#,
            "2026-01-01T00:00:00Z",
        );
        insert_episode(
            &conn,
            "ep-newer",
            r#"{"schema":"v2","session_id":"sess-2","project":"proj","timestamp":"2026-01-02T00:00:00Z","todos":[{"content":"ship the thing","status":"completed"}]}"#,
            "2026-01-02T00:00:00Z",
        );
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/other/checkout/src/storage/queries.rs",
            Some("run_query"),
            "aaa",
            "b3:1",
            VerdictKind::SupersededBy,
            "aaa",
        );

        let items = load_dream_items(&conn).unwrap();
        assert!(
            items.is_empty(),
            "a pending todo completed in a later episode is stale and must be dropped: {items:?}"
        );
    }

    #[test]
    fn pending_todo_not_suppressed_by_older_completed_episode() {
        let conn = open();
        insert_episode(
            &conn,
            "ep-older-completed",
            r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"ship the thing","status":"completed"}]}"#,
            "2026-01-01T00:00:00Z",
        );
        insert_episode(
            &conn,
            "ep-newer-pending",
            r#"{"schema":"v2","session_id":"sess-2","project":"proj","timestamp":"2026-01-02T00:00:00Z","todos":[{"content":"ship the thing","status":"pending"}],"files_modified":["/repo/src/storage/queries.rs"]}"#,
            "2026-01-02T00:00:00Z",
        );
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/other/checkout/src/storage/queries.rs",
            Some("run_query"),
            "aaa",
            "b3:1",
            VerdictKind::SupersededBy,
            "aaa",
        );

        let items = load_dream_items(&conn).unwrap();
        assert_eq!(
            items.len(),
            1,
            "a pending todo re-opened AFTER an older completion must survive: {items:?}"
        );
    }

    // ---- proposal evidence path ---------------------------------------------

    #[test]
    fn resolution_proposal_matching_session_adds_evidence() {
        let conn = open();
        insert_episode(
            &conn,
            "ep-1",
            r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"pending"}]}"#,
            "2026-01-01T00:00:00Z",
        );
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/repo/src/queries.rs",
            Some("run_query"),
            "aaa",
            "b3:1",
            VerdictKind::SupersededBy,
            "aaa",
        );
        conn.execute(
            "INSERT INTO resolution_proposals (chunk_id, claim, evidence, session_id, created_at)
             VALUES ('chunk-1', 'run_query now paginates', 'ev', 'sess-1', '2026-01-03 00:00:00')",
            [],
        )
        .unwrap();

        let items = load_dream_items(&conn).unwrap();
        assert_eq!(items.len(), 1);
        assert!(
            items[0]
                .evidence
                .iter()
                .any(|e| e.verdict == "proposal" && e.file.is_empty()),
            "a proposal from the item's own origin session must add proposal evidence: {:?}",
            items[0].evidence
        );
    }

    #[test]
    fn resolution_proposal_matching_claim_token_adds_evidence() {
        let conn = open();
        insert_episode(
            &conn,
            "ep-1",
            r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"fix `run_query` regression","status":"pending"}]}"#,
            "2026-01-01T00:00:00Z",
        );
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/repo/src/queries.rs",
            Some("run_query"),
            "aaa",
            "b3:1",
            VerdictKind::SupersededBy,
            "aaa",
        );
        conn.execute(
            "INSERT INTO resolution_proposals (chunk_id, claim, evidence, session_id, created_at)
             VALUES ('chunk-1', 'run_query now paginates', 'ev', 'unrelated-session', '2026-01-03 00:00:00')",
            [],
        )
        .unwrap();

        let items = load_dream_items(&conn).unwrap();
        assert_eq!(items.len(), 1);
        assert!(
            items[0].evidence.iter().any(|e| e.verdict == "proposal"),
            "a proposal whose claim contains the item's own code token must add evidence: {:?}",
            items[0].evidence
        );
    }

    #[test]
    fn resolution_proposal_alone_does_not_qualify_an_item() {
        let conn = open();
        insert_episode(
            &conn,
            "ep-1",
            r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"revisit the onboarding copy","status":"pending"}]}"#,
            "2026-01-01T00:00:00Z",
        );
        conn.execute(
            "INSERT INTO resolution_proposals (chunk_id, claim, evidence, session_id, created_at)
             VALUES ('chunk-1', 'something unrelated', 'ev', 'sess-1', '2026-01-03 00:00:00')",
            [],
        )
        .unwrap();

        let items = load_dream_items(&conn).unwrap();
        assert!(
            items.is_empty(),
            "a proposal match with no channel A/B evidence must not qualify the item: {items:?}"
        );
    }

    // ---- deterministic order --------------------------------------------------

    #[test]
    fn items_order_item_grade_then_witnessed_at_desc_then_text_asc() {
        let conn = open();
        // Three separate episodes so each item's own evidence stays isolated
        // — sharing one episode's `files_modified` would give every todo the
        // same channel-B evidence row and mask the witnessed_at comparison
        // this test is checking.
        insert_episode(
            &conn,
            "ep-zzz",
            r#"{"schema":"v2","session_id":"sess-3","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"zzz session grade item","status":"pending"}],"files_modified":["/repo/src/queries.rs"]}"#,
            "2026-01-01T00:00:00Z",
        );
        insert_episode(
            &conn,
            "ep-aaa",
            r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"aaa `sym_one` item grade older","status":"pending"}]}"#,
            "2026-01-01T00:00:00Z",
        );
        insert_episode(
            &conn,
            "ep-bbb",
            r#"{"schema":"v2","session_id":"sess-2","project":"proj","timestamp":"2026-01-01T00:00:00Z","todos":[{"content":"bbb `sym_two` item grade newer","status":"pending"}]}"#,
            "2026-01-01T00:00:00Z",
        );
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/repo/src/queries.rs",
            None,
            "sess-file",
            "b3:sess",
            VerdictKind::SupersededBy,
            "sess-oid",
        );
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/repo/src/one.rs",
            Some("sym_one"),
            "aaa",
            "b3:1",
            VerdictKind::SupersededBy,
            "aaa",
        );
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/repo/src/two.rs",
            Some("sym_two"),
            "bbb",
            "b3:2",
            VerdictKind::SupersededBy,
            "bbb",
        );
        // Force distinct `created_at` ordering for the two item-grade rows
        // via a direct UPDATE (fixture events would otherwise share the same
        // `datetime('now')` default at test speed).
        conn.execute(
            "UPDATE witness_verdicts SET created_at = '2026-01-01 00:00:00'
             WHERE witness_id = (SELECT id FROM witness_ledger WHERE symbol = 'sym_one')",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE witness_verdicts SET created_at = '2026-01-02 00:00:00'
             WHERE witness_id = (SELECT id FROM witness_ledger WHERE symbol = 'sym_two')",
            [],
        )
        .unwrap();

        let items = load_dream_items(&conn).unwrap();
        assert_eq!(items.len(), 3, "{items:?}");
        assert_eq!(items[0].grade, DreamItemGrade::ItemGrade);
        assert_eq!(items[1].grade, DreamItemGrade::ItemGrade);
        assert_eq!(items[2].grade, DreamItemGrade::SessionGrade);
        assert!(
            items[0].item.starts_with("bbb"),
            "the newer-witnessed item-grade row must come first: {}",
            items[0].item
        );
        assert!(
            items[1].item.starts_with("aaa"),
            "the older-witnessed item-grade row comes second: {}",
            items[1].item
        );
        assert!(items[2].item.starts_with("zzz"));
    }

    // ---- churn ------------------------------------------------------------

    #[test]
    fn churn_counts_files_modified_and_respects_window() {
        let conn = open();
        // Dates relative to the real wall clock (`julianday('now')` inside
        // the query is the actual system clock, not a fixture value), so
        // hardcoded absolute dates would drift out of any window as time
        // passes. `recent1`/`recent2` sit well inside a 3-day window; `old`
        // sits far outside it regardless of when this test runs.
        let now = chrono::Utc::now();
        let recent1 = (now - chrono::Duration::hours(20))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let recent2 = (now - chrono::Duration::hours(4))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        let old = (now - chrono::Duration::days(400))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        insert_episode(
            &conn,
            "ep-recent-1",
            &format!(
                r#"{{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"{recent1}","files_modified":["/repo/tools.rs","/repo/lib.rs"]}}"#
            ),
            &recent1,
        );
        insert_episode(
            &conn,
            "ep-recent-2",
            &format!(
                r#"{{"schema":"v2","session_id":"sess-2","project":"proj","timestamp":"{recent2}","files_modified":["/repo/tools.rs"]}}"#
            ),
            &recent2,
        );
        insert_episode(
            &conn,
            "ep-old",
            &format!(
                r#"{{"schema":"v2","session_id":"sess-3","project":"proj","timestamp":"{old}","files_modified":["/repo/tools.rs","/repo/tools.rs","/repo/tools.rs"]}}"#
            ),
            &old,
        );

        // A wide window pulls in the old episode too — 3 extra tools.rs touches.
        let wide = load_churn(&conn, "proj", 999_999).unwrap();
        let tools_wide = wide.iter().find(|t| t.file == "/repo/tools.rs").unwrap();
        assert_eq!(tools_wide.touches, 5);

        // A narrow window (well under the gap to the old episode) excludes it.
        let narrow = load_churn(&conn, "proj", 3).unwrap();
        let tools_narrow: u32 = narrow
            .iter()
            .find(|t| t.file == "/repo/tools.rs")
            .map(|t| t.touches)
            .unwrap_or(0);
        assert_eq!(tools_narrow, 2, "window must exclude the 2020 episode");
        assert!(narrow.iter().any(|t| t.file == "/repo/lib.rs"));

        // Ordering: touches desc, file asc.
        assert_eq!(wide[0].file, "/repo/tools.rs");
    }

    // ---- anchor tree --------------------------------------------------------

    #[test]
    fn anchor_tree_groups_verdict_bearing_symbols_by_file() {
        let conn = open();
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/checkout/src/storage/queries.rs",
            Some("run_query"),
            "aaa",
            "b3:1",
            VerdictKind::SupersededBy,
            "aaa",
        );
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/checkout/src/storage/queries.rs",
            Some("insert_chunk"),
            "bbb",
            "b3:2",
            VerdictKind::AnchorObsolete,
            "bbb",
        );
        insert_witness_with_verdict(
            &conn,
            "proj",
            "/checkout/src/other.rs",
            Some("unrelated_symbol"),
            "ccc",
            "b3:3",
            VerdictKind::SupersededBy,
            "ccc",
        );

        let tree =
            load_anchor_tree(&conn, "proj", &["src/storage/queries.rs".to_string()]).unwrap();
        assert_eq!(tree.len(), 1, "{tree:?}");
        assert_eq!(tree[0].0, "/checkout/src/storage/queries.rs");
        let symbols: Vec<&str> = tree[0].1.iter().map(|(s, _, _)| s.as_str()).collect();
        assert_eq!(symbols, vec!["insert_chunk", "run_query"]);
    }

    // ---- empty DB -----------------------------------------------------------

    #[test]
    fn empty_database_returns_empty_vecs_without_error() {
        let conn = open();
        assert!(load_dream_items(&conn).unwrap().is_empty());
        assert!(load_churn(&conn, "proj", 28).unwrap().is_empty());
        assert!(load_anchor_tree(&conn, "proj", &["src/lib.rs".to_string()])
            .unwrap()
            .is_empty());
    }
}
