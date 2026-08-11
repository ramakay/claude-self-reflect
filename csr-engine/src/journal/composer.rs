//! Journal v4 Phase 4 — the proposal composer and spend visibility.
//!
//! Four deliverables, each with its own honesty rule:
//!
//! * **BRIEF** — the context a fresh session needs (origin ask, what was
//!   completed, what the night pass concluded, the receipts). Every clause
//!   is copied from a stored row; a clause with no row behind it is dropped
//!   rather than filled. Truncation is explicit: [`Brief::truncation`]
//!   carries a measured `showing N of M` marker, never a silent cut.
//! * **COPY BLOCK** (locked decision 3) — a paste-ready markdown resume
//!   prompt for a NEW Claude Code session. Assembled from stored rows only.
//!   When a night-pass thread exists for the item's origin session it
//!   carries that thread's evidence quote **verbatim, with its receipt**;
//!   when none exists the block SAYS SO in the same slot instead of leaving
//!   a gap the reader could mistake for a finding.
//! * **STRUCTURED PLAN** (locked decision 2) — the model drafts
//!   context → steps → files → acceptance through the propose-verify
//!   machinery that already exists in [`crate::dream::threads`]: its
//!   [`NightActor`] abstraction, its model chain, its convergence hashing
//!   idiom and its `narrative_usage` accounting. Nothing here re-implements
//!   any of those. The verifier ([`verify_plan`]) is deterministic and has
//!   no LLM in it: a step whose citation is not a verbatim substring of the
//!   evidence corpus, or whose files are not in the allowlist, is
//!   **dropped**. Dropped lines are dropped — never softened, never
//!   rewritten into a hedge. Surviving steps are labelled
//!   [`PLAN_LABEL`] ("proposed — not executed") and carry their receipts
//!   inline.
//! * **SPEND** (locked decision 13) — tokens in / tokens out / cost per
//!   dream, summed from `narrative_usage` rows tagged with the convergence
//!   hash the work ran under. **If usage was not recorded for a dream this
//!   returns `None` and the surface renders nothing** — never a zero, which
//!   would read as "this dream was free".
//!
//! # Convergence, and why a GET never spends
//!
//! Plans are proposed by a background caller and **stored**; the detail
//! route only ever *reads* [`load_plan`]. A page view can therefore never
//! invoke an actor. A re-run over unchanged evidence hits either the stored
//! plan or the sentinel row (empty context + empty steps, written when
//! verification kept nothing), so a frozen corpus costs zero further spend —
//! the same convergence-by-construction contract as
//! `dream::threads::already_converged`.

use std::collections::BTreeSet;

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::dream::report::{iso_date, short_oid};
use crate::dream::threads::{self, DreamThread, NightActor, Receipt, ReceiptTier};
use crate::storage::dream_items::{DreamEvidence, DreamItem};
use crate::storage::queries::{self, NarrativeUsageByModel};
use crate::storage::Storage;

/// Bumped whenever the plan prompt or its output contract changes. Folded
/// into `plan_hash`, so every row cached under an older prompt version
/// misses deterministically — no ALTER, no backfill (same idiom as
/// `dream::threads::THREAD_PROMPT_VERSION`).
const PLAN_PROMPT_VERSION: u32 = 1;

/// The label every rendered plan carries. A plan is a proposal that survived
/// a deterministic verifier — it is never a record of work that happened.
pub const PLAN_LABEL: &str = "proposed — not executed";

/// Brief lines shown before truncation. The marker reports the true total.
pub const BRIEF_MAX_LINES: usize = 6;
/// Per-line character budget in the brief. A cut line ends with `…` and is
/// counted in [`Brief::truncated_lines`], so "this text was shortened" is a
/// measured fact on the view rather than something the reader must infer.
pub const BRIEF_MAX_LINE_CHARS: usize = 320;
/// Steps accepted per plan, matching the prompt's own "max 6".
const MAX_PLAN_STEPS: usize = 6;
/// Files listed on a plan.
const MAX_PLAN_FILES: usize = 12;
/// Hard prompt ceiling for the plan draft.
const PLAN_PROMPT_CAP_BYTES: usize = 8 * 1024;

// ─── spend ────────────────────────────────────────────────────────────────

/// Published list prices, US$ per million tokens, keyed by the longest
/// distinguishing fragment of a model id. Ordered **longest key first**: the
/// match is a substring test, so `"haiku-4"` must be tried before `"haiku"`.
///
/// This is a *list price* table, not a bill. `claude -p` may be served under
/// a subscription, in which case no per-call charge exists at all — which is
/// exactly why [`DreamSpend::cost_usd`] is presented as an at-list-price
/// figure and never as "you were charged this".
const MODEL_PRICES: &[(&str, f64, f64)] = &[
    ("mythos-5", 10.0, 50.0),
    ("sonnet-5", 3.0, 15.0),
    ("sonnet-4", 3.0, 15.0),
    ("fable-5", 10.0, 50.0),
    ("haiku-4", 1.0, 5.0),
    ("opus-5", 5.0, 25.0),
    ("opus-4", 5.0, 25.0),
    ("sonnet", 3.0, 15.0),
    ("haiku", 1.0, 5.0),
    ("opus", 5.0, 25.0),
];

/// Cache-read tokens bill at ~0.1× the input rate.
const CACHE_READ_MULTIPLIER: f64 = 0.1;
/// Cache-write tokens bill at ~1.25× the input rate (5-minute TTL).
const CACHE_WRITE_MULTIPLIER: f64 = 1.25;

/// `(input, output)` US$ per million tokens, or `None` when the model is not
/// in the table. An unknown model yields no cost — never a zero.
pub fn model_price(model: &str) -> Option<(f64, f64)> {
    let lower = model.to_ascii_lowercase();
    MODEL_PRICES
        .iter()
        .find(|(key, _, _)| lower.contains(key))
        .map(|(_, input, output)| (*input, *output))
}

/// Measured spend for one dream. Every field is a sum over rows that
/// actually exist; there is no constructor that fabricates one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DreamSpend {
    /// Number of `narrative_usage` rows attributed to this dream.
    pub calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    /// List-price cost in US$, or `None` when **any** contributing model has
    /// no published price — a partial total would understate the spend, so
    /// the figure is withheld rather than shown low.
    pub cost_usd: Option<f64>,
    /// Models that carried tokens but have no price on record. Rendered so
    /// the missing cost is explained rather than silently absent.
    pub unpriced_models: Vec<String>,
    /// Every model that contributed, for the detail view's breakdown.
    pub models: Vec<String>,
}

impl DreamSpend {
    /// Total the per-model rows. `None` when nothing was recorded.
    pub fn from_rows(rows: &[NarrativeUsageByModel]) -> Option<Self> {
        if rows.is_empty() {
            return None;
        }
        let mut spend = DreamSpend {
            calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd: None,
            unpriced_models: Vec::new(),
            models: Vec::new(),
        };
        let mut cost = 0.0_f64;
        for row in rows {
            spend.calls += row.calls;
            spend.input_tokens += row.input_tokens;
            spend.output_tokens += row.output_tokens;
            spend.cache_read_tokens += row.cache_read_tokens;
            spend.cache_creation_tokens += row.cache_creation_tokens;
            spend.models.push(row.model.clone());
            match model_price(&row.model) {
                Some((input_rate, output_rate)) => {
                    let per_million = |tokens: i64, rate: f64| (tokens as f64) * rate / 1_000_000.0;
                    cost += per_million(row.input_tokens, input_rate);
                    cost += per_million(row.output_tokens, output_rate);
                    cost += per_million(row.cache_read_tokens, input_rate) * CACHE_READ_MULTIPLIER;
                    cost +=
                        per_million(row.cache_creation_tokens, input_rate) * CACHE_WRITE_MULTIPLIER;
                }
                None => spend.unpriced_models.push(row.model.clone()),
            }
        }
        spend.unpriced_models.dedup();
        if spend.unpriced_models.is_empty() {
            spend.cost_usd = Some(cost);
        }
        Some(spend)
    }

    /// `"1,240 in · 380 out"` — always renderable, because both numbers were
    /// measured.
    pub fn tokens_label(&self) -> String {
        format!(
            "{} in · {} out",
            group_digits(self.input_tokens),
            group_digits(self.output_tokens)
        )
    }

    /// `"≈$0.0123 at list price"`, or an explicit sentence naming the model
    /// whose price is unknown. Never `$0.00` standing in for "unknown".
    pub fn cost_label(&self) -> String {
        match self.cost_usd {
            Some(cost) => format!("≈${cost:.4} at list price"),
            None => format!(
                "cost unavailable — no published price for {}",
                self.unpriced_models.join(", ")
            ),
        }
    }
}

