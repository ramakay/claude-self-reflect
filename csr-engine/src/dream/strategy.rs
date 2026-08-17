//! Strategy category (stage B of `csr-engine dreams`, slice one of
//! `.plans/dreams-product-shape.md`).
//!
//! Where [`crate::dream::cli`]'s unfinished-and-valuable category is zero-LLM
//! and deterministic, strategy is LLM-authored: one `claude -p` call per
//! project, gated behind a deterministic evidence bar so spend never happens
//! on thin evidence, and cached by content so a re-run of an unchanged corpus
//! costs nothing.
//!
//! # Pipeline, per project
//!
//! 1. [`build_project_evidence`] — the evidence bundle: this week's open
//!    items (title/kind/receipt, plus stored-plan "how" lines when a plan
//!    exists) and this week's night-pass thread sentences (with their
//!    receipts), both scoped to the project and the rolling 7-day window
//!    ([`crate::journal::week::within_week`]).
//! 2. Deterministic gate: [`ProjectEvidence::distinct_sessions`] must be
//!    `>= MIN_DISTINCT_SESSIONS` — below that, abstain before any spend is
//!    even considered. Independent evidence, not receipt count, is the bar.
//! 3. [`revision_hash`] — `blake3` over the evidence's sorted receipt-bearing
//!    strings plus [`PROMPT_VERSION`]. A cache hit in `dreams_v1` at
//!    `(project, "strategy", revision_hash)` reuses the stored prose VERBATIM
//!    — zero spend, not even a budget check.
//! 4. On a cache miss: invoke the model chain through
//!    [`crate::dream::threads::invoke_chain`] with a
//!    [`crate::dream::policy::BudgetedActor`]-wrapped actor (never
//!    `ProcessActor` bare — every invocation must count against the shared
//!    nightly budget the same way `dream::threads`'s own passes do), record
//!    usage via [`crate::dream::threads::record_attempts`], then
//!    [`parse_strategy_reply`] defensively. A literal `ABSTAIN` or anything
//!    that doesn't parse as the four required sections abstains — never a
//!    partial card — and nothing is cached for a miss (retried next run,
//!    same as `dream::threads`'s own "no usable reply" contract).
//!
//! # Category competition (decided in `cli::handle`)
//!
//! A project can get at most one dream. When both categories produce a
//! candidate for the same project, strategy wins the slot whenever it
//! produced ANY [`RenderedStrategy`] — a cache hit counts as "passed
//! authoring" exactly like a fresh call, since both mean "the corpus
//! currently supports a strategy judgment for this project". Only when
//! strategy has nothing (gate failed, kill-switched, or the model itself
//! abstained/malformed) does the unfinished card fill the slot. See the
//! comment at the call site in `cli::handle` for where this is decided.

use std::collections::BTreeSet;

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use crate::dream::policy::Budget;
use crate::dream::threads::{self, DreamThread, NightActor, Receipt};
use crate::journal::composer::{self, PlanStep};
use crate::journal::week;
use crate::storage::dream_clusters::OpenItem;
use crate::storage::Storage;

/// The category name stored in `dreams_v1.category` for every row this
/// module writes.
pub(crate) const CATEGORY: &str = "strategy";

/// Folded into every strategy [`revision_hash`] — bump on any change to the
/// prompt contract or the four-section rendering, so every cached row is
/// treated as stale on the next run.
pub(crate) const PROMPT_VERSION: &str = "dreams-strategy-v1";

/// Deterministic authoring gate: a project needs at least this many distinct
/// origin sessions of week evidence (open items + threads combined) before
/// ANY spend is considered. Independent corroboration, not a single loud
/// session, is what earns a strategy call.
const MIN_DISTINCT_SESSIONS: usize = 3;

// ─── evidence bundle ────────────────────────────────────────────────────

/// One open item's contribution to a project's evidence bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EvidenceOpenItem {
    item_id: String,
    title: String,
    kind: String,
    origin_session: String,
    origin_date: String,
    /// Composed from the item's stored plan (if any) — see
    /// [`compose_how_line`]. Empty when no plan is on record.
    how_lines: Vec<String>,
}

/// One night-pass thread's contribution to a project's evidence bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EvidenceThread {
    session_id: String,
    thread: String,
    /// Formatted receipt strings — see [`format_receipt`].
    receipts: Vec<String>,
}

