//! `csr-engine dreams` — headless dream generation, stdout only.
//!
//! Slice one of `.plans/dreams-product-shape.md`. NO journal HTML/routes/
//! templates are touched here, no browser surface — this module only reads
//! already-gated evidence and prints/persists.
//!
//! Zero-or-one dream per project. Abstention is correct: a project with
//! insufficient evidence prints nothing in plain-text mode, or appears with
//! an explicit "no dream" entry only in `--json` mode.
//!
//! Two categories compete for a project's slot:
//!
//! 1. **unfinished-and-valuable** (this file, fully implemented): zero LLM,
//!    deterministic. Feed = [`crate::journal::week::load_week_dreams`], which
//!    already ranks and caps (one card per origin session, `MAX_WEEK_DREAMS`
//!    total) — this module reuses that ranking verbatim rather than
//!    re-deriving it, so a project's unfinished slot can only ever be filled
//!    by an item that already cleared the home page's own bar.
//! 2. **strategy**: LLM-authored via `claude -p`, one call per project --
//!    fully implemented in [`crate::dream::strategy`]. Gated behind a
//!    deterministic evidence bar (>= 3 distinct origin sessions this week)
//!    before any spend is considered, and cached by content hash so a
//!    re-run of an unchanged corpus costs nothing. `--no-llm` (or either
//!    AI kill switch) skips this category entirely -- see
//!    [`crate::dream::strategy::category_disabled`].
//!
//! # Category competition
//!
//! A project gets at most one dream. When both categories produce a
//! candidate for the same project, strategy wins the slot whenever it
//! produced ANY dream -- a cache hit counts as "passed authoring" exactly
//! like a fresh call, since both mean the corpus currently supports a
//! strategy judgment. Only when strategy has nothing for a project (gate
//! failed, kill-switched, or the model abstained/malformed) does the
//! unfinished card fill that project's slot. Decided in [`handle`], where
//! `strategy_by_project` is checked before `unfinished_by_project`.
//!
//! # The global cross-project slot
//!
//! Deliberately built from the RAW open-item pool
//! ([`crate::storage::dream_clusters::load_open_items`]), not from the
//! already-capped `load_week_dreams` output — `WeekDream` carries no
//! session/date field to compare receipts by, and the global gate needs to
//! see every open item this week, not just the ≤3 that made the home page.
//! See [`global_headline`].
//!
//! # Dream id vs. attribution marker
//!
//! The design doc's example hash is blake3; this module uses `sha2::Sha256`
//! instead (already a project dependency — see
//! `storage::dream_clusters::item_id` for the identical pattern) rather than
//! pulling in a new hashing crate for an opaque CLI-only id. The attribution
//! marker itself is NOT reinvented: every card ends with
//! [`crate::storage::dream_attribution::marker_line`], the exact function
//! Journal v4 P4b copy blocks use, so a pasted dreams-CLI card is bound back
//! to its `dreams_v1` row the same way a journal copy block is bound back to
//! its dream.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::journal::week::{self, WeekDream};
use crate::storage::dream_attribution;
use crate::storage::dream_clusters::{self, OpenItem};
use crate::storage::Storage;

/// Hash version folded into every deterministic revision hash. Bumping this
/// forces every cached row to be treated as stale on the next run — the
/// unfinished category doesn't consult the cache today, but the strategy
/// category (stage B) will, and both share this constant so a rendering
/// change invalidates both at once.
const REVISION_HASH_VERSION: &str = "dreams-cli-v1";

// ─── entry point ────────────────────────────────────────────────────────

/// One project's chosen dream, whichever category won its slot — the shape
/// [`emit_text`]/[`emit_json`] render, already fully decided (rendered,
/// recorded) by the time [`handle`] builds it.
struct FinalCard {
    project: String,
    category: &'static str,
    dream_id: String,
    subject_key: Option<String>,
    text: String,
}

/// Run `csr-engine dreams`. Storage-only — no `Engine::new()`, no
/// embeddings, no HNSW index, following the `status` subcommand's pattern.
/// Thin: open storage, delegate to [`build_run`] for every decision, print.
/// The split exists so `build_run` — category competition included — is
/// unit-testable against an in-memory `Storage` without going through stdout.
pub fn handle(
    db_path: &Path,
    project_filter: Option<&str>,
    json: bool,
    no_llm: bool,
) -> Result<()> {
    let storage = Storage::open(db_path)?;
    let now = Utc::now();
    let (verdict, cards) = build_run(&storage, project_filter, no_llm, now)?;
    if json {
        emit_json(&verdict, &cards, project_filter)
    } else {
        emit_text(&verdict, &cards)
    }
}