fn group_digits(value: i64) -> String {
    let raw = value.abs().to_string();
    let mut out = String::with_capacity(raw.len() + raw.len() / 3);
    for (index, ch) in raw.chars().enumerate() {
        if index > 0 && (raw.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if value < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// Every `ref_id` a dream's spend could have been recorded under: the
/// convergence hashes of its night-pass threads plus its stored plan.
pub fn spend_refs(threads: &[DreamThread], plan: Option<&StoredPlan>) -> Vec<String> {
    let mut refs: BTreeSet<String> = threads
        .iter()
        .map(|thread| thread.episode_hash.clone())
        .collect();
    if let Some(plan) = plan {
        refs.insert(plan.plan_hash.clone());
    }
    refs.into_iter().collect()
}

/// Sum the usage rows tagged with any of `refs`. `None` when no row carries
/// one of them — the surface then renders nothing at all.
pub fn load_spend(conn: &Connection, refs: &[String]) -> Result<Option<DreamSpend>> {
    let rows = queries::narrative_usage_for_refs(conn, refs)?;
    Ok(DreamSpend::from_rows(&rows))
}

// ─── brief ────────────────────────────────────────────────────────────────

/// One labelled line of the brief. `label` is a fixed vocabulary, never
/// model text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BriefLine {
    pub label: &'static str,
    pub text: String,
    /// `true` when `text` was cut to [`BRIEF_MAX_LINE_CHARS`].
    pub truncated: bool,
}

/// The context a fresh session needs, with truncation stated rather than
/// hidden.
///
/// `Default` is implemented by hand, not derived: a derived one would set
/// `empty: false` on a brief with no lines, and the template branches on
/// exactly that flag. The default brief is the *no evidence at all* brief,
/// so it must say so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Brief {
    pub lines: Vec<BriefLine>,
    /// Lines actually rendered.
    pub shown: usize,
    /// Lines that had evidence behind them, before the display cap.
    pub total: usize,
    /// `"showing 6 of 9"` — `None` when nothing was cut.
    pub truncation: Option<String>,
    /// How many rendered lines had their own text shortened.
    pub truncated_lines: usize,
    /// `true` when no clause had evidence behind it. The view renders an
    /// explicit "nothing on record" state; it never renders an empty box.
    pub empty: bool,
}

impl Default for Brief {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            shown: 0,
            total: 0,
            truncation: None,
            truncated_lines: 0,
            empty: true,
        }
    }
}

/// The stored episode fields the brief and copy block quote. All optional:
/// a field the episode never carried drops its clause.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EpisodeFacts {
    pub request: Option<String>,
    pub completed: Option<String>,
    pub outcome: Option<String>,
    pub files: Vec<String>,
}

fn truncate_chars(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_string(), false);
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    (out, true)
}

fn push_line(lines: &mut Vec<BriefLine>, label: &'static str, text: Option<&str>) {
    let Some(text) = text.map(str::trim).filter(|t| !t.is_empty()) else {
        return; // no row behind this clause — drop it, never fill it.
    };
    let (text, truncated) = truncate_chars(text, BRIEF_MAX_LINE_CHARS);
    lines.push(BriefLine {
        label,
        text,
        truncated,
    });
}

/// Build the brief from stored rows only.
///
/// Clause order is fixed and meaningful: what was asked, what got done, what
/// the night pass concluded, then the receipts that back it. Each clause
/// drops independently when its row is absent.
pub fn build_brief(item: &DreamItem, episode: &EpisodeFacts, threads: &[DreamThread]) -> Brief {
    let mut lines: Vec<BriefLine> = Vec::new();

    push_line(&mut lines, "origin ask", episode.request.as_deref());
    push_line(&mut lines, "completed", episode.completed.as_deref());
    push_line(&mut lines, "outcome", episode.outcome.as_deref());

    for thread in threads.iter().take(2) {
        push_line(&mut lines, "night pass", Some(&thread.thread));
        push_line(
            &mut lines,
            "night-pass evidence",
            Some(&format!("“{}”", thread.evidence_quote)),
        );
    }

    let receipts = receipt_lines(&item.evidence);
    for receipt in receipts.iter().take(2) {
        push_line(&mut lines, "receipt", Some(receipt));
    }

    let total = lines.len();
    let truncated_lines = lines
        .iter()
        .take(BRIEF_MAX_LINES)
        .filter(|line| line.truncated)
        .count();
    lines.truncate(BRIEF_MAX_LINES);
    let shown = lines.len();
    Brief {
        lines,
        shown,
        total,
        truncation: (total > shown).then(|| format!("showing {shown} of {total}")),
        truncated_lines,
        empty: total == 0,
    }
}

/// `"run_report anchor_obsolete in report.rs ⌗abcdef12 · witnessed 2026-08-09"`
/// for each evidence row that actually carries a receipt oid. Rows without
/// one are skipped here — the copy block states their absence separately.
fn receipt_lines(evidence: &[DreamEvidence]) -> Vec<String> {
    evidence
        .iter()
        .filter_map(|row| {
            let oid = row.receipt_oid.as_deref()?;
            let subject = match &row.symbol {
                Some(symbol) => format!("{symbol} {}", row.verdict),
                None => row.verdict.clone(),
            };
            Some(format!(
                "{subject} in {} ⌗{} · witnessed {}",
                row.file,
                short_oid(oid),
                iso_date(&row.witnessed_at)
            ))
        })
        .collect()
}

// ─── copy block ───────────────────────────────────────────────────────────

/// Sentence used in the night-pass slot when no thread exists for the item's
/// origin session. Stating the gap is the point: the reader must not be able
/// to mistake silence for "the night pass found nothing wrong".
pub const NO_NIGHT_PASS: &str =
    "No night-pass thread is on record for this session. Nothing is claimed \
     about what a night pass concluded — the section is empty because the \
     evidence is, not because the work is clean.";

/// Sentence used in the plan slot when no verified plan is stored.
pub const NO_PLAN: &str = "No verified plan is on record. Work from the receipts above.";

/// A paste-ready resume prompt, plus the facts a caller may want to assert
/// about it without re-parsing the markdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CopyBlock {
    pub markdown: String,
    /// `true` iff a night-pass thread contributed a verbatim quote.
    pub has_night_pass: bool,
    /// `true` iff a stored, verified plan contributed steps.
    pub has_plan: bool,
}

/// Render one thread receipt as a single inline clause.
fn receipt_clause(receipt: &Receipt) -> String {
    match receipt {
        Receipt::Verdict {
            symbol,
            verdict,
            receipt_oid,
            witnessed_at,
        } => {
            let subject = match symbol {
                Some(symbol) => format!("{symbol} {verdict}"),
                None => verdict.clone(),
            };
            match receipt_oid {
                Some(oid) => format!(
                    "{subject} ⌗{} · witnessed {}",
                    short_oid(oid),
                    iso_date(witnessed_at)
                ),
                None => format!(
                    "{subject} · witnessed {} · no receipt",
                    iso_date(witnessed_at)
                ),
            }
        }
        Receipt::Witnessed {
            file,
            witness_count,
        } => format!("{file} · {witness_count} witness rows, no verdict"),
    }
}

fn tier_word(tier: ReceiptTier) -> &'static str {
    match tier {
        ReceiptTier::Verdict => "verdict-backed",
        ReceiptTier::Witnessed => "witnessed, no verdict",
        ReceiptTier::Unverified => "unverified",
    }
}

