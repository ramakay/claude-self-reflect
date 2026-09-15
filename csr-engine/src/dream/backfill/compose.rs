//! Dream backfill — Stage 6 compose + drain (`.plans/dream-backfill-design.md`
//! §3 "Stage 6 — compose + drain", with the D1/D7/D11 round-2 deltas from §8
//! folded in, since that section overrides §3-6 wherever they conflict).
//!
//! Two independent, already-computed feeds land here:
//!
//! 1. **Verified relations** ([`super::verify::verify_and_apply`]'s output):
//!    `dream_relations` rows a clean adjudication+verification pass promoted
//!    to `tier = 'witnessed'`. These are the ONLY relation rows this module
//!    treats as a claim worth composing — a row still `tier = 'unverified'`
//!    is either awaiting adjudication (`--dry-run`'s territory, not this
//!    module's) or was never a claim at all.
//! 2. **Queue U** ([`super::unfinished::scan_unfinished`]'s `queue` field):
//!    never-picked-up seeds that already cleared
//!    [`super::unfinished::queue_eligible`] (an open todo or blockers AND a
//!    live file) — the deterministic zero-LLM "unfinished" half of the
//!    drain.
//!
//! [`build_ranked_queue`] merges both into one [`RankedEntry`] list, ranked
//! now-hook-bearing-first-then-score (D11's interleave rule for `--report`).
//! [`drain`] is what a nightly cadence (or a manual `dream drain`) calls to
//! turn the top of that queue into `dreams_v1` rows, capped at N/night and
//! enforcing D7's "one open dream per (project, topic_key) per 30 days" plus
//! pairwise topic-distinctness within the batch itself. `--report`
//! ([`render_full_report`]) bypasses the drain cap entirely and renders the
//! FULL queue — this is what the design's §0 acceptance protocol ("3
//! mind-blowing dreams") reads.
//!
//! # Card voice (D11: "states receipt-backed facts plainly")
//!
//! [`render_supersession_card`] and [`render_unfinished_backfill_card`] are
//! pure field interpolation — every line is `format!` over a stored column,
//! never a generated sentence. Uncertainty is expressed ONLY through the
//! `[tier]` badge in the card header; no hedge word ("may have", "possibly",
//! "likely") ever appears anywhere in either template, checked directly by
//! this module's snapshot tests. This is deliberately narrower than
//! `dream::cli`'s `unfinished`/`strategy` cards, which DO carry a labeled
//! "Dream's take" interpretive paragraph — a `supersession` claim is a
//! machine-verified relation with quotes and (usually) a commit receipt
//! behind it, so there is nothing left to interpret; stating it plainly is
//! more honest than dressing it up.
//!
//! # Judgment calls (undocumented by the design)
//!
//! - Queue U carries no `topic_key` of its own (a queue entry is one
//!   episode, not a pair) — this module namespaces one as
//!   `"episode:<episode_id>"`, mirroring `pairs::PairCandidate::topic_key`'s
//!   own `symbol:`/`era:` prefixing convention so the two id spaces never
//!   collide by coincidence.
//! - Queue U's card is written under `dreams_v1.category = 'unfinished'` —
//!   the SAME category `dream::cli`'s home-page feed already uses (that
//!   category's CHECK value predates this stage), not a new one. Only
//!   `supersession` is genuinely new (the task's own instruction). The two
//!   feeds share a category because both tell the identical "you left this
//!   open" story, just sourced from different scans (this week vs. the full
//!   historical corpus) — a reader has no reason to see them as different
//!   kinds of dream.
//! - "drainable" (used for the now-hook-bearing/interleave split) is read
//!   off `dream_relations.status`, not the stored `now_hook` column
//!   directly: `status = 'queued'` is exactly the population
//!   [`super::adjudicate::load_queue`] draws from, so a `witnessed` row is
//!   `queued` if and only if it was now-hook-eligible when generated AND has
//!   not yet been drained. An `archived` row — never now-hook-eligible at
//!   rank time, or later ruled UNRELATED/discarded by adjudication — is
//!   exactly D1's "pure-historical supersession → archived in report, never
//!   a dream slot", and Queue U entries are always drainable by construction
//!   (`queue_eligible` already required a live consequence).
//! - The report's "would drain next" preview size ([`REPORT_PREVIEW_N`]) is
//!   not specified by the design beyond "top ~20" (§8 D11, describing the
//!   related `--dry-run` checkpoint) — reused here for the same order of
//!   magnitude, on the same "an operator eyeballs this before spending
//!   anything" spirit.
//!
//! Zero LLM calls anywhere in this module.

