//! Journal v4 Phase 2 — the **consequence-cluster** feed.
//!
//! Locked decision 1 (`.plans/journal-v4-dream-server.md`): the dream unit is
//! not an item card, it is a *consequence cluster* — **one ground-shift
//! conclusion plus every open item it affects**. The codex IA memo's binding
//! correction #1 is the reason: 12 item cards in the v3 comp rendered ~4
//! distinct session-level facts, so the card unit systematically overstated
//! how many independent conclusions the evidence actually supports.
//!
//! # What a cluster is keyed by
//!
//! * **Session-grade** items (the item's own text names nothing codegraph
//!   resolves; it qualified because its origin session's touched files
//!   overlap witnessed ground) are keyed by
//!   `(project, origin session, canonical evidence-set fingerprint)`. The
//!   fingerprint is a stable hash over the *sorted* evidence tuples
//!   (`file`, `symbol`, `verdict`, `receipt_oid`) — identical evidence sets
//!   collapse into one conclusion, different ones stay apart. Timestamps are
//!   deliberately NOT in the fingerprint: `witnessed_at` is a `MAX(created_at)`
//!   aggregate and would split two genuinely identical evidence sets.
//! * **Item-grade** items stay **standalone clusters of one** (codex memo #1,
//!   verbatim: "item-grade matches may stay standalone because their evidence
//!   is genuinely item-specific"). An item-grade item is never folded into its
//!   session's cluster, so it is never double-counted either.
//!
//! # Ranking is explicit tiers, never a synthetic score
//!
//! Codex memo #2. [`ClusterRank`] *is* the ranking: its derived `Ord` compares,
//! in declaration order,
//!
//! 1. grade — item-grade before session-grade;
//! 2. [`VerdictClass`] — receipt-bearing `anchor_obsolete`/`superseded_by`
//!    before restorative `anchor_reinstated`, unreceipted last;
//! 3. newest witnessed **date**, newest first;
//! 4. blocker before todo;
//! 5. oldest open item first.
//!
//! A sixth field, the cluster id, is a determinism backstop so the order is a
//! **total** order under shuffled input — it is not a priority tier and
//! carries no meaning. There is no score, no weighting, and **churn and
//! project activity never enter the comparison** (locked decision 10: churn is
//! context, never importance). Tier 3 compares the ISO *date*, not the full
//! timestamp, precisely so that two conclusions witnessed the same day fall
//! through to the blocker-before-todo tier instead of being separated by a
//! seconds-level difference that means nothing to a reader.
//!
//! # Partitions (locked decision 11 — nothing is deleted)
//!
//! * **Settled** — every item in the cluster was later completed in a *newer*
//!   episode. The [`CompletionReceipt`] naming the episode that recorded the
//!   completion is carried on the cluster and on each item; a cluster is never
//!   called settled without it.
//! * **Archive** — the cluster's newest evidence predates the project's most
//!   recent `witness_generations` publication, i.e. it is a conclusion from an
//!   older pass. The pass it belongs to is the newest generation created at or
//!   before that evidence; when no generation covers it, [`DreamCluster::archive_pass`]
//!   is `None` and the renderer drops the clause — an unnamed pass is never
//!   invented.
//! * **Active** — everything else.
//!
//! Settled is checked before archive: a completed item carries the strongest
//! receipt available and reads as news regardless of which pass witnessed it.
//!
//! # Honesty contract
//!
//! * Every count in [`ClusterCounts`] is a count of rows actually derived.
//!   `counts.items` / `counts.evidence` are the TRUE totals and may exceed
//!   `items.len()` / `evidence.len()`, which are display-capped — a renderer
//!   showing "N items" must use the count, and the difference is the honest
//!   "+N more".
//! * `receipt_oid`, `settled`, and `archive_pass` are `Option`; absence drops
//!   the clause and never becomes a placeholder oid or a fabricated date.
//! * Nothing here is derived from the absence of evidence. A cluster exists
//!   only because ≥1 verdict row matched; an empty database returns empty
//!   vectors, not a zero that means "all clear".
//!
//! # Reuse
//!
//! The evidence gate is the same two-channel gate `dream_items` documents, run
//! through its `pub(crate)` helpers ([`extract_code_tokens`],
//! [`last_two_segments`], [`verdict_rows_for_project`]). Two pieces are
//! re-stated locally because they are private there: the whole-token
//! symbol/file comparator ([`token_matches_verdict_row`]) and the item-id hash
//! ([`item_id`]). `clusters_open_items_match_load_dream_items` is the
//! regression test binding both to `load_dream_items`'s output on a shared
//! fixture, so a divergence fails a test rather than silently producing two
//! different feeds.
//!
//! `resolution_proposals` evidence, which `load_dream_items` adds per item, is
//! deliberately **not** part of a cluster: it carries no file/symbol, it is
//! item-specific rather than conclusion-defining, and folding it into the
//! fingerprint would split two identical verdict-evidence sets apart. The
//! per-item feed still surfaces it.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::dream::report::iso_date;
use crate::storage::dream_items::{
    extract_code_tokens, last_two_segments, verdict_rows_for_project, DreamEvidence,
    DreamItemGrade, VerdictGroupRow,
};