/// Everything `csr-engine dreams` decides, given an already-open `storage`:
/// the global cross-project verdict and every project's chosen card, category
/// competition already resolved. No printing — [`handle`] does that.
fn build_run(
    storage: &Storage,
    project_filter: Option<&str>,
    no_llm: bool,
    now: DateTime<Utc>,
) -> Result<(GlobalVerdict, Vec<FinalCard>)> {
    let (week_dreams, open_items, dream_threads) = storage.with_connection(|conn| {
        let wd = week::load_week_dreams(conn, now)?;
        let oi = dream_clusters::load_open_items(conn)?;
        let dt = crate::dream::threads::load_dream_threads(conn)?;
        Ok((wd, oi, dt))
    })?;

    let global_candidates = build_global_candidates(&open_items, now);
    let verdict = global_headline(&global_candidates);

    // unfinished-and-valuable candidates, one per project: first occurrence
    // in the already-ranked feed wins ("zero-or-one dream per project"
    // applied to an input that is itself already ranked highest-first).
    let mut seen_projects: HashSet<String> = HashSet::new();
    let mut unfinished_by_project: BTreeMap<String, &WeekDream> = BTreeMap::new();
    for wd in &week_dreams {
        if let Some(p) = project_filter {
            if wd.project != p {
                continue;
            }
        }
        if seen_projects.insert(wd.project.clone()) {
            unfinished_by_project.insert(wd.project.clone(), wd);
        }
    }

    // strategy candidates: gathered from every project with WEEK EVIDENCE at
    // all (raw open items + raw threads, same "don't limit to the capped
    // home feed" reasoning `build_global_candidates` already uses) rather
    // than only the ≤3 projects that made the unfinished home page — a
    // project can win a strategy slot even when it never had a shot at an
    // unfinished one.
    let mut strategy_by_project: BTreeMap<String, crate::dream::strategy::RenderedStrategy> =
        BTreeMap::new();
    if !crate::dream::strategy::category_disabled(no_llm) {
        let mut candidate_projects: BTreeSet<String> = BTreeSet::new();
        for item in &open_items {
            if item.completed.is_none() && week::within_week(&item.origin_ts, now) {
                candidate_projects.insert(item.project.clone());
            }
        }
        for t in &dream_threads {
            if !t.thread.is_empty() && week::within_week(&t.created_at, now) {
                candidate_projects.insert(t.project.clone());
            }
        }
        if let Some(p) = project_filter {
            candidate_projects.retain(|proj| proj == p);
        }

        let budget = crate::dream::policy::Budget::for_night(
            storage,
            crate::dream::policy::effort_tier_counted(storage),
            &crate::dream::policy::current_night_key(),
        );
        let process_actor = crate::dream::threads::ProcessActor;
        let actor = crate::dream::policy::BudgetedActor::new(&process_actor, &budget);

        for project in &candidate_projects {
            let evidence = storage.with_connection(|conn| {
                crate::dream::strategy::build_project_evidence(
                    conn,
                    project,
                    &open_items,
                    &dream_threads,
                    now,
                )
            })?;
            if let Some(rendered) =
                crate::dream::strategy::author_for_project(storage, &actor, &budget, &evidence, now)
            {
                strategy_by_project.insert(project.clone(), rendered);
            }
        }
    }

    // Category competition (see the module doc): strategy wins a project's
    // slot whenever it produced ANY dream — presence in `strategy_by_project`
    // already means "passed authoring" (cache hit or fresh call both count,
    // `author_for_project` only ever inserts a `Some` for one of those two
    // outcomes). Only when strategy has nothing for a project does the
    // unfinished card fill that slot.
    let mut project_order: BTreeSet<String> = unfinished_by_project.keys().cloned().collect();
    project_order.extend(strategy_by_project.keys().cloned());

    let mut cards: Vec<FinalCard> = Vec::new();
    for project in &project_order {
        if let Some(rendered) = strategy_by_project.remove(project) {
            cards.push(FinalCard {
                project: project.clone(),
                category: "strategy",
                dream_id: rendered.dream_id,
                subject_key: None,
                text: rendered.text,
            });
        } else if let Some(wd) = unfinished_by_project.get(project) {
            let revision_hash = unfinished_revision_hash(wd);
            // Idempotency: an unchanged subject+revision reuses its row's
            // dream_id rather than minting a fresh one every run (see the
            // module doc and `lookup_open_dream_id`'s own doc) — only a
            // genuinely new revision (or a first sighting) gets a new id and
            // a new row.
            let existing = lookup_open_dream_id(
                storage,
                &wd.project,
                "unfinished",
                &wd.item_id,
                &revision_hash,
            );
            let is_new_revision = existing.is_none();
            let dream_id = existing
                .unwrap_or_else(|| compute_dream_id(&wd.project, "unfinished", &wd.item_id, now));
            let rendered = render_unfinished_card(wd, &dream_id);
            if is_new_revision {
                // Fail-soft, matching the strategy path: the card is fully
                // rendered already, so a failed INSERT costs a dream_id for
                // future verdict tooling, never the card itself or the rest
                // of this run's output.
                if let Err(error) = record_dream_row(
                    storage,
                    &dream_id,
                    &wd.project,
                    "unfinished",
                    Some(&wd.item_id),
                    &revision_hash,
                    &rendered,
                ) {
                    tracing::warn!(%error, project = %wd.project, item_id = %wd.item_id, "failed to persist unfinished dream row");
                }
            }
            cards.push(FinalCard {
                project: project.clone(),
                category: "unfinished",
                dream_id,
                subject_key: Some(wd.item_id.clone()),
                text: rendered,
            });
        }
    }

    Ok((verdict, cards))
}

fn emit_text(verdict: &GlobalVerdict, cards: &[FinalCard]) -> Result<()> {
    println!("{}", render_global_line(verdict));
    println!();
    for card in cards {
        println!("{}", card.text);
    }
    Ok(())
}