/// Everything [`build_prompt`] and [`revision_hash`] read: one project's week
/// evidence, corpus data only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectEvidence {
    project: String,
    open_items: Vec<EvidenceOpenItem>,
    threads: Vec<EvidenceThread>,
}

impl ProjectEvidence {
    /// Distinct origin sessions across both evidence channels — the
    /// authoring gate's own measure of "independent corroboration".
    fn distinct_sessions(&self) -> usize {
        let mut sessions: BTreeSet<&str> = BTreeSet::new();
        for oi in &self.open_items {
            sessions.insert(oi.origin_session.as_str());
        }
        for t in &self.threads {
            sessions.insert(t.session_id.as_str());
        }
        sessions.len()
    }

    fn is_empty(&self) -> bool {
        self.open_items.is_empty() && self.threads.is_empty()
    }
}

/// First 8 chars of an oid/session id — the same short-receipt convention
/// `journal::week`'s how-line composer and the board's receipt lines use.
fn short_oid(raw: &str) -> &str {
    raw.get(..8).unwrap_or(raw)
}

/// One how-line from a stored plan step: `"{action} ⌗{short citation}"`, or
/// just the action when the step carries no citation. A step with a blank
/// action contributes nothing (mirrors `journal::week`'s private
/// `compose_how_line` — that function is not reachable from here, so this is
/// a deliberately smaller sibling: no file-basename segment, since the
/// receipt is what a strategy fact needs, not the file).
fn compose_how_line(step: &PlanStep) -> Option<String> {
    let action = step.action.trim();
    if action.is_empty() {
        return None;
    }
    let citation = step.citation.trim();
    if citation.is_empty() {
        return Some(action.to_string());
    }
    Some(format!("{action} ⌗{}", short_oid(citation)))
}

/// One receipt line, formatted for the prompt. `Verdict` receipts show the
/// verdict plus a short oid when one exists; `Witnessed` receipts show the
/// file and how many times it was witnessed.
fn format_receipt(receipt: &Receipt) -> String {
    match receipt {
        Receipt::Verdict {
            symbol,
            verdict,
            receipt_oid,
            witnessed_at: _,
        } => {
            let oid = receipt_oid.as_deref().map(short_oid).unwrap_or("no-oid");
            match symbol {
                Some(s) => format!("{s} {verdict} ⌗{oid}"),
                None => format!("{verdict} ⌗{oid}"),
            }
        }
        Receipt::Witnessed {
            file,
            witness_count,
        } => {
            format!("{file} (witnessed {witness_count}x)")
        }
    }
}

/// Build one project's evidence bundle: this week's still-open items (with
/// stored-plan how-lines when a plan exists) and this week's night-pass
/// threads, both scoped to `project` and the rolling window ending at `now`.
///
/// `open_items` and `dream_threads` are the caller's already-loaded, unfiltered
/// pools (`dream_clusters::load_open_items` / `threads::load_dream_threads`)
/// — this function only filters and shapes them; it issues one `load_plan`
/// query per candidate open item.
pub(crate) fn build_project_evidence(
    conn: &Connection,
    project: &str,
    open_items: &[OpenItem],
    dream_threads: &[DreamThread],
    now: DateTime<Utc>,
) -> Result<ProjectEvidence> {
    let mut items = Vec::new();
    for oi in open_items.iter().filter(|i| {
        i.project == project && i.completed.is_none() && week::within_week(&i.origin_ts, now)
    }) {
        let how_lines = composer::load_plan(conn, &oi.id)?
            .map(|plan| plan.steps.iter().filter_map(compose_how_line).collect())
            .unwrap_or_default();
        items.push(EvidenceOpenItem {
            item_id: oi.id.clone(),
            title: oi.item.clone(),
            kind: oi.kind.clone(),
            origin_session: oi.origin_session.clone(),
            origin_date: oi.origin_date.clone(),
            how_lines,
        });
    }

    let mut threads_ev = Vec::new();
    for t in dream_threads.iter().filter(|t| {
        t.project == project && !t.thread.is_empty() && week::within_week(&t.created_at, now)
    }) {
        threads_ev.push(EvidenceThread {
            session_id: t.session_id.clone(),
            thread: t.thread.clone(),
            receipts: t.receipts.iter().map(format_receipt).collect(),
        });
    }

    Ok(ProjectEvidence {
        project: project.to_string(),
        open_items: items,
        threads: threads_ev,
    })
}