/// Assemble the paste-ready resume prompt (locked decision 3).
///
/// Every section is built from a stored row. Where a row is missing the
/// section says so in words; there is no branch that omits a heading and
/// leaves the reader to guess whether the evidence was absent or merely not
/// rendered.
pub fn build_copy_block(
    item: &DreamItem,
    episode: &EpisodeFacts,
    threads: &[DreamThread],
    plan: Option<&StoredPlan>,
) -> CopyBlock {
    let mut md = String::new();

    md.push_str(&format!("## Resume: {}\n\n", item.item));
    md.push_str(&format!(
        "This {} was left open on {} in session `{}` (project `{}`). \
         Everything below is copied from stored rows; nothing is inferred.\n\n",
        item.kind,
        iso_date(&item.origin_ts),
        item.origin_session,
        item.project
    ));

    md.push_str("### What was originally asked\n\n");
    match episode
        .request
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(request) => md.push_str(&format!("{request}\n\n")),
        None => md.push_str("Not on record — the origin session stored no request text.\n\n"),
    }

    md.push_str("### What was completed before it stopped\n\n");
    match episode
        .completed
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(completed) => md.push_str(&format!("{completed}\n\n")),
        None => md.push_str("Not on record — the origin session stored no completion summary.\n\n"),
    }

    md.push_str("### What changed underneath it\n\n");
    let receipts = receipt_lines(&item.evidence);
    if receipts.is_empty() {
        md.push_str(
            "No evidence row on this item carries a receipt oid. The change is \
             witnessed but not receipted; treat it as unproven.\n\n",
        );
    } else {
        for line in &receipts {
            md.push_str(&format!("- {line}\n"));
        }
        md.push('\n');
    }

    md.push_str("### What the night pass concluded\n\n");
    let has_night_pass = !threads.is_empty();
    if has_night_pass {
        for thread in threads.iter().take(2) {
            md.push_str(&format!("**{}**\n\n", thread.thread));
            md.push_str(&format!("> {}\n\n", thread.evidence_quote));
            md.push_str(&format!("Receipt tier: {}", tier_word(thread.receipt_tier)));
            if thread.receipts.is_empty() {
                md.push_str(" — no receipt rows stored.\n\n");
            } else {
                md.push_str(".\n");
                for receipt in thread.receipts.iter().take(4) {
                    md.push_str(&format!("- {}\n", receipt_clause(receipt)));
                }
                md.push('\n');
            }
        }
    } else {
        md.push_str(NO_NIGHT_PASS);
        md.push_str("\n\n");
    }

    md.push_str("### Files to look at\n\n");
    let mut files: BTreeSet<String> = item
        .evidence
        .iter()
        .map(|evidence| evidence.file.clone())
        .collect();
    for thread in threads {
        files.extend(thread.files.iter().cloned());
    }
    files.extend(episode.files.iter().cloned());
    files.retain(|file| !file.trim().is_empty());
    if files.is_empty() {
        md.push_str("No file is on record for this item.\n\n");
    } else {
        for file in files.iter().take(MAX_PLAN_FILES) {
            md.push_str(&format!("- `{file}`\n"));
        }
        md.push('\n');
    }

    md.push_str("### Proposed plan\n\n");
    let has_plan = plan.is_some_and(|plan| !plan.steps.is_empty());
    match plan.filter(|plan| !plan.steps.is_empty()) {
        Some(plan) => {
            md.push_str(&format!("_{PLAN_LABEL}._\n\n"));
            if !plan.context.trim().is_empty() {
                md.push_str(&format!("{}\n\n", plan.context));
            }
            for (index, step) in plan.steps.iter().enumerate() {
                md.push_str(&format!("{}. {}\n", index + 1, step.action));
                md.push_str(&format!("   - traces to: {}\n", step.citation));
                for file in &step.files {
                    md.push_str(&format!("   - file: `{file}`\n"));
                }
            }
            md.push('\n');
            if plan.dropped > 0 {
                md.push_str(&format!(
                    "{} drafted step(s) were dropped by the verifier because their claims \
                     did not trace to a stored row. They are not shown in any form.\n\n",
                    plan.dropped
                ));
            }
        }
        None => {
            md.push_str(NO_PLAN);
            md.push_str("\n\n");
        }
    }

    md.push_str("### How to verify\n\n");
    match plan.and_then(|plan| plan.acceptance.clone()) {
        Some(acceptance) => md.push_str(&format!("{acceptance}\n")),
        None => md.push_str(
            "No acceptance check is on record. Verify against the receipts listed \
             above before treating the item as resolved.\n",
        ),
    }

    CopyBlock {
        markdown: md,
        has_night_pass,
        has_plan,
    }
}

// ─── structured plan: types ───────────────────────────────────────────────

/// What the actor is asked to produce, before verification.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RawPlan {
    pub context: String,
    pub steps: Vec<RawPlanStep>,
    pub acceptance: String,
    /// A verbatim quote backing `context` and `acceptance`.
    pub citation: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RawPlanStep {
    pub action: String,
    pub files: Vec<String>,
    /// Either a VERBATIM substring of the evidence text, or a receipt oid.
    pub citation: String,
}

/// One step that survived verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub action: String,
    pub files: Vec<String>,
    /// The verbatim quote or receipt oid this step traces to. Rendered
    /// inline — a step is never shown without the thing that grounds it.
    pub citation: String,
}

/// The evidence corpus the verifier checks against. Nothing outside this is
/// traceable, by construction.
#[derive(Debug, Clone, Default)]
pub struct PlanEvidence {
    /// Concatenated verbatim texts a citation may quote from.
    pub corpus: String,
    /// The only file paths a step may name.
    pub allowlist: Vec<String>,
    /// Receipt oids (full and short form) a citation may name instead of a
    /// quote.
    pub receipts: Vec<String>,
}

impl PlanEvidence {
    /// Build the corpus from stored rows only.
    pub fn build(item: &DreamItem, episode: &EpisodeFacts, threads: &[DreamThread]) -> Self {
        let mut corpus = String::new();
        corpus.push_str(&item.item);
        corpus.push('\n');
        for text in [
            episode.request.as_deref(),
            episode.completed.as_deref(),
            episode.outcome.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            corpus.push_str(text);
            corpus.push('\n');
        }
        for thread in threads {
            corpus.push_str(&thread.thread);
            corpus.push('\n');
            corpus.push_str(&thread.evidence_quote);
            corpus.push('\n');
        }
        for evidence in &item.evidence {
            corpus.push_str(&evidence.verdict);
            corpus.push(' ');
            if let Some(symbol) = &evidence.symbol {
                corpus.push_str(symbol);
                corpus.push(' ');
            }
            corpus.push_str(&evidence.file);
            corpus.push('\n');
        }

        let mut allowlist: BTreeSet<String> = item
            .evidence
            .iter()
            .map(|evidence| evidence.file.clone())
            .collect();
        for thread in threads {
            allowlist.extend(thread.files.iter().cloned());
        }
        allowlist.extend(episode.files.iter().cloned());
        allowlist.retain(|file| !file.trim().is_empty());

        let mut receipts: BTreeSet<String> = BTreeSet::new();
        for evidence in &item.evidence {
            if let Some(oid) = &evidence.receipt_oid {
                receipts.insert(oid.clone());
                receipts.insert(short_oid(oid));
            }
        }
        for thread in threads {
            for receipt in &thread.receipts {
                if let Receipt::Verdict {
                    receipt_oid: Some(oid),
                    ..
                } = receipt
                {
                    receipts.insert(oid.clone());
                    receipts.insert(short_oid(oid));
                }
            }
        }

        Self {
            corpus,
            allowlist: allowlist.into_iter().collect(),
            receipts: receipts.into_iter().collect(),
        }
    }
}

/// A plan after verification, ready to store or render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifiedPlan {
    /// Empty when the drafted context did not trace.
    pub context: String,
    pub steps: Vec<PlanStep>,
    /// Union of the surviving steps' files. Never a file no step cited.
    pub files: Vec<String>,
    /// `None` when the drafted acceptance check did not trace.
    pub acceptance: Option<String>,
    /// Measured count of drafted steps the verifier removed.
    pub dropped: usize,
}

impl VerifiedPlan {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

// ─── structured plan: the deterministic verifier ──────────────────────────

/// Does `citation` trace? A citation traces when it is a non-trivial
/// verbatim substring of the evidence corpus, or names a stored receipt oid.
/// Nothing else counts — in particular, a plausible-sounding paraphrase does
/// not.
fn citation_traces(citation: &str, evidence: &PlanEvidence) -> bool {
    let citation = citation.trim();
    if citation.len() < 8 {
        // Too short to be evidence of anything; a two-word fragment would
        // match almost any corpus by accident.
        return false;
    }
    if evidence.receipts.iter().any(|oid| oid == citation) {
        return true;
    }
    evidence.corpus.contains(citation)
}

/// Path-shaped tokens in free text: anything containing a `/` or ending in a
/// known source extension. Used to catch a step that names a file inside its
/// prose rather than in its `files` array.
fn path_like_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '`' | '"' | '\'' | '(' | ')' | ',' | ';'))
        .filter(|token| !token.is_empty())
        .map(|token| token.trim_end_matches(['.', ':']).to_string())
        .filter(|token| {
            token.contains('/')
                || token.ends_with(".rs")
                || token.ends_with(".ts")
                || token.ends_with(".py")
                || token.ends_with(".go")
                || token.ends_with(".swift")
                || token.ends_with(".java")
        })
        .collect()
}

