//! Server state and the **view-model boundary** the cluster feed (P2) swaps.
//!
//! P1 renders the feed that already exists — `storage::dream_items::
//! load_dream_items` — but the routes never touch that function directly.
//! They go through [`DreamFeed`], and they render [`CardView`] /
//! [`DetailView`], which are built here. When P2 lands
//! `load_dream_clusters`, it implements `DreamFeed` (or replaces
//! `StorageDreamFeed`'s body) and rebuilds the same two view structs; no
//! route, template, or test needs to move.
//!
//! Every rendered string in this module comes from
//! `crate::dream::report`'s already-certified projection helpers
//! (`dream_card_lines`, `dream_card_meta`, `dream_detail_evidence_lines`,
//! `dream_grade_slug_label`), so the live surface and the static
//! `dream --report` export cannot drift in wording. In particular the
//! verdict-phrase map's ban on "still live"/"re-verified" wording is
//! inherited, not re-implemented.
//!
//! Honesty contract carried into the views:
//!
//! * Counts are counts of rows actually loaded. Nothing is derived from the
//!   absence of evidence — an empty feed renders an explicit "nothing on
//!   record" state, never a fabricated zero-that-means-something-else.
//! * `witnessed` / `receipt` are `Option`: a missing receipt drops the
//!   clause, it never becomes a placeholder oid.
//! * The last-pass label is `None` until the ledger actually has an event.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;

use super::astdiff::{self, AstDiffCache, AstDiffOutcome, AstDiffRequest, SymbolNode};
use super::composer::{self, Brief, CopyBlock, DreamSpend, StoredPlan, PLAN_LABEL};
use crate::dream::report as projection;
use crate::storage::dream_items::{self, ChurnTile, DreamItem, DreamItemGrade};
use crate::storage::Storage;

/// Default page size for `/` and `/api/dreams`.
pub const DEFAULT_PAGE_LIMIT: usize = 20;
/// Hard ceiling on a caller-supplied `limit`, so a hostile-looking query
/// string cannot make the server render an unbounded page.
pub const MAX_PAGE_LIMIT: usize = 50;

/// The feed behind the journal. One method loads the ranked items; the
/// optional second reports the last dream pass so the masthead can show it
/// (and drop the clause when there has never been a pass).
///
/// Implementations run **blocking** SQLite work — routes call them inside
/// `spawn_blocking`, never on a runtime thread.
pub trait DreamFeed: Send + Sync + 'static {
    fn load(&self) -> Result<Vec<DreamItem>>;

    /// `(observed_head_oid, created_at)` of the newest ledger event, or
    /// `None` when nothing has ever been witnessed.
    fn last_pass(&self) -> Option<(String, String)> {
        None
    }

    /// Everything the detail page composes from stored rows: brief, copy
    /// block, verified plan, spend, AST slot, churn.
    ///
    /// The default returns [`DetailContext::default`] — an honest "nothing
    /// on record" for every section — so a feed that carries only items
    /// (the route tests, and any future fixture feed) still renders a
    /// truthful page rather than a fabricated one.
    fn detail_context(&self, _item: &DreamItem) -> DetailContext {
        DetailContext::default()
    }

    /// Per-dream spend for a landing card. `None` renders nothing at all —
    /// never a zero (locked decision 13).
    fn spend_for(&self, _item: &DreamItem) -> Option<DreamSpend> {
        None
    }
}

/// Everything the detail route needs beyond the item itself. Every field is
/// `Option`/empty-by-default: absence is rendered as absence.
#[derive(Debug, Clone, Default)]
pub struct DetailContext {
    pub brief: Brief,
    pub copy_block: Option<CopyBlock>,
    pub plan: Option<StoredPlan>,
    pub spend: Option<DreamSpend>,
    /// The two OIDs the AST slot compares, when both are on record.
    pub ast: Option<AstSlot>,
    /// Why the AST slot cannot render, when it cannot. Exactly one of
    /// `ast`/`ast_abstention` is `Some` in a well-formed context; when both
    /// are `None` the view renders the generic not-attempted sentence.
    pub ast_abstention: Option<String>,
    /// Measured churn tiles for the item's project. Empty means nothing was
    /// counted — the caption says so rather than showing an empty chart.
    pub churn: Vec<ChurnTile>,
}

/// A rendered AST comparison plus the witnessed-symbol set that decides
/// which nodes may be drawn solid.
#[derive(Debug, Clone)]
pub struct AstSlot {
    /// `Arc` because the LRU hands out shared results — the view never
    /// clones a diff just to read it.
    pub outcome: Arc<AstDiffOutcome>,
    /// `(symbol, receipt_oid)` pairs actually present in the witness
    /// evidence for this file. Only these may render solid.
    pub witnessed: Vec<(String, Option<String>)>,
}

/// Churn window, in days, for the detail page's context strip.
const CHURN_WINDOW_DAYS: u32 = 30;
/// Churn rows rendered. The count shown is the count measured.
const CHURN_MAX_ROWS: usize = 8;

