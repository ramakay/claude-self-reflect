//! Dream backfill — rank & gate + `--dry-run` (`.plans/dream-backfill-design.md`
//! §3 "Stage 3 — rank & gate", with the D1/D7/D11(partial) round-2 deltas
//! from §8 folded in, since that section overrides §3-6 wherever they
//! conflict).
//!
//! Scores every [`super::pairs`] candidate with the backfill-mode Queue S
//! formula (D1 — a separate const block from any future nightly-mode
//! weights), applies the now-hook eligibility gate, then a per-project
//! absolute floor + top-K + margin rule, and persists the survivors into
//! `dream_relations` as pre-adjudication `queued` rows (or `archived` for
//! now-hook-ineligible ones — D1: "pure-historical supersession → archived
//! in report, never a dream slot"). [`dry_run`] chains this after Stage 0
//! ([`crate::storage::dream_backfill::refresh_episode_index`]) and Stage 1
//! ([`super::unfinished::scan_unfinished`]) so a single call exercises the
//! whole zero-LLM half of the pipeline and produces the report the design's
//! acceptance protocol (§8 D11) reads before Stage 4 ever spends budget.
//!
//! # What gets persisted, and why not everything
//!
//! `dream_relations`'s own schema comment (`storage::migrations::run`)
//! frames it as holding "Stage 6 verified relations" — this stage does not
//! try to make it a dump of every candidate ever generated (candidates are
//! cheap to regenerate deterministically; nothing is lost by not keeping
//! them). Only two dispositions get a row, both consistent with the design's
//! own status vocabulary:
//!
//! - **`archived`**: now-hook-ineligible, regardless of score (D1's own
//!   language for this bucket).
//! - **`queued`**: now-hook-eligible, at/above the absolute floor, and
//!   inside the per-project top-K window after the margin-rule shrink.
//!
//! Eligible-but-below-floor and eligible-but-outside-the-window candidates
//! are simply not persisted — they are not "wrong", just not worth a
//! pre-adjudication row yet; a future backlog count (D11, a later stage) is
//! expected to re-run generation+gating on demand rather than read a table
//! of everything ever considered.
//!
//! `tier` is always written `'unverified'` (no adjudication has happened),
//! `quote_a`/`quote_b` are left at their table defaults (empty strings) —
//! Stage 4/5 fill those in once a candidate is actually adjudicated and
//! verified. Writes are `INSERT OR IGNORE` against the existing
//! `UNIQUE(project, ep_a, ep_b, relation)` index: a re-run never duplicates
//! or churns an already-recorded candidate's score out from under an
//! in-progress adjudication.
//!
//! Zero LLM calls anywhere in this module.

use std::cmp::Ordering;
use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

use crate::daemon::dream_cadence::dreaming_disabled;
use crate::hooks::intent::cosine_sim;
use crate::storage::dream_backfill::{
    episode_vectors, refresh_episode_index, AlivenessStats, MaterializeStats,
};

use super::family::Family;
use super::pairs::{
    self, EpisodeRow, Generator, OidProvenance, PairCandidate, PairReceipt, Relation,
};
use super::unfinished::{scan_unfinished_at, UnfinishedScanReport};

// ---------------------------------------------------------------------
// Backfill-mode Queue S weights (design §3 Stage 3, D1 override) — a
// DELIBERATELY separate const block from any future nightly-mode Queue S
// weights, per D1's explicit instruction.
// ---------------------------------------------------------------------

const WEIGHT_SIM: f64 = 0.25;
const WEIGHT_ERA_GAP: f64 = 0.20;
const WEIGHT_OUTCOME_CONTRAST: f64 = 0.15;
const WEIGHT_GENERATOR: f64 = 0.15;
const WEIGHT_NOW_HOOK: f64 = 0.25;
const RECENCY_PENALTY: f64 = 0.40;
const RECENCY_PENALTY_WINDOW_DAYS: i64 = 30;

const GENERATOR_WEIGHT_LEDGER: f64 = 1.0;
/// Equal to ledger's weight, not a lesser value, on purpose (undocumented by
/// the design beyond D2 naming relapse as its own generator): a relapse
/// candidate IS ledger-grounded evidence — it exists only because a symbol
/// already carries a `superseded_by`/`anchor_obsolete` verdict AND a later
/// episode re-touched it — so it deserves the same evidentiary weight as a
/// straight ledger pair, not era's softer topical-clustering discount.
const GENERATOR_WEIGHT_RELAPSE: f64 = 1.0;
const GENERATOR_WEIGHT_ERA: f64 = 0.7;

/// Days-between-episodes value at which the era-gap term saturates to 1.0.
/// Undocumented by the design (only the term's own weight, 0.20, is given);
/// a judgment call in the same spirit as Stage 2's `QUEUE_U_TODO_CAP`.
const ERA_GAP_CAP_DAYS: f64 = 90.0;

/// Absolute floor `tau_abs` (design §3 Stage 3: "Absolute floor τ_abs").
/// The design names the mechanism but not the number; a judgment call —
/// picked low enough that a genuinely-evidenced candidate (generator weight
/// alone contributes up to 0.15) is not reflexively floored out, high
/// enough that pure noise (near-zero on every term) cannot reach it.
const TAU_ABS_FLOOR: f64 = 0.35;

/// Per-project queue depth before the margin rule can shrink it further.
const TOP_K: usize = 12;

/// Margin-rule ambiguity threshold ("score[K]−score[K+1] < δ ⇒ shrink K").
/// Undocumented numeric value; a judgment call — small enough to only
/// trigger on genuine near-ties, not on every naturally-decaying ranking.
const MARGIN_DELTA: f64 = 0.02;

fn generator_weight(g: Generator) -> f64 {
    match g {
        Generator::Ledger => GENERATOR_WEIGHT_LEDGER,
        Generator::Relapse => GENERATOR_WEIGHT_RELAPSE,
        Generator::Era => GENERATOR_WEIGHT_ERA,
    }
}

/// P3 (F3) generator-priority tie-break: when two candidates score exactly
/// equal, the higher-revelation generator heads the queue. Relapse ranks
/// above ledger because an unconscious re-touch of an already-retired
/// symbol (D2's whole reason for existing) is a stronger "you forgot this"
/// signal than a ledger-derived supersession the advocate presumably
/// already knew about; ledger ranks above era because it is a git-verified
/// symbol-precision join, era only a topical-clustering proxy.
fn generator_rank(g: Generator) -> u8 {
    match g {
        Generator::Relapse => 0,
        Generator::Ledger => 1,
        Generator::Era => 2,
    }
}