fn file_allowed(file: &str, allowlist: &[String]) -> bool {
    allowlist.iter().any(|allowed| {
        allowed == file
            || allowed.ends_with(&format!("/{file}"))
            || file.ends_with(&format!("/{allowed}"))
    })
}

/// The deterministic verifier. **No LLM, no network, no clock.**
///
/// A step survives only when all three hold:
///
/// 1. its `action` is non-empty;
/// 2. every path-shaped token in the action *and* every entry in `files` is
///    in the allowlist — a step cannot introduce a file the evidence never
///    named;
/// 3. its `citation` traces (verbatim corpus substring, or a stored receipt
///    oid).
///
/// A step that fails any of these is **dropped**, and counted in
/// [`VerifiedPlan::dropped`]. It is never rewritten, hedged, or downgraded
/// to a "possible" step — a claim that does not trace has no honest weaker
/// form.
pub fn verify_plan(raw: &RawPlan, evidence: &PlanEvidence) -> VerifiedPlan {
    let mut steps: Vec<PlanStep> = Vec::new();
    let mut dropped = 0usize;

    for step in raw.steps.iter().take(MAX_PLAN_STEPS * 2) {
        let action = step.action.trim();
        if action.is_empty() {
            dropped += 1;
            continue;
        }
        if !citation_traces(&step.citation, evidence) {
            dropped += 1;
            continue;
        }
        let declared_ok = step
            .files
            .iter()
            .all(|file| file_allowed(file.trim(), &evidence.allowlist));
        let prose_ok = path_like_tokens(action)
            .iter()
            .all(|token| file_allowed(token, &evidence.allowlist));
        if !declared_ok || !prose_ok {
            dropped += 1;
            continue;
        }
        if steps.len() >= MAX_PLAN_STEPS {
            dropped += 1;
            continue;
        }
        steps.push(PlanStep {
            action: action.to_string(),
            files: step
                .files
                .iter()
                .map(|file| file.trim().to_string())
                .filter(|file| !file.is_empty())
                .collect(),
            citation: step.citation.trim().to_string(),
        });
    }

    let context = if citation_traces(&raw.citation, evidence) {
        raw.context.trim().to_string()
    } else {
        String::new()
    };
    let acceptance = {
        let acceptance = raw.acceptance.trim();
        let traces = citation_traces(&raw.citation, evidence)
            && path_like_tokens(acceptance)
                .iter()
                .all(|token| file_allowed(token, &evidence.allowlist));
        (!acceptance.is_empty() && traces).then(|| acceptance.to_string())
    };

    let mut files: BTreeSet<String> = BTreeSet::new();
    for step in &steps {
        files.extend(step.files.iter().cloned());
    }

    VerifiedPlan {
        context,
        steps,
        files: files.into_iter().take(MAX_PLAN_FILES).collect(),
        acceptance,
        dropped,
    }
}

// ─── structured plan: propose through the existing machinery ──────────────

const PLAN_RULES: &str = "You are drafting an ordered work plan for one unfinished item in a \
developer's private journal.\n\
\n\
Rules:\n\
- Return ONLY a JSON object, no markdown fence, no prose before or after.\n\
- Shape: {\"context\": <one sentence of situation>, \"citation\": <a VERBATIM substring copied \
exactly from the EVIDENCE text below, or one of the RECEIPTS>, \"steps\": [ up to 6 of \
{\"action\": <one imperative sentence>, \"files\": [<zero or more paths, ONLY from the FILES \
list below>], \"citation\": <a VERBATIM substring of EVIDENCE, or one of the RECEIPTS>} ], \
\"acceptance\": <one sentence naming how to check the work is done>}.\n\
- Every citation MUST be an exact, contiguous substring of the EVIDENCE text, or an exact \
RECEIPTS entry. Do not paraphrase, summarize, or abbreviate it.\n\
- files MUST be a subset of FILES. Never invent a path. Never name a path in an action that \
is not in FILES.\n\
- If you cannot ground a step, omit it. An empty steps array is a valid answer.\n";

fn build_plan_prompt(item: &DreamItem, evidence: &PlanEvidence) -> String {
    let record = serde_json::json!({
        "item": item.item,
        "kind": item.kind,
        "project": item.project,
        "left_open": iso_date(&item.origin_ts),
    });
    let prompt = format!(
        "{PLAN_RULES}\nITEM:\n{}\n\nEVIDENCE:\n{}\n\nFILES:\n{}\n\nRECEIPTS:\n{}\n",
        serde_json::to_string(&record).unwrap_or_default(),
        evidence.corpus,
        serde_json::to_string(&evidence.allowlist).unwrap_or_default(),
        serde_json::to_string(&evidence.receipts).unwrap_or_default(),
    );
    if prompt.len() > PLAN_PROMPT_CAP_BYTES {
        let mut end = PLAN_PROMPT_CAP_BYTES;
        while end > 0 && !prompt.is_char_boundary(end) {
            end -= 1;
        }
        prompt[..end].to_string()
    } else {
        prompt
    }
}

/// Same fence-stripping idiom as `dream::threads::strip_json_fences`.
fn strip_fences(text: &str) -> &str {
    let text = text.trim();
    let text = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```JSON"))
        .or_else(|| text.strip_prefix("```"))
        .unwrap_or(text)
        .trim();
    text.strip_suffix("```").unwrap_or(text).trim()
}

fn parse_plan(text: &str) -> RawPlan {
    serde_json::from_str::<RawPlan>(strip_fences(text)).unwrap_or_default()
}

/// SHA-256 over the plan's evidence inputs plus [`PLAN_PROMPT_VERSION`] and
/// the configured target model — computable *before* any invocation, so a
/// convergence check never itself costs a call. Same construction as
/// `dream::threads::episode_hash`.
pub fn plan_hash(item: &DreamItem, evidence: &PlanEvidence, model: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(item.id.as_bytes());
    hasher.update([0u8]);
    hasher.update(item.item.as_bytes());
    hasher.update([0u8]);
    hasher.update(evidence.corpus.as_bytes());
    hasher.update([0u8]);
    for file in &evidence.allowlist {
        hasher.update(file.as_bytes());
        hasher.update([0u8]);
    }
    for receipt in &evidence.receipts {
        hasher.update(receipt.as_bytes());
        hasher.update([0u8]);
    }
    hasher.update(PLAN_PROMPT_VERSION.to_le_bytes());
    hasher.update(model.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

/// Draft one plan through [`crate::dream::threads`]'s actor abstraction,
/// model chain and usage accounting, then verify it deterministically.
///
/// `None` means the actor never produced a usable reply — nothing is cached
/// and the caller retries next pass, exactly like `verify_reply`. `Some`
/// carries the verified plan (possibly empty, which the caller stores as a
/// sentinel) and the model that actually served it.
///
/// Usage rows are tagged `ref_id = plan_hash`, which is what makes the
/// dream's spend figure evidence rather than a timestamp guess.
pub(crate) fn propose_plan_with(
    actor: &dyn NightActor,
    chain: &[Option<String>],
    storage: &Storage,
    item: &DreamItem,
    evidence: &PlanEvidence,
    hash: &str,
) -> Option<(VerifiedPlan, String)> {
    let prompt = build_plan_prompt(item, evidence);
    let result = threads::invoke_chain(actor, chain, &prompt);
    threads::record_attempts(storage, &result.attempts, "dream_plan", Some(hash));
    let text = result.text?;
    let raw = parse_plan(&text);
    Some((verify_plan(&raw, evidence), result.model_used))
}

// ─── structured plan: storage ─────────────────────────────────────────────

/// A plan as stored. `plan_hash` doubles as the spend `ref_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPlan {
    pub plan_hash: String,
    pub item_id: String,
    pub context: String,
    pub steps: Vec<PlanStep>,
    pub files: Vec<String>,
    pub acceptance: Option<String>,
    pub dropped: usize,
    pub model: String,
    pub created_at: String,
}

/// `true` when a plan (real or sentinel) already exists for `hash` — the
/// convergence short-circuit that keeps a frozen corpus at zero spend.
pub fn plan_converged(conn: &Connection, hash: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM dream_plans WHERE plan_hash = ?1",
        params![hash],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// `INSERT OR IGNORE` — append-only by convention, never updated. An empty
/// `plan` is stored as the convergence sentinel.
pub fn store_plan(
    conn: &Connection,
    hash: &str,
    item: &DreamItem,
    plan: &VerifiedPlan,
    model: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO dream_plans
            (plan_hash, item_id, project, session_id, context, steps_json, files_json,
             acceptance, dropped, model)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            hash,
            item.id,
            item.project,
            item.origin_session,
            plan.context,
            serde_json::to_string(&plan.steps)?,
            serde_json::to_string(&plan.files)?,
            plan.acceptance,
            plan.dropped as i64,
            model,
        ],
    )?;
    Ok(())
}