/// Production feed: the existing `load_dream_items` over the shared
/// `Storage` connection, plus the P4 composer feeds.
pub struct StorageDreamFeed {
    storage: Arc<Storage>,
    /// Bounded LRU over `journal::astdiff` results, so re-visiting a detail
    /// page does not re-read two git blobs and re-parse them.
    ast_cache: AstDiffCache,
}

impl StorageDreamFeed {
    pub fn new(storage: Arc<Storage>) -> Self {
        Self {
            storage,
            ast_cache: AstDiffCache::new(),
        }
    }

    /// The witness ledger's `at_oid` for the newest witness matching this
    /// item's evidence — the AST comparison's *before* side. `None` when no
    /// row carries one, which makes the slot abstain rather than guess.
    fn before_oid(&self, project: &str, file: &str, symbol: Option<&str>) -> Option<String> {
        self.storage
            .with_connection(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT at_oid FROM witness_ledger
                     WHERE project = ?1 AND file = ?2 AND at_oid IS NOT NULL
                       AND (?3 IS NULL OR symbol = ?3)
                     ORDER BY id DESC LIMIT 1",
                )?;
                let mut rows = stmt.query(rusqlite::params![project, file, symbol])?;
                Ok(match rows.next()? {
                    Some(row) => row.get::<_, Option<String>>(0)?,
                    None => None,
                })
            })
            .ok()
            .flatten()
    }

    /// Build the AST slot, or the sentence explaining why it cannot be
    /// built. Never returns a half-populated slot.
    fn ast_slot(&self, item: &DreamItem) -> (Option<AstSlot>, Option<String>) {
        let Some(evidence) = item
            .evidence
            .iter()
            .find(|row| row.verdict != "proposal" && row.receipt_oid.is_some())
        else {
            return (
                None,
                Some(
                    "AST comparison abstained: no evidence row on this item carries a receipt \
                     oid, so there is no second commit to compare against."
                        .to_string(),
                ),
            );
        };
        let after_oid = evidence
            .receipt_oid
            .clone()
            .expect("filtered on receipt_oid being Some");
        let Some(before_oid) =
            self.before_oid(&item.project, &evidence.file, evidence.symbol.as_deref())
        else {
            return (
                None,
                Some(format!(
                    "AST comparison abstained: the witness ledger stores no commit for {} on \
                     the before side, so only one of the two commits is known.",
                    evidence.file
                )),
            );
        };
        let Some(repo_root) = self
            .storage
            .stored_repo_root_for_file(&evidence.file)
            .ok()
            .flatten()
        else {
            return (
                None,
                Some(format!(
                    "AST comparison abstained: no repository root is recorded for {}, so the \
                     two commits cannot be read.",
                    evidence.file
                )),
            );
        };
        let relative = evidence
            .file
            .strip_prefix(&repo_root)
            .unwrap_or(&evidence.file)
            .trim_start_matches('/')
            .to_string();

        let outcome = self.ast_cache.get_or_compute(&AstDiffRequest {
            repo_root: PathBuf::from(repo_root),
            path: relative,
            before_oid,
            after_oid,
        });

        let witnessed: Vec<(String, Option<String>)> = item
            .evidence
            .iter()
            .filter(|row| row.file == evidence.file)
            .filter_map(|row| {
                row.symbol
                    .clone()
                    .map(|symbol| (symbol, row.receipt_oid.clone()))
            })
            .collect();

        match outcome.as_ref() {
            AstDiffOutcome::Abstained(reason) => (None, Some(reason.sentence())),
            AstDiffOutcome::Diffed(_) => (Some(AstSlot { outcome, witnessed }), None),
        }
    }
}

impl DreamFeed for StorageDreamFeed {
    fn load(&self) -> Result<Vec<DreamItem>> {
        self.storage.with_connection(dream_items::load_dream_items)
    }

    fn last_pass(&self) -> Option<(String, String)> {
        self.storage.last_dream_run().ok().flatten()
    }

    fn detail_context(&self, item: &DreamItem) -> DetailContext {
        let loaded = self.storage.with_connection(|conn| {
            let episode = composer::episode_facts(conn, &item.origin_session)?;
            let threads = composer::threads_for_session(conn, &item.origin_session)?;
            let plan = composer::load_plan(conn, &item.id)?;
            let refs = composer::spend_refs(&threads, plan.as_ref());
            let spend = composer::load_spend(conn, &refs)?;
            let churn = dream_items::load_churn(conn, &item.project, CHURN_WINDOW_DAYS)?;
            Ok((episode, threads, plan, spend, churn))
        });
        let Ok((episode, threads, plan, spend, mut churn)) = loaded else {
            // A failed read renders as "nothing on record", never as
            // fabricated content — and the item's own sections still show.
            return DetailContext::default();
        };
        churn.truncate(CHURN_MAX_ROWS);

        let (ast, ast_abstention) = self.ast_slot(item);
        DetailContext {
            brief: composer::build_brief(item, &episode, &threads),
            copy_block: Some(composer::build_copy_block(
                item,
                &episode,
                &threads,
                plan.as_ref(),
            )),
            plan,
            spend,
            ast,
            ast_abstention,
            churn,
        }
    }