fn emit_json(
    verdict: &GlobalVerdict,
    cards: &[FinalCard],
    project_filter: Option<&str>,
) -> Result<()> {
    let global = GlobalJson::from(verdict);
    let mut projects: Vec<ProjectJson> = cards
        .iter()
        .map(|card| ProjectJson {
            project: card.project.clone(),
            category: Some(card.category.to_string()),
            dream_id: Some(card.dream_id.clone()),
            subject_key: card.subject_key.clone(),
            text: Some(card.text.clone()),
        })
        .collect();
    // A filtered project with no qualifying evidence gets an explicit
    // "no dream" entry — the one place abstention is visible in JSON mode.
    if let Some(p) = project_filter {
        if !projects.iter().any(|pj| pj.project == p) {
            projects.push(ProjectJson {
                project: p.to_string(),
                category: None,
                dream_id: None,
                subject_key: None,
                text: None,
            });
        }
    }
    let out = DreamsJson { global, projects };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

// ─── unfinished-and-valuable card rendering ────────────────────────────

/// `true` iff the item's stored plan actually contributed how-lines —
/// mirrors [`WeekDream::kind_label`]'s own two-value vocabulary rather than
/// re-deriving it from `how.is_empty()` (a hypothesis-only item also has an
/// empty `how`, and that must NOT read as "has a plan").
fn has_plan(dream: &WeekDream) -> bool {
    dream.kind_label == "natural direction"
}

/// Observed bullets. Every line carries a receipt: the item id when nothing
/// more specific is available, the how-line's own commit oid when it has
/// one, and — for the hypothesis line specifically — the session that
/// actually produced it (`hypothesis_session`), never the unrelated item id.
/// `select_week_dreams` guarantees at least a title plus (how OR
/// hypothesis), so this is never empty.
fn observed_lines(dream: &WeekDream) -> Vec<String> {
    let mut lines = vec![format!(
        "open since ⌗{} — \"{}\"",
        dream.item_id,
        dream.title.trim()
    )];
    if let Some(hypothesis) = &dream.hypothesis {
        // `hypothesis_session` is set whenever `hypothesis` is (see
        // `WeekDream`'s doc) — the item id fallback only guards a future
        // change to that invariant from panicking here.
        let session_receipt = dream
            .hypothesis_session
            .as_deref()
            .map(crate::dream::report::short_oid)
            .unwrap_or_else(|| dream.item_id.clone());
        lines.push(format!("night pass: \"{hypothesis}\" ⌗{session_receipt}"));
    }
    for how in &dream.how {
        if how.contains('⌗') {
            lines.push(how.clone());
        } else {
            // `compose_how_line` omits the oid segment when the step's
            // citation was blank — fall back to the item id so this line
            // still carries a receipt.
            lines.push(format!("{how} ⌗{}", dream.item_id));
        }
    }
    lines
}

/// The "Dream's take" paragraph. Labeled exactly like an LLM-authored
/// judgment would be, but composed by plain `format!` from `how`/
/// `hypothesis` only — no model call in this category.
fn dreams_take(dream: &WeekDream) -> String {
    match (&dream.hypothesis, dream.how.is_empty()) {
        (Some(hypothesis), false) => format!(
            "the night pass read this as \"{hypothesis}\", and a stored plan already lays out {} step(s) toward it — resuming beats re-diagnosing from scratch.",
            dream.how.len()
        ),
        (Some(hypothesis), true) => format!(
            "the night pass read this as \"{hypothesis}\", but no stored plan exists yet — the next move is diagnosis, not execution."
        ),
        (None, false) => format!(
            "no night-pass thread matched this item, but a stored plan with {} step(s) is already on record — likely still the right next move if nothing has shifted underneath it.",
            dream.how.len()
        ),
        (None, true) => {
            // Unreachable in practice: `select_week_dreams` drops any
            // candidate with neither a how-line nor a hypothesis. Handled
            // rather than panicking so a future change to that gate degrades
            // this line instead of crashing the CLI.
            "left open with neither a stored plan nor a matched night-pass thread — treat as a bare log entry.".to_string()
        }
    }
}

/// "Verify first" homework, only when applicable: an item with no stored
/// plan needs confirmation it's still actually open before anyone proposes
/// next steps on it.
fn verify_first_line(dream: &WeekDream) -> Option<String> {
    if has_plan(dream) {
        None
    } else {
        Some(
            "Verify first: no stored plan is on record — confirm this item is still open and \
             the context hasn't shifted before acting on it."
                .to_string(),
        )
    }
}

/// One verification-first directive, WITHOUT its header — the caller
/// ([`render_unfinished_card`]) prepends [`dream_attribution::DREAM_CARD_PROPOSAL_HEADER`].
/// Never a free-form execution command — it always routes through confirming
/// state, then names the stored next step (or asks for one when none
/// exists), then names the tool that records the verdict.
fn proposal_line(dream: &WeekDream) -> String {
    let next = dream
        .how
        .first()
        .map(|h| format!("proceed with: {h}"))
        .unwrap_or_else(|| "decide the next concrete step".to_string());
    format!(
        "verify \"{}\" (project `{}`, item ⌗{}) is still open, \
         then {next} — record the outcome via csr_resolve.",
        dream.title.trim(),
        dream.project,
        dream.item_id
    )
}

/// Sorted-receipt-ids-plus-prompt-version hash, per the design doc's
/// `revision_hash` contract. The unfinished category never consults this for
/// reuse (it recomputes every run — see the module doc), but still records
/// it so the schema is uniform with the strategy category stage B adds.
fn unfinished_revision_hash(dream: &WeekDream) -> String {
    let mut ids: Vec<String> = vec![dream.item_id.clone()];
    ids.extend(dream.how.iter().cloned());
    if let Some(hypothesis) = &dream.hypothesis {
        ids.push(hypothesis.clone());
    }
    ids.sort();
    let mut hasher = Sha256::new();
    hasher.update(REVISION_HASH_VERSION.as_bytes());
    for id in &ids {
        hasher.update(b"\0");
        hasher.update(id.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Opaque dream id: `sha256(project|category|subject_key|created_at)`,
/// truncated to 8 bytes (16 lowercase hex chars) — the same truncation
/// convention `dream_clusters::item_id` uses. `now` is folded in at
/// RFC 3339 (sub-second) precision specifically so two runs of the same item
/// seconds apart don't collide; a caller that wants a stable id across runs
/// should not expect one — identity here is "this run's row", not "this
/// item", by design (see the module doc's cache-reuse contract).
pub(crate) fn compute_dream_id(
    project: &str,
    category: &str,
    subject_key: &str,
    now: DateTime<Utc>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(project.as_bytes());
    hasher.update(b"|");
    hasher.update(category.as_bytes());
    hasher.update(b"|");
    hasher.update(subject_key.as_bytes());
    hasher.update(b"|");
    hasher.update(now.to_rfc3339().as_bytes());
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Render one unfinished-and-valuable card for `dream_id` — an id the caller
/// already resolved (reused from an existing open row at an unchanged
/// revision, or freshly minted for a new one; see [`lookup_open_dream_id`]
/// and the module doc's idempotency contract). This function is pure
/// rendering: it never computes or persists an id itself.
fn render_unfinished_card(dream: &WeekDream, dream_id: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "PROJECT {} — unfinished-and-valuable\n",
        dream.project
    ));
    out.push_str("Observed:\n");
    for line in observed_lines(dream) {
        out.push_str("  - ");
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str(dream_attribution::DREAM_CARD_TAKE_HEADER);
    out.push(' ');
    out.push_str(&dreams_take(dream));
    out.push('\n');
    if let Some(verify) = verify_first_line(dream) {
        out.push_str(&verify);
        out.push('\n');
    }
    out.push_str(dream_attribution::DREAM_CARD_PROPOSAL_HEADER);
    out.push(' ');
    out.push_str(&proposal_line(dream));
    out.push('\n');
    out.push_str(&dream_attribution::marker_line(dream_id));
    out.push('\n');
    out
}

/// Existing OPEN `dreams_v1` row's dream_id at
/// `(project, category, subject_key, revision_hash)`, if one exists —
/// idempotency for a category (like `unfinished`) whose logical identity
/// includes a `subject_key`, mirroring [`crate::dream::strategy`]'s
/// `lookup_cached` cache-hit contract but keyed additionally by that subject
/// and used only to reuse the id, never to reuse rendered prose (an
/// unfinished card is deterministic and always recomputed — see the module
/// doc). `None` on any error too (fail-open to "mint a fresh id" — a lookup
/// failure must never be the reason a project loses its dream this run).
fn lookup_open_dream_id(
    storage: &Storage,
    project: &str,
    category: &str,
    subject_key: &str,
    revision_hash: &str,
) -> Option<String> {
    storage
        .with_connection(|conn| {
            conn.query_row(
                "SELECT dream_id FROM dreams_v1 \
                 WHERE project = ?1 AND category = ?2 AND subject_key = ?3 \
                 AND revision_hash = ?4 AND status = 'open' \
                 ORDER BY id DESC LIMIT 1",
                params![project, category, subject_key, revision_hash],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(anyhow::Error::from)
        })
        .ok()
        .flatten()
}

pub(crate) fn record_dream_row(
    storage: &Storage,
    dream_id: &str,
    project: &str,
    category: &str,
    subject_key: Option<&str>,
    revision_hash: &str,
    prose: &str,
) -> Result<()> {
    storage.with_connection(|conn| {
        conn.execute(
            "INSERT INTO dreams_v1 (dream_id, project, category, subject_key, revision_hash, prose) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![dream_id, project, category, subject_key, revision_hash, prose],
        )?;
        Ok(())
    })
}

// ─── global cross-project slot ─────────────────────────────────────────

/// One independent piece of evidence behind a candidate cross-project theme:
/// which session reported it, on what day, and whether that report carried
/// stakes. `stakes` comes only from [`OpenItem::kind`] (`"blocker"`) — a
/// structured field, never LLM output, per the design's hard rule.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CandidateReceipt {
    session_id: String,
    day: String,
    stakes: bool,
}

/// A candidate cross-project theme: the same open-item text, seen
/// independently. Keyed by normalized (trimmed, lowercased) title text —
/// the only signal available without a semantic-similarity model.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GlobalCandidate {
    subject: String,
    projects: Vec<String>,
    receipts: Vec<CandidateReceipt>,
}

impl GlobalCandidate {
    fn distinct_receipt_count(&self) -> usize {
        self.receipts
            .iter()
            .map(|r| (r.session_id.clone(), r.day.clone()))
            .collect::<BTreeSet<_>>()
            .len()
    }

    fn has_stakes(&self) -> bool {
        self.receipts.iter().any(|r| r.stakes)
    }
}

/// The global slot's discrete gate result. No confidence floats: a candidate
/// either clears both bars and wins uniquely, or the slot abstains.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GlobalVerdict {
    Headline {
        subject: String,
        projects: Vec<String>,
        receipt_count: usize,
    },
    NoDefensibleVerdict,
}

/// Build one candidate per distinct open-item title text across the FULL raw
/// open-item pool (not the capped `load_week_dreams` output — see the module
/// doc for why). Completed items and items outside the rolling week are
/// excluded, same gate `load_week_dreams` applies to its own candidates.
fn build_global_candidates(open_items: &[OpenItem], now: DateTime<Utc>) -> Vec<GlobalCandidate> {
    let mut by_subject: BTreeMap<String, GlobalCandidate> = BTreeMap::new();
    for item in open_items {
        if item.completed.is_some() {
            continue;
        }
        if !week::within_week(&item.origin_ts, now) {
            continue;
        }
        let key = item.item.trim().to_lowercase();
        if key.is_empty() {
            continue;
        }
        let entry = by_subject.entry(key).or_insert_with(|| GlobalCandidate {
            subject: item.item.trim().to_string(),
            projects: Vec::new(),
            receipts: Vec::new(),
        });
        if !entry.projects.contains(&item.project) {
            entry.projects.push(item.project.clone());
        }
        entry.receipts.push(CandidateReceipt {
            session_id: item.origin_session.clone(),
            day: item.origin_date.clone(),
            stakes: item.kind == "blocker",
        });
    }
    by_subject.into_values().collect()
}

/// Discrete gate: a candidate qualifies only at ≥2 independent
/// session/day receipts, ≥1 stakes receipt, AND ≥2 distinct projects — this
/// is the CROSS-project slot, and [`render_global_line`] renders "across
/// {projects}"; a theme reported twice within a single project is real
/// corroboration but not cross-project scope, and must not publish under a
/// claim it doesn't back. Among qualifiers, a UNIQUE highest-receipt-count
/// winner publishes; a tie at the top (or zero qualifiers) abstains — "no
/// defensible verdict" is the correct answer far more often than a headline
/// is, by construction.
fn global_headline(candidates: &[GlobalCandidate]) -> GlobalVerdict {
    let mut qualifying: Vec<(&GlobalCandidate, usize)> = candidates
        .iter()
        .filter_map(|c| {
            let n = c.distinct_receipt_count();
            (n >= 2 && c.has_stakes() && c.projects.len() >= 2).then_some((c, n))
        })
        .collect();
    qualifying.sort_by_key(|(_, n)| std::cmp::Reverse(*n));

    match qualifying.as_slice() {
        [] => GlobalVerdict::NoDefensibleVerdict,
        [(only, n)] => GlobalVerdict::Headline {
            subject: only.subject.clone(),
            projects: only.projects.clone(),
            receipt_count: *n,
        },
        [(first, n1), (_, n2), ..] if n1 > n2 => GlobalVerdict::Headline {
            subject: first.subject.clone(),
            projects: first.projects.clone(),
            receipt_count: *n1,
        },
        _ => GlobalVerdict::NoDefensibleVerdict,
    }
}

fn render_global_line(verdict: &GlobalVerdict) -> String {
    match verdict {
        GlobalVerdict::Headline {
            subject,
            projects,
            receipt_count,
        } => format!(
            "HEADLINE — \"{subject}\" ({receipt_count} independent receipts across {})",
            projects.join(", ")
        ),
        GlobalVerdict::NoDefensibleVerdict => {
            "No defensible cross-project verdict this week.".to_string()
        }
    }
}

// ─── JSON output ─────────────────────────────────────────────────────────

#[derive(Serialize)]
struct DreamsJson {
    global: GlobalJson,
    projects: Vec<ProjectJson>,
}

#[derive(Serialize)]
#[serde(tag = "verdict")]
enum GlobalJson {
    #[serde(rename = "headline")]
    Headline {
        subject: String,
        projects: Vec<String>,
        receipt_count: usize,
    },
    #[serde(rename = "no_defensible_verdict")]
    None,
}

impl From<&GlobalVerdict> for GlobalJson {
    fn from(v: &GlobalVerdict) -> Self {
        match v {
            GlobalVerdict::Headline {
                subject,
                projects,
                receipt_count,
            } => GlobalJson::Headline {
                subject: subject.clone(),
                projects: projects.clone(),
                receipt_count: *receipt_count,
            },
            GlobalVerdict::NoDefensibleVerdict => GlobalJson::None,
        }
    }
}

#[derive(Serialize)]
struct ProjectJson {
    project: String,
    /// `None` only for a `--project` filter naming a project with no
    /// qualifying evidence this week — the one place abstention is visible
    /// in JSON mode.
    category: Option<String>,
    dream_id: Option<String>,
    subject_key: Option<String>,
    text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::dream_clusters::CompletionReceipt;

    fn wd(
        project: &str,
        item_id: &str,
        title: &str,
        hypothesis: Option<&str>,
        how: Vec<&str>,
        kind_label: &'static str,
    ) -> WeekDream {
        WeekDream {
            title: title.to_string(),
            hypothesis: hypothesis.map(str::to_string),
            hypothesis_session: hypothesis.map(|_| "sess-hyp01".to_string()),
            how: how.into_iter().map(str::to_string).collect(),
            project: project.to_string(),
            item_id: item_id.to_string(),
            kind_label,
        }
    }

    fn open_item(
        project: &str,
        id: &str,
        kind: &str,
        session: &str,
        ts: &str,
        title: &str,
        completed: bool,
    ) -> OpenItem {
        OpenItem {
            id: id.to_string(),
            project: project.to_string(),
            item: title.to_string(),
            kind: kind.to_string(),
            origin_session: session.to_string(),
            origin_ts: ts.to_string(),
            origin_date: ts[..10].to_string(),
            completed: if completed {
                Some(CompletionReceipt {
                    session_id: "later".into(),
                    completed_at: "2026-08-16T00:00:00Z".into(),
                    completed_date: "2026-08-16".into(),
                })
            } else {
                None
            },
            examined: true,
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-16T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    // --- card rendering ---------------------------------------------------

    #[test]
    fn unfinished_card_carries_every_required_section_and_a_receipt_per_line() {
        let dream = wd(
            "csr",
            "item1234abcd5678",
            "fix the release gate",
            Some("release gate blocked on codex review"),
            vec!["review week.rs ⌗abc12345"],
            "natural direction",
        );
        let dream_id = compute_dream_id(&dream.project, "unfinished", &dream.item_id, now());
        let text = render_unfinished_card(&dream, &dream_id);

        assert!(text.starts_with("PROJECT csr — unfinished-and-valuable\n"));
        assert!(text.contains("Observed:\n"));
        assert!(text.contains("Dream's take — not a fact:"));
        assert!(text.contains("Proposal — requires verdict:"));
        assert!(text.contains(&dream_attribution::marker_line(&dream_id)));
        // has_plan == true (kind_label == "natural direction") -> no Verify
        // first homework line.
        assert!(!text.contains("Verify first:"));

        // Every "Observed:" bullet line carries a receipt (⌗ marker).
        let observed_block = text
            .split("Observed:\n")
            .nth(1)
            .unwrap()
            .split("Dream's take")
            .next()
            .unwrap();
        for line in observed_block.lines().filter(|l| !l.trim().is_empty()) {
            assert!(
                line.contains('⌗'),
                "observed line missing a receipt: {line}"
            );
        }
    }

    #[test]
    fn unfinished_card_without_a_plan_carries_verify_first() {
        let dream = wd(
            "csr",
            "itemnoplan00000",
            "flaky test",
            Some("flaky test traced to a race"),
            vec![],
            "unfinished",
        );
        let dream_id = compute_dream_id(&dream.project, "unfinished", &dream.item_id, now());
        let text = render_unfinished_card(&dream, &dream_id);
        assert!(text.contains("Verify first:"));
    }

    #[test]
    fn observed_line_falls_back_to_item_id_when_a_how_line_carries_no_oid() {
        // compose_how_line omits the oid segment when citation was blank —
        // the how line itself carries no ⌗, so the card must add one.
        let dream = wd(
            "csr",
            "abc123",
            "resolve the blocker",
            None,
            vec!["resolve the blocker"],
            "natural direction",
        );
        let lines = observed_lines(&dream);
        assert!(lines.iter().any(|l| l == "resolve the blocker ⌗abc123"));
    }

    #[test]
    fn compute_dream_id_differs_across_runs_and_unfinished_revision_hash_is_content_only() {
        let dream = wd(
            "csr",
            "item1",
            "title",
            Some("hyp"),
            vec!["step ⌗oid1"],
            "natural direction",
        );
        let later = now() + chrono::Duration::seconds(5);
        let id1 = compute_dream_id(&dream.project, "unfinished", &dream.item_id, now());
        let id2 = compute_dream_id(&dream.project, "unfinished", &dream.item_id, later);
        assert_ne!(
            id1, id2,
            "compute_dream_id folds in `now`, must differ across calls"
        );
        assert_eq!(
            unfinished_revision_hash(&dream),
            unfinished_revision_hash(&dream),
            "revision_hash is content-only, must be stable across runs"
        );
    }

    #[test]
    fn an_unchanged_subject_and_revision_reuses_its_dream_id_across_runs() {
        // The major-finding regression: re-running against the identical
        // corpus must not churn a fresh dream_id (and a fresh row) every
        // time — that breaks any verdict tooling that targeted yesterday's
        // printed id.
        let storage = Storage::open_memory().unwrap();
        let dream = wd(
            "csr",
            "item-stable",
            "fix the release gate",
            Some("hyp"),
            vec!["step ⌗oid1"],
            "natural direction",
        );
        let revision_hash = unfinished_revision_hash(&dream);

        assert!(
            lookup_open_dream_id(&storage, "csr", "unfinished", "item-stable", &revision_hash)
                .is_none(),
            "nothing recorded yet"
        );
        let first_id = compute_dream_id(&dream.project, "unfinished", &dream.item_id, now());
        record_dream_row(
            &storage,
            &first_id,
            "csr",
            "unfinished",
            Some("item-stable"),
            &revision_hash,
            "card text",
        )
        .unwrap();

        // A later run, same subject, same revision (content unchanged) —
        // must find and reuse the same dream_id rather than minting a new
        // one, even though `now` has moved on.
        let later = now() + chrono::Duration::days(1);
        let reused =
            lookup_open_dream_id(&storage, "csr", "unfinished", "item-stable", &revision_hash);
        assert_eq!(reused.as_deref(), Some(first_id.as_str()));
        assert_ne!(
            reused.unwrap(),
            compute_dream_id(&dream.project, "unfinished", &dream.item_id, later),
            "sanity: a freshly minted id at a later `now` would have differed \
             had the lookup not short-circuited to the existing row"
        );

        // A genuinely changed revision (different evidence) does NOT reuse
        // the old id — it's a new logical fact, not the same one recomputed.
        let mut changed = dream.clone();
        changed.how = vec!["a different step ⌗oid2".to_string()];
        let changed_hash = unfinished_revision_hash(&changed);
        assert_ne!(changed_hash, revision_hash);
        assert!(
            lookup_open_dream_id(&storage, "csr", "unfinished", "item-stable", &changed_hash)
                .is_none(),
            "a changed revision must not resolve to the old row"
        );
    }

    // --- global gate --------------------------------------------------------

    fn candidate(
        subject: &str,
        projects: &[&str],
        receipts: &[(&str, &str, bool)],
    ) -> GlobalCandidate {
        GlobalCandidate {
            subject: subject.to_string(),
            projects: projects.iter().map(|s| s.to_string()).collect(),
            receipts: receipts
                .iter()
                .map(|(s, d, stakes)| CandidateReceipt {
                    session_id: s.to_string(),
                    day: d.to_string(),
                    stakes: *stakes,
                })
                .collect(),
        }
    }

    #[test]
    fn global_gate_publishes_a_unique_winner_with_two_receipts_and_stakes() {
        let winner = candidate(
            "release gate stuck",
            &["csr", "cc-enhance"],
            &[
                ("sess-a", "2026-08-10", true),
                ("sess-b", "2026-08-12", false),
            ],
        );
        let runner_up = candidate(
            "flaky test",
            &["csr"],
            &[
                ("sess-c", "2026-08-11", true),
                ("sess-d", "2026-08-13", true),
            ],
        );
        // runner_up also qualifies but has fewer... make it tie-free by
        // giving the winner one more receipt.
        let mut winner_three = winner.clone();
        winner_three.receipts.push(CandidateReceipt {
            session_id: "sess-e".into(),
            day: "2026-08-14".into(),
            stakes: false,
        });
        let got = global_headline(&[winner_three.clone(), runner_up]);
        assert_eq!(
            got,
            GlobalVerdict::Headline {
                subject: winner_three.subject.clone(),
                projects: winner_three.projects.clone(),
                receipt_count: 3,
            }
        );
    }

    #[test]
    fn global_gate_abstains_on_a_tie_at_the_top() {
        let a = candidate(
            "theme a",
            &["csr"],
            &[
                ("sess-a", "2026-08-10", true),
                ("sess-b", "2026-08-11", false),
            ],
        );
        let b = candidate(
            "theme b",
            &["cc-enhance"],
            &[
                ("sess-c", "2026-08-10", true),
                ("sess-d", "2026-08-11", false),
            ],
        );
        assert_eq!(global_headline(&[a, b]), GlobalVerdict::NoDefensibleVerdict);
    }

    #[test]
    fn global_gate_abstains_without_a_stakes_receipt() {
        let no_stakes = candidate(
            "theme",
            &["csr"],
            &[
                ("sess-a", "2026-08-10", false),
                ("sess-b", "2026-08-11", false),
            ],
        );
        assert_eq!(
            global_headline(&[no_stakes]),
            GlobalVerdict::NoDefensibleVerdict
        );
    }

    #[test]
    fn global_gate_abstains_on_a_single_project_even_with_two_stakes_receipts() {
        // The minor-finding regression: two independent, stakes-bearing
        // receipts within ONE project must not publish under a headline
        // whose own rendering claims "across <projects>" — that claim needs
        // more than one project behind it.
        let single_project = candidate(
            "recurring theme",
            &["csr"],
            &[
                ("sess-a", "2026-08-10", true),
                ("sess-b", "2026-08-11", true),
            ],
        );
        assert_eq!(
            global_headline(&[single_project]),
            GlobalVerdict::NoDefensibleVerdict
        );
    }

    #[test]
    fn global_gate_abstains_with_only_one_distinct_receipt() {
        // Two receipts, but same session+day -> one distinct piece of
        // evidence, not two independent ones.
        let one_distinct = candidate(
            "theme",
            &["csr"],
            &[
                ("sess-a", "2026-08-10", true),
                ("sess-a", "2026-08-10", true),
            ],
        );
        assert_eq!(
            global_headline(&[one_distinct]),
            GlobalVerdict::NoDefensibleVerdict
        );
    }

    #[test]
    fn global_gate_empty_candidates_abstains() {
        assert_eq!(global_headline(&[]), GlobalVerdict::NoDefensibleVerdict);
    }

    #[test]
    fn build_global_candidates_excludes_completed_and_out_of_week_items() {
        let now = now();
        let items = vec![
            open_item(
                "csr",
                "a",
                "blocker",
                "sess-a",
                "2026-08-10T10:00:00Z",
                "recurring theme",
                false,
            ),
            open_item(
                "cc-enhance",
                "b",
                "blocker",
                "sess-b",
                "2026-08-11T10:00:00Z",
                "recurring theme",
                false,
            ),
            // Out of the rolling week.
            open_item(
                "csr",
                "c",
                "blocker",
                "sess-c",
                "2026-07-01T10:00:00Z",
                "recurring theme",
                false,
            ),
            // Completed — excluded even though it names the same theme.
            open_item(
                "csr",
                "d",
                "blocker",
                "sess-d",
                "2026-08-12T10:00:00Z",
                "recurring theme",
                true,
            ),
        ];
        let candidates = build_global_candidates(&items, now);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].receipts.len(), 2);
        assert_eq!(
            candidates[0].projects,
            vec!["csr".to_string(), "cc-enhance".to_string()]
        );
    }

    // --- persistence ---------------------------------------------------

    #[test]
    fn record_dream_row_stores_the_correct_subject_key() {
        let storage = Storage::open_memory().unwrap();
        record_dream_row(
            &storage,
            "deadbeefcafef00d",
            "csr",
            "unfinished",
            Some("item-42"),
            "revhash",
            "card text",
        )
        .unwrap();

        let (subject_key, project, category, revision_hash, prose): (Option<String>, String, String, String, String) =
            storage
                .with_connection(|conn| {
                    conn.query_row(
                        "SELECT subject_key, project, category, revision_hash, prose FROM dreams_v1 WHERE dream_id = ?1",
                        params!["deadbeefcafef00d"],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                            ))
                        },
                    )
                    .map_err(Into::into)
                })
                .unwrap();

        assert_eq!(subject_key.as_deref(), Some("item-42"));
        assert_eq!(project, "csr");
        assert_eq!(category, "unfinished");
        assert_eq!(revision_hash, "revhash");
        assert_eq!(prose, "card text");
    }

    // --- build_run: category competition (minor finding, zero coverage) ---

    fn seed_v2_episode(conn: &rusqlite::Connection, id: &str, session: &str, ts: &str, todo: &str) {
        let content = format!(
            r#"{{"schema":"v2","session_id":"{session}","project":"csr","timestamp":"{ts}","todos":[{{"content":"{todo}","status":"pending"}}],"files_modified":[]}}"#
        );
        conn.execute(
            "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, '[]', ?3)",
            params![id, content, ts],
        )
        .unwrap();
    }

    #[test]
    fn a_cached_strategy_dream_takes_the_slot_over_a_qualifying_unfinished_one() {
        let storage = Storage::open_memory().unwrap();
        let now = DateTime::parse_from_rfc3339("2026-08-16T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // Three open items in one project, three distinct sessions — clears
        // strategy's MIN_DISTINCT_SESSIONS gate. Only the first gets a
        // stored plan, so it's also the one candidate that clears
        // `select_week_dreams`'s own gate and becomes the unfinished
        // candidate for this project's slot.
        storage
            .with_connection(|conn| {
                seed_v2_episode(
                    conn,
                    "ep-a",
                    "sess-a",
                    "2026-08-15T10:00:00Z",
                    "fix the release gate",
                );
                seed_v2_episode(
                    conn,
                    "ep-b",
                    "sess-b",
                    "2026-08-15T11:00:00Z",
                    "second item",
                );
                seed_v2_episode(conn, "ep-c", "sess-c", "2026-08-15T12:00:00Z", "third item");
                Ok(())
            })
            .unwrap();

        let item_id = storage
            .with_connection(|conn| {
                let items = dream_clusters::load_open_items(conn)?;
                Ok(items
                    .into_iter()
                    .find(|i| i.item == "fix the release gate")
                    .expect("seeded item")
                    .id)
            })
            .unwrap();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO dream_plans \
                     (plan_hash, item_id, project, session_id, context, steps_json, \
                      files_json, acceptance, dropped, model) \
                     VALUES ('hash1', ?1, 'csr', 'sess-a', 'ctx', \
                     '[{\"action\":\"review the gate\",\"files\":[],\"citation\":\"abc12345\"}]', \
                     '[]', 'ok', 0, 'sonnet-5')",
                    params![item_id],
                )?;
                Ok(())
            })
            .unwrap();

        // Confirm the unfinished candidate genuinely exists before adding
        // strategy to the mix — otherwise this test would prove nothing
        // about competition.
        let week_dreams_before = storage
            .with_connection(|conn| week::load_week_dreams(conn, now))
            .unwrap();
        assert_eq!(
            week_dreams_before.len(),
            1,
            "the unfinished candidate must exist"
        );
        assert_eq!(week_dreams_before[0].project, "csr");

        // Seed a strategy cache hit at the revision_hash `build_run` will
        // actually compute for this corpus, so the strategy branch inside
        // `author_for_project` hits cache (zero LLM spend, no shell-out).
        let (open_items, dream_threads) = storage
            .with_connection(|conn| {
                let oi = dream_clusters::load_open_items(conn)?;
                let dt = crate::dream::threads::load_dream_threads(conn)?;
                Ok((oi, dt))
            })
            .unwrap();
        let evidence = storage
            .with_connection(|conn| {
                crate::dream::strategy::build_project_evidence(
                    conn,
                    "csr",
                    &open_items,
                    &dream_threads,
                    now,
                )
            })
            .unwrap();
        let rev_hash = crate::dream::strategy::revision_hash(&evidence);
        record_dream_row(
            &storage,
            "cachedstrategyid",
            "csr",
            "strategy",
            None,
            &rev_hash,
            "PROJECT csr — strategy\ncached prose\n",
        )
        .unwrap();

        let (_, cards) = build_run(&storage, None, false, now).unwrap();
        assert_eq!(cards.len(), 1, "one project, one slot");
        assert_eq!(cards[0].project, "csr");
        assert_eq!(
            cards[0].category, "strategy",
            "strategy must win the slot whenever it produced any dream"
        );
        assert_eq!(cards[0].dream_id, "cachedstrategyid");
        assert!(cards[0].text.contains("cached prose"));
    }

    #[test]
    fn a_project_filter_naming_a_project_with_no_evidence_yields_zero_cards() {
        // Feeds emit_json's one abstention branch: a `--project` filter that
        // matches nothing must not error and must not synthesize a card —
        // the caller (emit_json) is what renders the explicit "no dream"
        // JSON entry from an empty card list.
        let storage = Storage::open_memory().unwrap();
        let now = Utc::now();
        let (verdict, cards) = build_run(&storage, Some("no-such-project"), true, now).unwrap();
        assert!(cards.is_empty());
        assert_eq!(verdict, GlobalVerdict::NoDefensibleVerdict);
    }
}