// ─── revision hash ──────────────────────────────────────────────────────

/// Every receipt-bearing string in the bundle, sorted — the content that, if
/// it changes, must invalidate the cache. Order-independent by construction
/// (sorted), so re-running against an unchanged corpus in a different
/// iteration order still hits the cache.
fn evidence_fingerprint(evidence: &ProjectEvidence) -> Vec<String> {
    let mut ids = Vec::new();
    for oi in &evidence.open_items {
        ids.push(format!("item:{}:{}:{}", oi.item_id, oi.kind, oi.title));
        for how in &oi.how_lines {
            ids.push(format!("how:{}:{how}", oi.item_id));
        }
    }
    for t in &evidence.threads {
        ids.push(format!("thread:{}:{}", t.session_id, t.thread));
        for r in &t.receipts {
            ids.push(format!("receipt:{}:{r}", t.session_id));
        }
    }
    ids.sort();
    ids
}

/// `blake3(PROMPT_VERSION || sorted evidence fingerprint)`, truncated to 16
/// bytes (32 hex chars) — same truncation length as `cli`'s sha256-based
/// dream ids, just a different hash function per the design's explicit
/// choice for this cache key. Stable across reordering of the input data
/// (the fingerprint is sorted first); changes whenever the evidence set
/// changes.
pub(crate) fn revision_hash(evidence: &ProjectEvidence) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PROMPT_VERSION.as_bytes());
    for id in evidence_fingerprint(evidence) {
        hasher.update(b"\0");
        hasher.update(id.as_bytes());
    }
    let digest = hasher.finalize();
    digest.to_hex()[..32].to_string()
}

// ─── prompt ─────────────────────────────────────────────────────────────

const STRATEGY_INSTRUCTIONS: &str = "\
INSTRUCTIONS: Author ONE strategy judgment for this project from the \
evidence above, nothing else. Do not invent a fact, receipt, or file name \
that is not present in the evidence above. If the evidence above does not \
support a defensible judgment, output the single word ABSTAIN and nothing \
else — no partial answer, no hedge, no explanation.\n\
\n\
Otherwise output EXACTLY these sections, in this order, plain text, no \
markdown, no commentary before or after:\n\
\n\
Observed:\n\
  - one bullet per material fact you are using. EVERY bullet MUST end with \
a receipt copied verbatim from the evidence above (a token starting with \
\u{2317}). A fact with no receipt must be dropped, never stated anyway.\n\
Dream's take \u{2014} not a fact: one paragraph of judgment, clearly your \
own reasoning, not a fact.\n\
Verify first: homework the corpus above cannot answer on its own (omit this \
line entirely if nothing needs verifying).\n\
Proposal \u{2014} requires verdict: ONE directive. It must be \
verification-first \u{2014} never a free-form execution command \u{2014} \
naming what to confirm before acting.\n";

/// Render the structured evidence + instructions the model sees. Every line
/// the model could cite carries the `⌗` receipt token it is instructed to
/// copy verbatim.
fn build_prompt(evidence: &ProjectEvidence) -> String {
    let mut p = String::new();
    p.push_str(&format!(
        "You are authoring one strategy dream for project \"{}\" from the corpus \
         evidence below.\n\n",
        evidence.project
    ));
    p.push_str(
        "EVIDENCE (this is the ONLY material you may cite; never invent anything \
         beyond it):\n\n",
    );
    p.push_str("Open items still unfinished this week:\n");
    if evidence.open_items.is_empty() {
        p.push_str("  (none)\n");
    }
    for oi in &evidence.open_items {
        p.push_str(&format!(
            "  - [{}] \"{}\" ⌗{} (session ⌗{}, {})\n",
            oi.kind,
            oi.title,
            oi.item_id,
            short_oid(&oi.origin_session),
            oi.origin_date
        ));
        for how in &oi.how_lines {
            p.push_str(&format!("      how: {how}\n"));
        }
    }
    p.push_str("\nNight-pass threads this week:\n");
    if evidence.threads.is_empty() {
        p.push_str("  (none)\n");
    }
    for t in &evidence.threads {
        p.push_str(&format!(
            "  - \"{}\" (session ⌗{})\n",
            t.thread,
            short_oid(&t.session_id)
        ));
        for r in &t.receipts {
            p.push_str(&format!("      receipt: {r}\n"));
        }
    }
    p.push('\n');
    p.push_str(STRATEGY_INSTRUCTIONS);
    p
}