    fn spend_for(&self, item: &DreamItem) -> Option<DreamSpend> {
        self.storage
            .with_connection(|conn| {
                let threads = composer::threads_for_session(conn, &item.origin_session)?;
                let plan = composer::load_plan(conn, &item.id)?;
                let refs = composer::spend_refs(&threads, plan.as_ref());
                composer::load_spend(conn, &refs)
            })
            .ok()
            .flatten()
    }
}

/// A fixed feed. Used by the route tests (no database, no socket) and
/// available to P2 for fixture-driven cluster rendering.
pub struct StaticDreamFeed {
    items: Vec<DreamItem>,
    last_pass: Option<(String, String)>,
    context: Option<DetailContext>,
    spend: Option<DreamSpend>,
}

impl StaticDreamFeed {
    pub fn new(items: Vec<DreamItem>) -> Self {
        Self {
            items,
            last_pass: None,
            context: None,
            spend: None,
        }
    }

    pub fn with_last_pass(mut self, head_oid: &str, created_at: &str) -> Self {
        self.last_pass = Some((head_oid.to_string(), created_at.to_string()));
        self
    }

    /// Inject a composed detail context — how the P4 tests exercise the
    /// brief / copy block / plan / AST / churn sections without a database.
    pub fn with_context(mut self, context: DetailContext) -> Self {
        self.context = Some(context);
        self
    }

    /// Inject measured spend. Leaving it unset is what a dream with no
    /// recorded usage looks like, and the views must render nothing for it.
    pub fn with_spend(mut self, spend: DreamSpend) -> Self {
        self.spend = Some(spend);
        self
    }
}

impl DreamFeed for StaticDreamFeed {
    fn load(&self) -> Result<Vec<DreamItem>> {
        Ok(self.items.clone())
    }

    fn last_pass(&self) -> Option<(String, String)> {
        self.last_pass.clone()
    }

    fn detail_context(&self, _item: &DreamItem) -> DetailContext {
        self.context.clone().unwrap_or_default()
    }

    fn spend_for(&self, _item: &DreamItem) -> Option<DreamSpend> {
        self.spend.clone()
    }
}

/// Shared, cheaply-cloned server state.
#[derive(Clone)]
pub struct JournalState {
    feed: Arc<dyn DreamFeed>,
}

impl JournalState {
    pub fn new(feed: Arc<dyn DreamFeed>) -> Self {
        Self { feed }
    }

    /// Convenience constructor for the production wiring.
    pub fn from_storage(storage: Arc<Storage>) -> Self {
        Self::new(Arc::new(StorageDreamFeed::new(storage)))
    }

    pub fn feed(&self) -> Arc<dyn DreamFeed> {
        self.feed.clone()
    }
}

// --- view models -------------------------------------------------------------

/// Per-dream spend as rendered (locked decision 13). Built **only** from a
/// measured [`DreamSpend`]; there is no constructor that produces a zeroed
/// one, so a card with no recorded usage carries `spend: None` and the
/// template emits no spend markup at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpendView {
    /// `"1,240 in · 380 out"`.
    pub tokens: String,
    /// `"≈$0.0123 at list price"`, or an explicit unavailable sentence.
    pub cost: String,
    /// `true` when a real cost figure exists (so the template can style the
    /// unavailable case differently without re-parsing the string).
    pub costed: bool,
    /// `"2 calls · sonnet-5"`.
    pub detail: String,
}

impl SpendView {
    fn from_spend(spend: &DreamSpend) -> Self {
        let mut models = spend.models.clone();
        models.sort();
        models.dedup();
        Self {
            tokens: spend.tokens_label(),
            cost: spend.cost_label(),
            costed: spend.cost_usd.is_some(),
            detail: format!(
                "{} · {}",
                projection::pluralize(spend.calls.max(0) as usize, "call"),
                models.join(", ")
            ),
        }
    }
}

/// One node in a rendered AST side. `witnessed` decides the geometry: solid
/// for a symbol the witness ledger actually names, dotted for one the AST
/// engine resolved deterministically but nothing witnessed. There is no
/// third state, and a dotted node never borrows a witnessed node's receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AstNodeView {
    pub name: String,
    pub kind: String,
    pub status: &'static str,
    pub depth: usize,
    pub line: usize,
    /// `true` → solid. `false` → dotted, labelled `resolved`.
    pub witnessed: bool,
    /// Inline receipt for a witnessed node. `None` on every dotted node, and
    /// on a witnessed node whose evidence row stored no oid.
    pub receipt: Option<String>,
    /// Measured touch count, or `None`. Never a zero standing in for
    /// "unmeasured" — `astdiff::ChurnTint` makes that unrepresentable.
    pub touches: Option<u32>,
}