/// P3 (F3): sort `eligible` by descending score with the generator-priority
/// tie-break above, then collapse duplicate `(project, ep_a, ep_b)` stories
/// across generators to the single best-ranked row. D9 already makes the
/// decided relation generator-independent (the LLM/verify pipeline never
/// looks at which generator proposed a pair), so a second hypothesis row for
/// the exact same pair is pure duplicated adjudication budget, not distinct
/// evidence — keeping the best-ranked one is lossless. Runs BEFORE the
/// margin-rule shrink so the score vector it inspects stays aligned with
/// `eligible`'s own (now deduplicated) order.
fn rank_and_dedup_eligible(
    mut eligible: Vec<(PairCandidate, f64, &'static str)>,
) -> Vec<(PairCandidate, f64, &'static str)> {
    eligible.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| generator_rank(a.0.generator).cmp(&generator_rank(b.0.generator)))
            .then_with(|| a.0.ep_a.cmp(&b.0.ep_a))
            .then_with(|| a.0.ep_b.cmp(&b.0.ep_b))
    });
    let mut seen: std::collections::HashSet<(String, String, String)> = Default::default();
    eligible.retain(|(c, _, _)| seen.insert((c.project.clone(), c.ep_a.clone(), c.ep_b.clone())));
    eligible
}

/// Undefined by the design beyond its 0.15 weight — a graded read
/// symmetrical with Stage 2's `severity`: the strongest contrast is
/// "was struggling, now resolved"; any other outcome mismatch is a weaker
/// signal; identical outcomes carry none.
fn outcome_contrast(outcome_a: &str, outcome_b: &str) -> f64 {
    let unresolved = |o: &str| o == "partial" || o == "failed";
    if unresolved(outcome_a) && outcome_b == "completed" {
        1.0
    } else if outcome_a != outcome_b {
        0.5
    } else {
        0.0
    }
}

/// D1 now-hook gate: "eligible only if it cites a live consequence (open
/// todo, non-empty blockers, or a live file still carrying the old
/// pattern)". Checked across BOTH episodes of the pair — either side's live
/// consequence is enough. Returns the reason (for the `now_hook` column and
/// the dry-run report) or `None` when ineligible.
fn now_hook_reason(a: &EpisodeRow, b: &EpisodeRow) -> Option<&'static str> {
    if a.todo_count > 0 || b.todo_count > 0 {
        Some("open_todo")
    } else if a.blockers.is_some() || b.blockers.is_some() {
        Some("blockers")
    } else if a.present_at_head == Some(true) || b.present_at_head == Some(true) {
        Some("live_file")
    } else {
        None
    }
}

/// Backfill-mode Queue S score (D1): `0.25*sim + 0.20*era_gap +
/// 0.15*outcome_contrast + 0.15*generator_w + 0.25*now_hook − 0.40*[event <
/// 30d old]`.
///
/// Returns `(base, final_score, now_hook_reason)` (P4/F4): `base` is the
/// score BEFORE the recency penalty — an evidence-quality reading the
/// [`TAU_ABS_FLOOR`] check gates on — and `final_score` is `base` minus the
/// penalty when it applies, the value actually ranked/persisted/queued. The
/// penalty is a forgetting-DEFICIT demotion (D1: "pure-historical
/// supersession... archived", not "erased"); the floor is an evidence-
/// quality certification. Applying the floor to `final_score` instead would
/// compose the two multiplicatively and erase a well-evidenced-but-recent
/// candidate entirely rather than merely ranking it last (F4).
fn gate_score(
    candidate: &PairCandidate,
    ep_a: &EpisodeRow,
    ep_b: &EpisodeRow,
    vectors: &HashMap<String, Vec<f32>>,
    now: DateTime<Utc>,
) -> (f64, f64, Option<&'static str>) {
    let sim = match (vectors.get(&candidate.ep_a), vectors.get(&candidate.ep_b)) {
        (Some(a), Some(b)) => (cosine_sim(a, b) as f64).max(0.0),
        _ => 0.0,
    };

    let days_apart = (candidate.ts_b - candidate.ts_a).num_days().max(0);
    let mut era_gap = (days_apart as f64 / ERA_GAP_CAP_DAYS).clamp(0.0, 1.0);
    if candidate.oid_provenance == OidProvenance::CreatedAtFallback {
        // D6: "fallback rows get era-gap term x0.5".
        era_gap *= 0.5;
    }

    let contrast = outcome_contrast(&ep_a.outcome, &ep_b.outcome);
    let gen_w = generator_weight(candidate.generator);
    let now_hook = now_hook_reason(ep_a, ep_b);
    let now_hook_term = if now_hook.is_some() { 1.0 } else { 0.0 };

    let base = WEIGHT_SIM * sim
        + WEIGHT_ERA_GAP * era_gap
        + WEIGHT_OUTCOME_CONTRAST * contrast
        + WEIGHT_GENERATOR * gen_w
        + WEIGHT_NOW_HOOK * now_hook_term;

    let final_score = if (now - candidate.event_time).num_days() < RECENCY_PENALTY_WINDOW_DAYS {
        base - RECENCY_PENALTY
    } else {
        base
    };

    (base, final_score, now_hook)
}

/// A gated candidate, ready to persist. Carries the full [`PairReceipt`] for
/// the dry-run report even though `dream_relations` has no column for
/// `receipt.symbol`/`receipt.hashes` — for ledger/relapse those are already
/// recoverable from `topic_key` (`"symbol:<name>"`); for era they are
/// generation-time-only detail, same as the design's own framing of
/// candidates as cheap to regenerate.
#[derive(Debug, Clone)]
pub struct GatedRelation {
    pub project: String,
    pub ep_a: String,
    pub ep_b: String,
    pub relation: Relation,
    pub generator: Generator,
    pub topic_key: String,
    pub receipt: PairReceipt,
    pub load_bearing_oid: Option<String>,
    pub aux_oid: Option<String>,
    pub oid_provenance: OidProvenance,
    pub gate_score: f64,
    pub now_hook: Option<&'static str>,
    pub status: &'static str, // "queued" | "archived"
}

#[derive(Debug, Clone, Default)]
pub struct GateStats {
    pub project: String,
    pub generated: usize,
    pub generated_ledger: usize,
    pub generated_relapse: usize,
    pub generated_era: usize,
    pub archived: usize,
    pub below_floor: usize,
    pub outside_window: usize,
    pub queued: usize,
}

/// Never shrink the margin-rule window below 1 when at least one eligible,
/// above-floor candidate exists — the rule's purpose is to avoid an
/// awkward cut through a tie cluster, not to suppress the single best
/// candidate when everything ties (a judgment call the design's literal
/// "shrink K" wording does not itself bound).
fn apply_margin_shrink(sorted_desc_scores: &[f64], max_k: usize, delta: f64) -> usize {
    let mut k = max_k.min(sorted_desc_scores.len());
    while k > 1 && k < sorted_desc_scores.len() {
        let gap = sorted_desc_scores[k - 1] - sorted_desc_scores[k];
        if gap < delta {
            k -= 1;
        } else {
            break;
        }
    }
    k
}

/// Generate, score, gate, and rank every candidate pair for one project.
/// Does not touch the database beyond the reads [`pairs::load_project_episodes`]
/// and [`episode_vectors`] already perform — persistence is a separate step
/// ([`persist_relations`]) so tests can inspect gating decisions without a
/// write.
pub fn gate_project(
    conn: &Connection,
    project: &str,
    now: DateTime<Utc>,
) -> Result<(Vec<GatedRelation>, GateStats)> {
    gate_family(conn, &Family::single(project), now)
}