/// Display cap on the evidence rows carried on a cluster. `counts.evidence`
/// stays the true total.
const MAX_CLUSTER_EVIDENCE: usize = 12;
/// Display cap on the items carried on a cluster. `counts.items` stays the
/// true total.
const MAX_CLUSTER_ITEMS: usize = 24;
/// Cap per partition. The `total_*` fields on [`DreamClusterFeed`] stay the
/// true pre-truncation totals.
const MAX_CLUSTERS_PER_PARTITION: usize = 60;

// --- public types -------------------------------------------------------------

/// Which section a cluster belongs to (locked decision 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ClusterPartition {
    Active,
    Settled,
    Archive,
}

impl ClusterPartition {
    pub fn as_str(&self) -> &'static str {
        match self {
            ClusterPartition::Active => "active",
            ClusterPartition::Settled => "settled",
            ClusterPartition::Archive => "archive",
        }
    }
}

/// Tier 2 of the ranking. Declaration order IS the priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VerdictClass {
    /// ≥1 `anchor_obsolete`/`superseded_by` row that actually carries a
    /// receipt oid.
    ReceiptBearingAdverse,
    /// ≥1 `anchor_reinstated` row and no receipt-bearing adverse row.
    Restorative,
    /// Adverse rows with no stored receipt, or anything else. Ranked last —
    /// never presented as if a receipt existed.
    Unreceipted,
}

impl VerdictClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            VerdictClass::ReceiptBearingAdverse => "receipt-bearing",
            VerdictClass::Restorative => "restorative",
            VerdictClass::Unreceipted => "unreceipted",
        }
    }
}

/// Tier 4 of the ranking. Declaration order IS the priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ItemKindRank {
    Blocker,
    Todo,
}

/// The five explicit priority tiers plus a determinism backstop. Comparing
/// this value is the whole ranking — see the module doc.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClusterRank {
    /// Tier 1 — `ItemGrade` sorts before `SessionGrade`.
    pub grade: DreamItemGrade,
    /// Tier 2.
    pub verdict_class: VerdictClass,
    /// Tier 3 — newest witnessed ISO date first.
    pub witnessed_date_desc: Reverse<String>,
    /// Tier 4 — blocker before todo.
    pub kind: ItemKindRank,
    /// Tier 5 — oldest open item first (normalized timestamp, ascending).
    pub oldest_open: String,
    /// NOT a tier: the cluster id, so the order is total and stable under
    /// shuffled input.
    pub id: String,
}

/// Proof that an item was completed later — the episode that recorded the
/// completion. A cluster is never labelled settled without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionReceipt {
    /// `session_id` of the episode whose todo list recorded the completion.
    pub session_id: String,
    /// That episode's timestamp, verbatim.
    pub completed_at: String,
    /// ISO date of `completed_at`.
    pub completed_date: String,
}

/// The `witness_generations` publication manifest a cluster's evidence falls
/// under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivePass {
    pub generation_id: String,
    pub head_oid: String,
    /// `'complete'` or `'incomplete'`, verbatim — an incomplete manifest is
    /// labelled, never silently presented as a published pass.
    pub status: String,
    pub created_at: String,
    pub created_date: String,
}

/// One open (or later-completed) item inside a cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterItem {
    /// Identical to `DreamItem::id` for the same item — deep links are shared
    /// between the item feed and the cluster feed.
    pub id: String,
    pub item: String,
    /// `"todo"` or `"blocker"`.
    pub kind: String,
    pub origin_session: String,
    pub origin_ts: String,
    pub origin_date: String,
    /// `Some` iff a newer episode recorded this text completed.
    pub completed: Option<CompletionReceipt>,
}

/// The ground-shift conclusion a cluster is about: the newest receipt-bearing
/// adverse evidence row when one exists, else the newest row. Every field is
/// copied from a stored row — nothing is composed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterConclusion {
    /// Verbatim `witness_verdicts.verdict`.
    pub verdict: String,
    pub verdict_class: VerdictClass,
    pub symbol: Option<String>,
    pub file: String,
    pub receipt_oid: Option<String>,
    pub witnessed_at: String,
    pub witnessed_date: String,
}