/// The AST slot: either a comparison, or an abstention sentence in the same
/// place. Never both, never neither.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct AstSlotView {
    pub rendered: bool,
    pub abstention: Option<String>,
    pub path: String,
    pub before_oid: String,
    pub after_oid: String,
    pub before: Vec<AstNodeView>,
    pub after: Vec<AstNodeView>,
    /// `"3 intact · 1 changed · 1 removed · 2 added"`.
    pub counts: String,
    /// `"showing 400 of 812 symbols"` when the engine capped a side.
    pub truncation: Option<String>,
}

/// Churn heat, rendered LAST and captioned as context (locked decision 10).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct ChurnView {
    pub rows: Vec<ChurnRowView>,
    /// Fixed caption. Says where activity concentrates; never says what
    /// matters, and never claims a ranking role.
    pub caption: &'static str,
    pub measured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChurnRowView {
    pub file: String,
    pub touches: u32,
    /// 0–100, relative to the busiest row shown. A tint width, not a score.
    pub width: u32,
}

/// The caption locked decision 10 mandates. Kept as a constant so a template
/// edit cannot quietly turn churn into a ranking signal.
pub const CHURN_CAPTION: &str =
    "CONTEXT · NOT USED FOR RANKING — where edit activity concentrated in this project \
     over the last 30 days. This says nothing about what matters.";

/// One card on the landing page. Field-for-field the static report's card,
/// plus the routing fields a live surface needs (`project`, `href`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CardView {
    pub id: String,
    pub project: String,
    /// Verb-first item text, verbatim from the open todo/blocker.
    pub item: String,
    pub kind: String,
    pub grade_slug: &'static str,
    pub grade_label: &'static str,
    /// Up to 2 evidence lines, wording owned by `dream::report`.
    pub dream_lines: Vec<String>,
    /// `"left open <date> · witnessed <date>[ · +N more]"`.
    pub meta: String,
    pub href: String,
    /// Measured spend, or `None` — which renders as nothing at all.
    pub spend: Option<SpendView>,
}

/// The detail shell. Field order here mirrors the render order the codex IA
/// memo mandates: evidence contract first, TOUCH NEXT second.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DetailView {
    pub id: String,
    pub project: String,
    pub title: String,
    pub kind: String,
    pub grade_slug: &'static str,
    pub grade_label: &'static str,
    /// ISO date of the newest evidence row, or `None` when the item has no
    /// evidence at all (the feed's gate makes that unreachable today; the
    /// `Option` keeps it honest if the gate ever loosens).
    pub witnessed: Option<String>,
    /// Short oid of the newest evidence row that actually carries a
    /// receipt. `None` drops the clause — never a placeholder.
    pub receipt: Option<String>,
    pub left_open: String,
    pub origin_session: String,
    /// Every evidence row, each with its own receipt clause or an explicit
    /// "no receipt".
    pub evidence_lines: Vec<String>,
    pub evidence_count: usize,
    /// What actually changed — the ground-shift itself, stated from the
    /// newest evidence row. Deliberately separate from `why_surfaced`:
    /// conflating "the code moved" with "this is why you are seeing it"
    /// is how a correlation gets read as a claim about the item.
    pub what_changed: String,
    /// Why this item surfaced — a sentence assembled from stored fields
    /// only, distinguishing the change from the reason it reached this item.
    pub why_surfaced: String,
    /// P4 composer output. Every one of these is independently droppable.
    pub brief: Brief,
    pub copy_block: Option<CopyBlock>,
    pub plan: Option<PlanView>,
    pub spend: Option<SpendView>,
    pub ast: AstSlotView,
    pub churn: ChurnView,
}

/// A stored, verified plan as rendered. Always labelled [`PLAN_LABEL`] and
/// always carrying each step's citation inline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanView {
    pub label: &'static str,
    pub context: Option<String>,
    pub steps: Vec<PlanStepView>,
    pub files: Vec<String>,
    pub acceptance: Option<String>,
    /// Sentence naming how many drafted steps the verifier removed, or
    /// `None` when it removed none. The dropped text itself is never shown
    /// in any form.
    pub dropped_note: Option<String>,
    pub model: String,
    /// `true` when the stored row is the convergence sentinel: the pass ran
    /// and verification kept nothing. Rendered as an explicit sentence, not
    /// as an empty section.
    pub empty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanStepView {
    pub ordinal: usize,
    pub action: String,
    pub citation: String,
    pub files: Vec<String>,
}

/// The landing page's full model, including its own pagination links so the
/// first page is complete in the DOM and the next page is a real `<a href>`
/// (the page works with JavaScript off).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LandingView {
    pub cards: Vec<CardView>,
    /// `"12 open items"` — a count of rows actually loaded.
    pub count_label: String,
    /// `"3 projects"`.
    pub project_label: String,
    /// `"last pass <date> · HEAD <oid8>"`, or `None` when the ledger has
    /// never recorded an event.
    pub pass_label: Option<String>,
    pub total: usize,
    pub shown_from: usize,
    pub shown_to: usize,
    pub next_href: Option<String>,
    pub prev_href: Option<String>,
    pub empty: bool,
}