use std::collections::{BTreeSet, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::dream::cli::compute_dream_id;
use crate::dream::report::short_oid;
use crate::storage::dream_attribution::{marker_line, DREAM_CARD_PROPOSAL_HEADER};
use crate::storage::Storage;

use super::adjudicate::{load_episode, EpisodeFacts};
use super::intent_channel::AbandonmentCandidate;
use super::subagent_citation::{build_dream_citation_evidence, DreamCitationEvidence};
use super::unfinished::scan_unfinished_at;

/// D7: at most one OPEN dream per (project, topic_key) within this window.
const TOPIC_DEDUP_WINDOW_DAYS: i64 = 30;

/// Default nightly drain size (design §3 Stage 6 / §4: "drained N/night
/// (default 3)").
pub const DEFAULT_DRAIN_N: usize = 3;

/// `--report`'s "would drain next" preview size — see the module doc's
/// judgment-call note.
pub const REPORT_PREVIEW_N: usize = 20;

/// New `dreams_v1.category` this stage adds (migration widen in
/// `storage::migrations::run`).
const CATEGORY_SUPERSESSION: &str = "supersession";
/// Reused, unmodified, from `dream::cli`'s pre-existing home-page feed — see
/// the module doc.
const CATEGORY_UNFINISHED: &str = "unfinished";

fn short_id(s: &str) -> String {
    s.chars().take(8).collect()
}

// ---------------------------------------------------------------------
// Ranked queue (verified relations + queue-U)
// ---------------------------------------------------------------------

/// What kind of candidate a [`RankedEntry`] carries — enough to render its
/// card and, on drain, to mark its source row accordingly.
#[derive(Debug, Clone)]
pub enum EntryKind {
    Supersession {
        relation_id: i64,
        ep_a: String,
        ep_b: String,
        /// `'replaced_by'` | `'extended_by'` — read as a raw string rather
        /// than reconstructed into `pairs::Relation`; nothing here needs the
        /// enum, only its already-verified DB value.
        relation: String,
        /// `'ledger'` | `'era'` | `'relapse'`.
        generator: String,
        quote_a: String,
        quote_b: String,
        load_bearing_oid: Option<String>,
    },
    Unfinished {
        episode_id: String,
    },
}

/// One entry in the combined post-verification ranked queue — a verified
/// relation or a Queue-U seed, already scored and ready to render.
#[derive(Debug, Clone)]
pub struct RankedEntry {
    pub project: String,
    /// D7 stable dedup/diversity key: `dream_relations.topic_key` for a
    /// supersession entry, `"episode:<id>"` for a Queue U entry (see the
    /// module doc).
    pub topic_key: String,
    pub score: f64,
    /// Whether this entry currently carries a live consequence and is
    /// eligible to actually become a dream (see the module doc's "drainable"
    /// judgment call). `false` only for a finally-archived relation row —
    /// D1's "pure-historical supersession".
    pub drainable: bool,
    pub generator: String,
    /// `Some("witnessed")` for a verified relation; `None` for Queue U,
    /// which carries no adjudication tier at all (it is a directly-read
    /// fact from `episode_index`, never an LLM-judged claim).
    pub tier: Option<String>,
    pub days_since_last_touch: Option<i64>,
    /// Short display form of the underlying receipt (a commit oid), when
    /// there is one.
    pub receipt: Option<String>,
    pub kind: EntryKind,
}

fn load_relation_entries(conn: &Connection) -> Result<Vec<RankedEntry>> {
    // `status != 'drained'` excludes relations already composed into a
    // dream on a previous run; `NOT (status = 'queued' AND tier =
    // 'unverified')` excludes the pre-adjudication backlog (`--dry-run`'s
    // territory) — every row this query returns has either been verified
    // (`tier = 'witnessed'`, always `status = 'queued'` since
    // `adjudicate::load_queue` never touches an already-`archived` row) or
    // finally archived (no-now-hook at rank time, or an adjudicated
    // UNRELATED/discard outcome).
    let mut stmt = conn.prepare(
        "SELECT r.id, r.project, r.ep_a, r.ep_b, r.relation, r.generator, r.topic_key,
                r.tier, r.quote_a, r.quote_b, r.load_bearing_oid, r.gate_score, r.status,
                eb.days_since_last_touch
         FROM dream_relations r
         LEFT JOIN episode_index eb ON eb.episode_id = r.ep_b
         WHERE r.status != 'drained' AND NOT (r.status = 'queued' AND r.tier = 'unverified')",
    )?;
    let rows = stmt.query_map([], |row| {
        let status: String = row.get(12)?;
        let generator: String = row.get(5)?;
        let tier: String = row.get(7)?;
        let load_bearing_oid: Option<String> = row.get(10)?;
        Ok(RankedEntry {
            project: row.get(1)?,
            topic_key: row.get(6)?,
            score: row.get::<_, Option<f64>>(11)?.unwrap_or(0.0),
            drainable: status == "queued",
            generator: generator.clone(),
            tier: Some(tier.clone()),
            days_since_last_touch: row.get(13)?,
            receipt: load_bearing_oid.clone(),
            kind: EntryKind::Supersession {
                relation_id: row.get(0)?,
                ep_a: row.get(2)?,
                ep_b: row.get(3)?,
                relation: row.get(4)?,
                generator,
                quote_a: row.get(8)?,
                quote_b: row.get(9)?,
                load_bearing_oid,
            },
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn load_unfinished_entries(conn: &Connection, now: DateTime<Utc>) -> Result<Vec<RankedEntry>> {
    let report = scan_unfinished_at(conn, now)?;
    if report.disabled {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(report.queue.len());
    for seed in &report.queue {
        let days: Option<i64> = conn
            .query_row(
                "SELECT days_since_last_touch FROM episode_index WHERE episode_id = ?1",
                params![seed.episode_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten();
        out.push(RankedEntry {
            project: seed.project.clone(),
            topic_key: format!("episode:{}", seed.episode_id),
            score: seed.priority,
            drainable: true, // queue_eligible already required a live consequence
            generator: "unfinished".to_string(),
            tier: None,
            days_since_last_touch: days,
            receipt: None,
            kind: EntryKind::Unfinished {
                episode_id: seed.episode_id.clone(),
            },
        });
    }
    Ok(out)
}

/// Build the full post-verification ranked queue: verified relations +
/// Queue U, sorted drainable-bearing first, then descending score, with a
/// deterministic tiebreak on `topic_key` (never on insertion order, which
/// `HashSet`-backed Queue U priority computation does not guarantee).
///
/// P5: `now` is threaded through to [`load_unfinished_entries`]'s
/// `scan_unfinished_at` call — Queue U's own priority depends on the D1
/// age>90d bonus, so the ranked queue this produces is itself
/// clock-dependent; callers that also compute their own `now` (e.g.
/// [`drain`]'s 30-day topic-reuse check) should reuse the SAME value here
/// rather than letting the two drift apart within one run.
pub fn build_ranked_queue(conn: &Connection, now: DateTime<Utc>) -> Result<Vec<RankedEntry>> {
    let mut entries = load_relation_entries(conn)?;
    entries.extend(load_unfinished_entries(conn, now)?);
    entries.sort_by(|a, b| {
        b.drainable
            .cmp(&a.drainable)
            .then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.topic_key.cmp(&b.topic_key))
    });
    Ok(entries)
}

/// Walk `entries` in rank order, picking up to `n` DRAINABLE ones with
/// pairwise-distinct `topic_key`s (D7/D11: "report top-N enforces pairwise
/// topic-distinctness"). A duplicate-topic entry is skipped, never
/// substituted for by a later same-topic candidate.
pub fn select_topic_distinct(entries: &[RankedEntry], n: usize) -> Vec<&RankedEntry> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for e in entries {
        if out.len() >= n {
            break;
        }
        if !e.drainable {
            continue;
        }
        if seen.insert(e.topic_key.as_str()) {
            out.push(e);
        }
    }
    out
}

// ---------------------------------------------------------------------
// `--report` rendering (D11: bypasses the drain, renders the FULL queue)
// ---------------------------------------------------------------------

fn entry_subject(e: &RankedEntry) -> String {
    match &e.kind {
        EntryKind::Supersession {
            ep_a,
            ep_b,
            relation,
            ..
        } => format!("{} -> {} ({relation})", short_id(ep_a), short_id(ep_b)),
        EntryKind::Unfinished { episode_id } => format!("episode {}", short_id(episode_id)),
    }
}

fn render_entry_line(e: &RankedEntry) -> String {
    let tier_badge = e.tier.as_deref().unwrap_or("observed");
    let days = e
        .days_since_last_touch
        .map(|d| d.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let receipt = e
        .receipt
        .as_deref()
        .map(short_oid)
        .unwrap_or_else(|| "-".to_string());
    let hook_flag = if e.drainable { "now" } else { "archived" };
    format!(
        "[{hook_flag}] [{tier_badge}] score={:.3} project={} topic_key={} generator={} \
         days_since_touch={days} receipt={receipt} {}",
        e.score,
        e.project,
        e.topic_key,
        e.generator,
        entry_subject(e),
    )
}

/// Full ranked-queue rendering for `dream backfill --report` (design §3
/// Stage 6 / §8 D11): a "would drain next" preview (topic-distinct, capped
/// at `preview_n`) followed by every entry in rank order, including
/// archived (never-a-dream-slot) ones — the FULL queue, unrestricted by the
/// per-night drain cap.
pub fn render_full_report(entries: &[RankedEntry], preview_n: usize) -> String {
    let mut out = String::new();
    let preview = select_topic_distinct(entries, preview_n);
    let _ = writeln!(
        out,
        "Would drain next (top {}, topic-distinct):",
        preview.len()
    );
    if preview.is_empty() {
        let _ = writeln!(out, "  (none — nothing drainable yet)");
    }
    for (i, e) in preview.iter().enumerate() {
        let _ = writeln!(out, "{:>2}. {}", i + 1, render_entry_line(e));
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "Full ranked queue ({} entries):", entries.len());
    if entries.is_empty() {
        let _ = writeln!(out, "  (empty)");
    }
    for (i, e) in entries.iter().enumerate() {
        let _ = writeln!(out, "{:>3}. {}", i + 1, render_entry_line(e));
    }
    out
}

// ---------------------------------------------------------------------
// Card templates — pure field interpolation, no generated sentences (see
// the module doc's "card voice" section).
// ---------------------------------------------------------------------

fn relation_verb(relation: &str) -> &'static str {
    match relation {
        "replaced_by" => "Replaced by",
        "extended_by" => "Extended by",
        // Defensive default for a future CHECK-constraint value this
        // stage doesn't know about yet — never a panic on a stored row.
        _ => "Related to",
    }
}

fn receipt_line(load_bearing_oid: Option<&str>, generator: &str) -> String {
    match load_bearing_oid {
        Some(oid) => format!("commit {} (generator: {generator})", short_oid(oid)),
        None => format!("no commit oid on record (generator: {generator})"),
    }
}

/// Render one `supersession` card: what was believed, what replaced it, both
/// quotes, the commit receipt, and a "requires verdict" proposal — design §3
/// Stage 6's own required contents, verbatim. Every line is field
/// interpolation; see the module doc.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_supersession_card(
    project: &str,
    ep_a: &str,
    ep_b: &str,
    relation: &str,
    generator: &str,
    tier: &str,
    quote_a: &str,
    quote_b: &str,
    load_bearing_oid: Option<&str>,
    days_since_last_touch: Option<i64>,
    dream_id: &str,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "PROJECT {project} — supersession [{tier}]");
    out.push_str("Observed:\n");
    let _ = writeln!(
        out,
        "  - what was believed: \"{quote_a}\" — episode ⌗{}",
        short_id(ep_a)
    );
    let _ = writeln!(
        out,
        "  - what replaced it ({}): \"{quote_b}\" — episode ⌗{}",
        relation_verb(relation),
        short_id(ep_b)
    );
    let _ = writeln!(
        out,
        "  - receipt: {}",
        receipt_line(load_bearing_oid, generator)
    );
    if let Some(days) = days_since_last_touch {
        let _ = writeln!(out, "  - last touched {days} day(s) ago");
    }
    out.push_str(DREAM_CARD_PROPOSAL_HEADER);
    out.push_str(" confirm via csr_resolve whether this supersession still holds.\n");
    out.push_str(&marker_line(dream_id));
    out.push('\n');
    out
}

/// Render one backfill `unfinished` card (Queue U) from `episode_index`
/// facts — plain field interpolation, same voice as the supersession card.
pub(super) fn render_unfinished_backfill_card(
    project: &str,
    facts: &EpisodeFacts,
    priority: f64,
    dream_id: &str,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "PROJECT {project} — unfinished (backfill)");
    out.push_str("Observed:\n");
    let _ = writeln!(
        out,
        "  - open since episode ⌗{} — \"{}\"",
        short_id(&facts.episode_id),
        facts.request.trim()
    );
    let _ = writeln!(out, "  - completed so far: \"{}\"", facts.completed.trim());
    if let Some(n) = &facts.next_steps {
        let _ = writeln!(out, "  - next steps: \"{}\"", n.trim());
    }
    if let Some(b) = &facts.blockers {
        let _ = writeln!(out, "  - blockers: \"{}\"", b.trim());
    }
    let _ = writeln!(out, "  - backfill priority: {priority:.3}");
    out.push_str(DREAM_CARD_PROPOSAL_HEADER);
    out.push_str(" confirm this is still open, then act — record the outcome via csr_resolve.\n");
    out.push_str(&marker_line(dream_id));
    out.push('\n');
    out
}

fn content_hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for p in parts {
        hasher.update(p.as_bytes());
        hasher.update(b"\0");
    }
    hasher
        .finalize()
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------------
// Drain
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DrainStats {
    /// `true` under `CSR_NO_DREAMING` — nothing was read or written.
    pub disabled: bool,
    pub candidates: usize,
    pub drained: usize,
    /// Skipped because a same-`(project, topic_key)` dream is already open
    /// within the last 30 days (D7).
    pub skipped_recent_topic: usize,
}

/// `created_at` in `dreams_v1` is `datetime('now')` — SQLite's own
/// `YYYY-MM-DD HH:MM:SS` (UTC, space-separated, no offset), NOT RFC3339.
/// The cutoff must be formatted identically or the lexical `>=` comparison
/// below silently compares apples to oranges.
fn sqlite_datetime(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn recent_open_dream_exists(
    conn: &Connection,
    project: &str,
    topic_key: &str,
    now: DateTime<Utc>,
) -> Result<bool> {
    let cutoff = sqlite_datetime(now - Duration::days(TOPIC_DEDUP_WINDOW_DAYS));
    let exists: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM dreams_v1
            WHERE project = ?1 AND subject_key = ?2 AND status = 'open' AND created_at >= ?3
         )",
        params![project, topic_key, cutoff],
        |r| r.get(0),
    )?;
    Ok(exists)
}

#[derive(Debug, serde::Deserialize)]
struct CitationAnchor {
    #[serde(default)]
    file: String,
    #[serde(default)]
    name: String,
}

#[derive(Debug)]
struct CitationEpisode {
    session_id: String,
    anchors: Vec<CitationAnchor>,
}

fn load_citation_episode(conn: &Connection, episode_id: &str) -> Result<Option<CitationEpisode>> {
    conn.query_row(
        "SELECT session_id, anchors_json FROM episode_index WHERE episode_id = ?1",
        params![episode_id],
        |row| {
            let anchors_json: String = row.get(1)?;
            Ok(CitationEpisode {
                session_id: row.get(0)?,
                anchors: serde_json::from_str(&anchors_json).unwrap_or_default(),
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Resolve the exact dash-encoded Claude project directory from the parent
/// transcript path already recorded by import. A missing/mismatched path is
/// fail-closed: this parent contributes no citations, and no path is guessed.
///
/// `pub(super)` (pass 3): [`super::intent_channel`] reuses this same
/// fail-closed lookup to build its own family-wide subagent-transcript
/// index (see that module's `build_family_subagent_index`), rather than
/// re-deriving the `import_state` resolution rule a second time.
pub(super) fn project_dir_for_parent(
    conn: &Connection,
    parent_session_id: &str,
    projects_root: &Path,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT file_path FROM import_state
         WHERE conversation_id = ?1 AND file_path IS NOT NULL AND file_path != ''
         ORDER BY imported_at DESC, file_path ASC",
    )?;
    let paths = stmt.query_map(params![parent_session_id], |row| row.get::<_, String>(0))?;
    let expected_filename = format!("{parent_session_id}.jsonl");
    let mut project_dirs = BTreeSet::new();
    for path in paths {
        let path = path?;
        let Ok(relative) = Path::new(&path).strip_prefix(projects_root) else {
            continue;
        };
        let mut components = relative.components();
        let Some(std::path::Component::Normal(project_dir)) = components.next() else {
            continue;
        };
        let Some(std::path::Component::Normal(filename)) = components.next() else {
            continue;
        };
        if components.next().is_some() || filename != std::ffi::OsStr::new(&expected_filename) {
            continue;
        }
        let Some(project_dir) = project_dir.to_str().filter(|value| !value.is_empty()) else {
            continue;
        };
        project_dirs.insert(project_dir.to_string());
    }
    if project_dirs.len() == 1 {
        Ok(project_dirs.into_iter().next())
    } else {
        Ok(None)
    }
}

fn citation_subject(episodes: &[CitationEpisode], topic_key: &str) -> Option<(String, String)> {
    let symbol = topic_key
        .strip_prefix("symbol:")
        .filter(|symbol| !symbol.is_empty())?
        .to_string();
    let files: BTreeSet<String> = episodes
        .iter()
        .flat_map(|episode| &episode.anchors)
        .filter(|anchor| anchor.name == symbol && !anchor.file.is_empty())
        .map(|anchor| super::family::canon_file(&anchor.file))
        .collect();
    if files.len() == 1 {
        Some((symbol, files.into_iter().next()?))
    } else {
        None
    }
}

fn stored_dream_provenance(
    conn: &Connection,
    episode_ids: &[&str],
    topic_key: &str,
    projects_root: Option<&Path>,
) -> Result<String> {
    let mut episodes = Vec::new();
    for episode_id in episode_ids {
        if let Some(episode) = load_citation_episode(conn, episode_id)? {
            episodes.push(episode);
        }
    }
    let parent_sessions: Vec<String> = episodes
        .iter()
        .map(|episode| episode.session_id.clone())
        .filter(|session| !session.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let subject = citation_subject(&episodes, topic_key);
    let mut citations = Vec::new();
    if let (Some(projects_root), Some((symbol, file))) = (projects_root, subject.as_ref()) {
        for parent_session in &parent_sessions {
            let Some(project_dir) = project_dir_for_parent(conn, parent_session, projects_root)?
            else {
                continue;
            };
            let evidence = build_dream_citation_evidence(
                std::slice::from_ref(parent_session),
                &project_dir,
                symbol,
                file,
                projects_root,
            );
            citations.extend(evidence.citations);
        }
    }
    let evidence = DreamCitationEvidence::audited(parent_sessions, citations);
    serde_json::to_string(&evidence).map_err(Into::into)
}

#[allow(clippy::too_many_arguments)]
fn record_backfill_dream_row(
    storage: &Storage,
    dream_id: &str,
    project: &str,
    category: &str,
    subject_key: Option<&str>,
    revision_hash: &str,
    prose: &str,
    evidence_provenance: &str,
) -> Result<()> {
    storage.with_connection(|conn| {
        conn.execute(
            "INSERT INTO dreams_v1
                (dream_id, project, category, subject_key, revision_hash, prose, evidence_provenance)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                dream_id,
                project,
                category,
                subject_key,
                revision_hash,
                prose,
                evidence_provenance
            ],
        )?;
        Ok(())
    })
}

/// Compose and persist one entry's `dreams_v1` row, and mark its source
/// accordingly (a supersession relation's `status` moves to `'drained'`;
/// Queue U has no separate source row to mark — its own dedup is entirely
/// the `dreams_v1` 30-day-per-topic check above, same idiom
/// `dream::cli::lookup_open_dream_id` already uses for the home-page
/// `unfinished` category). Fail-soft on a write error, matching
/// `dream::cli::build_run`'s own convention: a failed insert costs this
/// entry's slot for tonight, never the rest of the drain batch.
fn drain_one(
    storage: &Storage,
    entry: &RankedEntry,
    now: DateTime<Utc>,
    projects_root: Option<&Path>,
) -> Result<bool> {
    match &entry.kind {
        EntryKind::Supersession {
            relation_id,
            ep_a,
            ep_b,
            relation,
            generator,
            quote_a,
            quote_b,
            load_bearing_oid,
        } => {
            let tier = entry.tier.as_deref().unwrap_or("witnessed");
            let dream_id =
                compute_dream_id(&entry.project, CATEGORY_SUPERSESSION, &entry.topic_key, now);
            let prose = render_supersession_card(
                &entry.project,
                ep_a,
                ep_b,
                relation,
                generator,
                tier,
                quote_a,
                quote_b,
                load_bearing_oid.as_deref(),
                entry.days_since_last_touch,
                &dream_id,
            );
            let revision_hash = content_hash(&[ep_a, ep_b, relation, quote_a, quote_b]);
            let evidence_provenance = storage.with_connection(|conn| {
                stored_dream_provenance(
                    conn,
                    &[ep_a.as_str(), ep_b.as_str()],
                    &entry.topic_key,
                    projects_root,
                )
            })?;
            if let Err(error) = record_backfill_dream_row(
                storage,
                &dream_id,
                &entry.project,
                CATEGORY_SUPERSESSION,
                Some(&entry.topic_key),
                &revision_hash,
                &prose,
                &evidence_provenance,
            ) {
                tracing::warn!(%error, project = %entry.project, relation_id, "dream backfill: failed to persist supersession dream row");
                return Ok(false);
            }
            storage.with_connection(|conn| {
                conn.execute(
                    "UPDATE dream_relations SET status = 'drained', dream_id = ?1 WHERE id = ?2",
                    params![dream_id, relation_id],
                )?;
                Ok(())
            })?;
            Ok(true)
        }
        EntryKind::Unfinished { episode_id } => {
            let Some(facts) = storage.with_connection(|conn| load_episode(conn, episode_id))?
            else {
                // The seed's episode_index row vanished between scan and
                // drain (should not happen in practice — nothing refreshes
                // episode_index mid-drain — but never a panic).
                return Ok(false);
            };
            let dream_id =
                compute_dream_id(&entry.project, CATEGORY_UNFINISHED, &entry.topic_key, now);
            let prose =
                render_unfinished_backfill_card(&entry.project, &facts, entry.score, &dream_id);
            let revision_hash = content_hash(&[
                episode_id,
                &facts.request,
                &facts.completed,
                facts.next_steps.as_deref().unwrap_or(""),
                facts.blockers.as_deref().unwrap_or(""),
            ]);
            let evidence_provenance = storage.with_connection(|conn| {
                stored_dream_provenance(
                    conn,
                    &[episode_id.as_str()],
                    &entry.topic_key,
                    projects_root,
                )
            })?;
            if let Err(error) = record_backfill_dream_row(
                storage,
                &dream_id,
                &entry.project,
                CATEGORY_UNFINISHED,
                Some(episode_id),
                &revision_hash,
                &prose,
                &evidence_provenance,
            ) {
                tracing::warn!(%error, project = %entry.project, episode_id, "dream backfill: failed to persist unfinished dream row");
                return Ok(false);
            }
            Ok(true)
        }
    }
}

/// Drain up to `n` entries off the ranked queue into `dreams_v1` (design §3
/// Stage 6 / §4: "drained N/night (default 3)"). Enforces, in order:
///
/// 1. Only `drainable` entries are ever considered (D1's now-hook gate —
///    archived/pure-historical relations never fill a dream slot).
/// 2. Pairwise topic-distinctness WITHIN this batch (D7/D11) — a
///    duplicate-topic entry later in rank order is skipped, not
///    substituted for.
/// 3. D7's "one open dream per (project, topic_key) per 30 days" — a topic
///    with an already-open recent dream is skipped for this run entirely,
///    counted separately from the batch cap.
///
/// Respects `CSR_NO_DREAMING`.
pub fn drain(storage: &Storage, n: usize) -> Result<DrainStats> {
    if crate::daemon::dream_cadence::dreaming_disabled() {
        return Ok(DrainStats {
            disabled: true,
            ..Default::default()
        });
    }

    let now = Utc::now();
    let projects_root = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|home| home.join(".claude/projects"));
    let entries = storage.with_connection(|conn| build_ranked_queue(conn, now))?;
    let mut stats = DrainStats {
        candidates: entries.len(),
        ..Default::default()
    };

    let mut seen_topics: HashSet<String> = HashSet::new();
    for entry in entries.iter().filter(|e| e.drainable) {
        if stats.drained >= n {
            break;
        }
        if !seen_topics.insert(entry.topic_key.clone()) {
            continue;
        }
        let recent = storage.with_connection(|conn| {
            recent_open_dream_exists(conn, &entry.project, &entry.topic_key, now)
        })?;
        if recent {
            stats.skipped_recent_topic += 1;
            continue;
        }
        if drain_one(storage, entry, now, projects_root.as_deref())? {
            stats.drained += 1;
        }
    }
    Ok(stats)
}

// ---------------------------------------------------------------------
// Pass 3: silent-abandonment persistence (`intent_channel`)
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AbandonmentDrainStats {
    pub candidates: usize,
    pub persisted: usize,
    /// Subset of `persisted` whose `evidence_provenance.bar_clause_met` is
    /// `true` — i.e. a family subagent transcript (unrestricted by time —
    /// see `intent_channel::FamilySubagentIndex`'s doc) corroborates the
    /// target. A genuinely-fired abandonment candidate usually has NO
    /// citation for its own target (any transcript authoring it AFTER the
    /// prompt would already have suppressed the candidate via the guard's
    /// own leg C) — a low or zero count here is the expected, honest
    /// receipt, not a defect.
    pub bar_eligible: usize,
    /// Skipped because a same-`(project, subject_key)` dream is already
    /// open within the last 30 days (D7, same gate [`drain`] enforces).
    pub skipped_recent_topic: usize,
    pub persist_errors: usize,
}

/// Persist [`intent_channel::generate_abandonment_candidates`]'s surviving
/// candidates into `dreams_v1` — a new pipeline stage wired in by
/// `cli::handle_backfill` right after Stage 2-3, gated identically
/// (respects `CSR_NO_DREAMING` via that caller, which checks it before
/// generating candidates at all — see `intent_channel`'s own module doc).
///
/// Reuses the PRE-EXISTING `CATEGORY_UNFINISHED` value rather than adding a
/// fourth `dreams_v1.category` CHECK value: an abandonment claim tells the
/// identical "you left this open" story the home-page `unfinished` feed and
/// Queue U's own backfill card already tell (see the module doc's "card
/// voice" judgment-call note above) — just sourced from a third scan (raw
/// `history.jsonl` prompts) rather than `episode_index`/`dream_relations`.
/// Widening the CHECK constraint a THIRD time (after `strategy` then
/// `supersession`) with no behavioral need for a distinct value would
/// repeat the exact schema-churn-with-no-consumer anti-pattern that
/// constraint's own migration comment already warns against.
///
/// Applies the SAME D7 30-day-per-`(project, subject_key)` reuse gate
/// [`drain`] already enforces (`subject_key` here is the target phrase —
/// the closest stable topic key this channel has), so a repeated nightly
/// run over an unchanged corpus does not spam duplicate rows for a prompt
/// that is still abandoned.
pub fn persist_abandonment_candidates(
    storage: &Storage,
    candidates: &[AbandonmentCandidate],
    now: DateTime<Utc>,
) -> Result<AbandonmentDrainStats> {
    let mut stats = AbandonmentDrainStats {
        candidates: candidates.len(),
        ..Default::default()
    };
    for candidate in candidates {
        let subject_key = candidate.target.phrase.as_str();
        let recent = storage.with_connection(|conn| {
            recent_open_dream_exists(conn, &candidate.family, subject_key, now)
        })?;
        if recent {
            stats.skipped_recent_topic += 1;
            continue;
        }
        let dream_id = compute_dream_id(&candidate.family, CATEGORY_UNFINISHED, subject_key, now);
        let revision_hash = content_hash(&[
            &candidate.family,
            subject_key,
            &candidate.head_oid,
            &candidate.claim,
        ]);
        let Ok(evidence_provenance) = serde_json::to_string(&candidate.citation_evidence) else {
            stats.persist_errors += 1;
            continue;
        };
        if let Err(error) = record_backfill_dream_row(
            storage,
            &dream_id,
            &candidate.family,
            CATEGORY_UNFINISHED,
            Some(subject_key),
            &revision_hash,
            &candidate.claim,
            &evidence_provenance,
        ) {
            tracing::warn!(%error, project = %candidate.family, subject_key, "dream backfill: failed to persist abandonment dream row");
            stats.persist_errors += 1;
            continue;
        }
        stats.persisted += 1;
        if candidate.citation_evidence.bar_clause_met {
            stats.bar_eligible += 1;
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::dream::backfill::adjudicate::{
        run_adjudication_with, AdjudicateAttempt, EpisodeFacts as AdjEpisodeFacts,
    };
    use crate::narrative::ParsedNarrative;

    fn open() -> Storage {
        Storage::open_memory().unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_episode(
        conn: &Connection,
        id: &str,
        session: &str,
        project: &str,
        ts: &str,
        request: &str,
        completed: &str,
        todo: Option<&str>,
    ) {
        let todos = match todo {
            Some(t) => format!(r#"[{{"content":"{t}","status":"pending"}}]"#),
            None => "[]".to_string(),
        };
        conn.execute(
            "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, '[]', ?3)",
            params![
                id,
                format!(
                    r#"{{"schema":"v2","session_id":"{session}","project":"{project}",
                        "timestamp":"{ts}","request":"{request}","completed":"{completed}",
                        "outcome":"completed","todos":{todos},"files_modified":[],"anchors":[]}}"#
                ),
                ts
            ],
        )
        .unwrap();
        crate::storage::dream_backfill::materialize_episode_index(conn).unwrap();
    }

    // -----------------------------------------------------------------
    // Card snapshot tests (D11 voice contract)
    // -----------------------------------------------------------------

    const HEDGE_WORDS: &[&str] = &["may have", "possibly", "likely", "probably", "perhaps"];

    fn assert_no_hedge_words(text: &str) {
        let lower = text.to_lowercase();
        for word in HEDGE_WORDS {
            assert!(
                !lower.contains(word),
                "receipt-backed card must never hedge, found {word:?} in:\n{text}"
            );
        }
    }

    #[test]
    fn supersession_card_every_sentence_maps_to_an_input_field() {
        let card = render_supersession_card(
            "csr",
            "ep-old-1234",
            "ep-new-5678",
            "replaced_by",
            "ledger",
            "witnessed",
            "we believed X was the right approach",
            "we now do Y instead",
            Some("abcdef1234567890"),
            Some(12),
            "deadbeefcafef00d",
        );
        assert!(card.starts_with("PROJECT csr — supersession [witnessed]\n"));
        assert!(card.contains("we believed X was the right approach"));
        assert!(card.contains("ep-old-1"));
        assert!(card.contains("we now do Y instead"));
        assert!(card.contains("ep-new-5"));
        assert!(card.contains("Replaced by"));
        assert!(card.contains("abcdef12")); // short_oid
        assert!(card.contains("generator: ledger"));
        assert!(card.contains("last touched 12 day(s) ago"));
        assert!(card.contains(DREAM_CARD_PROPOSAL_HEADER));
        assert!(card.contains(&marker_line("deadbeefcafef00d")));
        assert_no_hedge_words(&card);
    }

    #[test]
    fn supersession_card_extended_by_uses_the_extended_verb_and_no_oid_line_when_absent() {
        let card = render_supersession_card(
            "csr",
            "a",
            "b",
            "extended_by",
            "era",
            "witnessed",
            "qa",
            "qb",
            None,
            None,
            "id1",
        );
        assert!(card.contains("Extended by"));
        assert!(card.contains("no commit oid on record"));
        assert!(!card.contains("last touched"));
        assert_no_hedge_words(&card);
    }

    #[test]
    fn unfinished_backfill_card_carries_every_field_with_no_hedging() {
        let facts = AdjEpisodeFacts {
            episode_id: "ep-1".to_string(),
            session_id: "s1".to_string(),
            ts: "2020-01-01T00:00:00Z".to_string(),
            request: "fix the flaky test".to_string(),
            completed: "diagnosed the race".to_string(),
            next_steps: Some("add a retry guard".to_string()),
            blockers: Some("CI is red".to_string()),
            files: vec![],
        };
        let card = render_unfinished_backfill_card("csr", &facts, 0.742, "id2");
        assert!(card.contains("fix the flaky test"));
        assert!(card.contains("diagnosed the race"));
        assert!(card.contains("add a retry guard"));
        assert!(card.contains("CI is red"));
        assert!(card.contains("0.742"));
        assert!(card.contains(DREAM_CARD_PROPOSAL_HEADER));
        assert_no_hedge_words(&card);
    }

    #[test]
    fn citation_subject_fails_closed_when_a_symbol_maps_to_multiple_files() {
        let episodes = [CitationEpisode {
            session_id: "session".to_string(),
            anchors: vec![
                CitationAnchor {
                    file: "/repo/a.rs".to_string(),
                    name: "shared".to_string(),
                },
                CitationAnchor {
                    file: "/repo/b.rs".to_string(),
                    name: "shared".to_string(),
                },
            ],
        }];

        assert!(citation_subject(&episodes, "symbol:shared").is_none());
    }

    #[test]
    fn citation_subject_uses_the_relation_generators_canonical_file_identity() {
        let episodes = [CitationEpisode {
            session_id: "session".to_string(),
            anchors: vec![
                CitationAnchor {
                    file: "/repo/.claude/worktrees/w1/src/Home.tsx".to_string(),
                    name: "Home".to_string(),
                },
                CitationAnchor {
                    file: "/repo/src/Home.tsx".to_string(),
                    name: "Home".to_string(),
                },
            ],
        }];

        assert_eq!(
            citation_subject(&episodes, "symbol:Home"),
            Some(("Home".to_string(), "/repo/src/Home.tsx".to_string()))
        );
    }

    #[test]
    fn citation_subject_does_not_guess_for_non_symbol_dreams() {
        let episodes = [CitationEpisode {
            session_id: "session".to_string(),
            anchors: vec![CitationAnchor {
                file: "/repo/a.rs".to_string(),
                name: "arbitrary".to_string(),
            }],
        }];

        assert!(citation_subject(&episodes, "episode:ep-1").is_none());
        assert!(citation_subject(&episodes, "era:deadbeef").is_none());
    }

    #[test]
    fn project_dir_resolution_rejects_ambiguous_session_paths() {
        let storage = open();
        let temp = tempfile::tempdir().unwrap();
        let projects_root = temp.path().join("claude-root");
        storage
            .with_connection(|conn| {
                for project_dir in ["-repo-a", "-repo-b"] {
                    let path = projects_root.join(project_dir).join("same-session.jsonl");
                    conn.execute(
                        "INSERT INTO import_state (file_path, conversation_id, chunks_imported)
                         VALUES (?1, 'same-session', 0)",
                        params![path.to_string_lossy()],
                    )?;
                }
                assert!(project_dir_for_parent(conn, "same-session", &projects_root)?.is_none());
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn project_dir_resolution_ignores_paths_outside_the_injected_root() {
        let storage = open();
        let temp = tempfile::tempdir().unwrap();
        let projects_root = temp.path().join("claude-root");
        storage
            .with_connection(|conn| {
                let outside = temp
                    .path()
                    .join("other-root")
                    .join("-wrong")
                    .join("session.jsonl");
                let inside = projects_root.join("-right").join("session.jsonl");
                for path in [outside, inside] {
                    conn.execute(
                        "INSERT INTO import_state (file_path, conversation_id, chunks_imported)
                         VALUES (?1, 'session', 0)",
                        params![path.to_string_lossy()],
                    )?;
                }
                assert_eq!(
                    project_dir_for_parent(conn, "session", &projects_root)?.as_deref(),
                    Some("-right")
                );
                Ok(())
            })
            .unwrap();
    }

    // -----------------------------------------------------------------
    // Ranked queue ordering
    // -----------------------------------------------------------------

    fn mk_entry(topic_key: &str, score: f64, drainable: bool) -> RankedEntry {
        RankedEntry {
            project: "p".to_string(),
            topic_key: topic_key.to_string(),
            score,
            drainable,
            generator: "ledger".to_string(),
            tier: Some("witnessed".to_string()),
            days_since_last_touch: None,
            receipt: None,
            kind: EntryKind::Unfinished {
                episode_id: "e".to_string(),
            },
        }
    }

    #[test]
    fn drainable_entries_sort_before_archived_regardless_of_score() {
        let mut entries = [
            mk_entry("t1", 0.99, false), // high score but archived
            mk_entry("t2", 0.10, true),  // low score but drainable
        ];
        entries.sort_by(|a, b| {
            b.drainable
                .cmp(&a.drainable)
                .then_with(|| b.score.partial_cmp(&a.score).unwrap())
        });
        assert!(entries[0].drainable);
        assert_eq!(entries[0].topic_key, "t2");
    }

    #[test]
    fn select_topic_distinct_skips_duplicate_topics_and_never_backfills() {
        let entries = vec![
            mk_entry("t1", 0.9, true),
            mk_entry("t1", 0.8, true), // same topic, lower score — skipped
            mk_entry("t2", 0.7, true),
            mk_entry("t3", 0.6, false), // not drainable — excluded entirely
        ];
        let picked = select_topic_distinct(&entries, 5);
        let topics: Vec<&str> = picked.iter().map(|e| e.topic_key.as_str()).collect();
        assert_eq!(topics, vec!["t1", "t2"]);
    }

    #[test]
    fn select_topic_distinct_respects_the_cap() {
        let entries = vec![
            mk_entry("t1", 0.9, true),
            mk_entry("t2", 0.8, true),
            mk_entry("t3", 0.7, true),
        ];
        let picked = select_topic_distinct(&entries, 2);
        assert_eq!(picked.len(), 2);
    }

    // -----------------------------------------------------------------
    // Integration: synthetic corpus + mocked adjudicator producing 2
    // verified relations => dry-run counts, report ordering, drain cap,
    // topic dedup.
    // -----------------------------------------------------------------

    fn mock_adjudicator_extracts_quotes_from_the_prompt(
    ) -> impl Fn(Option<&str>, &str) -> AdjudicateAttempt {
        |_model, prompt: &str| {
            // Pull the first "Request: ..." line out of each episode block —
            // a real substring of the prompt (and therefore of the episode
            // record text `quote_verified` checks against), so this mock
            // exercises the REAL quote-verification path rather than
            // sidestepping it.
            let extract = |label: &str| -> String {
                prompt
                    .split(label)
                    .nth(1)
                    .and_then(|rest| rest.split("Request: ").nth(1))
                    .and_then(|rest| rest.lines().next())
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            };
            let quote_a = extract("=== EPISODE A");
            let quote_b = extract("=== EPISODE B");
            let body = serde_json::json!({
                "quote_a_attests_a": true,
                "quote_b_attests_b": true,
                "incompatible": true,
                "extended": false,
                "quote_a": quote_a,
                "quote_b": quote_b,
                "oids": [],
            })
            .to_string();
            AdjudicateAttempt::Parsed(ParsedNarrative {
                text: body,
                model: "mock".to_string(),
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            })
        }
    }

    /// Directly seeds one adjudication-ready `dream_relations` row plus its
    /// two backing episodes — same shortcut `adjudicate.rs`'s own
    /// `seed_pair` test helper uses, so this test exercises Stage 5's new
    /// code (adjudicate -> verify -> compose -> drain) against realistic
    /// verified data without re-deriving pair-generation, which stages 2-3
    /// already cover exhaustively.
    fn seed_relation(
        conn: &Connection,
        n: usize,
        request_a: &str,
        request_b: &str,
        gate_score: f64,
    ) {
        let ep_a = format!("ep-{n}-a");
        let ep_b = format!("ep-{n}-b");
        insert_episode(
            conn,
            &ep_a,
            &ep_a,
            "p",
            "2020-01-01T00:00:00Z",
            request_a,
            "c",
            Some("t"),
        );
        insert_episode(
            conn,
            &ep_b,
            &ep_b,
            "p",
            "2020-02-01T00:00:00Z",
            request_b,
            "c",
            None,
        );
        conn.execute(
            "INSERT INTO dream_relations
                (project, ep_a, ep_b, relation, generator, topic_key, tier, gate_score,
                 now_hook, status, load_bearing_oid, oid_provenance)
             VALUES ('p', ?1, ?2, 'replaced_by', 'era', ?3, 'unverified', ?4,
                     'open_todo', 'queued', NULL, 'created_at_fallback')",
            params![ep_a, ep_b, format!("symbol:sym{n}"), gate_score],
        )
        .unwrap();
    }

    #[test]
    fn persisted_supersession_carries_audited_subagent_receipts() {
        let temp = tempfile::tempdir().unwrap();
        let projects_root = temp.path().join("projects");
        let project_dir = "-Users-rama-projects-repo";
        let subagents = projects_root
            .join(project_dir)
            .join("parent-a")
            .join("subagents");
        fs::create_dir_all(&subagents).unwrap();
        let transcript = subagents.join("agent-cited.jsonl");
        let transcript_bytes = format!(
            "{}\n",
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "tool-edit",
                        "name": "Edit",
                        "input": {
                            "file_path": "/repo/HomeScreen.tsx",
                            "old_string": "before",
                            "new_string": "prefix HomeScreen suffix"
                        }
                    }]
                }
            })
        );
        let expected_offset = transcript_bytes.rfind("HomeScreen").unwrap();
        fs::write(&transcript, transcript_bytes).unwrap();

        let storage = open();
        storage
            .with_connection(|conn| {
                insert_episode(
                    conn,
                    "ep-a",
                    "parent-a",
                    "p",
                    "2020-01-01T00:00:00Z",
                    "old approach",
                    "old completion",
                    None,
                );
                insert_episode(
                    conn,
                    "ep-b",
                    "parent-b",
                    "p",
                    "2020-02-01T00:00:00Z",
                    "new approach",
                    "new completion",
                    None,
                );
                let anchors = serde_json::json!([{
                    "file": "/repo/HomeScreen.tsx",
                    "node_kind": "function",
                    "name": "HomeScreen",
                    "body_hash": "hash"
                }])
                .to_string();
                conn.execute(
                    "UPDATE episode_index SET anchors_json = ?1 WHERE episode_id IN ('ep-a', 'ep-b')",
                    params![anchors],
                )?;
                for parent in ["parent-a", "parent-b"] {
                    let parent_path = projects_root
                        .join(project_dir)
                        .join(format!("{parent}.jsonl"));
                    conn.execute(
                        "INSERT INTO import_state (file_path, conversation_id, chunks_imported)
                         VALUES (?1, ?2, 0)",
                        params![parent_path.to_string_lossy(), parent],
                    )?;
                }
                conn.execute(
                    "INSERT INTO dream_relations
                        (project, ep_a, ep_b, relation, generator, topic_key, tier, gate_score,
                         now_hook, status, load_bearing_oid, oid_provenance, quote_a, quote_b)
                     VALUES ('p', 'ep-a', 'ep-b', 'replaced_by', 'ledger', 'symbol:HomeScreen',
                             'witnessed', 0.9, 'open_todo', 'queued', NULL,
                             'created_at_fallback', 'old approach', 'new approach')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let entry = storage
            .with_connection(|conn| {
                let mut entries = build_ranked_queue(conn, Utc::now())?;
                Ok(entries.remove(0))
            })
            .unwrap();

        assert!(drain_one(&storage, &entry, Utc::now(), Some(&projects_root),).unwrap());

        let stored: String = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT evidence_provenance FROM dreams_v1 WHERE category = 'supersession'",
                    [],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        let provenance: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(
            provenance["parent_sessions"],
            serde_json::json!(["parent-a", "parent-b"])
        );
        assert_eq!(
            provenance["subagent_sessions"],
            serde_json::json!(["cited"])
        );
        assert_eq!(provenance["bar_clause_met"], true);
        assert_eq!(provenance["attribution"], "transcript_path");
        assert_eq!(provenance["citations"][0]["session_id"], "cited");
        assert_eq!(provenance["citations"][0]["byte_offset"], expected_offset);
        assert_eq!(provenance["citations"][0]["needle"], "HomeScreen");
        assert_eq!(provenance["citations"][0]["tool_name"], "Edit");
        assert_eq!(
            provenance["citations"][0]["transcript_path"],
            transcript.to_string_lossy().as_ref()
        );
        let offset = provenance["citations"][0]["byte_offset"].as_u64().unwrap() as usize;
        let bytes = fs::read(transcript).unwrap();
        assert!(bytes[offset..].starts_with(b"HomeScreen"));
    }

    #[test]
    fn end_to_end_two_verified_relations_report_and_drain() {
        // Guards against a concurrently-running `CSR_NO_DREAMING`/
        // `CSR_NO_AI_NARRATIVES`-toggling test elsewhere in the crate
        // racing this test's `run_adjudication_with`/`scan_unfinished`
        // calls via the shared process-global env var — see
        // `env_test_guard`'s own doc.
        let _g = crate::daemon::dream_cadence::env_test_guard();
        let storage = open();
        storage
            .with_connection(|conn| {
                seed_relation(
                    conn,
                    1,
                    "use the old cache layer",
                    "use the new cache layer",
                    0.90,
                );
                seed_relation(
                    conn,
                    2,
                    "use synchronous IO for uploads",
                    "use async IO for uploads",
                    0.60,
                );
                Ok(())
            })
            .unwrap();

        // Before adjudication: nothing verified yet, `--dry-run`-equivalent
        // count (queue depth) is 2.
        let queue_before = storage
            .with_connection(super::super::adjudicate::queue_depth)
            .unwrap();
        assert_eq!(queue_before, 2, "dry-run/backlog count before adjudication");

        // Mocked adjudicator: both candidates come back REPLACED_BY with
        // quotes lifted straight from the real prompt text.
        let actor = mock_adjudicator_extracts_quotes_from_the_prompt();
        let stats = run_adjudication_with(&actor, &storage, 10).unwrap();
        assert_eq!(stats.attempted, 2);
        assert_eq!(stats.related, 2);
        assert_eq!(
            stats.verify_passed, 2,
            "both quotes must verify against the real episode text"
        );
        assert_eq!(stats.verify_failed, 0);

        // Both rows are now witnessed and queued (not yet drained).
        let witnessed: i64 = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM dream_relations WHERE tier = 'witnessed' AND status = 'queued'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(witnessed, 2);

        // --report: full ranked queue, higher gate_score first (both
        // drainable, so score alone decides order).
        let entries = storage
            .with_connection(|conn| build_ranked_queue(conn, Utc::now()))
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.drainable));
        assert!(
            entries[0].score >= entries[1].score,
            "report must rank by descending score among equally-drainable entries"
        );
        assert_eq!(entries[0].topic_key, "symbol:sym1");
        let report_text = render_full_report(&entries, REPORT_PREVIEW_N);
        assert!(report_text.contains("symbol:sym1"));
        assert!(report_text.contains("symbol:sym2"));
        assert!(report_text.contains("[now]"));

        // Drain cap: n=1 drains only the top-scoring topic.
        let drain_stats = drain(&storage, 1).unwrap();
        assert_eq!(drain_stats.candidates, 2);
        assert_eq!(drain_stats.drained, 1);
        let dream_count: i64 = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM dreams_v1 WHERE category = 'supersession'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(dream_count, 1);
        let uncited_provenance: String = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT evidence_provenance FROM dreams_v1 WHERE category = 'supersession'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        let uncited_provenance: serde_json::Value =
            serde_json::from_str(&uncited_provenance).unwrap();
        assert_eq!(uncited_provenance["bar_clause_met"], false);
        assert_eq!(
            uncited_provenance["subagent_sessions"],
            serde_json::json!([])
        );
        let drained_relation_status: String = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT status FROM dream_relations WHERE topic_key = 'symbol:sym1'",
                    [],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(drained_relation_status, "drained");

        // The remaining entry (sym2) is untouched and still drainable next run.
        let remaining = storage
            .with_connection(|conn| build_ranked_queue(conn, Utc::now()))
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].topic_key, "symbol:sym2");

        // Draining again with a larger n picks up exactly the remainder —
        // and does NOT re-drain sym1 (it left the queue entirely).
        let drain_stats2 = drain(&storage, 5).unwrap();
        assert_eq!(drain_stats2.drained, 1);
        assert_eq!(drain_stats2.candidates, 1);
    }

    #[test]
    fn drain_enforces_the_thirty_day_per_topic_reuse_gate() {
        // See `end_to_end_two_verified_relations_report_and_drain`'s guard comment.
        let _g = crate::daemon::dream_cadence::env_test_guard();
        let storage = open();
        storage
            .with_connection(|conn| {
                seed_relation(conn, 1, "approach one", "approach two", 0.90);
                Ok(())
            })
            .unwrap();
        let actor = mock_adjudicator_extracts_quotes_from_the_prompt();
        run_adjudication_with(&actor, &storage, 10).unwrap();

        let first = drain(&storage, 5).unwrap();
        assert_eq!(first.drained, 1);

        // Re-verify a fresh candidate on the SAME topic_key within the
        // 30-day window (simulating a re-generated candidate for the same
        // symbol) — it must be skipped, not drained a second time.
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO reflections (id, content, tags, timestamp)
                     VALUES ('ep-1-c', ?1, '[]', '2020-03-01T00:00:00Z')",
                    params![format!(
                        r#"{{"schema":"v2","session_id":"ep-1-c","project":"p",
                            "timestamp":"2020-03-01T00:00:00Z","request":"approach three",
                            "completed":"c","outcome":"completed","todos":[],
                            "files_modified":[],"anchors":[]}}"#
                    )],
                )?;
                crate::storage::dream_backfill::materialize_episode_index(conn)?;
                conn.execute(
                    "INSERT INTO dream_relations
                        (project, ep_a, ep_b, relation, generator, topic_key, tier, gate_score,
                         now_hook, status, load_bearing_oid, oid_provenance)
                     VALUES ('p', 'ep-1-b', 'ep-1-c', 'replaced_by', 'ledger', 'symbol:sym1',
                             'witnessed', 0.95, 'open_todo', 'queued', NULL, 'created_at_fallback')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let second = drain(&storage, 5).unwrap();
        assert_eq!(second.candidates, 1);
        assert_eq!(second.drained, 0);
        assert_eq!(second.skipped_recent_topic, 1);
    }

    #[test]
    fn disabled_drain_touches_nothing() {
        let _g = crate::daemon::dream_cadence::env_test_guard();
        std::env::set_var("CSR_NO_DREAMING", "1");
        let storage = open();
        let stats = drain(&storage, 5);
        std::env::remove_var("CSR_NO_DREAMING");
        let stats = stats.unwrap();
        assert!(stats.disabled);
        assert_eq!(stats.drained, 0);
    }

    // -----------------------------------------------------------------
    // Pass 3 (intent_channel silent-abandonment persistence).
    //
    // Required test (c): a fired candidate persists a `dreams_v1` row with
    // category='unfinished', non-empty prose, and the horizon-oid receipt.
    // `AbandonmentCandidate`'s fields are all public, so this constructs
    // one directly rather than standing up a real git fixture (that path
    // is already covered end-to-end by `intent_channel`'s own tests) --
    // this test is scoped to the PERSISTENCE half only.
    // -----------------------------------------------------------------

    #[test]
    fn persist_abandonment_candidates_writes_unfinished_rows_with_horizon_receipt() {
        use crate::dream::backfill::intent_channel::{ApproachTarget, LoadedPrompt, TargetKind};
        use crate::dream::backfill::subagent_citation::DreamCitationEvidence;

        let storage = open();
        let now = Utc::now();
        let head_oid = "deadbeefcafef00d1234".to_string();
        let prompt = LoadedPrompt {
            family: "p".to_string(),
            project_path: "/repo".to_string(),
            display: "please add `widget.rs`".to_string(),
            ts: (now - Duration::days(30)).timestamp(),
            line_no: 1,
            intent_receipt: None,
        };
        let target = ApproachTarget {
            phrase: "widget.rs".to_string(),
            byte_start: 12,
            byte_end: 21,
            kind: TargetKind::PathLike,
            ident: None,
        };
        let claim = format!(
            "On 2020-01-01, you asked: \"widget.rs\" — and as of {head_oid} (2020-02-01) \
             no commit after that prompt touches `widget.rs`, and none of the 0 later \
             prompts in this project revisit it. The request appears to have been \
             silently dropped."
        );
        let evidence = DreamCitationEvidence::audited(vec!["sess-a".to_string()], vec![]);
        let candidate = crate::dream::backfill::intent_channel::AbandonmentCandidate {
            family: "p".to_string(),
            prompt,
            target,
            head_oid: head_oid.clone(),
            claim: claim.clone(),
            receipts: vec![],
            citation_evidence: evidence,
        };

        let stats = persist_abandonment_candidates(&storage, &[candidate], now).unwrap();
        assert_eq!(stats.candidates, 1);
        assert_eq!(stats.persisted, 1);
        assert_eq!(stats.skipped_recent_topic, 0);
        assert_eq!(stats.persist_errors, 0);
        assert_eq!(
            stats.bar_eligible, 0,
            "no citation was attached -- honestly 0"
        );

        let (category, prose, subject_key): (String, String, Option<String>) = storage
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT category, prose, subject_key FROM dreams_v1 WHERE project = 'p'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(category, "unfinished");
        assert!(!prose.is_empty());
        assert_eq!(prose, claim);
        assert!(
            prose.contains(&head_oid),
            "prose must carry the horizon-oid receipt"
        );
        assert_eq!(subject_key.as_deref(), Some("widget.rs"));
    }

    #[test]
    fn persist_abandonment_candidates_respects_the_thirty_day_topic_reuse_gate() {
        use crate::dream::backfill::intent_channel::{ApproachTarget, LoadedPrompt, TargetKind};
        use crate::dream::backfill::subagent_citation::DreamCitationEvidence;

        let storage = open();
        let now = Utc::now();
        let make_candidate = || crate::dream::backfill::intent_channel::AbandonmentCandidate {
            family: "p".to_string(),
            prompt: LoadedPrompt {
                family: "p".to_string(),
                project_path: "/repo".to_string(),
                display: "please add `widget.rs`".to_string(),
                ts: (now - Duration::days(30)).timestamp(),
                line_no: 1,
                intent_receipt: None,
            },
            target: ApproachTarget {
                phrase: "widget.rs".to_string(),
                byte_start: 12,
                byte_end: 21,
                kind: TargetKind::PathLike,
                ident: None,
            },
            head_oid: "abc123".to_string(),
            claim: "claim text abc123".to_string(),
            receipts: vec![],
            citation_evidence: DreamCitationEvidence::audited(vec![], vec![]),
        };

        let first = persist_abandonment_candidates(&storage, &[make_candidate()], now).unwrap();
        assert_eq!(first.persisted, 1);

        let second = persist_abandonment_candidates(&storage, &[make_candidate()], now).unwrap();
        assert_eq!(
            second.persisted, 0,
            "same (project, subject_key) within 30 days must be skipped, not duplicated"
        );
        assert_eq!(second.skipped_recent_topic, 1);
    }

    #[test]
    fn persist_abandonment_candidates_is_a_no_op_over_an_empty_slice() {
        let storage = open();
        let stats = persist_abandonment_candidates(&storage, &[], Utc::now()).unwrap();
        assert_eq!(stats, AbandonmentDrainStats::default());
    }
}