/// Measured counts. `items`/`evidence` are TRUE totals and may exceed the
/// display-capped vectors.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClusterCounts {
    pub items: usize,
    pub blockers: usize,
    pub todos: usize,
    pub completed_items: usize,
    pub evidence: usize,
    pub receipts: usize,
    pub files: usize,
    pub symbols: usize,
}

/// One consequence cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamCluster {
    /// Stable 16-hex id over the cluster key — survives a re-run.
    pub id: String,
    pub project: String,
    /// The session the conclusion's evidence was reached through. For a
    /// standalone item-grade cluster this is that item's own origin session.
    pub origin_session: String,
    /// Canonical evidence-set fingerprint (16 hex).
    pub fingerprint: String,
    pub grade: DreamItemGrade,
    /// `true` for item-grade clusters of one.
    pub standalone: bool,
    pub partition: ClusterPartition,
    pub conclusion: ClusterConclusion,
    pub verdict_class: VerdictClass,
    /// Deduped, newest witnessed first, capped at [`MAX_CLUSTER_EVIDENCE`].
    pub evidence: Vec<DreamEvidence>,
    /// Distinct receipt oids across the full evidence set, newest first.
    pub receipts: Vec<String>,
    /// Newest witnessed timestamp across the full evidence set, verbatim.
    pub witnessed_at: String,
    pub witnessed_date: String,
    /// Oldest item origin timestamp in the cluster, verbatim.
    pub oldest_open_ts: String,
    pub oldest_open_date: String,
    /// Blocker-first, then oldest origin; capped at [`MAX_CLUSTER_ITEMS`].
    pub items: Vec<ClusterItem>,
    pub counts: ClusterCounts,
    /// `Some` iff `partition == Settled` — the receipt proving completion.
    pub settled: Option<CompletionReceipt>,
    /// The pass an archived cluster's evidence falls under, when one is
    /// recorded. `None` is honest, never a placeholder.
    pub archive_pass: Option<ArchivePass>,
    pub rank: ClusterRank,
}

/// The three sections plus their measured totals and the project navigation
/// order (locked decision 12: all projects present, current first).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DreamClusterFeed {
    pub active: Vec<DreamCluster>,
    pub settled: Vec<DreamCluster>,
    pub archive: Vec<DreamCluster>,
    /// Pre-truncation totals.
    pub total_active: usize,
    pub total_settled: usize,
    pub total_archive: usize,
    /// Every project with ≥1 cluster, the caller's current project first,
    /// the rest alphabetical. A current project with no clusters is NOT
    /// listed — presence here means evidence exists.
    pub projects: Vec<String>,
}