/// One page of the JSON API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DreamsPage {
    pub items: Vec<CardView>,
    /// Offset to pass back as `cursor` for the next page; absent on the
    /// last page.
    pub next_cursor: Option<String>,
    /// Total rows in the feed — a measured count, not an estimate.
    pub total: usize,
}

/// A caller-supplied cursor that is not a decimal offset. Routes turn this
/// into a 400 rather than silently clamping, so a broken link is visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadCursor(pub String);

impl std::fmt::Display for BadCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid cursor {:?}: expected a decimal offset", self.0)
    }
}

/// Parse `cursor` into a row offset. Empty/absent means "start".
pub fn parse_cursor(cursor: Option<&str>) -> Result<usize, BadCursor> {
    match cursor.map(str::trim) {
        None | Some("") => Ok(0),
        Some(raw) => raw.parse::<usize>().map_err(|_| BadCursor(raw.to_string())),
    }
}

/// Clamp a caller-supplied limit into `1..=MAX_PAGE_LIMIT`. An absent or
/// zero limit falls back to the default rather than erroring — a page size
/// is a display preference, not evidence.
pub fn clamp_limit(limit: Option<usize>) -> usize {
    match limit {
        None | Some(0) => DEFAULT_PAGE_LIMIT,
        Some(n) => n.min(MAX_PAGE_LIMIT),
    }
}

fn href_for(id: &str) -> String {
    format!("/dream/{id}")
}

/// Project one `DreamItem` into a card, reusing the static report's
/// wording helpers verbatim. `spend` is passed in rather than looked up so
/// the projection stays pure and the feed owns every database read.
pub fn build_card(item: &DreamItem, spend: Option<&DreamSpend>) -> CardView {
    let (grade_slug, grade_label) = projection::dream_grade_slug_label(item.grade);
    CardView {
        id: item.id.clone(),
        project: item.project.clone(),
        item: item.item.clone(),
        kind: item.kind.clone(),
        grade_slug,
        grade_label,
        dream_lines: projection::dream_card_lines(&item.evidence, item.grade),
        meta: projection::dream_card_meta(item),
        href: href_for(&item.id),
        spend: spend.map(SpendView::from_spend),
    }
}

/// Project a stored plan into its view. A sentinel row (no steps) becomes
/// `empty: true` with no fabricated content.
pub fn build_plan_view(plan: &StoredPlan) -> PlanView {
    PlanView {
        label: PLAN_LABEL,
        context: (!plan.context.trim().is_empty()).then(|| plan.context.clone()),
        steps: plan
            .steps
            .iter()
            .enumerate()
            .map(|(index, step)| PlanStepView {
                ordinal: index + 1,
                action: step.action.clone(),
                citation: step.citation.clone(),
                files: step.files.clone(),
            })
            .collect(),
        files: plan.files.clone(),
        acceptance: plan.acceptance.clone(),
        dropped_note: (plan.dropped > 0).then(|| {
            format!(
                "{} dropped by the verifier — their claims did not trace to a stored row, \
                 a verbatim quote, or a receipt. The dropped text is not shown in any form.",
                projection::pluralize(plan.dropped, "drafted step")
            )
        }),
        model: plan.model.clone(),
        empty: plan.steps.is_empty(),
    }
}

/// Project churn tiles into the detail page's context strip. `width` is a
/// tint relative to the busiest row shown — never a rank, never a score.
pub fn build_churn_view(tiles: &[ChurnTile]) -> ChurnView {
    let peak = tiles.iter().map(|tile| tile.touches).max().unwrap_or(0);
    ChurnView {
        rows: tiles
            .iter()
            .map(|tile| ChurnRowView {
                file: tile.file.clone(),
                touches: tile.touches,
                width: if peak == 0 {
                    0
                } else {
                    ((tile.touches as u64 * 100) / peak as u64) as u32
                },
            })
            .collect(),
        caption: CHURN_CAPTION,
        measured: !tiles.is_empty(),
    }
}

fn ast_node_view(node: &SymbolNode, witnessed: &[(String, Option<String>)]) -> AstNodeView {
    let display = match &node.container {
        Some(container) => format!("{container}::{}", node.name),
        None => node.name.clone(),
    };
    // A node is solid ONLY when the witness ledger names that exact symbol.
    // Everything else the AST engine resolved is dotted — deterministic, but
    // not witnessed, and the two must never look alike.
    let hit = witnessed
        .iter()
        .find(|(symbol, _)| *symbol == node.name || *symbol == display);
    AstNodeView {
        name: display,
        kind: node.kind.clone(),
        status: node.status.label(),
        depth: node.depth,
        line: node.line,
        witnessed: hit.is_some(),
        receipt: hit
            .and_then(|(_, oid)| oid.as_deref())
            .map(projection::short_oid),
        touches: node.churn.touches(),
    }
}