/// Newest stored plan for `item_id`, or `None`. Sentinel rows (no steps) are
/// returned as-is: the renderer distinguishes "converged to nothing" from
/// "never run" by whether `steps` is empty, and says so either way.
pub fn load_plan(conn: &Connection, item_id: &str) -> Result<Option<StoredPlan>> {
    let mut stmt = conn.prepare(
        "SELECT plan_hash, item_id, context, steps_json, files_json, acceptance, dropped,
                model, created_at
         FROM dream_plans WHERE item_id = ?1 ORDER BY id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query(params![item_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let steps_json: String = row.get(3)?;
    let files_json: String = row.get(4)?;
    Ok(Some(StoredPlan {
        plan_hash: row.get(0)?,
        item_id: row.get(1)?,
        context: row.get(2)?,
        steps: serde_json::from_str(&steps_json).unwrap_or_default(),
        files: serde_json::from_str(&files_json).unwrap_or_default(),
        acceptance: row.get(5)?,
        dropped: row.get::<_, i64>(6)?.max(0) as usize,
        model: row.get(7)?,
        created_at: row.get(8)?,
    }))
}

// ─── structured plan: the pass ────────────────────────────────────────────

/// Summary of one [`run_plan_pass`] — daemon logging only. Every field is a
/// count of something that happened, never an estimate.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PlanPassStats {
    pub skipped: bool,
    pub candidates: usize,
    pub plans_stored: usize,
    pub sentinels_stored: usize,
    pub steps_dropped: usize,
    pub errors: usize,
    /// Items never attempted because the pass budget was already spent.
    pub budget_queued: usize,
}

/// Default cap on dream items considered per pass.
const DEFAULT_PLAN_CAP: usize = 20;

fn plan_cap() -> usize {
    std::env::var("CSR_DREAM_PLANS_CAP")
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|&value| value > 0)
        .unwrap_or(DEFAULT_PLAN_CAP)
}

/// Draft and verify plans for the newest dream items that do not have one.
///
/// Gated by the same switches as night-pass thread extraction
/// (`threads::threads_disabled` — `CSR_NO_AI_NARRATIVES`, `CSR_NO_DREAMING`,
/// and the `CSR_DREAM_THREADS=1` opt-in), because this is the same class of
/// new LLM spend layered on the deterministic dream pass and must default
/// OFF for the same reason. Fail-open at every level: a per-item error is
/// counted and the pass continues; the pass never panics and never returns
/// an `Err`.
///
/// **Nothing on the request path calls this.** The detail route only reads
/// [`load_plan`], so a page view can never trigger an invocation.
pub fn run_plan_pass(storage: &Storage) -> PlanPassStats {
    let budget = crate::dream::policy::Budget::for_tier(crate::dream::policy::effort_tier());
    run_plan_pass_with_budget(storage, &budget)
}

/// [`run_plan_pass`] against an explicit pass budget (locked decision 8).
/// The budget is shared with night-pass thread extraction, so the cap is a
/// per-pass total across both producers. Items are consumed newest-first
/// (`load_dream_items`'s own order); once the budget is gone the remainder is
/// counted as queued and nothing is cached for it, so the next pass retries.
pub fn run_plan_pass_with_budget(
    storage: &Storage,
    budget: &crate::dream::policy::Budget,
) -> PlanPassStats {
    let mut stats = PlanPassStats::default();
    if threads::threads_disabled() {
        stats.skipped = true;
        return stats;
    }

    let items = match storage.with_connection(crate::storage::dream_items::load_dream_items) {
        Ok(items) => items,
        Err(error) => {
            tracing::warn!(%error, "dream-plan candidate load failed (non-fatal)");
            return stats;
        }
    };

    let chain = threads::thread_model_candidates();
    let target_model = threads::primary_thread_model();
    let process_actor = threads::ProcessActor;
    let actor = crate::dream::policy::BudgetedActor::new(&process_actor, budget);

    for item in items.iter().take(plan_cap()) {
        stats.candidates += 1;
        if budget.exhausted() {
            budget.note_queued();
            stats.budget_queued += 1;
            continue;
        }
        match plan_one_item(storage, &actor, &chain, &target_model, item) {
            Ok(Some(plan)) => {
                if plan.is_empty() {
                    stats.sentinels_stored += 1;
                } else {
                    stats.plans_stored += 1;
                }
                stats.steps_dropped += plan.dropped;
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    %error,
                    item = %item.id,
                    "dream-plan proposal failed for one item (non-fatal, continuing)"
                );
                stats.errors += 1;
            }
        }
    }

    tracing::info!(
        candidates = stats.candidates,
        plans_stored = stats.plans_stored,
        sentinels_stored = stats.sentinels_stored,
        steps_dropped = stats.steps_dropped,
        errors = stats.errors,
        budget_queued = stats.budget_queued,
        budget_used = budget.used(),
        budget_cap = budget.cap(),
        "dream-plan pass complete"
    );
    stats
}

/// `Ok(None)` means either already converged (no spend) or the actor never
/// replied (nothing cached, retried next pass).
fn plan_one_item(
    storage: &Storage,
    actor: &dyn NightActor,
    chain: &[Option<String>],
    target_model: &str,
    item: &DreamItem,
) -> Result<Option<VerifiedPlan>> {
    let (episode, threads_for_item) = storage.with_connection(|conn| {
        Ok((
            episode_facts(conn, &item.origin_session)?,
            threads_for_session(conn, &item.origin_session)?,
        ))
    })?;
    let evidence = PlanEvidence::build(item, &episode, &threads_for_item);
    let hash = plan_hash(item, &evidence, target_model);

    if storage.with_connection(|conn| plan_converged(conn, &hash))? {
        return Ok(None);
    }

    let Some((plan, model_used)) = propose_plan_with(actor, chain, storage, item, &evidence, &hash)
    else {
        return Ok(None);
    };
    storage.with_connection(|conn| store_plan(conn, &hash, item, &plan, &model_used))?;
    Ok(Some(plan))
}