// --- episode JSON (v2 schema) ------------------------------------------------
//
// Same deliberately-narrow `#[serde(default)]` projection pattern as
// `dream_items::EpisodeRecord` and `dream_report::EpisodeJson`: a missing or
// renamed field degrades to the default instead of failing the whole row.

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClusterEpisode {
    session_id: String,
    project: String,
    timestamp: String,
    todos: Vec<ClusterTodo>,
    files_modified: Vec<String>,
    investigated: Vec<String>,
    blockers: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ClusterTodo {
    content: String,
    status: String,
}

struct EpisodeRow {
    record: ClusterEpisode,
    origin_ts: String,
    origin_ts_julian: f64,
}

fn nonblank(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

/// Lexicographically comparable form of a timestamp: `2026-01-02T03:04:05Z`
/// and `2026-01-02 03:04:05` both become `2026-01-02 03:04:05`. SQLite's
/// `datetime('now')` writes the second form and episode JSON carries the
/// first, so the two must be normalized before any comparison.
fn normalize_ts(ts: &str) -> String {
    let replaced = ts.trim().replace('T', " ");
    replaced.trim_end_matches('Z').trim().to_string()
}

fn load_cluster_episodes(conn: &Connection) -> Result<Vec<EpisodeRow>> {
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
        // Fail-open, same as `dream_items::load_v2_episodes`: one corrupt
        // episode degrades that row's evidence, never the whole query.
        let Ok(record) = serde_json::from_str::<ClusterEpisode>(&content) else {
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

// --- gate helpers restated from `dream_items` (private there) -----------------

/// Case-insensitive **whole-token** match: `token` equals the row's symbol,
/// equals the row's file basename, or (for a path-shaped token) the row's file
/// path ends with it. Never a substring match — the naive substring matching
/// `dream_items` documents as banned ("cand", "Phase", "GOLD" matching real
/// symbol names) is exactly what this predicate exists to prevent.
///
/// Mirrors `dream_items::token_matches_row`, which is private to that module;
/// `clusters_open_items_match_load_dream_items` is the test binding the two
/// definitions together.
fn token_matches_verdict_row(token: &str, row: &VerdictGroupRow) -> bool {
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

fn hash16(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

/// `sha256(project || "\0" || normalized item text)`, first 16 hex — the exact
/// preimage `dream_items::stable_id` (private there) hashes, so `/dream/<id>`
/// links resolve against either feed. Written out rather than routed through
/// [`hash16`] precisely because that helper appends a trailing separator and
/// would mint a different digest; `clusters_open_items_match_load_dream_items`
/// is the test that caught it and keeps the two in lockstep.
fn item_id(project: &str, item: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(project.as_bytes());
    hasher.update(b"\0");
    hasher.update(item.trim().to_lowercase().as_bytes());
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

/// Stable hash over the SORTED canonical evidence tuples. Timestamps are
/// excluded on purpose (see the module doc).
fn evidence_fingerprint(evidence: &[DreamEvidence]) -> String {
    let mut tuples: Vec<String> = evidence
        .iter()
        .map(|e| {
            format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}",
                e.file,
                e.symbol.as_deref().unwrap_or(""),
                e.verdict,
                e.receipt_oid.as_deref().unwrap_or("")
            )
        })
        .collect();
    tuples.sort();
    tuples.dedup();
    let joined = tuples.join("\u{1e}");
    hash16(&[&joined])
}

fn is_adverse(verdict: &str) -> bool {
    matches!(verdict, "anchor_obsolete" | "superseded_by")
}

fn verdict_class_of(evidence: &[DreamEvidence]) -> VerdictClass {
    if evidence
        .iter()
        .any(|e| is_adverse(&e.verdict) && e.receipt_oid.is_some())
    {
        return VerdictClass::ReceiptBearingAdverse;
    }
    if evidence.iter().any(|e| e.verdict == "anchor_reinstated") {
        return VerdictClass::Restorative;
    }
    VerdictClass::Unreceipted
}

fn kind_rank(kind: &str) -> ItemKindRank {
    match kind {
        "blocker" => ItemKindRank::Blocker,
        _ => ItemKindRank::Todo,
    }
}

// --- candidates ---------------------------------------------------------------

struct Candidate {
    project: String,
    item: String,
    kind: &'static str,
    origin_session: String,
    origin_ts: String,
    origin_ts_julian: f64,
    /// This episode's `files_modified` + `investigated` — channel B's match
    /// set.
    channel_b_files: Vec<String>,
    /// Set when a NEWER episode recorded this exact text completed.
    completed: Option<CompletionReceipt>,
}

struct Completion {
    julian: f64,
    receipt: CompletionReceipt,
}

/// `(project, normalized text) -> newest completion`. Unlike
/// `load_dream_items`, which drops a completed-later item outright, the
/// cluster feed keeps it and carries the receipt — that is what makes the
/// settled section possible at all (locked decision 11: nothing is deleted).
fn completion_index(episodes: &[EpisodeRow]) -> BTreeMap<(String, String), Completion> {
    let mut out: BTreeMap<(String, String), Completion> = BTreeMap::new();
    for ep in episodes {
        for todo in &ep.record.todos {
            if todo.status != "completed" {
                continue;
            }
            let Some(text) = nonblank(todo.content.clone()) else {
                continue;
            };
            let key = (ep.record.project.clone(), text.trim().to_lowercase());
            let candidate = Completion {
                julian: ep.origin_ts_julian,
                receipt: CompletionReceipt {
                    session_id: ep.record.session_id.clone(),
                    completed_at: ep.origin_ts.clone(),
                    completed_date: iso_date(&ep.origin_ts),
                },
            };
            match out.get(&key) {
                Some(existing) if existing.julian >= candidate.julian => {}
                _ => {
                    out.insert(key, candidate);
                }
            }
        }
    }
    out
}

fn collect_candidates(episodes: &[EpisodeRow]) -> Vec<Candidate> {
    let completions = completion_index(episodes);

    let mut raw: Vec<Candidate> = Vec::new();
    for ep in episodes {
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
            raw.push(Candidate {
                project: ep.record.project.clone(),
                item: text,
                kind: "todo",
                origin_session: ep.record.session_id.clone(),
                origin_ts: ep.origin_ts.clone(),
                origin_ts_julian: ep.origin_ts_julian,
                channel_b_files: channel_b_files.clone(),
                completed: None,
            });
        }

        if let Some(blockers) = ep.record.blockers.clone().and_then(nonblank) {
            if blockers.trim().to_lowercase() != "none" {
                raw.push(Candidate {
                    project: ep.record.project.clone(),
                    item: blockers,
                    kind: "blocker",
                    origin_session: ep.record.session_id.clone(),
                    origin_ts: ep.origin_ts.clone(),
                    origin_ts_julian: ep.origin_ts_julian,
                    channel_b_files,
                    completed: None,
                });
            }
        }
    }

    // Same dedupe contract as `load_dream_items`: case-insensitive on
    // `(project, normalized text)`, newest origin wins, ties keep the
    // first-seen row (query order is `ORDER BY rowid`).
    let mut dedup: BTreeMap<(String, String), Candidate> = BTreeMap::new();
    for c in raw {
        let key = (c.project.clone(), c.item.trim().to_lowercase());
        let replace = match dedup.get(&key) {
            Some(existing) => c.origin_ts_julian > existing.origin_ts_julian,
            None => true,
        };
        if replace {
            dedup.insert(key, c);
        }
    }

    let mut out: Vec<Candidate> = dedup.into_values().collect();
    for candidate in &mut out {
        let key = (
            candidate.project.clone(),
            candidate.item.trim().to_lowercase(),
        );
        if let Some(completion) = completions.get(&key) {
            if completion.julian > candidate.origin_ts_julian {
                candidate.completed = Some(completion.receipt.clone());
            }
        }
    }
    out
}

// --- gated item, pre-grouping --------------------------------------------------

struct GatedItem {
    candidate: Candidate,
    grade: DreamItemGrade,
    evidence: Vec<DreamEvidence>,
    fingerprint: String,
}

fn sort_evidence(evidence: &mut [DreamEvidence]) {
    evidence.sort_by(|a, b| {
        b.witnessed_at
            .cmp(&a.witnessed_at)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.symbol.cmp(&b.symbol))
            .then_with(|| a.verdict.cmp(&b.verdict))
            .then_with(|| a.receipt_oid.cmp(&b.receipt_oid))
    });
}

/// Run the two-channel gate over one candidate. `None` means the candidate
/// qualified through neither channel and is simply not a dream item.
fn gate_candidate(candidate: Candidate, verdict_rows: &[VerdictGroupRow]) -> Option<GatedItem> {
    let tokens = extract_code_tokens(&candidate.item);
    let mut seen: BTreeSet<(String, String, String, String)> = BTreeSet::new();
    let mut evidence: Vec<DreamEvidence> = Vec::new();
    let mut has_channel_a = false;
    let mut has_channel_b = false;

    let push = |row: &VerdictGroupRow,
                evidence: &mut Vec<DreamEvidence>,
                seen: &mut BTreeSet<(String, String, String, String)>| {
        let key = (
            row.file.clone(),
            row.symbol.clone().unwrap_or_default(),
            row.verdict.clone(),
            row.receipt_oid.clone().unwrap_or_default(),
        );
        if seen.insert(key) {
            evidence.push(DreamEvidence {
                symbol: row.symbol.clone(),
                file: row.file.clone(),
                verdict: row.verdict.clone(),
                receipt_oid: row.receipt_oid.clone(),
                witnessed_at: row.witnessed_at.clone(),
            });
        }
    };

    if !tokens.is_empty() {
        for row in verdict_rows {
            if tokens.iter().any(|t| token_matches_verdict_row(t, row)) {
                has_channel_a = true;
                push(row, &mut evidence, &mut seen);
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
            if targets.contains(&last_two_segments(&row.file)) {
                has_channel_b = true;
                push(row, &mut evidence, &mut seen);
            }
        }
    }

    if !has_channel_a && !has_channel_b {
        return None;
    }

    sort_evidence(&mut evidence);
    let fingerprint = evidence_fingerprint(&evidence);
    let grade = if has_channel_a {
        DreamItemGrade::ItemGrade
    } else {
        DreamItemGrade::SessionGrade
    };
    Some(GatedItem {
        candidate,
        grade,
        evidence,
        fingerprint,
    })
}

// --- witness_generations ------------------------------------------------------

#[derive(Debug, Clone)]
struct GenerationRow {
    generation_id: String,
    head_oid: String,
    status: String,
    created_at: String,
    created_norm: String,
}

/// Every generation manifest for one project, newest first.
fn generations_for_project(conn: &Connection, project: &str) -> Result<Vec<GenerationRow>> {
    let mut stmt = conn.prepare(
        "SELECT generation_id, head_oid, status, created_at
         FROM witness_generations
         WHERE project = ?1
         ORDER BY created_at DESC, id DESC",
    )?;
    let rows = stmt
        .query_map(params![project], |row| {
            let created_at: String = row.get(3)?;
            Ok(GenerationRow {
                generation_id: row.get(0)?,
                head_oid: row.get(1)?,
                status: row.get(2)?,
                created_norm: normalize_ts(&created_at),
                created_at,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn to_archive_pass(row: &GenerationRow) -> ArchivePass {
    ArchivePass {
        generation_id: row.generation_id.clone(),
        head_oid: row.head_oid.clone(),
        status: row.status.clone(),
        created_at: row.created_at.clone(),
        created_date: iso_date(&row.created_at),
    }
}

// --- assembly -------------------------------------------------------------------

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct ClusterKey {
    /// `0` = standalone item-grade, `1` = session-grade. Keeps the two key
    /// spaces from ever colliding.
    kind_tag: u8,
    project: String,
    /// Origin session for a session cluster, item id for a standalone one.
    scope: String,
    fingerprint: String,
}

struct Accum {
    grade: DreamItemGrade,
    standalone: bool,
    evidence: Vec<DreamEvidence>,
    items: Vec<ClusterItem>,
}

/// Load the consequence-cluster feed.
///
/// `current_project`, when supplied, sorts first inside every partition
/// (locked decision 12). It never changes which clusters exist, and it never
/// enters [`ClusterRank`] — ranking and navigation are separate dimensions.
pub fn load_dream_clusters(
    conn: &Connection,
    current_project: Option<&str>,
) -> Result<DreamClusterFeed> {
    let episodes = load_cluster_episodes(conn)?;
    if episodes.is_empty() {
        return Ok(DreamClusterFeed::default());
    }

    let candidates = collect_candidates(&episodes);
    let mut verdict_cache: HashMap<String, Vec<VerdictGroupRow>> = HashMap::new();
    let mut gated: Vec<GatedItem> = Vec::new();
    for candidate in candidates {
        let rows = match verdict_cache.get(&candidate.project) {
            Some(rows) => rows,
            None => {
                let rows = verdict_rows_for_project(conn, &candidate.project)?;
                verdict_cache
                    .entry(candidate.project.clone())
                    .or_insert(rows)
            }
        };
        if let Some(item) = gate_candidate(candidate, rows) {
            gated.push(item);
        }
    }
    if gated.is_empty() {
        return Ok(DreamClusterFeed::default());
    }

    // --- group -----------------------------------------------------------
    let mut groups: BTreeMap<ClusterKey, Accum> = BTreeMap::new();
    for item in gated {
        let id = item_id(&item.candidate.project, &item.candidate.item);
        let standalone = item.grade == DreamItemGrade::ItemGrade;
        let key = ClusterKey {
            kind_tag: u8::from(!standalone),
            project: item.candidate.project.clone(),
            scope: if standalone {
                id.clone()
            } else {
                item.candidate.origin_session.clone()
            },
            fingerprint: item.fingerprint.clone(),
        };
        let cluster_item = ClusterItem {
            id,
            item: item.candidate.item.clone(),
            kind: item.candidate.kind.to_string(),
            origin_session: item.candidate.origin_session.clone(),
            origin_date: iso_date(&item.candidate.origin_ts),
            origin_ts: item.candidate.origin_ts.clone(),
            completed: item.candidate.completed.clone(),
        };
        let entry = groups.entry(key).or_insert_with(|| Accum {
            grade: item.grade,
            standalone,
            evidence: item.evidence.clone(),
            items: Vec::new(),
        });
        entry.items.push(cluster_item);
    }

    // --- build -----------------------------------------------------------
    let mut generation_cache: HashMap<String, Vec<GenerationRow>> = HashMap::new();
    let mut clusters: Vec<DreamCluster> = Vec::with_capacity(groups.len());
    for (key, accum) in groups {
        let generations = match generation_cache.get(&key.project) {
            Some(rows) => rows,
            None => {
                let rows = generations_for_project(conn, &key.project)?;
                generation_cache.entry(key.project.clone()).or_insert(rows)
            }
        };
        clusters.push(build_cluster(&key, accum, generations));
    }

    // --- partition + order ------------------------------------------------
    let order = project_order(&clusters, current_project);
    let projects = order_projects(&clusters, current_project);

    let mut active: Vec<DreamCluster> = Vec::new();
    let mut settled: Vec<DreamCluster> = Vec::new();
    let mut archive: Vec<DreamCluster> = Vec::new();
    for cluster in clusters {
        match cluster.partition {
            ClusterPartition::Active => active.push(cluster),
            ClusterPartition::Settled => settled.push(cluster),
            ClusterPartition::Archive => archive.push(cluster),
        }
    }
    for bucket in [&mut active, &mut settled, &mut archive] {
        bucket.sort_by(|a, b| {
            order
                .get(&a.project)
                .cmp(&order.get(&b.project))
                .then_with(|| a.rank.cmp(&b.rank))
        });
    }

    let total_active = active.len();
    let total_settled = settled.len();
    let total_archive = archive.len();
    active.truncate(MAX_CLUSTERS_PER_PARTITION);
    settled.truncate(MAX_CLUSTERS_PER_PARTITION);
    archive.truncate(MAX_CLUSTERS_PER_PARTITION);

    Ok(DreamClusterFeed {
        active,
        settled,
        archive,
        total_active,
        total_settled,
        total_archive,
        projects,
    })
}

/// One open item as the candidate pass sees it — **before** the two-channel
/// evidence gate decides anything about it.
///
/// This is the only type in this module that can describe an item the night
/// pass reached **no** conclusion about, which is precisely what the journal's
/// off-board Unexamined lane needs: showing such an item inside a board column
/// would imply a verdict that does not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenItem {
    /// Identical to `DreamItem::id` / [`ClusterItem::id`] for the same item.
    pub id: String,
    pub project: String,
    pub item: String,
    /// `"todo"` or `"blocker"`.
    pub kind: String,
    pub origin_session: String,
    pub origin_ts: String,
    pub origin_date: String,
    /// `Some` iff a NEWER episode recorded this exact text completed.
    pub completed: Option<CompletionReceipt>,
    /// `true` iff the two-channel gate matched ≥1 stored verdict row — i.e.
    /// this item is represented by a cluster. `false` means the pass concluded
    /// **nothing** about it; absence of evidence, never evidence of absence.
    pub examined: bool,
}

/// Every open item, each tagged with whether any verdict evidence matched it.
///
/// Same candidate collection, dedupe and completion index
/// [`load_dream_clusters`] uses — deliberately the same code path rather than
/// a parallel one, so the Unexamined lane can never disagree with the board
/// about which items exist.
pub fn load_open_items(conn: &Connection) -> Result<Vec<OpenItem>> {
    let episodes = load_cluster_episodes(conn)?;
    if episodes.is_empty() {
        return Ok(Vec::new());
    }
    let mut verdict_cache: HashMap<String, Vec<VerdictGroupRow>> = HashMap::new();
    let mut out: Vec<OpenItem> = Vec::new();
    for candidate in collect_candidates(&episodes) {
        let rows = match verdict_cache.get(&candidate.project) {
            Some(rows) => rows,
            None => {
                let rows = verdict_rows_for_project(conn, &candidate.project)?;
                verdict_cache
                    .entry(candidate.project.clone())
                    .or_insert(rows)
            }
        };
        let id = item_id(&candidate.project, &candidate.item);
        let project = candidate.project.clone();
        let item = candidate.item.clone();
        let kind = candidate.kind.to_string();
        let origin_session = candidate.origin_session.clone();
        let origin_ts = candidate.origin_ts.clone();
        let completed = candidate.completed.clone();
        let examined = gate_candidate(candidate, rows).is_some();
        out.push(OpenItem {
            id,
            project,
            item,
            kind,
            origin_session,
            origin_date: iso_date(&origin_ts),
            origin_ts,
            completed,
            examined,
        });
    }
    Ok(out)
}

fn build_cluster(key: &ClusterKey, accum: Accum, generations: &[GenerationRow]) -> DreamCluster {
    let Accum {
        grade,
        standalone,
        mut evidence,
        mut items,
    } = accum;

    sort_evidence(&mut evidence);
    items.sort_by(|a, b| {
        kind_rank(&a.kind)
            .cmp(&kind_rank(&b.kind))
            .then_with(|| normalize_ts(&a.origin_ts).cmp(&normalize_ts(&b.origin_ts)))
            .then_with(|| a.item.cmp(&b.item))
            .then_with(|| a.id.cmp(&b.id))
    });

    let verdict_class = verdict_class_of(&evidence);
    let lead = evidence
        .iter()
        .find(|e| is_adverse(&e.verdict) && e.receipt_oid.is_some())
        .or_else(|| evidence.first())
        .cloned()
        .unwrap_or_else(|| DreamEvidence {
            symbol: None,
            file: String::new(),
            verdict: String::new(),
            receipt_oid: None,
            witnessed_at: String::new(),
        });

    let witnessed_at = evidence
        .first()
        .map(|e| e.witnessed_at.clone())
        .unwrap_or_default();
    let mut receipts: Vec<String> = Vec::new();
    for e in &evidence {
        if let Some(oid) = &e.receipt_oid {
            if !receipts.contains(oid) {
                receipts.push(oid.clone());
            }
        }
    }
    let files: BTreeSet<&str> = evidence.iter().map(|e| e.file.as_str()).collect();
    let symbols: BTreeSet<&str> = evidence
        .iter()
        .filter_map(|e| e.symbol.as_deref())
        .collect();

    let counts = ClusterCounts {
        items: items.len(),
        blockers: items.iter().filter(|i| i.kind == "blocker").count(),
        todos: items.iter().filter(|i| i.kind == "todo").count(),
        completed_items: items.iter().filter(|i| i.completed.is_some()).count(),
        evidence: evidence.len(),
        receipts: receipts.len(),
        files: files.len(),
        symbols: symbols.len(),
    };

    let oldest_open_ts = items
        .iter()
        .map(|i| i.origin_ts.clone())
        .min_by(|a, b| normalize_ts(a).cmp(&normalize_ts(b)))
        .unwrap_or_default();

    // Settled: every item completed later, and the receipt proving it is the
    // newest completion among them.
    let settled = if counts.items > 0 && counts.completed_items == counts.items {
        items
            .iter()
            .filter_map(|i| i.completed.clone())
            .max_by(|a, b| normalize_ts(&a.completed_at).cmp(&normalize_ts(&b.completed_at)))
    } else {
        None
    };

    // Archive: evidence older than the project's most recent COMPLETE
    // publication. `archive_pass` names the generation the evidence falls
    // under; `None` when no manifest covers it — never invented.
    let witnessed_norm = normalize_ts(&witnessed_at);
    let current_pass = generations.iter().find(|g| g.status == "complete");
    let is_archive = settled.is_none()
        && current_pass
            .is_some_and(|pass| !witnessed_norm.is_empty() && witnessed_norm < pass.created_norm);
    let archive_pass = if is_archive {
        generations
            .iter()
            .find(|g| g.created_norm <= witnessed_norm)
            .map(to_archive_pass)
    } else {
        None
    };

    let partition = if settled.is_some() {
        ClusterPartition::Settled
    } else if is_archive {
        ClusterPartition::Archive
    } else {
        ClusterPartition::Active
    };

    let id = hash16(&[
        "cluster",
        &key.kind_tag.to_string(),
        &key.project,
        &key.scope,
        &key.fingerprint,
    ]);

    let rank = ClusterRank {
        grade,
        verdict_class,
        witnessed_date_desc: Reverse(iso_date(&witnessed_at)),
        kind: items
            .iter()
            .map(|i| kind_rank(&i.kind))
            .min()
            .unwrap_or(ItemKindRank::Todo),
        oldest_open: normalize_ts(&oldest_open_ts),
        id: id.clone(),
    };

    let origin_session = items
        .first()
        .map(|i| i.origin_session.clone())
        .unwrap_or_default();

    let conclusion = ClusterConclusion {
        verdict: lead.verdict.clone(),
        verdict_class,
        symbol: lead.symbol.clone(),
        file: lead.file.clone(),
        receipt_oid: lead.receipt_oid.clone(),
        witnessed_date: iso_date(&lead.witnessed_at),
        witnessed_at: lead.witnessed_at.clone(),
    };

    evidence.truncate(MAX_CLUSTER_EVIDENCE);
    items.truncate(MAX_CLUSTER_ITEMS);

    DreamCluster {
        id,
        project: key.project.clone(),
        origin_session,
        fingerprint: key.fingerprint.clone(),
        grade,
        standalone,
        partition,
        conclusion,
        verdict_class,
        evidence,
        receipts,
        witnessed_date: iso_date(&witnessed_at),
        witnessed_at,
        oldest_open_date: iso_date(&oldest_open_ts),
        oldest_open_ts,
        items,
        counts,
        settled,
        archive_pass,
        rank,
    }
}

/// `project -> sort position`: the caller's current project is 0, every other
/// project follows alphabetically. Navigation only — never a rank tier.
fn project_order(clusters: &[DreamCluster], current_project: Option<&str>) -> HashMap<String, u32> {
    let mut order = HashMap::new();
    let mut position = 0u32;
    if let Some(current) = current_project {
        if clusters.iter().any(|c| c.project == current) {
            order.insert(current.to_string(), position);
            position += 1;
        }
    }
    let rest: BTreeSet<&str> = clusters
        .iter()
        .map(|c| c.project.as_str())
        .filter(|p| !order.contains_key(*p))
        .collect();
    for project in rest {
        order.insert(project.to_string(), position);
        position += 1;
    }
    order
}

fn order_projects(clusters: &[DreamCluster], current_project: Option<&str>) -> Vec<String> {
    let order = project_order(clusters, current_project);
    let mut projects: Vec<(u32, String)> = order.into_iter().map(|(k, v)| (v, k)).collect();
    projects.sort();
    projects.into_iter().map(|(_, name)| name).collect()
}

#[cfg(test)]
#[path = "dream_clusters_tests.rs"]
mod tests;