/// Project the AST slot. An abstention is rendered *in the same slot* as a
/// diff would have been — the page never silently omits the section.
pub fn build_ast_view(slot: Option<&AstSlot>, abstention: Option<&str>) -> AstSlotView {
    let Some(slot) = slot else {
        return AstSlotView {
            rendered: false,
            abstention: Some(
                abstention
                    .unwrap_or(
                        "AST comparison abstained: no deterministic comparison was attempted \
                         for this item.",
                    )
                    .to_string(),
            ),
            ..AstSlotView::default()
        };
    };
    let Some(diff) = slot.outcome.diff() else {
        return AstSlotView {
            rendered: false,
            abstention: Some(
                slot.outcome
                    .abstention()
                    .map(astdiff::Abstention::sentence)
                    .unwrap_or_else(|| {
                        "AST comparison abstained: the engine returned no comparison.".to_string()
                    }),
            ),
            ..AstSlotView::default()
        };
    };
    AstSlotView {
        rendered: true,
        abstention: None,
        path: diff.path.clone(),
        before_oid: projection::short_oid(&diff.before_oid),
        after_oid: projection::short_oid(&diff.after_oid),
        before: diff
            .before
            .iter()
            .map(|node| ast_node_view(node, &slot.witnessed))
            .collect(),
        after: diff
            .after
            .iter()
            .map(|node| ast_node_view(node, &slot.witnessed))
            .collect(),
        counts: format!(
            "{} intact · {} changed · {} removed · {} added",
            diff.counts.intact, diff.counts.changed, diff.counts.removed, diff.counts.added
        ),
        truncation: diff.truncated().then(|| {
            format!(
                "showing {} of {} symbols before, {} of {} after",
                diff.before.len(),
                diff.before_total,
                diff.after.len(),
                diff.after_total
            )
        }),
    }
}

/// The "why these items surfaced" sentence. Item-grade and session-grade
/// qualify through different channels and must not be described the same
/// way — the session-grade wording never claims the item's own symbol was
/// witnessed, because it was not.
fn why_surfaced(item: &DreamItem) -> String {
    match item.grade {
        DreamItemGrade::ItemGrade => format!(
            "This {} names ground that was witnessed changing: the evidence below matches the \
             item's own symbols.",
            item.kind
        ),
        DreamItemGrade::SessionGrade => format!(
            "This {} is still open from a session whose touched files overlap witnessed ground. \
             The evidence below is about that ground, not about the item's own symbols.",
            item.kind
        ),
    }
}

/// "What changed" — the ground-shift itself, read off the newest evidence
/// row. Kept apart from [`why_surfaced`] on purpose: the codex IA memo's
/// correction 3 requires the description to distinguish the change from the
/// reason these items surfaced, and one sentence doing both jobs is exactly
/// how a file-overlap correlation gets read as a claim about the item.
fn what_changed(item: &DreamItem) -> String {
    let Some(newest) = item
        .evidence
        .iter()
        .find(|evidence| evidence.verdict != "proposal")
    else {
        return "No verdict row is stored for this item, so nothing is claimed to have changed."
            .to_string();
    };
    let subject = match &newest.symbol {
        Some(symbol) => format!("`{symbol}` in {}", newest.file),
        None => newest.file.clone(),
    };
    let receipt = match &newest.receipt_oid {
        Some(oid) => format!(" with receipt ⌗{}", projection::short_oid(oid)),
        None => ", with no receipt stored".to_string(),
    };
    format!(
        "What changed: {subject} was witnessed {} on {}{receipt}.",
        newest.verdict.replace('_', " "),
        projection::iso_date(&newest.witnessed_at)
    )
}

/// Project one `DreamItem` into the detail shell, with the P4 composer
/// sections attached.
pub fn build_detail(item: &DreamItem, context: &DetailContext) -> DetailView {
    let (grade_slug, grade_label) = projection::dream_grade_slug_label(item.grade);
    let newest = item.evidence.first();
    DetailView {
        what_changed: what_changed(item),
        brief: context.brief.clone(),
        copy_block: context.copy_block.clone(),
        plan: context.plan.as_ref().map(build_plan_view),
        spend: context.spend.as_ref().map(SpendView::from_spend),
        ast: build_ast_view(context.ast.as_ref(), context.ast_abstention.as_deref()),
        churn: build_churn_view(&context.churn),
        id: item.id.clone(),
        project: item.project.clone(),
        title: item.item.clone(),
        kind: item.kind.clone(),
        grade_slug,
        grade_label,
        witnessed: newest.map(|e| projection::iso_date(&e.witnessed_at)),
        receipt: item
            .evidence
            .iter()
            .find_map(|e| e.receipt_oid.as_deref())
            .map(projection::short_oid),
        left_open: projection::iso_date(&item.origin_ts),
        origin_session: item.origin_session.clone(),
        evidence_lines: projection::dream_detail_evidence_lines(&item.evidence, item.grade),
        evidence_count: item.evidence.len(),
        why_surfaced: why_surfaced(item),
    }
}