// ─── defensive parse ────────────────────────────────────────────────────

const HDR_OBSERVED: &str = "Observed:";
const HDR_TAKE: &str = "Dream's take — not a fact:";
const HDR_VERIFY: &str = "Verify first:";
const HDR_PROPOSAL: &str = "Proposal — requires verdict:";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedStrategyCard {
    observed: Vec<String>,
    take: String,
    verify: Option<String>,
    proposal: String,
}

/// Parse a reply into the four required sections, or `None` for anything
/// that isn't a clean, fully-formed card — a literal `ABSTAIN`, a missing
/// section, sections out of order, an empty required section, or an
/// `Observed` bullet with no `⌗` receipt. There is no partial-credit path:
/// malformed output and an explicit `ABSTAIN` are indistinguishable to the
/// caller, both meaning "no strategy dream this run".
fn parse_strategy_reply(raw: &str) -> Option<ParsedStrategyCard> {
    let text = raw.trim();
    if text == "ABSTAIN" {
        return None;
    }

    let observed_pos = text.find(HDR_OBSERVED)?;
    let take_pos = text.find(HDR_TAKE)?;
    let proposal_pos = text.find(HDR_PROPOSAL)?;
    if !(observed_pos < take_pos && take_pos < proposal_pos) {
        return None;
    }
    let verify_pos = text.find(HDR_VERIFY);
    if let Some(vp) = verify_pos {
        if !(take_pos < vp && vp < proposal_pos) {
            return None;
        }
    }

    let observed_block = &text[observed_pos + HDR_OBSERVED.len()..take_pos];
    let observed: Vec<String> = observed_block
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.trim_start_matches('-').trim().to_string())
        .collect();
    if observed.is_empty() || !observed.iter().all(|l| l.contains('⌗')) {
        return None;
    }

    let take_end = verify_pos.unwrap_or(proposal_pos);
    let take = text[take_pos + HDR_TAKE.len()..take_end].trim().to_string();
    if take.is_empty() {
        return None;
    }

    let verify = verify_pos
        .map(|vp| text[vp + HDR_VERIFY.len()..proposal_pos].trim().to_string())
        .filter(|s| !s.is_empty());

    let proposal_tail = text[proposal_pos + HDR_PROPOSAL.len()..].trim();
    let proposal = proposal_tail
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    if proposal.is_empty() {
        return None;
    }

    Some(ParsedStrategyCard {
        observed,
        take,
        verify,
        proposal,
    })
}