/// Every stored night-pass thread for one session, strongest receipt tier
/// first, then newest.
pub fn threads_for_session(conn: &Connection, session_id: &str) -> Result<Vec<DreamThread>> {
    let mut all = threads::load_dream_threads(conn)?;
    all.retain(|thread| thread.session_id == session_id);
    all.sort_by(|a, b| {
        a.receipt_tier
            .cmp(&b.receipt_tier)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    Ok(all)
}

/// The v2 episode fields the brief and copy block quote, for one session.
/// A missing/malformed row yields the empty default — every clause built
/// from it then drops on its own.
pub fn episode_facts(conn: &Connection, session_id: &str) -> Result<EpisodeFacts> {
    #[derive(Debug, Default, Deserialize)]
    #[serde(default)]
    struct Record {
        request: String,
        completed: String,
        outcome: String,
        files_modified: Vec<String>,
        investigated: Vec<String>,
    }

    let mut stmt = conn.prepare(
        "SELECT content FROM reflections
         WHERE json_valid(content)
           AND json_extract(content, '$.schema') = 'v2'
           AND json_extract(content, '$.session_id') = ?1
         ORDER BY rowid DESC LIMIT 1",
    )?;
    let mut rows = stmt.query(params![session_id])?;
    let Some(row) = rows.next()? else {
        return Ok(EpisodeFacts::default());
    };
    let content: String = row.get(0)?;
    let Ok(record) = serde_json::from_str::<Record>(&content) else {
        return Ok(EpisodeFacts::default());
    };
    let nonblank = |value: String| (!value.trim().is_empty()).then_some(value);
    let mut files: Vec<String> = record
        .files_modified
        .into_iter()
        .chain(record.investigated)
        .filter(|file| !file.trim().is_empty())
        .collect();
    files.sort();
    files.dedup();
    Ok(EpisodeFacts {
        request: nonblank(record.request),
        completed: nonblank(record.completed),
        outcome: nonblank(record.outcome),
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::dream_items::{DreamEvidence, DreamItemGrade};

    fn item() -> DreamItem {
        DreamItem {
            id: "id00".to_string(),
            project: "csr".to_string(),
            item: "finish the release gate".to_string(),
            kind: "todo".to_string(),
            origin_session: "sess-1".to_string(),
            origin_ts: "2026-08-01T09:00:00Z".to_string(),
            grade: DreamItemGrade::ItemGrade,
            evidence: vec![DreamEvidence {
                symbol: Some("run_report".to_string()),
                file: "csr-engine/src/dream/report.rs".to_string(),
                verdict: "anchor_obsolete".to_string(),
                receipt_oid: Some("abcdef1234567890".to_string()),
                witnessed_at: "2026-08-09T12:00:00Z".to_string(),
            }],
        }
    }

    fn episode() -> EpisodeFacts {
        EpisodeFacts {
            request: Some("close the v10.1 release gate".to_string()),
            completed: Some("wrote the gate but never ran it".to_string()),
            outcome: Some("partial".to_string()),
            files: vec!["csr-engine/src/dream/report.rs".to_string()],
        }
    }

    fn thread() -> DreamThread {
        DreamThread {
            id: 1,
            episode_hash: "hash-a".to_string(),
            session_id: "sess-1".to_string(),
            project: "csr".to_string(),
            thread: "the release gate was written but never executed".to_string(),
            evidence_quote: "wrote the gate but never ran it".to_string(),
            files: vec!["csr-engine/src/dream/report.rs".to_string()],
            receipt_tier: ReceiptTier::Verdict,
            receipts: vec![Receipt::Verdict {
                symbol: Some("run_report".to_string()),
                verdict: "anchor_obsolete".to_string(),
                receipt_oid: Some("abcdef1234567890".to_string()),
                witnessed_at: "2026-08-09T12:00:00Z".to_string(),
            }],
            model: "sonnet-5".to_string(),
            created_at: "2026-08-09T13:00:00Z".to_string(),
        }
    }

    // ---- copy block ------------------------------------------------------

    #[test]
    fn copy_block_carries_the_night_pass_quote_verbatim_with_its_receipt() {
        let item = item();
        let threads = vec![thread()];
        let block = build_copy_block(&item, &episode(), &threads, None);

        assert!(block.has_night_pass);
        assert!(
            block.markdown.contains("> wrote the gate but never ran it"),
            "the evidence quote must appear verbatim, not paraphrased:\n{}",
            block.markdown
        );
        assert!(
            block.markdown.contains("⌗abcdef12"),
            "the quote must travel with its receipt:\n{}",
            block.markdown
        );
        assert!(block.markdown.contains("Receipt tier: verdict-backed"));
        assert!(
            !block.markdown.contains(NO_NIGHT_PASS),
            "the no-thread sentence must not appear when a thread exists"
        );
    }

    #[test]
    fn copy_block_without_a_night_pass_says_so_instead_of_leaving_a_gap() {
        let block = build_copy_block(&item(), &episode(), &[], None);

        assert!(!block.has_night_pass);
        assert!(
            block.markdown.contains("### What the night pass concluded"),
            "the heading must still be present — a missing section reads as \
             'not rendered', which is a different claim from 'no evidence'"
        );
        assert!(
            block.markdown.contains(NO_NIGHT_PASS),
            "the gap must be stated in words:\n{}",
            block.markdown
        );
        assert!(
            !block.markdown.contains('>'),
            "no blockquote may appear when there is no quote to attribute"
        );
    }

    #[test]
    fn copy_block_states_missing_episode_fields_rather_than_inventing_them() {
        let block = build_copy_block(&item(), &EpisodeFacts::default(), &[], None);
        assert!(block
            .markdown
            .contains("the origin session stored no request text"));
        assert!(block
            .markdown
            .contains("the origin session stored no completion summary"));
        assert!(block.markdown.contains(NO_PLAN));
    }

    #[test]
    fn copy_block_renders_a_stored_plan_with_its_citations_inline() {
        let plan = StoredPlan {
            plan_hash: "h".into(),
            item_id: "id00".into(),
            context: "the gate exists but was never run".into(),
            steps: vec![PlanStep {
                action: "run the gate".into(),
                files: vec!["csr-engine/src/dream/report.rs".into()],
                citation: "wrote the gate but never ran it".into(),
            }],
            files: vec!["csr-engine/src/dream/report.rs".into()],
            acceptance: Some("the gate exits zero".into()),
            dropped: 2,
            model: "sonnet-5".into(),
            created_at: "2026-08-10T00:00:00Z".into(),
        };
        let block = build_copy_block(&item(), &episode(), &[], Some(&plan));
        assert!(block.has_plan);
        assert!(block.markdown.contains(PLAN_LABEL));
        assert!(block.markdown.contains("1. run the gate"));
        assert!(block
            .markdown
            .contains("traces to: wrote the gate but never ran it"));
        assert!(
            block.markdown.contains("2 drafted step(s) were dropped"),
            "the dropped count is reported; the dropped text never is"
        );
    }

    // ---- brief -----------------------------------------------------------

    #[test]
    fn brief_truncation_marker_reports_the_measured_total() {
        let mut threads = Vec::new();
        for index in 0..4 {
            let mut thread = thread();
            thread.thread = format!("thread {index}");
            thread.evidence_quote = format!("quote {index}");
            threads.push(thread);
        }
        let brief = build_brief(&item(), &episode(), &threads);

        assert_eq!(brief.shown, BRIEF_MAX_LINES);
        assert!(brief.total > BRIEF_MAX_LINES);
        assert_eq!(
            brief.truncation,
            Some(format!("showing {} of {}", brief.shown, brief.total))
        );
        assert_eq!(brief.lines.len(), BRIEF_MAX_LINES);
        assert!(!brief.empty);
    }

    #[test]
    fn brief_without_truncation_carries_no_marker() {
        let brief = build_brief(&item(), &EpisodeFacts::default(), &[]);
        assert_eq!(brief.truncation, None, "nothing cut, nothing claimed");
        assert_eq!(brief.shown, brief.total);
    }

    #[test]
    fn brief_with_no_evidence_at_all_is_explicitly_empty() {
        let mut bare = item();
        bare.evidence.clear();
        let brief = build_brief(&bare, &EpisodeFacts::default(), &[]);
        assert!(brief.empty);
        assert_eq!(brief.total, 0);
        assert_eq!(brief.truncation, None);
    }

    #[test]
    fn a_long_brief_line_is_cut_and_the_cut_is_counted() {
        let mut long = episode();
        long.request = Some("x".repeat(BRIEF_MAX_LINE_CHARS * 2));
        let brief = build_brief(&item(), &long, &[]);
        assert_eq!(brief.truncated_lines, 1);
        assert!(brief.lines[0].truncated);
        assert!(brief.lines[0].text.ends_with('…'));
        assert_eq!(brief.lines[0].text.chars().count(), BRIEF_MAX_LINE_CHARS);
    }

    // ---- plan verifier ---------------------------------------------------

    fn evidence() -> PlanEvidence {
        PlanEvidence::build(&item(), &episode(), &[thread()])
    }

    fn traceable_step() -> RawPlanStep {
        RawPlanStep {
            action: "run the gate end to end".into(),
            files: vec!["csr-engine/src/dream/report.rs".into()],
            citation: "wrote the gate but never ran it".into(),
        }
    }

    #[test]
    fn verifier_keeps_a_step_whose_citation_is_verbatim() {
        let raw = RawPlan {
            context: "the gate was written but never executed".into(),
            steps: vec![traceable_step()],
            acceptance: "the gate exits zero".into(),
            citation: "close the v10.1 release gate".into(),
        };
        let plan = verify_plan(&raw, &evidence());
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.dropped, 0);
        assert_eq!(plan.steps[0].citation, "wrote the gate but never ran it");
        assert_eq!(plan.acceptance.as_deref(), Some("the gate exits zero"));
    }

    #[test]
    fn verifier_drops_a_step_whose_claim_does_not_trace() {
        let raw = RawPlan {
            context: String::new(),
            steps: vec![
                traceable_step(),
                RawPlanStep {
                    // Plausible, well-formed, and grounded in nothing.
                    action: "revert the regression the maintainer introduced last week".into(),
                    files: vec![],
                    citation: "the maintainer introduced a regression last week".into(),
                },
            ],
            acceptance: String::new(),
            citation: String::new(),
        };
        let plan = verify_plan(&raw, &evidence());

        assert_eq!(plan.steps.len(), 1, "only the traceable step survives");
        assert_eq!(plan.dropped, 1);
        let rendered = format!("{plan:?}");
        assert!(
            !rendered.contains("regression"),
            "a dropped line must be dropped, not softened into the output: {rendered}"
        );
    }

    #[test]
    fn verifier_drops_a_step_naming_a_file_outside_the_allowlist() {
        let raw = RawPlan {
            context: String::new(),
            steps: vec![RawPlanStep {
                action: "run the gate end to end".into(),
                files: vec!["csr-engine/src/secrets.rs".into()],
                citation: "wrote the gate but never ran it".into(),
            }],
            acceptance: String::new(),
            citation: String::new(),
        };
        let plan = verify_plan(&raw, &evidence());
        assert!(plan.steps.is_empty());
        assert_eq!(plan.dropped, 1);
    }

    #[test]
    fn verifier_drops_a_step_that_smuggles_a_path_into_its_prose() {
        let raw = RawPlan {
            context: String::new(),
            steps: vec![RawPlanStep {
                action: "also patch csr-engine/src/elsewhere.rs while you are here".into(),
                files: vec![],
                citation: "wrote the gate but never ran it".into(),
            }],
            acceptance: String::new(),
            citation: String::new(),
        };
        let plan = verify_plan(&raw, &evidence());
        assert!(
            plan.steps.is_empty(),
            "the files array is not the only place a path can appear"
        );
        assert_eq!(plan.dropped, 1);
    }

    #[test]
    fn verifier_accepts_a_receipt_oid_as_a_citation() {
        let raw = RawPlan {
            context: String::new(),
            steps: vec![RawPlanStep {
                action: "re-anchor the obsolete symbol".into(),
                files: vec![],
                citation: "abcdef1234567890".into(),
            }],
            acceptance: String::new(),
            citation: String::new(),
        };
        let plan = verify_plan(&raw, &evidence());
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.dropped, 0);
    }

    #[test]
    fn verifier_rejects_a_citation_too_short_to_be_evidence() {
        let raw = RawPlan {
            context: String::new(),
            steps: vec![RawPlanStep {
                action: "do the thing".into(),
                files: vec![],
                citation: "the".into(),
            }],
            acceptance: String::new(),
            citation: String::new(),
        };
        assert_eq!(verify_plan(&raw, &evidence()).dropped, 1);
    }

    #[test]
    fn verifier_drops_the_context_and_acceptance_when_their_citation_fails() {
        let raw = RawPlan {
            context: "a confident summary of nothing".into(),
            steps: vec![traceable_step()],
            acceptance: "everything looks fine".into(),
            citation: "a quote that appears nowhere in the evidence".into(),
        };
        let plan = verify_plan(&raw, &evidence());
        assert_eq!(plan.context, "", "unverified context is dropped whole");
        assert_eq!(plan.acceptance, None);
        assert_eq!(plan.steps.len(), 1, "the steps are judged independently");
    }

    // ---- propose-verify through the existing machinery -------------------

    #[test]
    fn propose_plan_runs_through_the_night_actor_and_records_tagged_usage() {
        use crate::dream::threads::ActorAttempt;
        use crate::narrative::ParsedNarrative;

        let storage = Storage::open_memory().expect("storage");
        let item = item();
        let evidence = evidence();
        let hash = plan_hash(&item, &evidence, "sonnet-5");

        let actor = |_model: Option<&str>, _prompt: &str| {
            ActorAttempt::Parsed(ParsedNarrative {
                text: r#"{"context":"the gate was written but never executed",
                          "citation":"close the v10.1 release gate",
                          "steps":[{"action":"run the gate end to end",
                                    "files":["csr-engine/src/dream/report.rs"],
                                    "citation":"wrote the gate but never ran it"},
                                   {"action":"undo the sabotage",
                                    "files":[],
                                    "citation":"someone sabotaged the build"}],
                          "acceptance":"the gate exits zero"}"#
                    .to_string(),
                model: "sonnet-5".into(),
                input_tokens: 1_200,
                output_tokens: 340,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            })
        };

        let chain = vec![Some("sonnet-5".to_string())];
        let (plan, model) =
            propose_plan_with(&actor, &chain, &storage, &item, &evidence, &hash).expect("reply");

        assert_eq!(model, "sonnet-5");
        assert_eq!(plan.steps.len(), 1, "the ungrounded step is dropped");
        assert_eq!(plan.dropped, 1);

        // The usage row is tagged with the plan hash — that tag is the whole
        // basis of the per-dream spend figure.
        let spend = storage
            .with_connection(|conn| load_spend(conn, std::slice::from_ref(&hash)))
            .expect("spend query")
            .expect("usage was recorded");
        assert_eq!(spend.calls, 1);
        assert_eq!(spend.input_tokens, 1_200);
        assert_eq!(spend.output_tokens, 340);
    }

    #[test]
    fn a_plan_round_trips_through_storage_and_converges() {
        let storage = Storage::open_memory().expect("storage");
        let item = item();
        let plan = VerifiedPlan {
            context: "context".into(),
            steps: vec![PlanStep {
                action: "run the gate".into(),
                files: vec!["csr-engine/src/dream/report.rs".into()],
                citation: "wrote the gate but never ran it".into(),
            }],
            files: vec!["csr-engine/src/dream/report.rs".into()],
            acceptance: Some("the gate exits zero".into()),
            dropped: 1,
        };
        storage
            .with_connection(|conn| {
                assert!(!plan_converged(conn, "hash-1")?);
                store_plan(conn, "hash-1", &item, &plan, "sonnet-5")?;
                assert!(plan_converged(conn, "hash-1")?);
                let loaded = load_plan(conn, &item.id)?.expect("stored plan");
                assert_eq!(loaded.steps, plan.steps);
                assert_eq!(loaded.dropped, 1);
                assert_eq!(loaded.plan_hash, "hash-1");
                Ok(())
            })
            .expect("round trip");
    }

    #[test]
    fn an_empty_plan_is_stored_as_a_convergence_sentinel() {
        let storage = Storage::open_memory().expect("storage");
        let item = item();
        let empty = VerifiedPlan {
            context: String::new(),
            steps: Vec::new(),
            files: Vec::new(),
            acceptance: None,
            dropped: 3,
        };
        storage
            .with_connection(|conn| {
                store_plan(conn, "hash-empty", &item, &empty, "sonnet-5")?;
                assert!(
                    plan_converged(conn, "hash-empty")?,
                    "a run that kept nothing must still converge, or it re-spends forever"
                );
                let loaded = load_plan(conn, &item.id)?.expect("sentinel row");
                assert!(loaded.steps.is_empty());
                assert_eq!(loaded.dropped, 3);
                Ok(())
            })
            .expect("sentinel");
    }

    #[test]
    fn plan_hash_changes_with_the_evidence_and_the_model() {
        let item = item();
        let base = evidence();
        let a = plan_hash(&item, &base, "sonnet-5");
        assert_eq!(a, plan_hash(&item, &base, "sonnet-5"), "stable");
        assert_ne!(
            a,
            plan_hash(&item, &base, "haiku-4-5"),
            "model is folded in"
        );

        let mut moved = base.clone();
        moved.corpus.push_str("a new stored row\n");
        assert_ne!(
            a,
            plan_hash(&item, &moved, "sonnet-5"),
            "evidence is folded in"
        );
    }

    // ---- spend -----------------------------------------------------------

    #[test]
    fn spend_renders_only_when_usage_was_recorded() {
        let storage = Storage::open_memory().expect("storage");

        // Nothing recorded → None. NOT a zeroed DreamSpend: a dream with no
        // recorded usage was unmeasured, not free.
        let none = storage
            .with_connection(|conn| load_spend(conn, &["hash-a".to_string()]))
            .expect("query");
        assert!(none.is_none(), "no rows must render nothing at all");

        storage
            .record_narrative_usage_for(
                &crate::storage::NarrativeUsageRow {
                    call_site: "dream_threads".into(),
                    model: "claude-sonnet-5".into(),
                    input_tokens: 1_000_000,
                    output_tokens: 1_000_000,
                    cache_read_tokens: 0,
                    cache_creation_tokens: 0,
                    duration_ms: 0,
                    success: true,
                },
                Some("hash-a"),
            )
            .expect("record");

        let spend = storage
            .with_connection(|conn| load_spend(conn, &["hash-a".to_string()]))
            .expect("query")
            .expect("recorded");
        assert_eq!(spend.calls, 1);
        assert_eq!(spend.input_tokens, 1_000_000);
        assert_eq!(spend.output_tokens, 1_000_000);
        // 1 MTok in at $3 + 1 MTok out at $15.
        assert_eq!(spend.cost_usd, Some(18.0));
        assert_eq!(spend.tokens_label(), "1,000,000 in · 1,000,000 out");
        assert!(spend.cost_label().starts_with("≈$18.0000 at list price"));
    }

    #[test]
    fn usage_recorded_without_a_ref_is_never_attributed_to_a_dream() {
        let storage = Storage::open_memory().expect("storage");
        storage
            .record_narrative_usage(&crate::storage::NarrativeUsageRow {
                call_site: "briefing".into(),
                model: "claude-sonnet-5".into(),
                input_tokens: 999,
                output_tokens: 999,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                duration_ms: 0,
                success: true,
            })
            .expect("record");
        let spend = storage
            .with_connection(|conn| load_spend(conn, &["hash-a".to_string()]))
            .expect("query");
        assert!(
            spend.is_none(),
            "an untagged row belongs to no dream and must not be borrowed by one"
        );
    }

    #[test]
    fn an_unpriced_model_withholds_the_cost_instead_of_understating_it() {
        let storage = Storage::open_memory().expect("storage");
        for model in ["claude-sonnet-5", "some-local-model"] {
            storage
                .record_narrative_usage_for(
                    &crate::storage::NarrativeUsageRow {
                        call_site: "dream_plan".into(),
                        model: model.into(),
                        input_tokens: 1_000,
                        output_tokens: 100,
                        cache_read_tokens: 0,
                        cache_creation_tokens: 0,
                        duration_ms: 0,
                        success: true,
                    },
                    Some("hash-b"),
                )
                .expect("record");
        }
        let spend = storage
            .with_connection(|conn| load_spend(conn, &["hash-b".to_string()]))
            .expect("query")
            .expect("recorded");
        assert_eq!(spend.calls, 2);
        assert_eq!(
            spend.cost_usd, None,
            "a partial total would read as the whole cost"
        );
        assert_eq!(spend.unpriced_models, vec!["some-local-model".to_string()]);
        assert!(spend.cost_label().contains("cost unavailable"));
        assert!(spend.cost_label().contains("some-local-model"));
        // Tokens were still measured, so they are still reported.
        assert_eq!(spend.tokens_label(), "2,000 in · 200 out");
    }

    #[test]
    fn cache_tokens_are_priced_at_their_published_multipliers() {
        let rows = vec![NarrativeUsageByModel {
            model: "claude-haiku-4-5".into(),
            calls: 1,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 1_000_000,
            cache_creation_tokens: 1_000_000,
        }];
        let spend = DreamSpend::from_rows(&rows).expect("rows");
        // Haiku input is $1/MTok: 1M read at 0.1x + 1M write at 1.25x.
        assert_eq!(spend.cost_usd, Some(1.35));
    }

    #[test]
    fn model_price_matches_bare_aliases_and_full_ids_alike() {
        assert_eq!(model_price("sonnet-5"), Some((3.0, 15.0)));
        assert_eq!(model_price("claude-sonnet-5"), Some((3.0, 15.0)));
        assert_eq!(model_price("claude-opus-4-8"), Some((5.0, 25.0)));
        assert_eq!(model_price("claude-haiku-4-5"), Some((1.0, 5.0)));
        assert_eq!(model_price("claude-fable-5"), Some((10.0, 50.0)));
        assert_eq!(model_price("unknown"), None);
        assert_eq!(
            model_price("default"),
            None,
            "the chain's placeholder label must not be priced as a model"
        );
    }

    #[test]
    fn spend_refs_come_from_stored_hashes_only() {
        let threads = vec![thread()];
        let plan = StoredPlan {
            plan_hash: "hash-plan".into(),
            item_id: "id00".into(),
            context: String::new(),
            steps: Vec::new(),
            files: Vec::new(),
            acceptance: None,
            dropped: 0,
            model: "sonnet-5".into(),
            created_at: String::new(),
        };
        assert_eq!(
            spend_refs(&threads, Some(&plan)),
            vec!["hash-a".to_string(), "hash-plan".to_string()]
        );
        assert_eq!(spend_refs(&[], None), Vec::<String>::new());
    }

    // ---- feeds -----------------------------------------------------------

    #[test]
    fn episode_facts_degrade_to_empty_rather_than_failing() {
        let storage = Storage::open_memory().expect("storage");
        let facts = storage
            .with_connection(|conn| episode_facts(conn, "missing"))
            .expect("query");
        assert_eq!(facts, EpisodeFacts::default());
    }

    #[test]
    fn threads_for_session_orders_verdict_backed_first() {
        let storage = Storage::open_memory().expect("storage");
        storage
            .with_connection(|conn| {
                for (hash, tier, thread) in [
                    ("h1", "unverified", "weak"),
                    ("h2", "verdict", "strong"),
                    ("h3", "witnessed", "middle"),
                ] {
                    conn.execute(
                        "INSERT INTO dream_threads
                           (episode_hash, session_id, project, thread, evidence_quote,
                            files_json, receipt_tier, receipts_json, model)
                         VALUES (?1, 'sess-1', 'csr', ?2, 'q', '[]', ?3, '[]', 'sonnet-5')",
                        params![hash, thread, tier],
                    )?;
                }
                Ok(())
            })
            .expect("seed");
        let threads = storage
            .with_connection(|conn| threads_for_session(conn, "sess-1"))
            .expect("query");
        let order: Vec<&str> = threads.iter().map(|t| t.thread.as_str()).collect();
        assert_eq!(order, vec!["strong", "middle", "weak"]);
        assert_eq!(threads[0].episode_hash, "h2");
    }

    /// One item-grade dream item: a v2 episode whose open todo names a symbol
    /// that a receipt-bearing verdict covers.
    fn seed_one_dream_item(storage: &Storage) {
        use crate::storage::witness_ledger::{self, WitnessLedgerRow};
        use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO reflections (id, content, tags, timestamp)
                     VALUES ('ep-1', ?1, '[]', '2026-08-11T00:00:00Z')",
                    params![
                        r#"{"schema":"v2","session_id":"sess-1","project":"proj","timestamp":"2026-08-11T00:00:00Z","todos":[{"content":"Fix `parse_config` before ship","status":"pending"}]}"#
                    ],
                )?;
                witness_ledger::insert_witness(
                    conn,
                    &WitnessLedgerRow {
                        project: "proj".into(),
                        file: "/repo/src/config.rs".into(),
                        symbol: Some("parse_config".into()),
                        stamp: "b3:1".into(),
                        tier: "committed".into(),
                        at_oid: Some("aaa".into()),
                        source_kind: "backfill".into(),
                        source_id: Some("aaa".into()),
                        ..Default::default()
                    },
                )?;
                let witness_id: i64 = conn.query_row(
                    "SELECT id FROM witness_ledger WHERE stamp = 'b3:1'",
                    [],
                    |row| row.get(0),
                )?;
                witness_verdicts::insert_verdict_if_changed(
                    conn,
                    &WitnessVerdictRow {
                        witness_id,
                        verdict: VerdictKind::SupersededBy,
                        successor_witness_id: None,
                        receipt_oid: Some("aaa".into()),
                        observed_head_oid: "head".into(),
                    },
                )?;
                Ok(())
            })
            .expect("seed");
    }

    #[test]
    fn an_exhausted_budget_queues_every_remaining_item_without_invoking_anything() {
        // The plan pass shares one budget with night-pass extraction, so an
        // extraction that spent the cap must leave the plan pass with nothing
        // to spend — and a queued remainder rather than a silent skip.
        // (Nothing is invoked here: the exhausted check precedes the actor,
        // and the actor is wrapped by the budget besides.)
        let storage = Storage::open_memory().expect("storage");
        let previous = std::env::var("CSR_DREAM_THREADS").ok();
        std::env::set_var("CSR_DREAM_THREADS", "1");
        seed_one_dream_item(&storage);
        let budget = crate::dream::policy::Budget::new(0);
        let stats = run_plan_pass_with_budget(&storage, &budget);
        match previous {
            Some(value) => std::env::set_var("CSR_DREAM_THREADS", value),
            None => std::env::remove_var("CSR_DREAM_THREADS"),
        }
        assert!(!stats.skipped, "the pass ran; it simply had no budget");
        assert_eq!(stats.candidates, stats.budget_queued);
        assert!(stats.candidates > 0, "the fixture must produce a candidate");
        assert_eq!(stats.plans_stored, 0);
        assert_eq!(budget.used(), 0);
        assert_eq!(budget.queued(), stats.budget_queued);
    }

    #[test]
    fn the_plan_pass_is_off_unless_explicitly_opted_in() {
        // `run_plan_pass` is a new LLM spend surface layered on the
        // deterministic dream pass, so it inherits the same default-OFF gate.
        let storage = Storage::open_memory().expect("storage");
        let previous = std::env::var("CSR_DREAM_THREADS").ok();
        std::env::remove_var("CSR_DREAM_THREADS");
        let stats = run_plan_pass(&storage);
        assert!(stats.skipped);
        assert_eq!(stats.plans_stored, 0);
        if let Some(value) = previous {
            std::env::set_var("CSR_DREAM_THREADS", value);
        }
    }
}