fn pass_label(last_pass: Option<(String, String)>) -> Option<String> {
    last_pass.map(|(head, created_at)| {
        format!(
            "last pass {} · HEAD {}",
            projection::iso_date(&created_at),
            projection::short_oid(&head)
        )
    })
}

fn page_href(offset: usize, limit: usize) -> String {
    if limit == DEFAULT_PAGE_LIMIT {
        format!("/?cursor={offset}")
    } else {
        format!("/?cursor={offset}&limit={limit}")
    }
}

/// Slice the feed into one landing page.
///
/// `spend_of` is a lookup rather than a field on the item so the projection
/// stays pure: it is only ever called for the rows actually shown, and a
/// feed that records no usage returns `None` for every one of them — which
/// renders as no spend markup at all.
pub fn build_landing(
    items: &[DreamItem],
    last_pass: Option<(String, String)>,
    offset: usize,
    limit: usize,
    spend_of: &dyn Fn(&DreamItem) -> Option<DreamSpend>,
) -> LandingView {
    let total = items.len();
    let start = offset.min(total);
    let end = start.saturating_add(limit).min(total);
    let cards: Vec<CardView> = items[start..end]
        .iter()
        .map(|item| build_card(item, spend_of(item).as_ref()))
        .collect();

    let mut projects: Vec<&str> = items.iter().map(|i| i.project.as_str()).collect();
    projects.sort_unstable();
    projects.dedup();

    LandingView {
        cards,
        count_label: projection::pluralize(total, "open item"),
        project_label: projection::pluralize(projects.len(), "project"),
        pass_label: pass_label(last_pass),
        total,
        // 1-based inclusive display range; both are 0 when the page is empty.
        shown_from: if end > start { start + 1 } else { 0 },
        shown_to: end,
        next_href: (end < total).then(|| page_href(end, limit)),
        prev_href: (start > 0).then(|| page_href(start.saturating_sub(limit), limit)),
        empty: total == 0,
    }
}

/// Slice the feed into one JSON API page.
pub fn build_page(
    items: &[DreamItem],
    offset: usize,
    limit: usize,
    spend_of: &dyn Fn(&DreamItem) -> Option<DreamSpend>,
) -> DreamsPage {
    let total = items.len();
    let start = offset.min(total);
    let end = start.saturating_add(limit).min(total);
    DreamsPage {
        items: items[start..end]
            .iter()
            .map(|item| build_card(item, spend_of(item).as_ref()))
            .collect(),
        next_cursor: (end < total).then(|| end.to_string()),
        total,
    }
}

/// Find one item by its stable id. The id is compared against ids that came
/// out of the feed — it is never interpolated into SQL, a path, or a shell
/// command, so an arbitrary `:id` in the URL can only ever 404.
pub fn find_item<'a>(items: &'a [DreamItem], id: &str) -> Option<&'a DreamItem> {
    items.iter().find(|item| item.id == id)
}