/// Render a parsed card into the same plain-text card shape
/// [`crate::dream::cli`]'s unfinished cards use, ending with the existing
/// dream attribution marker.
fn render_strategy_card(project: &str, parsed: &ParsedStrategyCard, dream_id: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("PROJECT {project} — strategy\n"));
    out.push_str("Observed:\n");
    for line in &parsed.observed {
        out.push_str("  - ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!("Dream's take — not a fact: {}\n", parsed.take));
    if let Some(verify) = &parsed.verify {
        out.push_str("Verify first: ");
        out.push_str(verify);
        out.push('\n');
    }
    out.push_str(&format!(
        "Proposal — requires verdict: {}\n",
        parsed.proposal
    ));
    out.push_str(&crate::storage::dream_attribution::marker_line(dream_id));
    out.push('\n');
    out
}

// ─── cache ──────────────────────────────────────────────────────────────

/// `(dream_id, prose)` of the newest `dreams_v1` row at
/// `(project, CATEGORY, revision_hash)`, if one exists. `None` on any error
/// (fail-open to "no cache hit, author fresh" — a cache lookup must never be
/// the reason a strategy dream disappears).
fn lookup_cached(
    storage: &Storage,
    project: &str,
    revision_hash: &str,
) -> Option<(String, String)> {
    storage
        .with_connection(|conn| {
            conn.query_row(
                "SELECT dream_id, prose FROM dreams_v1 \
                 WHERE project = ?1 AND category = ?2 AND revision_hash = ?3 \
                 ORDER BY id DESC LIMIT 1",
                params![project, CATEGORY, revision_hash],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(anyhow::Error::from)
        })
        .ok()
        .flatten()
}

// ─── authoring ──────────────────────────────────────────────────────────

/// One strategy dream, ready to print — either freshly authored or a
/// verbatim cache hit. `cache_hit` is exposed for tests and for anything
/// `status`-shaped that later wants to distinguish spend from reuse; the
/// category-competition logic in `cli::handle` treats both the same way
/// (see the module doc).
pub(crate) struct RenderedStrategy {
    pub(crate) dream_id: String,
    pub(crate) text: String,
    #[allow(dead_code)] // observability seam, not consumed by this slice's renderer
    pub(crate) cache_hit: bool,
}

/// Author (or reuse) one project's strategy dream. `actor` must already be
/// budget-wrapped (`policy::BudgetedActor`) by the caller — this function
/// additionally consults `budget` directly so a cache hit is never blocked by
/// an exhausted budget, and so a genuine miss under an exhausted budget is
/// counted as queued rather than silently dropped.
///
/// Returns `None` when: the evidence gate isn't met, the actor never
/// produced a usable reply, or the reply parsed as `ABSTAIN`/malformed. Every
/// `None` path is a deliberate abstention, never an error swallowed silently
/// — non-fatal issues are logged at `debug` (per-project spend decisions are
/// routine, not warning-worthy).
pub(crate) fn author_for_project(
    storage: &Storage,
    actor: &dyn NightActor,
    budget: &Budget,
    evidence: &ProjectEvidence,
    now: DateTime<Utc>,
) -> Option<RenderedStrategy> {
    if evidence.is_empty() || evidence.distinct_sessions() < MIN_DISTINCT_SESSIONS {
        tracing::debug!(
            project = %evidence.project,
            distinct_sessions = evidence.distinct_sessions(),
            "strategy dream: below the distinct-session gate, abstaining"
        );
        return None;
    }

    let rev_hash = revision_hash(evidence);

    if let Some((dream_id, prose)) = lookup_cached(storage, &evidence.project, &rev_hash) {
        return Some(RenderedStrategy {
            dream_id,
            text: prose,
            cache_hit: true,
        });
    }

    if budget.exhausted() {
        budget.note_queued();
        tracing::debug!(
            project = %evidence.project,
            "strategy dream: nightly budget exhausted, queued for next run"
        );
        return None;
    }

    let prompt = build_prompt(evidence);
    let chain = threads::thread_model_candidates();
    let result = threads::invoke_chain(actor, &chain, &prompt);
    let failures = threads::record_attempts(
        storage,
        &result.attempts,
        "dreams_cli_strategy",
        Some(&rev_hash),
    );
    if failures > 0 {
        tracing::debug!(
            project = %evidence.project,
            failures,
            "strategy dream: usage accounting had failures (counted in status)"
        );
    }

    let text = result.text?;
    let parsed = parse_strategy_reply(&text).or_else(|| {
        tracing::debug!(
            project = %evidence.project,
            "strategy dream reply was ABSTAIN or did not parse; abstaining"
        );
        None
    })?;

    let dream_id = super::cli::compute_dream_id(&evidence.project, CATEGORY, &rev_hash, now);
    let card_text = render_strategy_card(&evidence.project, &parsed, &dream_id);

    if let Err(error) = super::cli::record_dream_row(
        storage,
        &dream_id,
        &evidence.project,
        CATEGORY,
        None,
        &rev_hash,
        &card_text,
    ) {
        // The call was already paid for; dropping the card because the
        // insert failed would waste the spend for nothing. The next run
        // simply re-authors (and re-pays) since nothing was cached.
        tracing::warn!(%error, project = %evidence.project, "failed to persist strategy dream row");
    }

    Some(RenderedStrategy {
        dream_id,
        text: card_text,
        cache_hit: false,
    })
}

/// `true` when the whole strategy category must be skipped this run:
/// `--no-llm`, `CSR_NO_AI_NARRATIVES=1`, or `CSR_NO_DREAMING=1`. A `true`
/// here means no gate check, no cache lookup, no invocation — the category
/// contributes nothing to any project's slot.
pub(crate) fn category_disabled(no_llm: bool) -> bool {
    no_llm
        || crate::narrative::narratives_disabled()
        || crate::daemon::dream_cadence::dreaming_disabled()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dream::threads::{ActorAttempt, ReceiptTier};

    /// Serializes tests that touch the process-global kill-switch env vars
    /// this module reads via `category_disabled` — same idiom as
    /// `dream::threads`'s own `ENV_LOCK`.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn clear_kill_switch_env() {
        std::env::remove_var("CSR_NO_AI_NARRATIVES");
        std::env::remove_var("CSR_NO_DREAMING");
    }

    fn evidence(project: &str, sessions: &[&str]) -> ProjectEvidence {
        ProjectEvidence {
            project: project.to_string(),
            open_items: sessions
                .iter()
                .enumerate()
                .map(|(i, s)| EvidenceOpenItem {
                    item_id: format!("item{i}"),
                    title: format!("title {i}"),
                    kind: "todo".to_string(),
                    origin_session: s.to_string(),
                    origin_date: "2026-08-10".to_string(),
                    how_lines: vec![format!("do thing {i} ⌗oid{i:04}abcd")],
                })
                .collect(),
            threads: vec![],
        }
    }

    fn thread(id: i64, project: &str, session: &str, thread_text: &str) -> DreamThread {
        DreamThread {
            id,
            episode_hash: format!("hash{id}"),
            session_id: session.to_string(),
            project: project.to_string(),
            thread: thread_text.to_string(),
            evidence_quote: "quote".to_string(),
            files: vec![],
            receipt_tier: ReceiptTier::Unverified,
            receipts: vec![],
            model: "sonnet-5".to_string(),
            created_at: "2026-08-10 10:00:00".to_string(),
        }
    }

    // --- revision hash -----------------------------------------------------

    #[test]
    fn revision_hash_is_stable_for_identical_content_and_reordered_receipts() {
        let a = evidence("csr", &["sess-a", "sess-b", "sess-c"]);
        let mut b = a.clone();
        b.open_items.reverse();
        assert_eq!(
            revision_hash(&a),
            revision_hash(&b),
            "reordering the same evidence must not change the hash"
        );
    }

    #[test]
    fn revision_hash_changes_when_the_evidence_set_changes() {
        let a = evidence("csr", &["sess-a", "sess-b", "sess-c"]);
        let mut b = a.clone();
        b.open_items.push(EvidenceOpenItem {
            item_id: "extra".to_string(),
            title: "extra title".to_string(),
            kind: "blocker".to_string(),
            origin_session: "sess-d".to_string(),
            origin_date: "2026-08-11".to_string(),
            how_lines: vec![],
        });
        assert_ne!(
            revision_hash(&a),
            revision_hash(&b),
            "a changed evidence set must invalidate the cache"
        );
    }

    // --- gate ----------------------------------------------------------

    #[test]
    fn below_the_session_gate_abstains_without_touching_the_actor() {
        let storage = Storage::open_memory().unwrap();
        let calls = std::cell::Cell::new(0_usize);
        let inner = |_m: Option<&str>, _p: &str| {
            calls.set(calls.get() + 1);
            ActorAttempt::Failed("must never be called".to_string())
        };
        let budget = Budget::new(10);
        let actor = crate::dream::policy::BudgetedActor::new(&inner, &budget);
        let ev = evidence("csr", &["sess-a", "sess-b"]); // only 2 distinct sessions
        let now = Utc::now();

        let result = author_for_project(&storage, &actor, &budget, &ev, now);
        assert!(result.is_none());
        assert_eq!(
            calls.get(),
            0,
            "the actor must never be invoked below the gate"
        );
    }

    // --- cache ----------------------------------------------------------

    #[test]
    fn a_cache_hit_reuses_prose_verbatim_and_never_calls_the_actor() {
        let storage = Storage::open_memory().unwrap();
        let ev = evidence("csr", &["sess-a", "sess-b", "sess-c"]);
        let rev_hash = revision_hash(&ev);

        super::super::cli::record_dream_row(
            &storage,
            "cachedid00000000",
            "csr",
            CATEGORY,
            None,
            &rev_hash,
            "PROJECT csr — strategy\ncached prose\n",
        )
        .unwrap();

        let calls = std::cell::Cell::new(0_usize);
        let inner = |_m: Option<&str>, _p: &str| {
            calls.set(calls.get() + 1);
            ActorAttempt::Failed("must never be called on a cache hit".to_string())
        };
        let budget = Budget::new(10);
        let actor = crate::dream::policy::BudgetedActor::new(&inner, &budget);
        let now = Utc::now();

        let result = author_for_project(&storage, &actor, &budget, &ev, now).unwrap();
        assert_eq!(result.dream_id, "cachedid00000000");
        assert_eq!(result.text, "PROJECT csr — strategy\ncached prose\n");
        assert!(result.cache_hit);
        assert_eq!(calls.get(), 0, "a cache hit must never invoke the actor");
    }

    // --- parse -----------------------------------------------------------

    #[test]
    fn parse_rejects_a_literal_abstain() {
        assert!(parse_strategy_reply("ABSTAIN").is_none());
        assert!(parse_strategy_reply("  ABSTAIN  ").is_none());
    }

    #[test]
    fn parse_rejects_malformed_output_missing_a_required_section() {
        // No "Proposal —" section at all.
        let text = "Observed:\n  - fact one ⌗abcd1234\nDream's take — not a fact: judgment\n";
        assert!(parse_strategy_reply(text).is_none());
    }

    #[test]
    fn parse_rejects_an_observed_line_with_no_receipt() {
        let text = "Observed:\n  - fact with no receipt\nDream's take — not a fact: j\nProposal — requires verdict: do x";
        assert!(parse_strategy_reply(text).is_none());
    }

    #[test]
    fn parse_accepts_a_well_formed_reply_with_and_without_verify() {
        let with_verify = "Observed:\n  - fact one ⌗abcd1234\n  - fact two ⌗efgh5678\n\
             Dream's take — not a fact: this is the judgment paragraph.\n\
             Verify first: confirm the deploy actually happened.\n\
             Proposal — requires verdict: check the release notes first.";
        let parsed = parse_strategy_reply(with_verify).unwrap();
        assert_eq!(parsed.observed.len(), 2);
        assert_eq!(
            parsed.verify.as_deref(),
            Some("confirm the deploy actually happened.")
        );
        assert_eq!(parsed.proposal, "check the release notes first.");

        let without_verify = "Observed:\n  - fact one ⌗abcd1234\n\
             Dream's take — not a fact: judgment.\n\
             Proposal — requires verdict: do the thing.";
        let parsed2 = parse_strategy_reply(without_verify).unwrap();
        assert!(parsed2.verify.is_none());
    }

    // --- authoring end-to-end (fake actor, no shell-out) ------------------

    #[test]
    fn a_well_formed_reply_is_rendered_cached_and_returned() {
        let storage = Storage::open_memory().unwrap();
        let ev = evidence("csr", &["sess-a", "sess-b", "sess-c"]);
        let inner = |_m: Option<&str>, _p: &str| {
            ActorAttempt::Parsed(crate::narrative::ParsedNarrative {
                text: "Observed:\n  - fact one ⌗oid00000abcd\n\
                       Dream's take — not a fact: this needs attention.\n\
                       Proposal — requires verdict: verify the item is still open."
                    .to_string(),
                model: "sonnet-5".to_string(),
                input_tokens: 10,
                output_tokens: 10,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            })
        };
        let budget = Budget::new(10);
        let actor = crate::dream::policy::BudgetedActor::new(&inner, &budget);
        let now = Utc::now();

        let result = author_for_project(&storage, &actor, &budget, &ev, now).unwrap();
        assert!(!result.cache_hit);
        assert!(result.text.starts_with("PROJECT csr — strategy\n"));
        assert!(result.text.contains("Proposal — requires verdict:"));

        // A second run with the same evidence must hit the cache instead of
        // calling the actor again.
        let calls = std::cell::Cell::new(0_usize);
        let inner2 = |_m: Option<&str>, _p: &str| {
            calls.set(calls.get() + 1);
            ActorAttempt::Failed("must not be called".to_string())
        };
        let budget2 = Budget::new(10);
        let actor2 = crate::dream::policy::BudgetedActor::new(&inner2, &budget2);
        let second = author_for_project(&storage, &actor2, &budget2, &ev, now).unwrap();
        assert!(second.cache_hit);
        assert_eq!(second.dream_id, result.dream_id);
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn an_abstain_reply_is_never_cached_and_returns_none() {
        let storage = Storage::open_memory().unwrap();
        let ev = evidence("csr", &["sess-a", "sess-b", "sess-c"]);
        let inner = |_m: Option<&str>, _p: &str| {
            ActorAttempt::Parsed(crate::narrative::ParsedNarrative {
                text: "ABSTAIN".to_string(),
                model: "sonnet-5".to_string(),
                input_tokens: 5,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            })
        };
        let budget = Budget::new(10);
        let actor = crate::dream::policy::BudgetedActor::new(&inner, &budget);
        let now = Utc::now();

        assert!(author_for_project(&storage, &actor, &budget, &ev, now).is_none());
        assert!(
            lookup_cached(&storage, "csr", &revision_hash(&ev)).is_none(),
            "an ABSTAIN reply must never be cached"
        );
    }

    // --- kill switches ---------------------------------------------------

    #[test]
    fn no_llm_flag_disables_the_category_regardless_of_env() {
        let _g = env_guard();
        clear_kill_switch_env();
        assert!(category_disabled(true));
        clear_kill_switch_env();
    }

    #[test]
    fn csr_no_ai_narratives_disables_the_category() {
        let _g = env_guard();
        clear_kill_switch_env();
        std::env::set_var("CSR_NO_AI_NARRATIVES", "1");
        assert!(category_disabled(false));
        clear_kill_switch_env();
    }

    #[test]
    fn csr_no_dreaming_disables_the_category() {
        let _g = env_guard();
        clear_kill_switch_env();
        std::env::set_var("CSR_NO_DREAMING", "1");
        assert!(category_disabled(false));
        clear_kill_switch_env();
    }

    #[test]
    fn category_is_enabled_when_nothing_is_set() {
        let _g = env_guard();
        clear_kill_switch_env();
        assert!(!category_disabled(false));
    }

    // --- evidence bundle builder ------------------------------------------

    #[test]
    fn build_project_evidence_scopes_to_project_and_the_rolling_week() {
        use crate::storage::dream_clusters::CompletionReceipt;

        let now = DateTime::parse_from_rfc3339("2026-08-16T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let open_items = vec![
            OpenItem {
                id: "a".to_string(),
                project: "csr".to_string(),
                item: "in scope".to_string(),
                kind: "todo".to_string(),
                origin_session: "sess-a".to_string(),
                origin_ts: "2026-08-15T00:00:00Z".to_string(),
                origin_date: "2026-08-15".to_string(),
                completed: None,
                examined: true,
            },
            OpenItem {
                id: "b".to_string(),
                project: "other".to_string(),
                item: "wrong project".to_string(),
                kind: "todo".to_string(),
                origin_session: "sess-b".to_string(),
                origin_ts: "2026-08-15T00:00:00Z".to_string(),
                origin_date: "2026-08-15".to_string(),
                completed: None,
                examined: true,
            },
            OpenItem {
                id: "c".to_string(),
                project: "csr".to_string(),
                item: "completed".to_string(),
                kind: "todo".to_string(),
                origin_session: "sess-c".to_string(),
                origin_ts: "2026-08-15T00:00:00Z".to_string(),
                origin_date: "2026-08-15".to_string(),
                completed: Some(CompletionReceipt {
                    session_id: "later".into(),
                    completed_at: "2026-08-16T00:00:00Z".into(),
                    completed_date: "2026-08-16".into(),
                }),
                examined: true,
            },
            OpenItem {
                id: "d".to_string(),
                project: "csr".to_string(),
                item: "too old".to_string(),
                kind: "todo".to_string(),
                origin_session: "sess-d".to_string(),
                origin_ts: "2026-07-01T00:00:00Z".to_string(),
                origin_date: "2026-07-01".to_string(),
                completed: None,
                examined: true,
            },
        ];
        let dream_threads = vec![
            thread(1, "csr", "sess-e", "in scope thread"),
            thread(2, "other", "sess-f", "wrong project thread"),
        ];

        let storage = Storage::open_memory().unwrap();
        let evidence = storage
            .with_connection(|conn| {
                build_project_evidence(conn, "csr", &open_items, &dream_threads, now)
            })
            .unwrap();

        assert_eq!(evidence.open_items.len(), 1);
        assert_eq!(evidence.open_items[0].item_id, "a");
        assert_eq!(evidence.threads.len(), 1);
        assert_eq!(evidence.threads[0].session_id, "sess-e");
    }
}