/// Family-corpus Stage 2-3 (2026-08-26 design ruling: generators run over
/// the cross-project family corpus; the project key is attribution
/// metadata, not a partition boundary). [`gate_project`] is the
/// single-member compatibility wrapper — identical behavior for a
/// one-key family, which is also exactly the pre-family semantics the
/// golden fixture pins.
pub fn gate_family(
    conn: &Connection,
    family: &Family,
    now: DateTime<Utc>,
) -> Result<(Vec<GatedRelation>, GateStats)> {
    let episodes = pairs::load_family_episodes(conn, family)?;
    let ledger_idx = pairs::FamilyLedgerIndex::load(conn, family)?;
    let by_id: HashMap<&str, &EpisodeRow> = episodes
        .iter()
        .map(|e| (e.episode_id.as_str(), e))
        .collect();

    let all_vectors = episode_vectors(conn)?;
    let episode_ids: std::collections::HashSet<&str> =
        episodes.iter().map(|e| e.episode_id.as_str()).collect();
    let vectors: HashMap<String, Vec<f32>> = all_vectors
        .into_iter()
        .filter(|(id, _)| episode_ids.contains(id.as_str()))
        .collect();

    let mut candidates = pairs::generate_ledger_pairs(conn, family, &ledger_idx, &episodes)?;
    let ledger_count = candidates.len();
    candidates.extend(pairs::generate_relapse_pairs(
        conn,
        family,
        &ledger_idx,
        &episodes,
    )?);
    let relapse_count = candidates.len() - ledger_count;
    candidates.extend(pairs::generate_era_pairs(
        conn, family, &episodes, &vectors,
    )?);
    let era_count = candidates.len() - ledger_count - relapse_count;

    let mut stats = GateStats {
        project: family.name.clone(),
        generated: candidates.len(),
        generated_ledger: ledger_count,
        generated_relapse: relapse_count,
        generated_era: era_count,
        ..Default::default()
    };

    let mut relations = Vec::new();
    let mut eligible: Vec<(PairCandidate, f64, &'static str)> = Vec::new();

    for candidate in candidates {
        let (Some(&ep_a), Some(&ep_b)) = (
            by_id.get(candidate.ep_a.as_str()),
            by_id.get(candidate.ep_b.as_str()),
        ) else {
            continue; // defensive: every candidate's episodes come from `episodes` itself
        };
        let (base, final_score, now_hook) = gate_score(&candidate, ep_a, ep_b, &vectors, now);

        match now_hook {
            None => {
                stats.archived += 1;
                relations.push(to_gated(&candidate, final_score, None, "archived"));
            }
            Some(reason) => {
                // P4/F4: the floor certifies evidential quality and is
                // checked against `base` (pre-penalty) — the recency
                // penalty only demotes an already-evidenced candidate's
                // rank, it must never compose with the floor to erase it.
                if base < TAU_ABS_FLOOR {
                    stats.below_floor += 1;
                    continue;
                }
                eligible.push((candidate, final_score, reason));
            }
        }
    }

    let eligible = rank_and_dedup_eligible(eligible);
    let scores: Vec<f64> = eligible.iter().map(|(_, s, _)| *s).collect();
    // The top-K window budget is PER PROJECT KEY, preserved under family
    // merging: [`TOP_K`] was sized when every key gated separately, so a
    // family of N keys gets N windows' worth — otherwise merging keys
    // would silently shrink the total queue budget the same corpus had
    // before the 2026-08-26 family ruling (measured on the live corpus:
    // one 12-slot window over the merged anukriti family margin-shrank to
    // 2 queued where the split keys had queued 16). Single-member
    // families ([`Family::single`], the golden fixture) are byte-identical
    // to the pre-family behavior.
    let k = apply_margin_shrink(&scores, TOP_K * family.members.len(), MARGIN_DELTA);
    stats.outside_window = eligible.len().saturating_sub(k);
    stats.queued = k;

    for (candidate, score, reason) in eligible.into_iter().take(k) {
        relations.push(to_gated(&candidate, score, Some(reason), "queued"));
    }

    Ok((relations, stats))
}

fn to_gated(
    candidate: &PairCandidate,
    score: f64,
    now_hook: Option<&'static str>,
    status: &'static str,
) -> GatedRelation {
    GatedRelation {
        project: candidate.project.clone(),
        ep_a: candidate.ep_a.clone(),
        ep_b: candidate.ep_b.clone(),
        relation: candidate.relation,
        generator: candidate.generator,
        topic_key: candidate.topic_key.clone(),
        receipt: candidate.receipt.clone(),
        load_bearing_oid: candidate.receipt.receipt_oid.clone(),
        aux_oid: candidate.aux_oid.clone(),
        oid_provenance: candidate.oid_provenance,
        gate_score: score,
        now_hook,
        status,
    }
}

/// Persist `relations` into `dream_relations` — `INSERT OR IGNORE` against
/// the existing `UNIQUE(project, ep_a, ep_b, relation)` index (see the
/// module doc's "what gets persisted" section for the full write policy).
/// Returns the number of rows actually inserted (vs. already present).
pub fn persist_relations(conn: &Connection, relations: &[GatedRelation]) -> Result<usize> {
    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO dream_relations
            (project, ep_a, ep_b, relation, generator, topic_key, tier,
             load_bearing_oid, aux_oid, oid_provenance, gate_score, now_hook, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'unverified', ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;
    let mut inserted = 0usize;
    for r in relations {
        let changed = stmt.execute(params![
            r.project,
            r.ep_a,
            r.ep_b,
            r.relation.as_str(),
            r.generator.as_str(),
            r.topic_key,
            r.load_bearing_oid,
            r.aux_oid,
            r.oid_provenance.as_str(),
            r.gate_score,
            r.now_hook,
            r.status,
        ])?;
        inserted += changed;
    }
    Ok(inserted)
}

// `distinct_projects` (the raw-key enumerator this stage used before the
// 2026-08-26 family ruling) is gone: both the CLI's real run and `dry_run`
// enumerate `super::family::compute_families` now, so no caller wants raw
// keys any more.

/// Round-robin interleave of already-`queued` relations across projects,
/// each project's own list sorted descending by score first — "per-project
/// round-robin" (design §3 Stage 3), unweighted (the design's "weighted by
/// 30-day activity" qualifier applies to the nightly steady-state, §5; this
/// stage's own task instruction asks only for plain per-project
/// round-robin, so the activity weighting is out of scope here).
fn round_robin_top_n(
    relations: Vec<GatedRelation>,
    projects: &[String],
    n: usize,
) -> Vec<GatedRelation> {
    let mut by_project: HashMap<String, Vec<GatedRelation>> = HashMap::new();
    for r in relations {
        by_project.entry(r.project.clone()).or_default().push(r);
    }
    for list in by_project.values_mut() {
        list.sort_by(|a, b| {
            b.gate_score
                .partial_cmp(&a.gate_score)
                .unwrap_or(Ordering::Equal)
        });
    }

    let mut cursors: HashMap<&str, usize> = HashMap::new();
    let mut out = Vec::new();
    loop {
        if out.len() >= n {
            break;
        }
        let mut progressed = false;
        for p in projects {
            if out.len() >= n {
                break;
            }
            let idx = cursors.entry(p.as_str()).or_insert(0);
            if let Some(list) = by_project.get(p.as_str()) {
                if let Some(r) = list.get(*idx) {
                    out.push(r.clone());
                    *idx += 1;
                    progressed = true;
                }
            }
        }
        if !progressed {
            break;
        }
    }
    out
}

/// Full pre-adjudication pipeline result: Stage 0 (materialize + aliveness),
/// Stage 1 (unfinished scan), Stage 2-3 (generate + gate, per project) — the
/// design's "--dry-run: stages 0-3 only" scope (§4), zero LLM calls anywhere.
#[derive(Debug, Clone)]
pub struct DryRunReport {
    pub disabled: bool,
    pub materialize: MaterializeStats,
    pub aliveness: AlivenessStats,
    pub unfinished: UnfinishedScanReport,
    pub gate_stats: Vec<GateStats>,
    /// Round-robin top-N `queued` relations across every project, newest
    /// gate_score first within each project (design §8 D11: "top ~20 queue
    /// heads reviewed before stage 4 spends any budget").
    pub top_heads: Vec<GatedRelation>,
}

const DRY_RUN_TOP_N: usize = 20;

/// Production entry point: Stage 0 -> Stage 1 -> (Stage 2-3 per project),
/// persisting gated candidates into `dream_relations` as it goes (writes are
/// cheap, zero-LLM, and idempotent — see the module doc). Respects
/// `CSR_NO_DREAMING`: returns a disabled report and writes nothing when set,
/// same idiom `scan_unfinished` already follows. Thin wrapper around
/// [`dry_run_at`] supplying the real wall clock.
pub fn dry_run(conn: &Connection) -> Result<DryRunReport> {
    dry_run_at(conn, Utc::now())
}

/// P5: core dry-run with `now` injected — threaded through to BOTH Stage 1
/// ([`scan_unfinished_at`]) and Stage 2-3 ([`gate_project`]) so a single
/// call uses exactly one reference time throughout, rather than each stage
/// independently sampling the wall clock moments apart. [`dry_run`] is the
/// production entry point.
pub fn dry_run_at(conn: &Connection, now: DateTime<Utc>) -> Result<DryRunReport> {
    if dreaming_disabled() {
        return Ok(DryRunReport {
            disabled: true,
            materialize: MaterializeStats::default(),
            aliveness: AlivenessStats::default(),
            unfinished: UnfinishedScanReport {
                disabled: true,
                tau_fit: crate::dream::backfill::unfinished::TauFit {
                    tau: 0.0,
                    positive_count: 0,
                    negative_count: 0,
                    confusion: Default::default(),
                },
                dispositions: Vec::new(),
                queue: Vec::new(),
                stats: Default::default(),
            },
            gate_stats: Vec::new(),
            top_heads: Vec::new(),
        });
    }

    let (materialize, aliveness) = refresh_episode_index(conn)?;
    let unfinished = scan_unfinished_at(conn, now)?;

    let families = super::family::compute_families(conn)?;
    let family_names: Vec<String> = families.iter().map(|f| f.name.clone()).collect();
    let mut gate_stats = Vec::new();
    let mut all_queued = Vec::new();

    for family in &families {
        let (relations, stats) = gate_family(conn, family, now)?;
        persist_relations(conn, &relations)?;
        all_queued.extend(relations.into_iter().filter(|r| r.status == "queued"));
        gate_stats.push(stats);
    }

    let top_heads = round_robin_top_n(all_queued, &family_names, DRY_RUN_TOP_N);

    Ok(DryRunReport {
        disabled: false,
        materialize,
        aliveness,
        unfinished,
        gate_stats,
        top_heads,
    })
}

/// Human-readable rendering of a [`DryRunReport`] — candidate counts per
/// generator/project plus the top queue heads with scores, generators,
/// topic keys, and receipts (design §4: "prints candidate counts + top
/// queue heads with scores"). Pure formatting, no I/O — a future CLI stage
/// prints this string.
pub fn render_dry_run_report(report: &DryRunReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if report.disabled {
        let _ = writeln!(out, "dream backfill dry-run: disabled (CSR_NO_DREAMING)");
        return out;
    }

    let _ = writeln!(
        out,
        "Stage 0: {} episodes materialized ({} unresolved aliveness)",
        report.materialize.upserted, report.aliveness.episodes_unresolved
    );
    let _ = writeln!(
        out,
        "Stage 1: {} seeds considered, {} never picked up ({} queue-eligible)",
        report.unfinished.stats.seeds_considered,
        report.unfinished.stats.never_picked_up,
        report.unfinished.queue.len()
    );

    for s in &report.gate_stats {
        let _ = writeln!(
            out,
            "Stage 2-3 [{}]: generated {} (ledger {} / relapse {} / era {}) -> queued {} / archived {} / below_floor {} / outside_window {}",
            s.project, s.generated, s.generated_ledger, s.generated_relapse, s.generated_era,
            s.queued, s.archived, s.below_floor, s.outside_window
        );
    }

    let _ = writeln!(out, "\nTop {} queue heads:", report.top_heads.len());
    for (i, r) in report.top_heads.iter().enumerate() {
        let _ = writeln!(
            out,
            "{:>2}. [{}] score={:.3} generator={} topic_key={} {} -> {} receipt(symbol={}, oid={}, hashes={:?})",
            i + 1,
            r.project,
            r.gate_score,
            r.generator.as_str(),
            r.topic_key,
            r.ep_a,
            r.ep_b,
            r.receipt.symbol,
            r.receipt.receipt_oid.as_deref().unwrap_or("-"),
            r.receipt.hashes,
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::witness_ledger::{insert_witness, WitnessLedgerRow};
    use crate::storage::witness_verdicts::{
        insert_verdict_if_changed, VerdictKind, WitnessVerdictRow,
    };

    fn open() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        conn
    }

    fn v2_episode(session_id: &str, project: &str, ts: &str, outcome: &str, todos: &str) -> String {
        format!(
            r#"{{
                "schema": "v2",
                "session_id": "{session_id}",
                "project": "{project}",
                "timestamp": "{ts}",
                "request": "req",
                "investigated": [],
                "completed": "done",
                "next_steps": null,
                "blockers": null,
                "outcome": "{outcome}",
                "error_signatures": [],
                "tools_used": [],
                "files_modified": [],
                "message_count": 1,
                "duration_minutes": 1,
                "todos": {todos},
                "approved_plan": null,
                "prev_episode_id": null,
                "anchors": []
            }}"#
        )
    }

    fn insert_episode(
        conn: &Connection,
        id: &str,
        session_id: &str,
        project: &str,
        ts: &str,
        outcome: &str,
        todos: &str,
    ) {
        conn.execute(
            "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, '[]', ?3)",
            params![id, v2_episode(session_id, project, ts, outcome, todos), ts],
        )
        .unwrap();
        // See `pairs::tests::insert_episode`'s comment: materialize
        // immediately so `episode_index` reflects every insert right away.
        crate::storage::dream_backfill::materialize_episode_index(conn).unwrap();
    }

    fn mk_row(
        outcome: &str,
        todo_count: i64,
        blockers: Option<&str>,
        present_at_head: Option<bool>,
    ) -> EpisodeRow {
        EpisodeRow {
            episode_id: "e".to_string(),
            session_id: "sess-e".to_string(),
            ts: crate::temporal::parse_timestamp("2020-01-01T00:00:00Z").unwrap(),
            outcome: outcome.to_string(),
            todo_count,
            blockers: blockers.map(|s| s.to_string()),
            request: String::new(),
            completed: String::new(),
            next_steps: None,
            anchors: Vec::new(),
            present_at_head,
        }
    }

    fn mk_candidate(
        ts_a: &str,
        ts_b: &str,
        generator: Generator,
        oid_provenance: OidProvenance,
        event_time: &str,
    ) -> PairCandidate {
        PairCandidate {
            project: "p".to_string(),
            ep_a: "a".to_string(),
            ep_b: "b".to_string(),
            ts_a: crate::temporal::parse_timestamp(ts_a).unwrap(),
            ts_b: crate::temporal::parse_timestamp(ts_b).unwrap(),
            relation: Relation::ReplacedBy,
            generator,
            topic_key: "symbol:foo".to_string(),
            receipt: PairReceipt {
                receipt_oid: Some("oid".to_string()),
                symbol: "foo".to_string(),
                hashes: vec![],
            },
            aux_oid: Some("head".to_string()),
            oid_provenance,
            event_time: crate::temporal::parse_timestamp(event_time).unwrap(),
        }
    }

    // -----------------------------------------------------------------
    // Now-hook gate
    // -----------------------------------------------------------------

    #[test]
    fn now_hook_fires_on_open_todo_blockers_or_live_file_but_not_otherwise() {
        let dead = mk_row("completed", 0, None, Some(false));
        let alive = mk_row("completed", 0, None, Some(true));
        let with_todo = mk_row("completed", 1, None, None);
        let with_blockers = mk_row("completed", 0, Some("stuck"), None);
        let nothing = mk_row("completed", 0, None, None);

        assert_eq!(now_hook_reason(&with_todo, &nothing), Some("open_todo"));
        assert_eq!(now_hook_reason(&nothing, &with_blockers), Some("blockers"));
        assert_eq!(now_hook_reason(&nothing, &alive), Some("live_file"));
        assert_eq!(now_hook_reason(&nothing, &dead), None);
        assert_eq!(now_hook_reason(&nothing, &nothing), None);
    }

    #[test]
    fn ineligible_pair_is_archived_never_queued() {
        let conn = open();
        insert_episode(
            &conn,
            "a",
            "sa",
            "p",
            "2020-01-01T00:00:00Z",
            "completed",
            "[]",
        );
        insert_episode(
            &conn,
            "b",
            "sb",
            "p",
            "2020-06-01T00:00:00Z",
            "completed",
            "[]",
        );
        // Both episodes carry no open todo, no blockers, and unresolved
        // aliveness (never fed by `fill_aliveness` in this synthetic test) —
        // now-hook must fail, so any candidate touching them is archived.
        let candidate = mk_candidate(
            "2020-01-01T00:00:00Z",
            "2020-06-01T00:00:00Z",
            Generator::Ledger,
            OidProvenance::GitDerived,
            "2020-06-01T00:00:00Z",
        );
        let episodes = pairs::load_project_episodes(&conn, "p").unwrap();
        let by_id: HashMap<&str, &EpisodeRow> = episodes
            .iter()
            .map(|e| (e.episode_id.as_str(), e))
            .collect();
        let vectors = HashMap::new();
        let (base, final_score, now_hook) = gate_score(
            &candidate,
            by_id["a"],
            by_id["b"],
            &vectors,
            crate::temporal::parse_timestamp("2021-01-01T00:00:00Z").unwrap(),
        );
        assert_eq!(now_hook, None);
        assert!(base >= 0.0); // score is still computed, just gated out
        assert!(final_score >= 0.0);
    }

    // -----------------------------------------------------------------
    // Recency penalty
    // -----------------------------------------------------------------

    #[test]
    fn recency_penalty_applies_only_inside_the_30_day_window() {
        let a = mk_row("failed", 1, None, Some(true));
        let b = mk_row("completed", 0, None, Some(true));
        let vectors = HashMap::new();
        let now = crate::temporal::parse_timestamp("2020-02-01T00:00:00Z").unwrap();

        let recent_event = mk_candidate(
            "2020-01-01T00:00:00Z",
            "2020-01-20T00:00:00Z",
            Generator::Ledger,
            OidProvenance::GitDerived,
            "2020-01-15T00:00:00Z", // 17 days before `now` — inside the window
        );
        let old_event = mk_candidate(
            "2020-01-01T00:00:00Z",
            "2020-01-20T00:00:00Z",
            Generator::Ledger,
            OidProvenance::GitDerived,
            "2019-01-01T00:00:00Z", // well outside the window
        );

        let (_, score_recent, _) = gate_score(&recent_event, &a, &b, &vectors, now);
        let (_, score_old, _) = gate_score(&old_event, &a, &b, &vectors, now);
        assert!(
            score_old - score_recent > RECENCY_PENALTY - 0.001,
            "an event inside the 30-day window must score ~0.40 lower than an identical one outside it"
        );
    }

    #[test]
    fn era_gap_term_is_halved_for_created_at_fallback_provenance() {
        let a = mk_row("completed", 0, None, None);
        let b = mk_row("completed", 0, None, None);
        let vectors = HashMap::new();
        let now = crate::temporal::parse_timestamp("2030-01-01T00:00:00Z").unwrap(); // far outside recency window

        let git_derived = mk_candidate(
            "2020-01-01T00:00:00Z",
            "2020-04-01T00:00:00Z", // 91 days apart -> era_gap saturates at 1.0
            Generator::Ledger,
            OidProvenance::GitDerived,
            "2020-04-01T00:00:00Z",
        );
        let fallback = mk_candidate(
            "2020-01-01T00:00:00Z",
            "2020-04-01T00:00:00Z",
            Generator::Ledger,
            OidProvenance::CreatedAtFallback,
            "2020-04-01T00:00:00Z",
        );

        let (_, score_git, now_hook_git) = gate_score(&git_derived, &a, &b, &vectors, now);
        let (_, score_fallback, now_hook_fallback) = gate_score(&fallback, &a, &b, &vectors, now);
        assert_eq!(now_hook_git, None);
        assert_eq!(now_hook_fallback, None);
        // Both otherwise-identical; the only difference is the era_gap term
        // being halved (0.20 * 1.0 vs 0.20 * 0.5 = a 0.10 difference).
        assert!((score_git - score_fallback - 0.10).abs() < 1e-9);
    }

    // -----------------------------------------------------------------
    // Margin rule
    // -----------------------------------------------------------------

    #[test]
    fn margin_shrink_never_cuts_through_a_near_tie() {
        // max_k=4 would cut between index 3 (0.65, rank 4) and index 4
        // (0.648, rank 5) — that boundary gap (0.002) is within
        // MARGIN_DELTA, so the cut must shrink to 3, where the boundary
        // (index 2 vs index 3: 0.70 vs 0.65, gap 0.05) is clean.
        let scores = vec![0.90, 0.80, 0.70, 0.65, 0.648, 0.40];
        let k = apply_margin_shrink(&scores, 4, MARGIN_DELTA);
        assert_eq!(k, 3);
    }

    #[test]
    fn margin_shrink_never_goes_below_one() {
        // Every adjacent boundary from rank 4 down to rank 1 is a near-tie
        // — the shrink must walk all the way down but stop at 1, never 0.
        let scores = vec![0.504, 0.503, 0.502, 0.501, 0.500];
        let k = apply_margin_shrink(&scores, 4, MARGIN_DELTA);
        assert_eq!(k, 1);
    }

    #[test]
    fn margin_shrink_is_a_no_op_when_the_boundary_gap_is_clean() {
        let scores = vec![0.9, 0.5, 0.1];
        let k = apply_margin_shrink(&scores, 2, MARGIN_DELTA);
        assert_eq!(k, 2);
    }

    // -----------------------------------------------------------------
    // Persistence idempotency
    // -----------------------------------------------------------------

    #[test]
    fn persist_relations_is_idempotent_against_the_unique_index() {
        let conn = open();
        insert_episode(
            &conn,
            "a",
            "sa",
            "p",
            "2020-01-01T00:00:00Z",
            "completed",
            "[]",
        );
        insert_episode(
            &conn,
            "b",
            "sb",
            "p",
            "2020-06-01T00:00:00Z",
            "completed",
            "[]",
        );
        let relation = GatedRelation {
            project: "p".to_string(),
            ep_a: "a".to_string(),
            ep_b: "b".to_string(),
            relation: Relation::ReplacedBy,
            generator: Generator::Ledger,
            topic_key: "symbol:foo".to_string(),
            receipt: PairReceipt {
                receipt_oid: Some("oid".to_string()),
                symbol: "foo".to_string(),
                hashes: vec![],
            },
            load_bearing_oid: Some("oid".to_string()),
            aux_oid: Some("head".to_string()),
            oid_provenance: OidProvenance::GitDerived,
            gate_score: 0.9,
            now_hook: Some("open_todo"),
            status: "queued",
        };
        let first = persist_relations(&conn, std::slice::from_ref(&relation)).unwrap();
        let second = persist_relations(&conn, std::slice::from_ref(&relation)).unwrap();
        assert_eq!(first, 1);
        assert_eq!(second, 0, "re-run must not duplicate or churn the row");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM dream_relations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    // -----------------------------------------------------------------
    // End-to-end gate_project smoke test (ledger generator, real DB)
    // -----------------------------------------------------------------

    #[test]
    fn gate_project_queues_an_eligible_high_scoring_ledger_pair() {
        let conn = open();
        insert_witness(
            &conn,
            &WitnessLedgerRow {
                id: 0,
                project: "p".to_string(),
                file: "a.rs".to_string(),
                symbol: Some("foo".to_string()),
                span_start: None,
                span_end: None,
                stamp: "b3:old".to_string(),
                tier: "committed".to_string(),
                at_oid: Some("oid1".to_string()),
                source_kind: "backfill".to_string(),
                source_id: None,
            },
        )
        .unwrap();
        let wid: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE file = 'a.rs' AND symbol = 'foo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: wid,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: None,
                observed_head_oid: "head1".to_string(),
            },
        )
        .unwrap();

        // Episode before the verdict, with an open todo (now-hook signal)
        // touching the retired symbol.
        conn.execute(
            "INSERT INTO reflections (id, content, tags, timestamp) VALUES ('ep-old', ?1, '[]', '2020-01-01T00:00:00Z')",
            params![format!(
                r#"{{"schema":"v2","session_id":"s1","project":"p","timestamp":"2020-01-01T00:00:00Z","request":"r","completed":"c","outcome":"partial","todos":[{{"content":"t","status":"pending"}}],"files_modified":[],"anchors":[{{"file":"a.rs","node_kind":"function","name":"foo","body_hash":"h1"}}]}}"#
            )],
        )
        .unwrap();
        // Episode after the verdict, touching the same file. The verdict
        // above has no `receipt_oid`, so its event time falls back to the
        // verdict's own `created_at` — the REAL wall-clock time this test
        // ran (`resolve_event_time`'s `CreatedAtFallback` path). This
        // episode's ts must be safely after that (hence 2099, not a fixed
        // near-term date), and the `now` passed to `gate_project` below
        // must be safely more than 30 days after it too, so the recency
        // penalty's outcome is deterministic regardless of when this test
        // actually runs.
        conn.execute(
            "INSERT INTO reflections (id, content, tags, timestamp) VALUES ('ep-new', ?1, '[]', '2099-01-01T00:00:00Z')",
            params![format!(
                r#"{{"schema":"v2","session_id":"s2","project":"p","timestamp":"2099-01-01T00:00:00Z","request":"r","completed":"c","outcome":"completed","todos":[],"files_modified":[],"anchors":[{{"file":"a.rs","node_kind":"function","name":"bar","body_hash":"h2"}}]}}"#
            )],
        )
        .unwrap();
        crate::storage::dream_backfill::materialize_episode_index(&conn).unwrap();

        let now = crate::temporal::parse_timestamp("2100-01-01T00:00:00Z").unwrap();
        let (relations, stats) = gate_project(&conn, "p", now).unwrap();
        assert_eq!(stats.generated_ledger, 1);
        assert_eq!(
            stats.queued, 1,
            "open-todo now-hook + above-floor score must queue"
        );
        let queued: Vec<&GatedRelation> =
            relations.iter().filter(|r| r.status == "queued").collect();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].ep_a, "ep-old");
        assert_eq!(queued[0].ep_b, "ep-new");
        assert_eq!(queued[0].now_hook, Some("open_todo"));

        let inserted = persist_relations(&conn, &relations).unwrap();
        assert_eq!(inserted, relations.len());
    }

    // -----------------------------------------------------------------
    // Family corpus (2026-08-26 design ruling): a funeral recorded under a
    // sibling project key must be visible to generation. This pins the
    // measured live defect — CSR's 244 funerals under
    // `claude-self-reflect-csr-engine` were invisible to episodes keyed
    // `claude-self-reflect`, and the run-3 stage line read "generated 0".
    // -----------------------------------------------------------------

    #[test]
    fn gate_family_sees_a_funeral_recorded_under_a_sibling_project_key() {
        // A-c (dream-backfill pass 1): family membership is now git repo
        // identity, not a name-prefix heuristic, so "acme" and
        // "acme-engine" only merge when a REAL file recorded under each key
        // resolves to the same git toplevel. Give both keys a real,
        // shared, temp git repo behind them.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let mut init = std::process::Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                init.env_remove(&k);
            }
        }
        init.arg("init").arg("-q").arg(&repo);
        if !init.status().map(|s| s.success()).unwrap_or(false) {
            return; // git unavailable in this environment -- fail-soft skip
        }
        let file = repo.join("a.rs");
        std::fs::write(&file, "fn foo() {}\n").unwrap();
        let file_str = file.to_string_lossy().to_string();

        let conn = open();

        // The funeral lives under the cwd-subdir key "acme-engine"...
        insert_witness(
            &conn,
            &WitnessLedgerRow {
                id: 0,
                project: "acme-engine".to_string(),
                file: file_str.clone(),
                symbol: Some("foo".to_string()),
                span_start: None,
                span_end: None,
                stamp: "b3:old".to_string(),
                tier: "committed".to_string(),
                at_oid: Some("oid1".to_string()),
                source_kind: "backfill".to_string(),
                source_id: None,
            },
        )
        .unwrap();
        let wid: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE file = ?1 AND symbol = 'foo'",
                params![file_str],
                |r| r.get(0),
            )
            .unwrap();
        insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: wid,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: None,
                observed_head_oid: "head1".to_string(),
            },
        )
        .unwrap();

        // ...while both episodes are keyed to the repo root "acme". Same
        // temporal construction as the single-key ledger test above: no
        // receipt_oid, so the event time is the verdict's real created_at
        // and 2099 is safely after it.
        conn.execute(
            "INSERT INTO reflections (id, content, tags, timestamp) VALUES ('ep-old', ?1, '[]', '2020-01-01T00:00:00Z')",
            params![
                format!(
                    r#"{{"schema":"v2","session_id":"s1","project":"acme","timestamp":"2020-01-01T00:00:00Z","request":"r","completed":"c","outcome":"partial","todos":[{{"content":"t","status":"pending"}}],"files_modified":[],"anchors":[{{"file":"{file_str}","node_kind":"function","name":"foo","body_hash":"h1"}}]}}"#
                )
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO reflections (id, content, tags, timestamp) VALUES ('ep-new', ?1, '[]', '2099-01-01T00:00:00Z')",
            params![
                format!(
                    r#"{{"schema":"v2","session_id":"s2","project":"acme","timestamp":"2099-01-01T00:00:00Z","request":"r","completed":"c","outcome":"completed","todos":[],"files_modified":[],"anchors":[{{"file":"{file_str}","node_kind":"function","name":"bar","body_hash":"h2"}}]}}"#
                )
            ],
        )
        .unwrap();
        crate::storage::dream_backfill::materialize_episode_index(&conn).unwrap();

        let now = crate::temporal::parse_timestamp("2100-01-01T00:00:00Z").unwrap();

        // Single-key gating (the pre-family behavior): the funeral is
        // invisible from "acme", so nothing generates.
        let (_, single_stats) = gate_project(&conn, "acme", now).unwrap();
        assert_eq!(
            single_stats.generated, 0,
            "exact-key join must miss the sibling-key funeral (the defect this test pins)"
        );

        // Family gating: "acme-engine" groups into the "acme" family and
        // the funeral becomes generation evidence.
        let families = super::super::family::compute_families(&conn).unwrap();
        let family = super::super::family::family_containing(&families, "acme")
            .expect("acme family exists")
            .clone();
        assert_eq!(family.members, vec!["acme", "acme-engine"]);
        let (relations, stats) = gate_family(&conn, &family, now).unwrap();
        assert_eq!(stats.generated_ledger, 1);
        assert_eq!(stats.queued, 1);
        let queued: Vec<&GatedRelation> =
            relations.iter().filter(|r| r.status == "queued").collect();
        assert_eq!(queued.len(), 1);
        // F6 fix (Codex review pass 1 / main-thread finding): the family
        // is now DISPLAY-named after the repo's own toplevel basename
        // (here, the temp git repo directory literally named "repo") when
        // one resolved, not the shortest raw project key ("acme") — this
        // assertion cares that the queued relation's project matches
        // WHATEVER the family actually resolved to, not a specific string.
        assert_eq!(queued[0].project, family.name);
        assert_eq!(queued[0].ep_a, "ep-old");
        assert_eq!(queued[0].ep_b, "ep-new");
    }

    // -----------------------------------------------------------------
    // P4 (F4): the absolute floor certifies pre-penalty evidence quality,
    // never the post-penalty (ranked) score.
    // -----------------------------------------------------------------

    #[test]
    fn floor_check_uses_the_pre_penalty_base_score_not_the_final_ranked_score() {
        let a = mk_row("completed", 0, None, Some(true));
        let b = mk_row("completed", 0, None, Some(true));
        let vectors = HashMap::new();
        // Event well inside the 30-day recency window, so the -0.40 penalty
        // bites; era_gap saturated (91 days apart, git-derived so no 0.5
        // halving) contributes the full 0.20 on top of generator (0.15) and
        // now-hook (0.25) -- base = 0.20+0.15+0.25 = 0.60, comfortably above
        // TAU_ABS_FLOOR (0.35), but final = 0.60-0.40 = 0.20, BELOW the
        // floor. If the floor were (wrongly) checked against final, this
        // candidate would be erased entirely instead of merely demoted.
        let now = crate::temporal::parse_timestamp("2020-04-15T00:00:00Z").unwrap();
        let candidate = mk_candidate(
            "2020-01-01T00:00:00Z",
            "2020-04-01T00:00:00Z",
            Generator::Ledger,
            OidProvenance::GitDerived,
            "2020-04-01T00:00:00Z", // 14 days before `now` -- inside the window
        );
        let (base, final_score, now_hook) = gate_score(&candidate, &a, &b, &vectors, now);
        assert!(now_hook.is_some());
        assert!(base >= TAU_ABS_FLOOR, "base={base} must clear the floor");
        assert!(
            final_score < TAU_ABS_FLOOR,
            "final={final_score} must sit below the floor -- exactly the case P4 must not erase"
        );
    }

    // -----------------------------------------------------------------
    // P3 (F3): generator-priority tie-break + cross-generator dedup.
    // -----------------------------------------------------------------

    fn mk_eligible(
        ep_a: &str,
        ep_b: &str,
        generator: Generator,
        score: f64,
    ) -> (PairCandidate, f64, &'static str) {
        (
            PairCandidate {
                project: "p".to_string(),
                ep_a: ep_a.to_string(),
                ep_b: ep_b.to_string(),
                ts_a: crate::temporal::parse_timestamp("2020-01-01T00:00:00Z").unwrap(),
                ts_b: crate::temporal::parse_timestamp("2020-02-01T00:00:00Z").unwrap(),
                relation: Relation::ReplacedBy,
                generator,
                topic_key: "symbol:foo".to_string(),
                receipt: PairReceipt {
                    receipt_oid: None,
                    symbol: "foo".to_string(),
                    hashes: vec![],
                },
                aux_oid: None,
                oid_provenance: OidProvenance::CreatedAtFallback,
                event_time: crate::temporal::parse_timestamp("2020-02-01T00:00:00Z").unwrap(),
            },
            score,
            "live_file",
        )
    }

    #[test]
    fn tie_break_prefers_relapse_over_ledger_over_era_at_equal_score() {
        let eligible = vec![
            mk_eligible("a", "b", Generator::Ledger, 0.50),
            mk_eligible("c", "d", Generator::Era, 0.50),
            mk_eligible("e", "f", Generator::Relapse, 0.50),
        ];
        let ranked = rank_and_dedup_eligible(eligible);
        assert_eq!(ranked.len(), 3);
        assert_eq!(ranked[0].0.generator, Generator::Relapse);
        assert_eq!(ranked[1].0.generator, Generator::Ledger);
        assert_eq!(ranked[2].0.generator, Generator::Era);
    }

    #[test]
    fn cross_generator_dedup_keeps_only_the_best_row_for_the_same_pair() {
        // Same (project, ep_a, ep_b) story, proposed by two generators with
        // different scores -- only the higher-scoring (and, at an exact
        // tie, higher-priority-generator) row may survive.
        let eligible = vec![
            mk_eligible("a", "b", Generator::Ledger, 0.60),
            mk_eligible("a", "b", Generator::Relapse, 0.90),
            mk_eligible("x", "y", Generator::Era, 0.10),
        ];
        let ranked = rank_and_dedup_eligible(eligible);
        assert_eq!(
            ranked.len(),
            2,
            "the duplicate (a,b) story collapses to one row"
        );
        assert_eq!(ranked[0].0.ep_a, "a");
        assert_eq!(ranked[0].0.generator, Generator::Relapse);
        assert_eq!(ranked[0].1, 0.90);
        assert_eq!(ranked[1].0.ep_a, "x");
    }

    // -----------------------------------------------------------------
    // End-to-end gate_project smoke test (relapse generator, real DB)
    // -----------------------------------------------------------------

    #[test]
    fn gate_project_queues_an_eligible_high_scoring_relapse_pair() {
        // A-b (dream-backfill pass 1): the relapse generator's event time
        // is now a git-verified death (`death_time::resolve_relapse_death_time`)
        // -- a `CreatedAtFallback` death is unorderable and can no longer
        // satisfy the before/after gate, so this fixture needs a real repo
        // behind the witness.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let mut init = std::process::Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                init.env_remove(&k);
            }
        }
        init.arg("init").arg("-q").arg(&repo);
        if !init.status().map(|s| s.success()).unwrap_or(false) {
            return; // git unavailable in this environment -- fail-soft skip
        }
        let run = |args: &[&str]| -> bool {
            let mut cmd = std::process::Command::new("git");
            for (k, _) in std::env::vars_os() {
                if k.to_string_lossy().starts_with("GIT_") {
                    cmd.env_remove(&k);
                }
            }
            cmd.arg("-C").arg(&repo).args(args);
            cmd.status().map(|s| s.success()).unwrap_or(false)
        };
        let head_oid = || -> String {
            let mut cmd = std::process::Command::new("git");
            for (k, _) in std::env::vars_os() {
                if k.to_string_lossy().starts_with("GIT_") {
                    cmd.env_remove(&k);
                }
            }
            cmd.arg("-C").arg(&repo).arg("rev-parse").arg("HEAD");
            String::from_utf8(cmd.output().unwrap().stdout)
                .unwrap()
                .trim()
                .to_string()
        };
        let file = repo.join("a.rs");
        std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
        assert!(run(&["add", "-A"]));
        assert!(run(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "d0"
        ]));
        let d0 = head_oid();
        let stamp = codewitness::StampKind::Raw
            .compute(&std::fs::read(&file).unwrap())
            .as_str()
            .to_string();
        // The death commit: foo's body actually changes here.
        std::fs::write(&file, "fn foo() {\n    2\n}\n").unwrap();
        assert!(run(&["add", "-A"]));
        assert!(run(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "d1"
        ]));
        let d1 = head_oid();
        let file_str = file.to_string_lossy().to_string();

        let conn = open();
        insert_witness(
            &conn,
            &WitnessLedgerRow {
                id: 0,
                project: "p".to_string(),
                file: file_str.clone(),
                symbol: Some("foo".to_string()),
                span_start: None,
                span_end: None,
                stamp,
                tier: "committed".to_string(),
                at_oid: Some(d0),
                source_kind: "backfill".to_string(),
                source_id: None,
            },
        )
        .unwrap();
        let wid: i64 = conn
            .query_row(
                "SELECT id FROM witness_ledger WHERE file = ?1 AND symbol = 'foo'",
                params![file_str],
                |r| r.get(0),
            )
            .unwrap();
        insert_verdict_if_changed(
            &conn,
            &WitnessVerdictRow {
                witness_id: wid,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: None,
                observed_head_oid: d1,
            },
        )
        .unwrap();

        // Episode before the verdict, touching the retired symbol.
        conn.execute(
            "INSERT INTO reflections (id, content, tags, timestamp) VALUES ('ep-before', ?1, '[]', '2020-01-01T00:00:00Z')",
            params![format!(
                r#"{{"schema":"v2","session_id":"s1","project":"p","timestamp":"2020-01-01T00:00:00Z","request":"r","completed":"c","outcome":"completed","todos":[],"files_modified":[],"anchors":[{{"file":"{file_str}","node_kind":"function","name":"foo","body_hash":"h1"}}]}}"#
            )],
        )
        .unwrap();
        // The relapse episode, well after the git-verified death, with an
        // open todo (now-hook signal) re-touching the retired symbol.
        conn.execute(
            "INSERT INTO reflections (id, content, tags, timestamp) VALUES ('ep-relapse', ?1, '[]', '2099-01-01T00:00:00Z')",
            params![format!(
                r#"{{"schema":"v2","session_id":"s2","project":"p","timestamp":"2099-01-01T00:00:00Z","request":"r","completed":"c","outcome":"partial","todos":[{{"content":"t","status":"pending"}}],"files_modified":[],"anchors":[{{"file":"{file_str}","node_kind":"function","name":"foo","body_hash":"h2"}}]}}"#
            )],
        )
        .unwrap();
        crate::storage::dream_backfill::materialize_episode_index(&conn).unwrap();

        let now = crate::temporal::parse_timestamp("2100-01-01T00:00:00Z").unwrap();
        let (relations, stats) = gate_project(&conn, "p", now).unwrap();
        assert_eq!(stats.generated_relapse, 1);
        assert_eq!(
            stats.queued, 1,
            "open-todo now-hook + above-floor score must queue"
        );
        let queued: Vec<&GatedRelation> =
            relations.iter().filter(|r| r.status == "queued").collect();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].ep_a, "ep-before");
        assert_eq!(queued[0].ep_b, "ep-relapse");
        assert_eq!(queued[0].generator, Generator::Relapse);
        assert_eq!(queued[0].now_hook, Some("open_todo"));
    }
}