/// One fully-formed `DreamItem` for tests in this module tree (`render`'s
/// template tests and `routes`' oneshot tests both build feeds from it).
///
/// It lives outside `mod tests` because `routes` and `render` import it, and
/// re-exporting an item out of a test module trips
/// `clippy::items_after_test_module`.
#[cfg(test)]
pub(crate) fn sample_item(id: &str, project: &str, item: &str) -> DreamItem {
    use crate::storage::dream_items::DreamEvidence;

    DreamItem {
        id: id.to_string(),
        project: project.to_string(),
        item: item.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A feed with no recorded usage — the default every pre-P4 assertion
    /// keeps running under.
    fn no_spend(_item: &DreamItem) -> Option<DreamSpend> {
        None
    }

    fn items(n: usize) -> Vec<DreamItem> {
        (0..n)
            .map(|i| sample_item(&format!("id{i:02}"), "proj", &format!("item {i}")))
            .collect()
    }

    #[test]
    fn parse_cursor_accepts_absent_empty_and_decimal() {
        assert_eq!(parse_cursor(None), Ok(0));
        assert_eq!(parse_cursor(Some("")), Ok(0));
        assert_eq!(parse_cursor(Some("  ")), Ok(0));
        assert_eq!(parse_cursor(Some("42")), Ok(42));
    }

    #[test]
    fn parse_cursor_rejects_garbage_instead_of_clamping() {
        assert_eq!(parse_cursor(Some("-1")), Err(BadCursor("-1".to_string())));
        assert_eq!(
            parse_cursor(Some("'; DROP TABLE chunks--")),
            Err(BadCursor("'; DROP TABLE chunks--".to_string()))
        );
    }

    #[test]
    fn clamp_limit_bounds_the_page() {
        assert_eq!(clamp_limit(None), DEFAULT_PAGE_LIMIT);
        assert_eq!(clamp_limit(Some(0)), DEFAULT_PAGE_LIMIT);
        assert_eq!(clamp_limit(Some(7)), 7);
        assert_eq!(clamp_limit(Some(10_000)), MAX_PAGE_LIMIT);
    }

    #[test]
    fn build_page_walks_the_feed_and_stops() {
        let all = items(5);
        let first = build_page(&all, 0, 2, &no_spend);
        assert_eq!(first.items.len(), 2);
        assert_eq!(first.total, 5);
        assert_eq!(first.next_cursor.as_deref(), Some("2"));

        let second = build_page(&all, 2, 2, &no_spend);
        assert_eq!(second.items[0].id, "id02");
        assert_eq!(second.next_cursor.as_deref(), Some("4"));

        let last = build_page(&all, 4, 2, &no_spend);
        assert_eq!(last.items.len(), 1);
        assert_eq!(last.next_cursor, None, "last page must not advertise more");
    }

    #[test]
    fn build_page_past_the_end_is_empty_not_wrapped() {
        let all = items(3);
        let page = build_page(&all, 99, 10, &no_spend);
        assert!(page.items.is_empty());
        assert_eq!(page.total, 3);
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn landing_counts_are_measured_not_page_sized() {
        let all = items(5);
        let view = build_landing(&all, None, 0, 2, &no_spend);
        assert_eq!(view.total, 5);
        assert_eq!(view.count_label, "5 open items");
        assert_eq!(view.project_label, "1 project");
        assert_eq!(view.shown_from, 1);
        assert_eq!(view.shown_to, 2);
        assert_eq!(view.next_href.as_deref(), Some("/?cursor=2&limit=2"));
        assert_eq!(view.prev_href, None);
        assert!(!view.empty);
    }

    #[test]
    fn landing_on_empty_feed_says_so_and_invents_nothing() {
        let view = build_landing(&[], None, 0, DEFAULT_PAGE_LIMIT, &no_spend);
        assert!(view.empty);
        assert_eq!(view.total, 0);
        assert_eq!(view.count_label, "0 open items");
        assert_eq!(view.shown_from, 0);
        assert_eq!(view.shown_to, 0);
        assert_eq!(view.next_href, None);
        assert_eq!(
            view.pass_label, None,
            "no pass on record must drop, not zero"
        );
    }

    #[test]
    fn pass_label_renders_only_with_a_real_event() {
        assert_eq!(pass_label(None), None);
        assert_eq!(
            pass_label(Some((
                "0123456789abcdef".to_string(),
                "2026-08-09T12:00:00Z".to_string()
            ))),
            Some("last pass 2026-08-09 · HEAD 01234567".to_string())
        );
    }

    #[test]
    fn detail_drops_the_receipt_clause_when_none_is_stored() {
        let mut item = sample_item("id00", "proj", "finish the gate");
        item.evidence[0].receipt_oid = None;
        let detail = build_detail(&item, &DetailContext::default());
        assert_eq!(detail.receipt, None);
        assert!(detail
            .evidence_lines
            .iter()
            .any(|line| line.contains("no receipt")));
    }

    #[test]
    fn detail_wording_comes_from_the_certified_projection() {
        let item = sample_item("id00", "proj", "finish the gate");
        let detail = build_detail(&item, &DetailContext::default());
        assert_eq!(detail.witnessed.as_deref(), Some("2026-08-09"));
        assert_eq!(detail.receipt.as_deref(), Some("abcdef12"));
        assert!(detail.evidence_lines[0].contains("run_report anchor obsolete"));
        // The banned "still live"/"re-verified" framing must never appear.
        for line in &detail.evidence_lines {
            assert!(!line.contains("still live"), "banned wording: {line}");
            assert!(!line.contains("re-verified"), "banned wording: {line}");
        }
    }

    #[test]
    fn session_grade_detail_does_not_claim_the_items_own_symbol() {
        let mut item = sample_item("id00", "proj", "finish the gate");
        item.grade = DreamItemGrade::SessionGrade;
        let detail = build_detail(&item, &DetailContext::default());
        assert!(detail
            .why_surfaced
            .contains("not about the item's own symbols"));
        assert_eq!(detail.grade_slug, "session-grade");
    }

    #[test]
    fn find_item_matches_only_a_stored_id() {
        let all = items(3);
        assert!(find_item(&all, "id01").is_some());
        assert!(find_item(&all, "id99").is_none());
        assert!(find_item(&all, "../../etc/passwd").is_none());
    }

    #[test]
    fn static_feed_round_trips_through_the_trait() {
        let feed = StaticDreamFeed::new(items(2)).with_last_pass("deadbeefcafe", "2026-08-09");
        let state = JournalState::new(Arc::new(feed));
        let loaded = state.feed().load().expect("static feed load");
        assert_eq!(loaded.len(), 2);
        assert_eq!(
            state.feed().last_pass(),
            Some(("deadbeefcafe".to_string(), "2026-08-09".to_string()))
        );
    }
}
